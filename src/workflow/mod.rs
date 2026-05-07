pub mod contracts;
pub mod dars;
pub mod kick;
pub mod onboarding;
pub mod state;
pub mod storage;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{SignedTopologyTransaction, TopologyTransaction, topology_mapping},
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::SchemaRead,
    error::Result,
    noise::{MessageType, client::NoiseClient, server::NoiseServer},
    participant_id::CantonId,
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

pub use contracts::{ContractsConfig, ContractsStep};
pub use dars::{DarsConfig, DarsStep};
pub use kick::{KickConfig, KickStep};
pub use onboarding::{OnboardingConfig, OnboardingStep};
pub use state::WorkflowState;

/// Workflow types that can be run
#[derive(Clone, Copy, Debug)]
pub enum WorkflowType {
    Onboarding,
    Contracts,
    Dars,
    Kick,
}

/// Result from a coordinator workflow, optionally containing the created party ID
pub struct CoordinatorResult {
    /// The created dec party ID (only set for onboarding workflows)
    pub created_party_id: Option<CantonId>,
}

/// Start a coordinator workflow (called when this node initiates the workflow from UI)
#[allow(clippy::too_many_arguments)]
pub async fn start_coordinator(
    node_config: NodeConfig,
    db: SqlitePool,
    workflow_type: WorkflowType,
    onboarding_config: Option<OnboardingConfig>,
    kick_config: Option<KickConfig>,
    contracts_config: Option<ContractsConfig>,
    dars_config: Option<DarsConfig>,
    workflow_auth: Option<WorkflowAuth>,
) -> Result<CoordinatorResult> {
    tracing::info!("Loading peers from database...");
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);

    tracing::info!("Starting {workflow_type:?} workflow as COORDINATOR");

    match workflow_type {
        WorkflowType::Onboarding => {
            let config = onboarding_config.ok_or_else(|| {
                anyhow::anyhow!("OnboardingConfig is required for Onboarding workflow")
            })?;
            let party_id =
                onboarding::coordinator::start_coordinator(node_config, network_config, config, db)
                    .await?;
            Ok(CoordinatorResult {
                created_party_id: Some(party_id),
            })
        }
        WorkflowType::Contracts => {
            let config = contracts_config.ok_or_else(|| {
                anyhow::anyhow!("ContractsConfig is required for Contracts workflow")
            })?;
            contracts::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                workflow_auth,
                db,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        WorkflowType::Dars => {
            let config = dars_config
                .ok_or_else(|| anyhow::anyhow!("DarsConfig is required for Dars workflow"))?;
            dars::coordinator::start_coordinator(node_config, network_config, config, db).await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        WorkflowType::Kick => {
            let config = kick_config
                .ok_or_else(|| anyhow::anyhow!("KickConfig is required for Kick workflow"))?;
            kick::coordinator::start_coordinator(node_config, network_config, config, db).await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
    }
}

/// Maximum time the coordinator's `run_workflow` loop is allowed to spend in
/// the same step before treating the run as failed. Wait-states like
/// `WaitingForAttestors`, `SignDns`, `SignP2p` only advance when attestors
/// post messages over Noise; if every attestor goes away (process killed,
/// network partition) the loop has no other way to give up. Without this
/// budget the spawned coordinator task blocks forever and the persisted
/// `workflow_runs` row stays `inprogress`, leaving operators no way to
/// retry/dismiss.
///
/// Sized to be larger than any legitimate per-step wait we've observed in
/// CI (attestor sign+round-trip is sub-30s) but well under the chaos
/// suite's tightest deadline (P1 = 120s for the row to flip to `failed`).
pub const COORDINATOR_STEP_STALENESS_THRESHOLD: Duration = Duration::from_secs(90);

/// Per-iteration watchdog used by every coordinator's `run_workflow` loop.
/// Tracks how long the workflow has been pinned on the same step; once the
/// dwell time crosses [`COORDINATOR_STEP_STALENESS_THRESHOLD`], `check`
/// returns an error so `start_coordinator` propagates it back to the
/// spawning task, which calls `mark_run_failed` and surfaces the run as
/// `failed` in the API/DB.
///
/// Step transitions reset the timer on the next `check`, so long active
/// steps (e.g., `submit_dns_proposals`, which can block the loop on Canton
/// topology propagation) don't trip it: the loop only re-enters `check`
/// after the step returns and `advance_step` fires.
pub struct StepStalenessWatchdog<S> {
    last_step: Option<S>,
    last_change: Instant,
    threshold: Duration,
}

impl<S: PartialEq + Copy + std::fmt::Debug> StepStalenessWatchdog<S> {
    pub fn new(threshold: Duration) -> Self {
        Self {
            last_step: None,
            last_change: Instant::now(),
            threshold,
        }
    }

    pub fn check(&mut self, current: S) -> Result {
        if self.last_step != Some(current) {
            self.last_step = Some(current);
            self.last_change = Instant::now();
            return Ok(());
        }
        let elapsed = self.last_change.elapsed();
        if elapsed >= self.threshold {
            anyhow::bail!(
                "Coordinator stalled in step {current:?} for {elapsed:?} \
                 (threshold {threshold:?}); attestors likely unreachable",
                threshold = self.threshold
            );
        }
        Ok(())
    }
}

pub async fn run_server_with_workflow<S: state::WorkflowStep + 'static>(
    server: Arc<NoiseServer<S>>,
    workflow_handle: tokio::task::JoinHandle<Result>,
) -> Result {
    tokio::select! {
        result = server.start() => {
            result?;
        }
        result = workflow_handle => {
            match result {
                Ok(Ok(())) => {
                    tracing::info!("Workflow completed successfully, shutting down");
                }
                Ok(Err(e)) => {
                    tracing::error!("Workflow failed: {e:#}");
                    anyhow::bail!("Coordinator workflow failed: {e:#}");
                }
                Err(e) => {
                    tracing::error!("Workflow task panicked: {e}");
                    anyhow::bail!("Coordinator workflow task panicked: {e}");
                }
            }
        }
    }
    Ok(())
}

/// Start node in attestor mode (client)
/// Called when this node receives a workflow invite from the coordinator.
///
/// `instance_name` is the local attestor-side `workflow_runs` row's primary
/// key — accept_invitation creates the row with a synthetic name (e.g.
/// `attestor-onboarding-<pubkey>-<ts>`) and we use that same name for every
/// `workflow_artifacts` write, so the FK constraint to `workflow_runs` is
/// satisfied. The coordinator's logical instance_name (carried in
/// OnboardingConfig/ContractsConfig/KickConfig payloads) is the coordinator's
/// own primary key on its DB and is not used for storage on the attestor side.
pub async fn start_attestor(
    node_config: NodeConfig,
    coordinator: Peer,
    db: SqlitePool,
    instance_name: String,
) -> Result {
    tracing::info!(
        "Initializing Noise client to connect to coordinator {}...",
        coordinator.participant_id
    );

    let client = NoiseClient::new(node_config.clone(), coordinator).await?;

    tracing::info!("Noise client initialized, entering command polling loop");

    // Command polling loop
    let mut consecutive_errors = 0;
    let mut consecutive_step_failures = 0;
    loop {
        // Poll coordinator for next command (with payload for commands that need data)
        let message = match client.get_next_command_with_payload().await {
            Ok(msg) => {
                consecutive_errors = 0; // Reset error count on success
                msg
            }
            Err(e) => {
                consecutive_errors += 1;

                // Per-attempt failures are WARN, not ERROR — the loop's design
                // is to tolerate transient blips (e.g. coordinator restarting
                // briefly during peer-config reload). Only the final-strike
                // bail is logged at ERROR. This keeps test logs readable when
                // a known restart cycle produces one or two retries that
                // succeed on the next attempt.
                tracing::warn!("Attempt {consecutive_errors}/3: failed to get next command: {e}");

                // If we get multiple connection refused errors in a row,
                // the coordinator has likely shut down or there's a persistent error
                if consecutive_errors >= 3 {
                    tracing::error!(
                        "Failed to communicate with coordinator after 3 attempts. Aborting."
                    );
                    anyhow::bail!(
                        "Attestor failed: persistent communication errors with coordinator"
                    );
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let command = message.msg_type;
        let payload = message.payload;

        match command {
            MessageType::Wait => {
                tracing::trace!("Received command: Wait");
                // Coordinator says to wait, poll again after delay
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
            MessageType::Disconnect => {
                tracing::info!("Received disconnect command, shutting down");
                break;
            }
            MessageType::UploadDars => {
                tracing::info!("Executing: Upload DARs");

                // Decode DAR files from payload and upload directly
                let dar_files = if payload.is_empty() {
                    Vec::new()
                } else {
                    match utils::decode_files(&payload) {
                        Ok(files) => files,
                        Err(e) => {
                            tracing::error!("Failed to decode DARs from coordinator: {e}");
                            continue;
                        }
                    }
                };

                if let Err(e) = contracts::upload_dars_from_bytes(&node_config, dar_files).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = client.send_status(b"UploadDars completed".to_vec()).await {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::GenerateKeys => {
                tracing::info!("Executing: Generate keys");
                // Deserialize onboarding config from payload (we still need
                // the prefix for namespace_key_name / daml_key_name; the
                // config's `instance_name` is the coordinator's view and is
                // intentionally unused here).
                let onboarding_config: onboarding::OnboardingConfig =
                    match serde_json::from_slice(&payload) {
                        Ok(config) => config,
                        Err(e) => {
                            tracing::error!("Failed to deserialize onboarding config: {e}");
                            consecutive_step_failures += 1;
                            if consecutive_step_failures >= 3 {
                                anyhow::bail!("Aborting attestor: 3 consecutive step failures");
                            }
                            continue;
                        }
                    };

                if let Err(e) =
                    onboarding::generate_keys(&node_config, &db, &instance_name, &onboarding_config)
                        .await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= 3 {
                        anyhow::bail!("Aborting attestor: 3 consecutive step failures: {e}");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = onboarding::attestor::send_keys_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send keys to coordinator: {e}");
                }
            }
            MessageType::SignDns => {
                tracing::info!("Executing: Sign DNS proposal");
                if payload.is_empty() {
                    tracing::error!("No DNS proposal payload received from coordinator");
                    continue;
                }
                if let Err(e) =
                    onboarding::sign_dns_proposals(&node_config, &db, &instance_name, &payload)
                        .await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= 3 {
                        anyhow::bail!("Aborting attestor: 3 consecutive step failures: {e}");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = onboarding::attestor::send_dns_signature_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send DNS signature to coordinator: {e}");
                }
            }
            MessageType::SignP2p => {
                tracing::info!("Executing: Sign P2P proposals");
                if payload.is_empty() {
                    tracing::error!("No P2P proposal payload received from coordinator");
                    continue;
                }
                if let Err(e) =
                    onboarding::sign_p2p_proposals(&node_config, &db, &instance_name, &payload)
                        .await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= 3 {
                        anyhow::bail!("Aborting attestor: 3 consecutive step failures: {e}");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = onboarding::attestor::send_p2p_signatures_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send P2P signatures to coordinator: {e}");
                }

                // Identity hook (attestor side): the SignP2p payload is a
                // SignedTopologyTransaction whose PartyToParticipant mapping
                // carries the resolved dec_party_id in its `party` field. By
                // now the namespace has been signed and submitted by the
                // coordinator, so we can persist this attestor's keys +
                // participant id under the dec_party_identity table for use
                // by post-onboarding workflows on this node.
                match extract_party_id_from_p2p_payload(&payload) {
                    Ok(dec_party_id) => {
                        if let Err(e) = onboarding::attestor::copy_self_identity_for_party(
                            &db,
                            &instance_name,
                            &node_config,
                            &dec_party_id,
                        )
                        .await
                        {
                            tracing::error!("Failed to copy self identity for {dec_party_id}: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to extract dec_party_id from P2P payload: {e}");
                    }
                }
            }
            MessageType::SignSubmissions => {
                tracing::info!("Executing: Sign submissions");

                if payload.is_empty() {
                    tracing::error!("No submissions payload received from coordinator");
                    continue;
                }

                // Decode config and files from payload: [config_json, files_payload]
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode SignSubmissions payload: {e}");
                        continue;
                    }
                };

                let contracts_config: contracts::ContractsConfig =
                    match serde_json::from_slice(&items[0]) {
                        Ok(config) => config,
                        Err(e) => {
                            tracing::error!("Failed to deserialize contracts config: {e}");
                            continue;
                        }
                    };

                let dec_party_id = contracts_config.decentralized_party_id.clone();

                // Persist the prepared submissions sent by the coordinator into
                // this attestor's workflow_artifacts so sign_submissions can
                // read them back via list_artifacts.
                if let Err(e) =
                    save_prepared_submissions_from_payload(&items[1], &db, &instance_name).await
                {
                    tracing::error!("Failed to save prepared submissions from coordinator: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= 3 {
                        anyhow::bail!("Aborting attestor: 3 consecutive step failures: {e}");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }

                if let Err(e) =
                    contracts::sign_submissions(&node_config, &db, &instance_name, &dec_party_id)
                        .await
                {
                    tracing::error!("Step execution failed: {e:#}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= 3 {
                        anyhow::bail!("Aborting attestor: 3 consecutive step failures: {e}");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = contracts::attestor::send_submission_signatures_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send submission signatures to coordinator: {e}");
                }
            }
            MessageType::SignKick => {
                tracing::info!("Executing: Sign kick proposals");
                if payload.is_empty() {
                    tracing::error!("No kick proposals payload received from coordinator");
                    continue;
                }

                let items = match utils::decode_length_prefixed(&payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode SignKick payload: {e}");
                        continue;
                    }
                };

                let _kick_config: kick::KickConfig = match serde_json::from_slice(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        tracing::error!("Failed to deserialize kick config: {e}");
                        continue;
                    }
                };

                let kick_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) =
                    kick::sign_proposals(&node_config, &db, &instance_name, &kick_data).await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= 3 {
                        anyhow::bail!("Aborting attestor: 3 consecutive step failures: {e}");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = kick::attestor::send_kick_signatures_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send kick signatures to coordinator: {e}");
                }
            }
            _ => {
                tracing::warn!("Unexpected message type: {command:?}");
            }
        }
    }

    tracing::info!("Attestor shutting down");
    Ok(())
}

/// Extract the resolved decentralized party id from a SignP2p command payload.
/// The payload is a `varint(len)||SignedTopologyTransaction` blob whose
/// `transaction.mapping` is a `PartyToParticipant` mapping carrying `party`
/// (i.e. `{prefix}::{namespace_fingerprint}`). We pull that out so the
/// attestor's identity hook can key its `dec_party_identity` rows.
fn extract_party_id_from_p2p_payload(payload: &[u8]) -> Result<CantonId> {
    let signed: SignedTopologyTransaction = utils::read_first_message_from_bytes(payload)?;

    // `signed.transaction` is an UntypedVersionedMessage envelope (not raw
    // TopologyTransaction bytes) — Canton wraps every protocol-versioned
    // message this way. Unwrap one layer, then decode the inner data.
    let versioned = UntypedVersionedMessage::decode(signed.transaction.as_slice())
        .context("Failed to decode UntypedVersionedMessage from SignedTopologyTransaction")?;
    let inner_bytes = match versioned.wrapper {
        Some(untyped_versioned_message::Wrapper::Data(b)) => b,
        None => anyhow::bail!("UntypedVersionedMessage has no wrapper data"),
    };
    let tx = TopologyTransaction::decode(inner_bytes.as_slice())
        .context("Failed to decode TopologyTransaction from versioned wrapper")?;
    let mapping = tx
        .mapping
        .and_then(|m| m.mapping)
        .ok_or_else(|| anyhow::anyhow!("TopologyTransaction has no mapping"))?;
    match mapping {
        topology_mapping::Mapping::PartyToParticipant(p2p) => CantonId::parse(&p2p.party),
        other => anyhow::bail!("Expected PartyToParticipant mapping, got {other:?}"),
    }
}

/// Persist the prepared submissions blob received from the coordinator into
/// `workflow_artifacts` keyed by the same zero-padded ordinals the coordinator
/// used. The byte-for-byte payload of each submission is preserved so the
/// attestor's sign step decodes them identically to what the coordinator
/// produced.
async fn save_prepared_submissions_from_payload(
    payload: &[u8],
    db: &SqlitePool,
    instance_name: &str,
) -> Result {
    let files = utils::decode_files(payload)?;

    tracing::info!(
        "Received {count} prepared submission artefact(s) from coordinator",
        count = files.len()
    );

    for (ordinal, data) in &files {
        db.write_artifact(
            instance_name,
            artifact_kinds::PREPARED_SUBMISSION,
            Some(ordinal),
            data,
        )
        .await?;
        tracing::debug!("Saved prepared submission ordinal {ordinal}");
    }

    Ok(())
}
