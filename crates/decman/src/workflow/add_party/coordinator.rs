use std::sync::Arc;

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        kick::coordinator::split_signed_kick_pair,
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
    },
};

use super::{
    AddPartyConfig, AddPartyStep, resolve_ledger_token,
    steps::{
        clear_onboarding::wait_for_flag_cleared, collect_party_package_ids, create_proposals,
        export_party_acs, export_state, submit_clear_proposal, submit_proposals,
    },
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    add_party_config: AddPartyConfig,
    workflow_auth: Option<WorkflowAuth>,
    db: SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server for add-party...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        add_party_config.instance_name.clone(),
        AddPartyStep::WaitingForPeers,
        None, // the new member must connect — nobody is excluded
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let add_party_config_clone = add_party_config.clone();
    let db_clone = db.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config_clone,
            add_party_config_clone,
            workflow_auth,
            db_clone,
        )
        .await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::AddParty(server),
        instance,
        workflow_handle,
    )
    .await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<AddPartyStep>>,
    node_config: NodeConfig,
    add_party_config: AddPartyConfig,
    workflow_auth: Option<WorkflowAuth>,
    db: SqlitePool,
) -> Result {
    let instance_name = add_party_config.instance_name.clone();
    let ledger_token =
        resolve_ledger_token(&workflow_auth, &add_party_config.decentralized_party_id).await;
    let config_payload =
        serde_json::to_vec(&add_party_config).context("Failed to serialize add-party config")?;

    // GenerateAddPartyKeys is the first command after the connection gate and
    // carries the config; set it before the gate can auto-advance.
    workflow_state
        .set_command_payload(config_payload.clone())
        .await;

    loop {
        let current_step = workflow_state.current_step().await;
        tracing::debug!("Add-party coordinator in step: {current_step:?}");

        match current_step {
            AddPartyStep::WaitingForPeers
            | AddPartyStep::GenerateNewMemberKeys
            | AddPartyStep::SignProposals
            | AddPartyStep::SyncAcs
            | AddPartyStep::ProposeClearOnboarding
            | AddPartyStep::SignClearOnboarding => {
                // Peer-gated (or connection-gated) — the listener advances the
                // state as peers report in; the coordinator just idles.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            AddPartyStep::ExportState => {
                tracing::info!("Coordinator executing: Export state");
                save_new_member_keys(&workflow_state, &db, &instance_name, &add_party_config)
                    .await?;
                export_state(
                    &node_config,
                    &db,
                    &instance_name,
                    &add_party_config,
                    ledger_token.as_deref(),
                )
                .await?;
                workflow_state.advance_step().await;
            }
            AddPartyStep::CreateProposals => {
                tracing::info!("Coordinator executing: Create proposals");
                create_proposals(&node_config, &db, &instance_name, &add_party_config).await?;

                let dns_data = db
                    .read_artifact(&instance_name, artifact_kinds::ADD_PARTY_DNS_PROPOSAL, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "ADD_PARTY_DNS_PROPOSAL artifact missing after CreateProposals"
                        )
                    })?;
                let p2p_data = db
                    .read_artifact(&instance_name, artifact_kinds::ADD_PARTY_P2P_PROPOSAL, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "ADD_PARTY_P2P_PROPOSAL artifact missing after CreateProposals"
                        )
                    })?;

                let payload =
                    utils::encode_length_prefixed(&[&config_payload, &dns_data, &p2p_data]);
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::SubmitProposals => {
                tracing::info!("Coordinator executing: Submit proposals");
                save_signature_pairs(&workflow_state, &db, &instance_name).await?;
                submit_proposals(&node_config, &db, &instance_name, &add_party_config).await?;
                copy_new_member_identity(&db, &instance_name, &add_party_config).await?;

                // The topology is live; export the party's ACS for the new
                // member and ship it with the ImportAcs command. Empty when
                // the party has no active contracts — the new member skips.
                let snapshot =
                    export_party_acs(&node_config, &db, &instance_name, &add_party_config).await?;
                // Package ids the new member must have to validate the imported
                // ACS — its import preflight fails fast (before disconnecting) if
                // any are missing, instead of the import dying mid-window. Skip the
                // ledger scan when the snapshot is empty: no contracts, no packages.
                let package_ids = if snapshot.is_empty() {
                    Vec::new()
                } else {
                    let party_id = add_party_config.decentralized_party_id.to_string();
                    collect_party_package_ids(&node_config, &party_id, ledger_token.as_deref())
                        .await?
                };
                let package_ids_payload = package_ids.join("\n").into_bytes();
                let payload = utils::encode_length_prefixed(&[
                    &config_payload,
                    &snapshot,
                    &package_ids_payload,
                ]);
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::PrepareClearOnboarding => {
                // Swap the (potentially large) ACS payload for the bare
                // config before the next peer-gated command.
                workflow_state
                    .set_command_payload(config_payload.clone())
                    .await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::PrepareClearSign => {
                tracing::info!("Coordinator executing: Prepare clearing proposal");
                // The clearing proposal is AUTHORED BY THE NEW MEMBER (Canton
                // requires the onboarding participant to issue the flag-clear
                // transaction) and arrived as its ProposeClearOnboarding data
                // upload; other peers replied with skip statuses (no data).
                let peer_data = workflow_state.get_all_peer_data().await;
                let mut proposal_blob = Vec::new();
                for (peer_id, data) in peer_data {
                    if peer_id != add_party_config.new_participant_id {
                        anyhow::bail!(
                            "Unexpected clearing-proposal upload from {peer_id} — only the \
                             new member authors it"
                        );
                    }
                    proposal_blob = data;
                }
                workflow_state.clear_peer_data().await;

                if proposal_blob.is_empty() {
                    // The new member reported the flag already cleared.
                    // Verify against head state (bounded poll) before
                    // skipping the sign round — completing the workflow with
                    // the marker still set would leave the party suspended
                    // on the new member.
                    let synchronizer_id = utils::get_synchronizer_id(&node_config).await?;
                    wait_for_flag_cleared(
                        &node_config,
                        &synchronizer_id,
                        &add_party_config.decentralized_party_id,
                        &add_party_config.new_participant_id,
                    )
                    .await?;
                    tracing::info!("Onboarding flag already cleared — skipping sign round");
                }
                // An empty blob doubles as the "already cleared" skip marker
                // for both the peers and SubmitClearOnboarding.
                db.write_artifact(
                    &instance_name,
                    artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL,
                    None,
                    &proposal_blob,
                )
                .await?;

                let payload = utils::encode_length_prefixed(&[&config_payload, &proposal_blob]);
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::SubmitClearOnboarding => {
                let proposal = db
                    .read_artifact(
                        &instance_name,
                        artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL,
                        None,
                    )
                    .await?
                    .unwrap_or_default();
                if proposal.is_empty() {
                    tracing::info!("Onboarding flag already cleared — skipping submit");
                    workflow_state.clear_peer_data().await;
                } else {
                    tracing::info!("Coordinator executing: Submit clearing proposal");
                    save_clear_signatures(&workflow_state, &db, &instance_name).await?;
                    submit_clear_proposal(&node_config, &db, &instance_name, &add_party_config)
                        .await?;
                }
                workflow_state.advance_step().await;
            }
            AddPartyStep::Complete => {
                tracing::info!("Add-party workflow complete!");
                tracing::debug!("Waiting for peers to receive Disconnect command...");
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                break;
            }
        }
    }

    Ok(())
}

/// Persist the new member's `keys||participant_id` upload (the only data
/// payload of the GenerateNewMemberKeys step — other peers reply with a
/// skip status that carries no data).
async fn save_new_member_keys(
    workflow_state: &WorkflowState<AddPartyStep>,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    let peer_data = workflow_state.get_all_peer_data().await;
    let expected = &add_party_config.new_participant_id;

    let mut found = false;
    for (peer_id, data) in peer_data {
        if &peer_id != expected {
            anyhow::bail!(
                "Unexpected key upload from {peer_id} — only the new member \
                 {expected} generates keys in this run"
            );
        }
        let items = utils::decode_length_prefixed(&data, 2)
            .with_context(|| format!("Invalid key payload from new member {peer_id}"))?;
        let peer_key = peer_id.to_string();
        storage
            .write_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&peer_key),
                &items[0],
            )
            .await?;
        storage
            .write_artifact(
                instance_name,
                artifact_kinds::PARTICIPANT_ID,
                Some(&peer_key),
                &items[1],
            )
            .await?;
        found = true;
    }
    if !found {
        // Resume path: a coordinator restart drops the in-memory peer_data,
        // but a previous pass through this step may already have persisted
        // the artefacts — re-entering ExportState must not fail the run then.
        let already_saved = storage
            .read_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&expected.to_string()),
            )
            .await?
            .is_some();
        if !already_saved {
            anyhow::bail!(
                "New member {expected} completed GenerateNewMemberKeys without uploading keys"
            );
        }
        tracing::info!("New member keys already persisted (resumed run) — continuing");
    }

    workflow_state.clear_peer_data().await;
    Ok(())
}

/// Guard the resume path for a coordinator step that turns peer uploads into
/// per-peer artefacts.
///
/// `peer_data` lives only in memory (`WorkflowState::from_persisted` restores
/// `completed_peers` but re-initialises `peer_data` empty), while the peers
/// that uploaded are already marked `completed` and will never re-send. So a
/// coordinator restart in the window between the peer-gated step advancing and
/// this step writing its artefacts leaves nothing to write.
///
/// Writing nothing used to be silent: the loop over an empty map is a no-op,
/// and the submit step's `len == len` check passes at `0 == 0`, so the run
/// submitted carrying only the coordinator's own signature. For threshold > 1
/// Canton rejects that, and `retry_workflow` cannot recover it because the
/// peers stay complete.
///
/// So: nothing uploaded this pass is fine only if a previous pass already
/// persisted artefacts of this kind. Otherwise the signatures are gone, and
/// failing here names the reason instead of failing later as an opaque
/// under-signed submission.
async fn require_persisted_or_uploaded(
    storage: &SqlitePool,
    instance_name: &str,
    artifact_kind: &str,
    step: &str,
) -> Result {
    let persisted = storage.list_artifacts(instance_name, artifact_kind).await?;
    if persisted.is_empty() {
        anyhow::bail!(
            "{step}: peers are marked complete but no {artifact_kind} artefacts exist and no \
             uploads are buffered. The uploads were lost with the in-memory state, most likely a \
             coordinator restart mid-handoff; the peers will not re-send. Start a fresh add-party \
             run for this party."
        );
    }
    tracing::info!(
        "{step}: no uploads this pass; {count} {artifact_kind} artefact(s) already persisted \
         (resumed run) — continuing",
        count = persisted.len()
    );
    Ok(())
}

/// Split each peer's combined DNS||P2P signature upload into the two
/// per-peer artefacts the submit step joins by peer id.
async fn save_signature_pairs(
    workflow_state: &WorkflowState<AddPartyStep>,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    let peer_data = workflow_state.get_all_peer_data().await;
    if peer_data.is_empty() {
        require_persisted_or_uploaded(
            storage,
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            "SubmitProposals",
        )
        .await?;
    }
    for (peer_id, combined) in &peer_data {
        let (dns_blob, p2p_blob) = split_signed_kick_pair(combined).with_context(|| {
            format!("Failed to split signed add-party pair from peer {peer_id}")
        })?;
        let peer_key = peer_id.to_string();
        storage
            .write_artifact(
                instance_name,
                artifact_kinds::SIGNED_ADD_PARTY_DNS,
                Some(&peer_key),
                &dns_blob,
            )
            .await?;
        storage
            .write_artifact(
                instance_name,
                artifact_kinds::SIGNED_ADD_PARTY_P2P,
                Some(&peer_key),
                &p2p_blob,
            )
            .await?;
    }
    workflow_state.clear_peer_data().await;
    Ok(())
}

/// Persist each peer's signed clearing proposal (a single blob — no split).
async fn save_clear_signatures(
    workflow_state: &WorkflowState<AddPartyStep>,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    let peer_data = workflow_state.get_all_peer_data().await;
    if peer_data.is_empty() {
        require_persisted_or_uploaded(
            storage,
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
            "SubmitClearOnboarding",
        )
        .await?;
    }
    for (peer_id, data) in &peer_data {
        storage
            .write_artifact(
                instance_name,
                artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
                Some(&peer_id.to_string()),
                data,
            )
            .await?;
    }
    workflow_state.clear_peer_data().await;
    Ok(())
}

/// Identity hook: once the topology includes the new member, copy its
/// `PEER_PUBLIC_KEYS` + `PARTICIPANT_ID` artefacts into `dec_party_identity`
/// keyed by the party — post-add workflows (kick, contracts) on this node
/// read peer identities from there, and they must include the new member.
async fn copy_new_member_identity(
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    let party_id = &add_party_config.decentralized_party_id;
    let new_member = add_party_config.new_participant_id.to_string();

    for (workflow_kind, identity_kind) in [
        (
            artifact_kinds::PEER_PUBLIC_KEYS,
            identity_kinds::PEER_PUBLIC_KEYS,
        ),
        (
            artifact_kinds::PARTICIPANT_ID,
            identity_kinds::PARTICIPANT_ID,
        ),
    ] {
        if let Some(payload) = storage
            .read_artifact(instance_name, workflow_kind, Some(&new_member))
            .await?
        {
            storage
                .write_identity(party_id, identity_kind, &new_member, &payload)
                .await?;
        }
    }
    tracing::info!("Persisted new member identity for {party_id}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::MIGRATOR;

    /// The window this guards: peers uploaded, the peer-gated step advanced
    /// and persisted, then the coordinator restarted before the artefacts were
    /// written. `peer_data` comes back empty and the peers stay `completed`,
    /// so nothing will re-send.
    ///
    /// Before the guard this returned `Ok(())` and the run submitted with only
    /// the coordinator's own signature — Canton then rejected it for
    /// threshold > 1, with nothing pointing at the cause.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn no_uploads_and_no_artefacts_fails_loudly(pool: SqlitePool) -> anyhow::Result<()> {
        let err = require_persisted_or_uploaded(
            &pool,
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            "SubmitProposals",
        )
        .await
        .expect_err("empty uploads with no persisted artefacts must not pass");

        let msg = format!("{err}");
        assert!(
            msg.contains("coordinator restart"),
            "unhelpful error: {msg}"
        );
        assert!(msg.contains("will not re-send"), "unhelpful error: {msg}");
        Ok(())
    }

    /// The legitimate resume: a previous pass already wrote the artefacts, so
    /// re-entering the step with an empty buffer must continue rather than
    /// fail. Without this the guard would break every resumed run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn no_uploads_but_artefacts_present_continues(pool: SqlitePool) -> anyhow::Result<()> {
        pool.write_artifact(
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            Some("peer-a"),
            b"sig",
        )
        .await?;

        require_persisted_or_uploaded(
            &pool,
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            "SubmitProposals",
        )
        .await?;
        Ok(())
    }

    /// The two steps key on different artefact kinds, so a run that persisted
    /// DNS signatures must not thereby satisfy the clearing-signature guard.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_guard_is_per_artefact_kind(pool: SqlitePool) -> anyhow::Result<()> {
        pool.write_artifact(
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            Some("peer-a"),
            b"sig",
        )
        .await?;

        assert!(
            require_persisted_or_uploaded(
                &pool,
                "run-1",
                artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
                "SubmitClearOnboarding",
            )
            .await
            .is_err(),
            "DNS artefacts must not satisfy the clearing-signature guard"
        );
        Ok(())
    }
}
