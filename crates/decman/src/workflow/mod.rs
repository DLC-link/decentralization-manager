pub mod add_party;
pub mod contracts;
pub mod dars;
pub mod kick;
pub mod onboarding;
pub mod state;
pub mod storage;
pub mod topology;

use std::sync::Arc;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{SignedTopologyTransaction, TopologyTransaction, topology_mapping},
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, Peer},
    consts::{MAX_CONSECUTIVE_NO_WORKFLOW_POLLS, MAX_CONSECUTIVE_STEP_FAILURES},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    noise::{MessageType, NoiseError, client::NoiseClient, server::ActiveWorkflow},
    server::{WorkflowInstance, WorkflowKind, peer_status::LastSeen},
    utils,
    workflow::{
        state::WorkflowStep,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

pub use add_party::{AddPartyConfig, AddPartyStep};
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
    AddParty,
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
    add_party_config: Option<AddPartyConfig>,
    workflow_auth: Option<WorkflowAuth>,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result<CoordinatorResult> {
    tracing::info!("Loading peers from database...");
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);

    tracing::info!("Starting {workflow_type:?} workflow as COORDINATOR");

    match workflow_type {
        WorkflowType::Onboarding => {
            let config = onboarding_config.ok_or_else(|| {
                anyhow::anyhow!("OnboardingConfig is required for Onboarding workflow")
            })?;
            let party_id = onboarding::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
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
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        WorkflowType::Dars => {
            let config = dars_config
                .ok_or_else(|| anyhow::anyhow!("DarsConfig is required for Dars workflow"))?;
            dars::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        WorkflowType::Kick => {
            let config = kick_config
                .ok_or_else(|| anyhow::anyhow!("KickConfig is required for Kick workflow"))?;
            kick::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        WorkflowType::AddParty => {
            let config = add_party_config.ok_or_else(|| {
                anyhow::anyhow!("AddPartyConfig is required for AddParty workflow")
            })?;
            add_party::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                workflow_auth,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
    }
}

/// Register the coordinator's Noise handle on its [`WorkflowInstance`] so the
/// always-on listener can route this run's peer command messages to it, then
/// drive the workflow loop to completion. No port is bound here — the single
/// always-on listener already owns port 9000. The instance's registry entry is
/// removed by the caller's [`WorkflowGuard`](crate::server::WorkflowGuard) on
/// return (success, error, or panic), so a finished or failed workflow stops
/// receiving routed commands.
pub async fn run_workflow_with_handler(
    active: ActiveWorkflow,
    instance: Arc<WorkflowInstance>,
    workflow_handle: tokio::task::JoinHandle<Result>,
) -> Result {
    instance.set_active(active);
    match workflow_handle.await {
        Ok(Ok(())) => {
            tracing::info!("Workflow completed successfully, shutting down");
            Ok(())
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

/// Start node in peer mode (client)
/// Called when this node receives a workflow invite from the coordinator.
///
/// `instance_name` is the local peer-side `workflow_runs` row's primary
/// key — accept_invitation creates the row with a synthetic name (e.g.
/// `peer-onboarding-<pubkey>-<ts>`) and we use that same name for every
/// `workflow_artifacts` write, so the FK constraint to `workflow_runs` is
/// satisfied. The coordinator's logical instance_name (carried in
/// OnboardingConfig/ContractsConfig/KickConfig payloads) is the coordinator's
/// own primary key on its DB and is not used for storage on the peer side.
pub async fn start_peer(
    node_config: NodeConfig,
    coordinator: Peer,
    db: SqlitePool,
    instance_name: String,
    coordinator_instance: String,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    tracing::info!(
        "Initializing Noise client to connect to coordinator {}...",
        coordinator.participant_id
    );

    let client = NoiseClient::new(node_config.clone(), coordinator, coordinator_instance).await?;

    tracing::info!("Noise client initialized, entering command polling loop");

    // Cache the workflow kind so each polled command can be mapped to a
    // human-readable step (current_step / step_index) on this peer's
    // workflow_runs row. Falling back to None just means the notification
    // feed UI keeps showing the row's initial "Active" placeholder for this
    // run, which is harmless.
    let peer_kind: Option<WorkflowKind> = match db.get_workflow_run(&instance_name).await {
        Ok(Some(run)) => Some(run.kind),
        Ok(None) => {
            tracing::warn!("peer step persist: no workflow_runs row for {instance_name}");
            None
        }
        Err(e) => {
            tracing::warn!("peer step persist: lookup failed for {instance_name}: {e}");
            None
        }
    };

    // Command polling loop
    let mut consecutive_errors = 0;
    let mut consecutive_no_workflow = 0;
    let mut consecutive_step_failures = 0;
    loop {
        // Poll coordinator for next command (with payload for commands that need data)
        let message = match client.get_next_command_with_payload().await {
            Ok(msg) => {
                consecutive_errors = 0; // Reset error count on success
                consecutive_no_workflow = 0;
                msg
            }
            Err(e) => {
                // A 503 means the coordinator is reachable but has no active
                // workflow registered — either it is still resuming after a
                // restart (transient) or its workflow was cancelled / dismissed
                // while we were offline (permanent). The TCP round-trip
                // succeeded, so reset the connection-error count and instead
                // give the coordinator a bounded number of polls to
                // (re-)register before giving up, so a resumed run doesn't poll
                // a dead coordinator forever. The counter resets on any real
                // reply above, so a slow resume rides through.
                if matches!(&e, NoiseError::BadStatusCode(code) if code.as_u16() == 503) {
                    consecutive_errors = 0;
                    consecutive_no_workflow += 1;
                    if consecutive_no_workflow >= MAX_CONSECUTIVE_NO_WORKFLOW_POLLS {
                        tracing::error!(
                            "Coordinator reported no active workflow for \
                             {MAX_CONSECUTIVE_NO_WORKFLOW_POLLS} polls; the workflow was \
                             cancelled or dismissed. Aborting."
                        );
                        anyhow::bail!(
                            "Peer failed: coordinator has no active workflow (cancelled or dismissed)"
                        );
                    }
                    tracing::warn!(
                        "Poll {consecutive_no_workflow}/{MAX_CONSECUTIVE_NO_WORKFLOW_POLLS}: \
                         coordinator has no active workflow yet, retrying"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }

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
                    anyhow::bail!("Peer failed: persistent communication errors with coordinator");
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let command = message.msg_type;
        let payload = message.payload;

        if let Some(kind) = peer_kind {
            persist_peer_step(&db, &instance_name, kind, command).await;
        }

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
                let onboarding_config: onboarding::OnboardingConfig = match serde_json::from_slice(
                    &payload,
                ) {
                    Ok(config) => config,
                    Err(e) => {
                        tracing::error!("Failed to deserialize onboarding config: {e}");
                        consecutive_step_failures += 1;
                        if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                            anyhow::bail!(
                                "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures"
                            );
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
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = onboarding::peer::send_keys_to_coordinator(
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
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = onboarding::peer::send_dns_signature_to_coordinator(
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
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = onboarding::peer::send_p2p_signatures_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send P2P signatures to coordinator: {e}");
                }

                // Identity hook (peer side): the SignP2p payload is a
                // SignedTopologyTransaction whose PartyToParticipant mapping
                // carries the resolved dec_party_id in its `party` field. By
                // now the namespace has been signed and submitted by the
                // coordinator, so we can persist this peer's keys +
                // participant id under the dec_party_identity table for use
                // by post-onboarding workflows on this node.
                match extract_party_id_from_p2p_payload(&payload) {
                    Ok(dec_party_id) => {
                        if let Err(e) = onboarding::peer::copy_self_identity_for_party(
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
                // this peer's workflow_artifacts so sign_submissions can
                // read them back via list_artifacts.
                if let Err(e) =
                    save_prepared_submissions_from_payload(&items[1], &db, &instance_name).await
                {
                    tracing::error!("Failed to save prepared submissions from coordinator: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
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
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = contracts::peer::send_submission_signatures_to_coordinator(
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
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = kick::peer::send_kick_signatures_to_coordinator(
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
            MessageType::GenerateAddPartyKeys => {
                tracing::info!("Executing: Generate add-party keys");
                let Some(add_party_config) = decode_add_party_config(&payload) else {
                    continue;
                };
                if !is_new_member(&node_config, &add_party_config) {
                    send_skip_status(&client, "GenerateAddPartyKeys").await;
                    continue;
                }

                let ledger_token = add_party::resolve_ledger_token(
                    &workflow_auth,
                    &add_party_config.decentralized_party_id,
                )
                .await;
                if let Err(e) = add_party::generate_keys(
                    &node_config,
                    &db,
                    &instance_name,
                    &add_party_config,
                    ledger_token.as_deref(),
                )
                .await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = add_party::peer::send_keys_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send add-party keys to coordinator: {e}");
                }
            }
            MessageType::SignAddParty => {
                tracing::info!("Executing: Sign add-party proposals");
                if payload.is_empty() {
                    tracing::error!("No add-party proposals payload received from coordinator");
                    continue;
                }

                let items = match utils::decode_length_prefixed(&payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode SignAddParty payload: {e}");
                        continue;
                    }
                };
                let Some(add_party_config) = decode_add_party_config(&items[0]) else {
                    continue;
                };

                let proposal_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) =
                    add_party::sign_proposals(&node_config, &db, &instance_name, &proposal_data)
                        .await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = add_party::peer::send_add_party_signatures_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send add-party signatures to coordinator: {e}");
                }

                // Identity hook (new member): same point in the protocol
                // where onboarding peers persist their identity — the party
                // id is authoritative from the config, and the keys were
                // written by GenerateAddPartyKeys.
                if is_new_member(&node_config, &add_party_config)
                    && let Err(e) = onboarding::peer::copy_self_identity_for_party(
                        &db,
                        &instance_name,
                        &node_config,
                        &add_party_config.decentralized_party_id,
                    )
                    .await
                {
                    tracing::error!(
                        "Failed to copy self identity for {party}: {e}",
                        party = add_party_config.decentralized_party_id
                    );
                }
            }
            MessageType::ImportAcs => {
                tracing::info!("Executing: Import party ACS");
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode ImportAcs payload: {e}");
                        continue;
                    }
                };
                let Some(add_party_config) = decode_add_party_config(&items[0]) else {
                    continue;
                };
                if !is_new_member(&node_config, &add_party_config) {
                    send_skip_status(&client, "ImportAcs").await;
                    continue;
                }

                if let Err(e) =
                    add_party::import_party_acs(&node_config, &add_party_config, items[1].clone())
                        .await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = client.send_status(b"ImportAcs completed".to_vec()).await {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::ClearOnboardingFlag => {
                tracing::info!("Executing: Clear onboarding flag");
                let Some(add_party_config) = decode_add_party_config(&payload) else {
                    continue;
                };
                if !is_new_member(&node_config, &add_party_config) {
                    send_skip_status(&client, "ClearOnboardingFlag").await;
                    continue;
                }

                match add_party::clear_onboarding_flag(
                    &node_config,
                    &db,
                    &instance_name,
                    &add_party_config,
                )
                .await
                {
                    Ok(add_party::ClearOutcome::Proposed) => {
                        // Canton requires the ONBOARDING PARTICIPANT to issue
                        // the flag-clear transaction — author it here and ship
                        // it to the coordinator for the threshold-signing
                        // round (the coordinator lacks an appropriate key).
                        match add_party::author_clear_proposal(&node_config, &add_party_config)
                            .await
                        {
                            Ok(Some(proposal)) => {
                                consecutive_step_failures = 0;
                                if let Err(e) = client.send_add_party_clear_proposal(proposal).await
                                {
                                    tracing::error!(
                                        "Failed to send clearing proposal to coordinator: {e}"
                                    );
                                }
                            }
                            Ok(None) => {
                                // Flag dropped between the poll and the
                                // authoring — report as cleared.
                                consecutive_step_failures = 0;
                                if let Err(e) = client
                                    .send_status(b"ClearOnboardingFlag: Cleared".to_vec())
                                    .await
                                {
                                    tracing::error!("Failed to send completion status: {e}");
                                }
                            }
                            Err(e) => {
                                tracing::error!("Step execution failed: {e}");
                                consecutive_step_failures += 1;
                                if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                                    anyhow::bail!(
                                        "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                                    );
                                }
                                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                            }
                        }
                    }
                    Ok(add_party::ClearOutcome::Cleared) => {
                        consecutive_step_failures = 0;
                        if let Err(e) = client
                            .send_status(b"ClearOnboardingFlag: Cleared".to_vec())
                            .await
                        {
                            tracing::error!("Failed to send completion status: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Step execution failed: {e}");
                        consecutive_step_failures += 1;
                        if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                            anyhow::bail!(
                                "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                            );
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    }
                }
            }
            MessageType::SignClearOnboarding => {
                tracing::info!("Executing: Sign onboarding-flag clearing proposal");
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode SignClearOnboarding payload: {e}");
                        continue;
                    }
                };
                if items[1].is_empty() {
                    // Skip marker: the flag already cleared without a
                    // signing round (e.g. a single-owner-threshold party).
                    send_skip_status(&client, "SignClearOnboarding").await;
                    continue;
                }

                if let Err(e) =
                    add_party::sign_clear_proposal(&node_config, &db, &instance_name, &items[1])
                        .await
                {
                    tracing::error!("Step execution failed: {e}");
                    consecutive_step_failures += 1;
                    if consecutive_step_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
                        anyhow::bail!(
                            "Aborting peer: {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures: {e}"
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = add_party::peer::send_clear_signature_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send clearing signature to coordinator: {e}");
                }
            }
            _ => {
                tracing::warn!("Unexpected message type: {command:?}");
            }
        }
    }

    tracing::info!("Peer shutting down");
    Ok(())
}

/// Decode an `AddPartyConfig` from a command payload. Errors are logged and
/// collapse to `None` so the peer loop's `continue` keeps polling (the
/// coordinator re-serves the command until the peer completes).
fn decode_add_party_config(payload: &[u8]) -> Option<add_party::AddPartyConfig> {
    match serde_json::from_slice(payload) {
        Ok(config) => Some(config),
        Err(e) => {
            tracing::error!("Failed to deserialize add-party config: {e}");
            None
        }
    }
}

/// Whether this node is the member being added by the run.
fn is_new_member(node_config: &NodeConfig, config: &add_party::AddPartyConfig) -> bool {
    node_config.participant_id() == &config.new_participant_id
}

/// Reply with the add-party skip status for a command that only the new
/// member executes. Completes this peer for the step on the coordinator.
async fn send_skip_status(client: &NoiseClient, step: &str) {
    tracing::info!("{step}: not the new member — replying skip");
    if let Err(e) = client
        .send_status(add_party::peer::SKIP_STATUS.to_vec())
        .await
    {
        tracing::error!("Failed to send skip status for {step}: {e}");
    }
}

/// Extract the resolved decentralized party id from a SignP2p command payload.
/// The payload is a `varint(len)||SignedTopologyTransaction` blob whose
/// `transaction.mapping` is a `PartyToParticipant` mapping carrying `party`
/// (i.e. `{prefix}::{namespace_fingerprint}`). We pull that out so the
/// peer's identity hook can key its `dec_party_identity` rows.
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

/// Map an inbound coordinator command to the peer's view of step
/// progress for the given workflow kind. Returns `(step_name, step_index,
/// step_total)` for commands that correspond to a real step on the peer
/// side; returns `None` for `Wait` (no transition) and any unrelated
/// command.
fn peer_step_for_command(
    kind: WorkflowKind,
    command: MessageType,
) -> Option<(&'static str, i64, i64)> {
    match kind {
        WorkflowKind::Onboarding => {
            let step = match command {
                MessageType::GenerateKeys => OnboardingStep::GenerateKeys,
                MessageType::SignDns => OnboardingStep::SignDns,
                MessageType::SignP2p => OnboardingStep::SignP2p,
                MessageType::Disconnect => OnboardingStep::Complete,
                _ => return None,
            };
            Some((
                step.step_name(),
                step.step_index(),
                OnboardingStep::step_total(),
            ))
        }
        WorkflowKind::Kick => {
            let step = match command {
                MessageType::SignKick => KickStep::SignProposals,
                MessageType::Disconnect => KickStep::Complete,
                _ => return None,
            };
            Some((step.step_name(), step.step_index(), KickStep::step_total()))
        }
        WorkflowKind::Contracts => {
            let step = match command {
                MessageType::SignSubmissions => ContractsStep::SignSubmissions,
                MessageType::Disconnect => ContractsStep::Complete,
                _ => return None,
            };
            Some((
                step.step_name(),
                step.step_index(),
                ContractsStep::step_total(),
            ))
        }
        WorkflowKind::Dars => {
            let step = match command {
                MessageType::UploadDars => DarsStep::UploadDars,
                MessageType::Disconnect => DarsStep::Complete,
                _ => return None,
            };
            Some((step.step_name(), step.step_index(), DarsStep::step_total()))
        }
        WorkflowKind::AddParty => {
            let step = match command {
                MessageType::GenerateAddPartyKeys => AddPartyStep::GenerateNewMemberKeys,
                MessageType::SignAddParty => AddPartyStep::SignProposals,
                MessageType::ImportAcs => AddPartyStep::SyncAcs,
                MessageType::ClearOnboardingFlag => AddPartyStep::ProposeClearOnboarding,
                MessageType::SignClearOnboarding => AddPartyStep::SignClearOnboarding,
                MessageType::Disconnect => AddPartyStep::Complete,
                _ => return None,
            };
            Some((
                step.step_name(),
                step.step_index(),
                AddPartyStep::step_total(),
            ))
        }
    }
}

/// Persist peer-side step progress for a command. Best-effort: any
/// failure is logged at WARN — the workflow itself doesn't depend on this
/// row staying in sync, only the notification feed UI does.
async fn persist_peer_step(
    db: &SqlitePool,
    instance_name: &str,
    kind: WorkflowKind,
    command: MessageType,
) {
    let Some((step_name, step_index, _step_total)) = peer_step_for_command(kind, command) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tx = match db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("peer step persist: begin_transaction failed: {e}");
            return;
        }
    };
    if let Err(e) = tx
        .update_workflow_run_step(instance_name, step_name, step_index, &[], now)
        .await
    {
        tracing::warn!("peer step persist: update failed: {e}");
        return;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("peer step persist: commit failed: {e}");
    }
}

/// Persist the prepared submissions blob received from the coordinator into
/// `workflow_artifacts` keyed by the same zero-padded ordinals the coordinator
/// used. The byte-for-byte payload of each submission is preserved so the
/// peer's sign step decodes them identically to what the coordinator
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_step_maps_known_commands_and_rejects_the_rest() {
        // Commands that correspond to a real peer-side step for each kind.
        let mapped = [
            (WorkflowKind::Onboarding, MessageType::GenerateKeys),
            (WorkflowKind::Onboarding, MessageType::SignDns),
            (WorkflowKind::Onboarding, MessageType::SignP2p),
            (WorkflowKind::Onboarding, MessageType::Disconnect),
            (WorkflowKind::Kick, MessageType::SignKick),
            (WorkflowKind::Kick, MessageType::Disconnect),
            (WorkflowKind::Contracts, MessageType::SignSubmissions),
            (WorkflowKind::Contracts, MessageType::Disconnect),
            (WorkflowKind::Dars, MessageType::UploadDars),
            (WorkflowKind::Dars, MessageType::Disconnect),
            (WorkflowKind::AddParty, MessageType::GenerateAddPartyKeys),
            (WorkflowKind::AddParty, MessageType::SignAddParty),
            (WorkflowKind::AddParty, MessageType::ImportAcs),
            (WorkflowKind::AddParty, MessageType::ClearOnboardingFlag),
            (WorkflowKind::AddParty, MessageType::SignClearOnboarding),
            (WorkflowKind::AddParty, MessageType::Disconnect),
        ];
        for (kind, command) in mapped {
            assert!(
                peer_step_for_command(kind, command).is_some(),
                "{kind:?}/{command:?} should map to a step"
            );
        }

        // `Wait` (no transition) and an unrelated command never map, on any kind.
        for kind in [
            WorkflowKind::Onboarding,
            WorkflowKind::Kick,
            WorkflowKind::Contracts,
            WorkflowKind::Dars,
            WorkflowKind::AddParty,
        ] {
            assert!(peer_step_for_command(kind, MessageType::Wait).is_none());
            assert!(peer_step_for_command(kind, MessageType::Ping).is_none());
        }

        // A command belonging to a different kind must not cross over.
        assert!(peer_step_for_command(WorkflowKind::Onboarding, MessageType::SignKick).is_none());
        assert!(peer_step_for_command(WorkflowKind::Kick, MessageType::GenerateKeys).is_none());
        assert!(peer_step_for_command(WorkflowKind::Contracts, MessageType::UploadDars).is_none());
        assert!(peer_step_for_command(WorkflowKind::Dars, MessageType::SignSubmissions).is_none());
        assert!(peer_step_for_command(WorkflowKind::AddParty, MessageType::GenerateKeys).is_none());
        assert!(peer_step_for_command(WorkflowKind::Kick, MessageType::SignAddParty).is_none());
    }

    #[test]
    fn peer_step_distinguishes_steps_within_a_kind() {
        // GenerateKeys and SignDns are different onboarding steps: distinct
        // indices, identical totals.
        match (
            peer_step_for_command(WorkflowKind::Onboarding, MessageType::GenerateKeys),
            peer_step_for_command(WorkflowKind::Onboarding, MessageType::SignDns),
        ) {
            (Some((_, gen_idx, total_a)), Some((_, dns_idx, total_b))) => {
                assert_ne!(gen_idx, dns_idx);
                assert_eq!(total_a, total_b);
                assert!(total_a >= 2);
            }
            _ => panic!("onboarding commands should map to steps"),
        }
    }

    #[test]
    fn extract_party_id_rejects_garbage_payload() {
        // Empty and non-proto payloads must error, not panic.
        assert!(extract_party_id_from_p2p_payload(b"").is_err());
        assert!(extract_party_id_from_p2p_payload(b"not-a-valid-proto-blob").is_err());
    }
}
