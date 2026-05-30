use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use bytes::{Buf, BufMut, BytesMut};

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{ActiveWorkflowSlot, peer_status::LastSeen},
    utils,
    workflow::{
        COORDINATOR_STEP_STALENESS_THRESHOLD, StepStalenessWatchdog,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

use super::{
    KickConfig, KickStep,
    steps::{create_proposals, export_state, submit_kick},
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    kick_config: KickConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    active_workflow: ActiveWorkflowSlot,
) -> Result {
    tracing::info!("Initializing Noise server...");

    // Exclude the participant being kicked from peers
    let excluded_participants = vec![kick_config.participant_id.to_string()];

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        kick_config.instance_name.clone(),
        KickStep::WaitingForPeers,
        Some(excluded_participants),
        last_seen,
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let kick_config = kick_config.clone();
        let db = db.clone();
        let instance_name = kick_config.instance_name.clone();

        tokio::spawn(async move {
            let mut coordinator_completed_steps = HashSet::new();
            let mut watchdog = StepStalenessWatchdog::new(COORDINATOR_STEP_STALENESS_THRESHOLD);

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");
                watchdog.check(current_step)?;

                match current_step {
                    KickStep::WaitingForPeers => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::ExportState => {
                        if !coordinator_completed_steps.contains(&KickStep::ExportState) {
                            tracing::info!("Coordinator executing: Export state");
                            export_state(&node_config, &db, &instance_name, &kick_config).await?;
                            coordinator_completed_steps.insert(KickStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &db, &instance_name, &kick_config).await?;

                        // Load kick proposals to send to peers with SignKick command.
                        // Combine kick config + DNS and P2P kick proposals into a single
                        // length-prefixed payload (decoded as 3 items by the peer).
                        let dns_data = db
                            .read_artifact(&instance_name, artifact_kinds::KICK_DNS_PROPOSAL, None)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "KICK_DNS_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;
                        let p2p_data = db
                            .read_artifact(&instance_name, artifact_kinds::KICK_P2P_PROPOSAL, None)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "KICK_P2P_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;

                        let config_data = serde_json::to_vec(&kick_config)
                            .context("Failed to serialize kick config")?;

                        let payload =
                            utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                        workflow_state.set_command_payload(payload).await;
                        workflow_state.advance_step().await;
                    }
                    KickStep::SignProposals => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::SubmitKick => {
                        tracing::info!("Coordinator executing: Submit kick");

                        // Each peer sent us a single buffer containing two
                        // length-prefixed protobufs (DNS sig followed by P2P sig).
                        // Split that pair back into separate per-peer artefacts so
                        // submit_kick can `list_artifacts(SIGNED_KICK_DNS)` and
                        // `list_artifacts(SIGNED_KICK_P2P)` and join by peer id.
                        let peer_data = workflow_state.get_all_peer_data().await;
                        for (peer_id, combined) in &peer_data {
                            let (dns_blob, p2p_blob) = split_signed_kick_pair(combined)
                                .with_context(|| {
                                    format!(
                                        "Failed to split signed kick pair from \
                                         peer {peer_id}"
                                    )
                                })?;
                            let peer_key = peer_id.to_string();
                            db.write_artifact(
                                &instance_name,
                                artifact_kinds::SIGNED_KICK_DNS,
                                Some(&peer_key),
                                &dns_blob,
                            )
                            .await?;
                            db.write_artifact(
                                &instance_name,
                                artifact_kinds::SIGNED_KICK_P2P,
                                Some(&peer_key),
                                &p2p_blob,
                            )
                            .await?;
                        }
                        workflow_state.clear_peer_data().await;

                        submit_kick(&node_config, &db, &instance_name).await?;
                        workflow_state.advance_step().await;
                    }
                    KickStep::Complete => {
                        tracing::info!("Kick workflow complete!");
                        tracing::debug!("Waiting for peers to receive Disconnect command...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Kick(server),
        active_workflow,
        coordinator_workflow,
    )
    .await
}

/// Split the combined `varint(len_dns)||dns_proto||varint(len_p2p)||p2p_proto`
/// buffer peers send back into two separate `varint(len)||proto` blobs.
/// The output blobs are byte-identical to what each peer wrote as its
/// own `SIGNED_KICK_DNS` / `SIGNED_KICK_P2P` artefact, so re-encoding via
/// `read_first_message_from_bytes` round-trips cleanly.
fn split_signed_kick_pair(combined: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut cursor: &[u8] = combined;

    let dns_len = prost::encoding::decode_varint(&mut cursor)? as usize;
    if cursor.remaining() < dns_len {
        anyhow::bail!(
            "Truncated combined kick signatures: expected {dns_len} DNS bytes, have {remaining}",
            remaining = cursor.remaining()
        );
    }
    let dns_proto = &cursor[..dns_len];
    let dns_blob = encode_length_prefixed_bytes(dns_proto);
    cursor.advance(dns_len);

    let p2p_len = prost::encoding::decode_varint(&mut cursor)? as usize;
    if cursor.remaining() < p2p_len {
        anyhow::bail!(
            "Truncated combined kick signatures: expected {p2p_len} P2P bytes, have {remaining}",
            remaining = cursor.remaining()
        );
    }
    let p2p_proto = &cursor[..p2p_len];
    let p2p_blob = encode_length_prefixed_bytes(p2p_proto);
    cursor.advance(p2p_len);

    if cursor.has_remaining() {
        anyhow::bail!(
            "Trailing {remaining} bytes after parsing combined kick signatures",
            remaining = cursor.remaining()
        );
    }

    Ok((dns_blob, p2p_blob))
}

/// Re-emit a single proto with its varint length prefix. Round-trips the
/// `varint(len)||proto` framing produced by `utils::write_message_to_file`.
fn encode_length_prefixed_bytes(proto: &[u8]) -> Vec<u8> {
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(proto.len() as u64, &mut buffer);
    buffer.put_slice(proto);
    buffer.to_vec()
}

// Tests below exercise the round-trip between the per-peer blob format
// (what sign.rs writes to storage) and the combined wire-format the peer
// sends to the coordinator.
#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `varint(len)||bytes` blob — the per-message framing the
    /// production sign step uses, here applied to opaque payload bytes so the
    /// round-trip test stays decoupled from any concrete protobuf shape.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = BytesMut::new();
        prost::encoding::encode_varint(payload.len() as u64, &mut out);
        out.put_slice(payload);
        out.to_vec()
    }

    #[test]
    fn split_round_trips_for_two_messages() -> Result<()> {
        let dns_payload: &[u8] = &[1, 2, 3, 4];
        let p2p_payload: &[u8] = &[5, 6, 7, 8, 9, 10];

        let dns_blob = frame(dns_payload);
        let p2p_blob = frame(p2p_payload);

        let mut combined = Vec::new();
        combined.extend_from_slice(&dns_blob);
        combined.extend_from_slice(&p2p_blob);

        let (dns_back, p2p_back) = split_signed_kick_pair(&combined)?;
        assert_eq!(dns_back, dns_blob);
        assert_eq!(p2p_back, p2p_blob);
        Ok(())
    }

    #[test]
    fn split_rejects_trailing_garbage() {
        let mut combined = Vec::new();
        combined.extend_from_slice(&frame(&[1, 2]));
        combined.extend_from_slice(&frame(&[3, 4]));
        combined.push(0xff);
        assert!(split_signed_kick_pair(&combined).is_err());
    }

    #[test]
    fn split_rejects_truncated_p2p() {
        let mut combined = Vec::new();
        combined.extend_from_slice(&frame(&[1, 2]));
        // Encode a length larger than the remaining bytes.
        let mut header = BytesMut::new();
        prost::encoding::encode_varint(8, &mut header);
        combined.extend_from_slice(&header);
        combined.extend_from_slice(&[9, 9]);
        assert!(split_signed_kick_pair(&combined).is_err());
    }
}
