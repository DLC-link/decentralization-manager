use std::collections::HashSet;

use anyhow::Context;
use bytes::{Buf, BufMut, BytesMut};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::{MessageType, client::NoiseClient},
    workflow::{
        state::{WorkflowState, WorkflowStep},
        storage::WorkflowStorage,
        topology::DnsP2pArtifactKinds,
    },
};

/// Send this node's signed DNS + P2P pair to the coordinator as one
/// `message_type` message.
///
/// # Errors
/// Returns an error if either signed artefact is missing or the send fails.
pub(crate) async fn send_signed_pair(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
    kinds: DnsP2pArtifactKinds<'_>,
    message_type: MessageType,
) -> Result {
    let node_id = node_config.participant_id().to_string();
    let payload = read_signed_pair(storage, instance_name, &node_id, kinds).await?;
    client.send(message_type, payload).await?;
    Ok(())
}

/// `node_id`'s signed DNS and P2P artefacts back to back. Each is a
/// `varint(len)||proto` blob that `sign_dns_p2p_proposals` wrote, so the
/// result is the two-message buffer the coordinator splits again.
///
/// # Errors
/// Returns an error if either artefact is missing or the read fails.
async fn read_signed_pair(
    storage: &SqlitePool,
    instance_name: &str,
    node_id: &str,
    kinds: DnsP2pArtifactKinds<'_>,
) -> Result<Vec<u8>> {
    let dns = read_signed(storage, instance_name, node_id, kinds.signed_dns).await?;
    let p2p = read_signed(storage, instance_name, node_id, kinds.signed_p2p).await?;

    let mut payload = Vec::with_capacity(dns.len() + p2p.len());
    payload.extend_from_slice(&dns);
    payload.extend_from_slice(&p2p);
    Ok(payload)
}

/// One of `node_id`'s signed artefacts. The error names the kind in capitals,
/// as its constant is named.
async fn read_signed(
    storage: &SqlitePool,
    instance_name: &str,
    node_id: &str,
    kind: &str,
) -> Result<Vec<u8>> {
    storage
        .read_artifact(instance_name, kind, Some(node_id))
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{name} artifact missing for {node_id}",
                name = kind.to_ascii_uppercase()
            )
        })
}

/// Rebuild both per-peer artifacts from durable uploads on every submit.
/// Retaining the upload makes a restart between output writes recoverable.
pub(crate) async fn save_signature_pairs(
    storage: &impl WorkflowStorage,
    instance_name: &str,
    upload_kind: &str,
    dns_kind: &str,
    p2p_kind: &str,
) -> Result {
    for (peer_id, combined) in storage.list_artifacts(instance_name, upload_kind).await? {
        let (dns_blob, p2p_blob) = split_signed_pair(&combined)
            .with_context(|| format!("Failed to split {upload_kind} from peer {peer_id}"))?;
        storage
            .write_artifact(instance_name, dns_kind, Some(&peer_id), &dns_blob)
            .await?;
        storage
            .write_artifact(instance_name, p2p_kind, Some(&peer_id), &p2p_blob)
            .await?;
    }
    Ok(())
}

/// The expected peers that have no artifact of `artifact_kind`, sorted.
///
/// The check uses the expected set, because advancing a signing step clears
/// the completed set. Each caller writes its own error message.
pub(crate) async fn peers_without_artifact<S: WorkflowStep + 'static>(
    workflow_state: &WorkflowState<S>,
    storage: &impl WorkflowStorage,
    instance_name: &str,
    artifact_kind: &str,
) -> Result<Vec<String>> {
    let stored: HashSet<String> = storage
        .list_artifacts(instance_name, artifact_kind)
        .await?
        .into_iter()
        .map(|(peer, _)| peer)
        .collect();
    let mut missing: Vec<String> = workflow_state
        .expected_peers()
        .iter()
        .map(ToString::to_string)
        .filter(|peer| !stored.contains(peer))
        .collect();
    missing.sort();
    Ok(missing)
}

/// Split the combined `varint(len_dns)||dns_proto||varint(len_p2p)||p2p_proto`
/// buffer peers send back into two separate `varint(len)||proto` blobs.
/// Each output preserves the original length-prefixed protobuf framing.
pub(crate) fn split_signed_pair(combined: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut cursor: &[u8] = combined;

    let dns_len = prost::encoding::decode_varint(&mut cursor)? as usize;
    if cursor.remaining() < dns_len {
        anyhow::bail!(
            "Truncated combined signatures: expected {dns_len} DNS bytes, have {remaining}",
            remaining = cursor.remaining()
        );
    }
    let dns_proto = &cursor[..dns_len];
    let dns_blob = encode_length_prefixed_bytes(dns_proto);
    cursor.advance(dns_len);

    let p2p_len = prost::encoding::decode_varint(&mut cursor)? as usize;
    if cursor.remaining() < p2p_len {
        anyhow::bail!(
            "Truncated combined signatures: expected {p2p_len} P2P bytes, have {remaining}",
            remaining = cursor.remaining()
        );
    }
    let p2p_proto = &cursor[..p2p_len];
    let p2p_blob = encode_length_prefixed_bytes(p2p_proto);
    cursor.advance(p2p_len);

    if cursor.has_remaining() {
        anyhow::bail!(
            "Trailing {remaining} bytes after parsing combined signatures",
            remaining = cursor.remaining()
        );
    }

    Ok((dns_blob, p2p_blob))
}

/// Re-emit a single proto with its varint length prefix. Round-trips the
/// `varint(len)||proto` framing of `utils::encode_length_prefixed_message`.
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

        let (dns_back, p2p_back) = split_signed_pair(&combined)?;
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
        assert!(split_signed_pair(&combined).is_err());
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
        assert!(split_signed_pair(&combined).is_err());
    }

    mod storage {
        use sqlx::SqlitePool;

        use crate::{
            canton_id::{CantonId, NAMESPACE_LENGTH, Namespace},
            db::MIGRATOR,
            workflow::{
                add_party, change_threshold, kick,
                storage::{WorkflowStorage, artifact_kinds},
            },
        };

        use super::super::*;
        use super::frame;

        const RUN: &str = "run-1";

        /// `workflow_artifacts.instance_name` is a foreign key onto
        /// `workflow_runs`, so an artefact needs its run row first.
        async fn insert_run(pool: &SqlitePool) -> anyhow::Result<()> {
            sqlx::query(
                "INSERT INTO workflow_runs (
                     instance_name, kind, role, status, current_step, step_index, step_total,
                     config_json, expected_peers_json, completed_peers_json,
                     created_at, updated_at
                 ) VALUES (?1, 'Kick', 'Coordinator', 'inprogress', 'SubmitKick',
                           0, 1, '{}', '[]', '[]', 0, 0)",
            )
            .bind(RUN)
            .execute(pool)
            .await?;
            Ok(())
        }

        fn peer(n: u8) -> CantonId {
            CantonId::new(format!("p{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
        }

        /// What a peer sends is what the coordinator stores, half by half,
        /// and reading the halves back gives the same buffer.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn a_stored_pair_reads_back_as_the_sent_buffer(pool: SqlitePool) -> Result<()> {
            insert_run(&pool).await?;
            let (dns, p2p) = (frame(&[1, 2, 3]), frame(&[4, 5]));
            let combined = [dns.clone(), p2p.clone()].concat();
            let key = peer(1).to_string();
            pool.write_artifact(
                RUN,
                artifact_kinds::KICK_SIGNATURE_UPLOAD,
                Some(&key),
                &combined,
            )
            .await?;

            save_signature_pairs(
                &pool,
                RUN,
                artifact_kinds::KICK_SIGNATURE_UPLOAD,
                kick::ARTIFACTS.signed_dns,
                kick::ARTIFACTS.signed_p2p,
            )
            .await?;

            let stored_dns = pool
                .read_artifact(RUN, kick::ARTIFACTS.signed_dns, Some(&key))
                .await?;
            let stored_p2p = pool
                .read_artifact(RUN, kick::ARTIFACTS.signed_p2p, Some(&key))
                .await?;
            assert_eq!(stored_dns, Some(dns));
            assert_eq!(stored_p2p, Some(p2p));
            assert_eq!(
                read_signed_pair(&pool, RUN, &key, kick::ARTIFACTS).await?,
                combined
            );
            Ok(())
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn a_pair_that_does_not_split_names_the_upload_and_peer(
            pool: SqlitePool,
        ) -> Result<()> {
            insert_run(&pool).await?;
            let key = peer(2).to_string();
            pool.write_artifact(
                RUN,
                artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
                Some(&key),
                &[0xff],
            )
            .await?;

            let Err(e) = save_signature_pairs(
                &pool,
                RUN,
                artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
                change_threshold::ARTIFACTS.signed_dns,
                change_threshold::ARTIFACTS.signed_p2p,
            )
            .await
            else {
                panic!("a one-byte pair must not split");
            };

            assert_eq!(
                e.to_string(),
                format!("Failed to split change_threshold_signature_upload from peer {key}")
            );
            Ok(())
        }

        /// The error names the artefact the way the per-workflow senders
        /// did: the kind's constant name, in capitals.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn a_missing_half_is_named_in_capitals(pool: SqlitePool) -> Result<()> {
            insert_run(&pool).await?;

            let Err(e) = read_signed_pair(&pool, RUN, "p3", add_party::ARTIFACTS).await else {
                panic!("nothing was signed");
            };
            assert_eq!(
                e.to_string(),
                "SIGNED_ADD_PARTY_DNS artifact missing for p3"
            );

            pool.write_artifact(RUN, add_party::ARTIFACTS.signed_dns, Some("p3"), &[1])
                .await?;
            let Err(e) = read_signed_pair(&pool, RUN, "p3", add_party::ARTIFACTS).await else {
                panic!("the P2P half is missing");
            };
            assert_eq!(
                e.to_string(),
                "SIGNED_ADD_PARTY_P2P artifact missing for p3"
            );
            Ok(())
        }

        /// Peers and coordinators on other builds read these kinds from the
        /// database, so the strings must not move.
        #[test]
        fn artifact_kinds_are_pinned() {
            for (artifacts, prefix) in [
                (kick::ARTIFACTS, "kick"),
                (change_threshold::ARTIFACTS, "change_threshold"),
                (add_party::ARTIFACTS, "add_party"),
            ] {
                assert_eq!(artifacts.dns_proposal, format!("{prefix}_dns_proposal"));
                assert_eq!(artifacts.p2p_proposal, format!("{prefix}_p2p_proposal"));
                assert_eq!(artifacts.signed_dns, format!("signed_{prefix}_dns"));
                assert_eq!(artifacts.signed_p2p, format!("signed_{prefix}_p2p"));
            }
        }
    }
}
