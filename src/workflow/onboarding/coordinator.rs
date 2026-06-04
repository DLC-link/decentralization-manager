use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{ActiveWorkflowSlot, peer_status::LastSeen},
    utils,
    workflow::{
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

use super::{
    OnboardingConfig, OnboardingStep,
    steps::{create_proposals, generate_keys, submit_dns_proposals, submit_final_proposals},
};

/// Save each peer's combined `keys||participant_id` payload into
/// `workflow_artifacts` as two separate per-peer rows
/// (`PEER_PUBLIC_KEYS`, `PARTICIPANT_ID`). Mirrors how kick's coordinator
/// splits per-peer data.
async fn save_keys_and_ids<S: crate::workflow::state::WorkflowStep + 'static>(
    workflow_state: &WorkflowState<S>,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    let peer_data = workflow_state.get_all_peer_data().await;

    for (peer_id, data) in peer_data {
        let items = utils::decode_length_prefixed(&data, 2)
            .with_context(|| format!("Invalid payload from {peer_id}"))?;

        let keys_data = &items[0];
        let id_data = &items[1];
        let peer_key = peer_id.to_string();

        tracing::debug!(
            "Received from {peer_id}: {keys_len} bytes keys + {id_len} bytes participant ID",
            keys_len = keys_data.len(),
            id_len = id_data.len()
        );

        storage
            .write_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&peer_key),
                keys_data,
            )
            .await?;

        storage
            .write_artifact(
                instance_name,
                artifact_kinds::PARTICIPANT_ID,
                Some(&peer_key),
                id_data,
            )
            .await?;
    }

    workflow_state.clear_peer_data().await;
    Ok(())
}

/// Save each peer's signed proposal payload (DNS or P2P) into
/// `workflow_artifacts` keyed by peer id. The byte-shape stored matches
/// what the peer's sign step persisted locally (single
/// `varint(len)||proto`), so the submit step can reconstruct the original
/// SignedTopologyTransaction(s) via `read_first_message_from_bytes`.
async fn save_signed_proposals<S: crate::workflow::state::WorkflowStep + 'static>(
    workflow_state: &WorkflowState<S>,
    storage: &SqlitePool,
    instance_name: &str,
    artifact_kind: &str,
) -> Result {
    let peer_data = workflow_state.get_all_peer_data().await;

    for (peer_id, data) in peer_data {
        let peer_key = peer_id.to_string();
        storage
            .write_artifact(instance_name, artifact_kind, Some(&peer_key), &data)
            .await?;
    }

    workflow_state.clear_peer_data().await;
    Ok(())
}

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    onboarding_config: OnboardingConfig,
    db: SqlitePool,
    last_seen: LastSeen,
    active_workflow: ActiveWorkflowSlot,
) -> Result<CantonId> {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        onboarding_config.instance_name.clone(),
        OnboardingStep::WaitingForPeers,
        None, // No excluded participants
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let onboarding_config_clone = onboarding_config.clone();
    let db_clone = db.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config_clone,
            onboarding_config_clone,
            db_clone,
        )
        .await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Onboarding(server),
        active_workflow,
        workflow_handle,
    )
    .await?;

    // Read the resolved party id from workflow_artifacts (written by
    // CreateProposals). This survives across the await on the server task
    // even though the workflow's runtime artefacts will be cleaned up later.
    let party_id_bytes = db
        .read_artifact(
            &onboarding_config.instance_name,
            artifact_kinds::PARTY_ID,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("PARTY_ID artifact missing — did CreateProposals run?"))?;
    let party_id_str = String::from_utf8(party_id_bytes).context("Party ID is not valid UTF-8")?;
    CantonId::parse(party_id_str.trim())
}

/// Ensure the coordinator has generated its own onboarding keys and waited for
/// Canton to propagate the namespace delegations — exactly once per run.
///
/// The coordinator normally does this when it processes the `GenerateKeys` step.
/// But the workflow advances `GenerateKeys -> CreateProposals` as soon as every
/// *peer* has completed `GenerateKeys` (see `WorkflowState::peer_completed`),
/// independently of the coordinator's own loop. When peers complete quickly —
/// notably on resume, where they re-upload already-generated keys almost
/// instantly — the state can race past `GenerateKeys` before the coordinator
/// loop reaches that arm, skipping the key generation and the propagation wait.
/// `create_proposals` then fails with `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`.
/// Calling this at the start of `CreateProposals` as well makes the keys a
/// precondition of proposal creation regardless of the race. It is a no-op once
/// the keys have been generated.
async fn ensure_coordinator_keys(
    completed: &mut HashSet<OnboardingStep>,
    node_config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    onboarding_config: &OnboardingConfig,
) -> Result {
    if completed.contains(&OnboardingStep::GenerateKeys) {
        return Ok(());
    }
    tracing::info!("Coordinator executing: Generate keys");
    generate_keys(node_config, db, instance_name, onboarding_config).await?;
    completed.insert(OnboardingStep::GenerateKeys);
    tracing::info!("Waiting 3 seconds for Canton to process namespace delegations...");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    Ok(())
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<OnboardingStep>>,
    node_config: NodeConfig,
    onboarding_config: OnboardingConfig,
    db: SqlitePool,
) -> Result {
    let instance_name = onboarding_config.instance_name.clone();
    let mut coordinator_completed_steps = HashSet::new();

    // Set the onboarding config as payload for GenerateKeys step
    let config_payload =
        serde_json::to_vec(&onboarding_config).context("Failed to serialize onboarding config")?;
    workflow_state.set_command_payload(config_payload).await;

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            OnboardingStep::WaitingForPeers => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::GenerateKeys => {
                ensure_coordinator_keys(
                    &mut coordinator_completed_steps,
                    &node_config,
                    &db,
                    &instance_name,
                    &onboarding_config,
                )
                .await?;
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::CreateProposals => {
                // The GenerateKeys arm above is skipped entirely if peers advance
                // the state past it before this loop reaches it (common on
                // resume); make the coordinator's keys a precondition here so the
                // proposals can be signed with them. No-op if already generated.
                ensure_coordinator_keys(
                    &mut coordinator_completed_steps,
                    &node_config,
                    &db,
                    &instance_name,
                    &onboarding_config,
                )
                .await?;
                tracing::info!("Coordinator executing: Create proposals");
                // Save peer keys and participant IDs uploaded during GenerateKeys step
                save_keys_and_ids(&workflow_state, &db, &instance_name).await?;
                create_proposals(&node_config, &db, &instance_name, &onboarding_config).await?;

                // Load DNS proposal to send to peers with SignDns command
                let dns_proposal_data = db
                    .read_artifact(&instance_name, artifact_kinds::DNS_PROTO, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("DNS_PROTO artifact missing after CreateProposals")
                    })?;
                workflow_state.set_command_payload(dns_proposal_data).await;

                workflow_state.advance_step().await;
            }
            OnboardingStep::SignDns => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitDns => {
                tracing::info!("Coordinator executing: Submit DNS proposals");
                save_signed_proposals(
                    &workflow_state,
                    &db,
                    &instance_name,
                    artifact_kinds::SIGNED_DNS_PROPOSAL,
                )
                .await?;
                submit_dns_proposals(&node_config, &db, &instance_name).await?;

                // Load P2P proposal to send to peers with SignP2p command
                let p2p_proposal_data = db
                    .read_artifact(&instance_name, artifact_kinds::P2P_PROTO, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("P2P_PROTO artifact missing after CreateProposals")
                    })?;
                workflow_state.set_command_payload(p2p_proposal_data).await;

                workflow_state.advance_step().await;
            }
            OnboardingStep::SignP2p => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitFinal => {
                tracing::info!("Coordinator executing: Submit final proposals");
                save_signed_proposals(
                    &workflow_state,
                    &db,
                    &instance_name,
                    artifact_kinds::SIGNED_P2P_PROPOSAL,
                )
                .await?;
                submit_final_proposals(&node_config, &db, &instance_name, &onboarding_config)
                    .await?;
                workflow_state.advance_step().await;
            }
            OnboardingStep::Complete => {
                tracing::info!("Onboarding workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}
