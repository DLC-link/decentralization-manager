//! Coordinator driver for the decentrally-hosted external-party workflow.
//!
//! The wallet holds the party's single Ed25519 key: it generates the key, asks
//! DPM to build the multi-host onboarding topology (naming every host), and
//! signs the multi-hash — all client-side. The coordinator receives that
//! party-signed bundle, authorizes hosting on its own participant, then fans the
//! same bundle out to each hosting peer over Noise (`AllocatePeers`); each peer
//! authorizes hosting on its own participant. The topology stays a proposal
//! until the last host has signed.
//!
//! Each step is idempotent (deterministic party id, `ALREADY_EXISTS`-tolerant
//! allocate), so a restart re-runs from the first step and converges on the same
//! party.

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::{Commitable, SchemaWrite},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    workflow::{
        external_party::{ExternalPartyConfig, ExternalPartyStep, steps::allocate_party},
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Run the external-party workflow to completion and return the allocated
/// party id.
///
/// # Errors
/// Returns an error if party allocation, peer fan-out, or artifact persistence
/// fails.
pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: ExternalPartyConfig,
    db: SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result<CantonId> {
    // Build the Noise server (its `WorkflowState` derives the expected-peer set
    // from the persisted run row the HTTP handler already inserted) and register
    // its handle so the always-on listener routes this run's peer traffic to it.
    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        config.instance_name.clone(),
        ExternalPartyStep::WaitingForPeers,
        None,
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let config_clone = config.clone();
    let db_clone = db.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(workflow_state, node_config_clone, config_clone, db_clone).await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::ExternalParty(server),
        instance,
        workflow_handle,
    )
    .await?;

    let party_id_bytes = db
        .read_artifact(
            &config.instance_name,
            artifact_kinds::EXTERNAL_PARTY_ID,
            None,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("EXTERNAL_PARTY_ID artifact missing — did PrepareTopology run?")
        })?;
    let party_id_str = String::from_utf8(party_id_bytes).context("Party ID is not valid UTF-8")?;
    let party_id = CantonId::parse(party_id_str.trim())?;

    // Persist the allocated party id on the run row. `workflow_artifacts` are
    // wiped when the run reaches a terminal state (see `set_workflow_run_status`),
    // so the EXTERNAL_PARTY_ID artifact can't be read after completion — the
    // run's durable `dec_party_id` column is what `GET /external-parties` reads.
    // The wallet keeps the private key, so there is nothing else for DPM to
    // persist.
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_dec_party_id(&config.instance_name, &party_id)
        .await?;
    Commitable::commit(tx).await?;

    Ok(party_id)
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<ExternalPartyStep>>,
    node_config: NodeConfig,
    config: ExternalPartyConfig,
    db: SqlitePool,
) -> Result {
    let instance_name = config.instance_name.clone();

    loop {
        match workflow_state.current_step().await {
            ExternalPartyStep::WaitingForPeers => {
                // Connection gate: advanced by peer_connected events once every
                // hosting peer is online.
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            ExternalPartyStep::PrepareTopology => {
                // The wallet already generated the key, asked Canton to build the
                // topology, and signed the multi-hash. Allocate directly from that
                // bundle on the coordinator's own participant and fan the same
                // bundle out to the hosts — the coordinator never touches the key.
                let bundle = &config.prepared_bundle;
                tracing::info!(
                    "external-party: allocating wallet-signed party on coordinator participant"
                );
                db.write_artifact(
                    &instance_name,
                    artifact_kinds::EXTERNAL_PARTY_ID,
                    None,
                    bundle.party_id.as_bytes(),
                )
                .await?;
                allocate_party(&node_config, bundle).await?;
                let payload = serde_json::to_vec(bundle)
                    .context("serialize external-party allocate bundle")?;
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
            }
            ExternalPartyStep::AllocatePeers => {
                // Peer-gated: each hosting peer authorizes on its own participant;
                // advanced by peer_completed events once every host has signed.
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            ExternalPartyStep::Complete => {
                tracing::info!("external-party workflow complete for {instance_name}");
                // Keep the Noise handle registered briefly so every hosting peer
                // polls once more and receives the Disconnect (Complete's command)
                // to finish cleanly — otherwise a peer that polls after teardown
                // sees "no active workflow" and fails. Mirrors onboarding.
                tokio::time::sleep(Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}
