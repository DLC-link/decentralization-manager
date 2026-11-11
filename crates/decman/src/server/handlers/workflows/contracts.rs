//! The contracts workflow: start, status and invites.

use std::{collections::HashSet, time::Duration};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::server::handlers::parties::resolve_owner_keys_from_peers;
use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ContractsInvitePayload, ContractsRequest, ErrorResponse, WorkflowGuard,
            WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole,
            WorkflowStatusResponse,
        },
    },
    workflow::{self, ContractsStep},
};

use super::{
    onboarding::interpret_invite_reply,
    preflight::{
        find_inprogress_run_for_party, format_incompatible_peers, preflight_incompatible_peers,
    },
    runs::{kind_status, mark_run_completed, mark_run_failed, now_secs, register_and_persist},
};

// ============================================================================
// Contracts Workflow
// ============================================================================

/// Start a contracts workflow to upload DARs and create contracts
#[utoipa::path(
    tag = "Workflows",
    request_body = ContractsRequest,
    responses(
        (status = 202, description = "Contracts workflow started", body = WorkflowResponse),
        (status = 400, description = "No contracts to deploy, or the deployment cannot be resolved", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
#[post("/contracts")]
pub async fn start_contracts(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ContractsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    // Peers sign one prepared create per contract, so an empty run would
    // invite them and then fail on every one of them at signing.
    if body.contracts.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "The request lists no contracts to deploy".to_string(),
        });
    }

    // Create contracts config from request
    let timestamp = now_secs();
    let instance_name = format!(
        "{}-contracts-{timestamp}",
        body.decentralized_party_id.prefix
    );
    let mut contracts_config = workflow::ContractsConfig::new(
        body.decentralized_party_id.clone(),
        body.participant_ids.clone(),
        body.participant_parties.clone(),
        body.operator_party.clone(),
        body.contracts.clone(),
        instance_name,
    );

    // Scope to the party's actual signers, not every peer this node knows
    // about. A dec party can be a strict subset of the configured mesh (see
    // start_onboarding), so inviting/waiting for non-member peers would pull
    // outsiders into the deployment or stall WaitingForPeers on a peer that
    // never joins. Keep only configured peers that are this party's
    // participants, minus self.
    let participant_set: HashSet<CantonId> = body.participant_ids.iter().cloned().collect();
    let contracts_invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| p != data.config.participant_id() && participant_set.contains(p))
            .collect(),
        Err(e) => {
            tracing::warn!("Failed to load peers for cancel-invite tracking: {e}");
            Vec::new()
        }
    };

    let instance_name_for_run = contracts_config.instance_name.clone();
    let instance = WorkflowInstance::new(
        instance_name_for_run.clone(),
        WorkflowKind::Contracts,
        WorkflowRole::Coordinator,
    );
    let contracts_state = &instance.http;
    // Refuse a second workflow targeting the SAME decentralized party (see
    // `find_inprogress_run_for_party`).
    if let Some((run, kind)) =
        find_inprogress_run_for_party(&data.db, &body.decentralized_party_id).await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {} already has a {kind} workflow in flight (run {run}); wait for it \
                 to finish or cancel it first",
                body.decentralized_party_id
            ),
        });
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible =
        preflight_incompatible_peers(&data.config, &data.db, &contracts_invitees).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    // Resolve aliases once, before persisting or inviting. Preparation and
    // retries must use the same package IDs the operators approved.
    if let Err(error) = workflow::contracts::steps::prepare::resolve_package_ids(
        &data.config,
        &mut contracts_config.contracts,
    )
    .await
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("Cannot resolve deployment packages: {error:#}"),
        });
    }

    // Commit to the exact creates before persisting or inviting. Peers verify
    // the prepared transactions against these, including on a resumed run.
    match workflow::contracts::steps::prepare::commit_deployment(&data.db, &contracts_config).await
    {
        Ok(committed) => contracts_config.committed_deployment = Some(committed),
        Err(error) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Cannot commit the deployment: {error:#}"),
            });
        }
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        ContractsStep::WaitingForPeers,
        &contracts_config,
        &contracts_invitees,
        Some(body.decentralized_party_id.clone()),
    )
    .await
    {
        return resp;
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let workflow_auth = data.auth.read().await.clone();
    let auth_lock = data.auth.clone();
    let discovery_gate = super::DiscoveryGate::of(&data);
    let contracts_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let party_credentials = data.party_credentials.clone();
    let last_seen = data.last_seen.clone();
    let contracts_invitees_for_invites = contracts_invitees.clone();
    *contracts_state.invited_peers.write().await = contracts_invitees;
    let instance_for_task = instance_name_for_run.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent
    // /contracts/cancel can never observe "status=InProgress + abort_handle=None"
    // and bail.
    let mut abort_guard = contracts_state.abort_handle.lock().await;
    let mut status_guard = contracts_state.status.write().await;
    let mut error_guard = contracts_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to this party's members before starting coordinator workflow
        let invite_result = send_contracts_invites(
            &config,
            &db,
            &contracts_config,
            &contracts_invitees_for_invites,
        )
        .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send contracts invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = contracts_state_clone.status.write().await;
                let mut error = contracts_state_clone.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config.clone(),
            db.clone(),
            workflow::WorkflowType::Contracts,
            None, // No onboarding config
            None, // No kick config
            Some(contracts_config.clone()),
            None, // No dars config
            None, // No add-party config
            None, // No change-threshold config
            workflow_auth,
            last_seen,
            instance_for_coord,
        )
        .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /contracts/status acquires a read lock to
        // serve every poll; if a writer holds the lock during the DB write,
        // every concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                // The run row is marked terminal BEFORE the in-memory status
                // flips. The two are read by different callers — `/…/status`
                // reads this lock, while the "already has a workflow in flight"
                // guard scans the run rows — so flipping memory first opens a
                // window where the API reports a run finished and refuses the
                // next workflow for that party in the same breath. Doing the
                // durable write first can only report a finished run as still
                // running for an instant, which costs a poll rather than a
                // rejected request.
                mark_run_completed(&db, kind, &instance_for_task).await;
                {
                    let mut status = contracts_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("Contracts workflow completed successfully");

                // Refresh dec_party cache to pick up new contracts
                let bg_config = config.clone();
                let bg_db = db.clone();
                let bg_auth = auth_lock.clone();
                let bg_creds = party_credentials.clone();
                let bg_gate = discovery_gate.clone();
                tokio::spawn(async move {
                    let auth = bg_auth.read().await.clone();
                    let creds = bg_creds.read().await.clone();
                    // Through the shared gate, as above.
                    match super::discover_and_cache(
                        &bg_gate,
                        &bg_config,
                        &bg_db,
                        "",
                        auth,
                        &creds,
                        Default::default(),
                    )
                    .await
                    {
                        super::Discovery::Done(resp) => {
                            resolve_owner_keys_from_peers(&bg_config, &bg_db, &resp.parties).await;
                        }
                        super::Discovery::InFlight
                        | super::Discovery::AtCapacity
                        | super::Discovery::Superseded => {
                            tracing::debug!("Post-contracts refresh skipped: already running");
                        }
                        super::Discovery::Failed(e) => {
                            tracing::warn!(
                                "Failed to refresh parties after contract deployment: {e}"
                            );
                        }
                    }
                });
            }
            Err(e) => {
                let msg = format!("{e}");
                // Durable row first, in-memory status second — same ordering as the
                // success path above, and for the same reason: a caller that reads
                // Failed from `/…/status` and retries must not be refused by the
                // in-flight guard, which scans the run rows.
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                {
                    let mut status = contracts_state_clone.status.write().await;
                    let mut error = contracts_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Contracts workflow failed: {e}");
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Contracts workflow started".to_string(),
        instance_name: instance_name_for_run,
    })
}

/// Get the current status of the contracts workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Contracts workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/contracts/status")]
pub async fn get_contracts_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Contracts).await)
}

/// Send contracts invites to this party's members (`invitees`) over Noise.
async fn send_contracts_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    contracts_config: &workflow::ContractsConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    // Carry the dec party, member set, and package names so the peer card
    // renders the same rich summary the coordinator shows (mirrors the Kick
    // invite). Skip empty names, then sort+dedup — `dedup` only removes
    // adjacent duplicates, and multiple contracts can share a package name.
    let mut package_names: Vec<String> = contracts_config
        .contracts
        .iter()
        .map(|c| c.name.clone())
        .filter(|n| !n.is_empty())
        .collect();
    package_names.sort();
    package_names.dedup();
    let payload = ContractsInvitePayload {
        dec_party_id: contracts_config.decentralized_party_id.clone(),
        participants: invitees.to_vec(),
        package_names,
        contract_intents: contracts_config.committed()?.intents.clone(),
        workflow_instance: Some(contracts_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode ContractsInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteContracts, payload_bytes);

    for peer in &network_config.peers {
        if peer.participant_id == *current_participant_id {
            continue;
        }

        // Only this party's members (see start_contracts) — never every
        // configured peer.
        if !invitee_set.contains(&peer.participant_id) {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        // Use participant_id as identity (must match what server expects in peer_keys lookup)
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending contracts invite to {} at {}:{}",
            peer.participant_id,
            peer.address,
            peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(&peer.participant_id, "contracts", &response)?;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send contracts invite to {}: {e}",
                    peer.participant_id
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{pid, test_cid};
    use super::*;
    use crate::{auth::Principal, server::AppState};
    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};

    #[test]
    fn contracts_invite_payload_roundtrips_with_defaults() -> anyhow::Result<()> {
        let payload = ContractsInvitePayload {
            dec_party_id: test_cid("dec")?,
            participants: vec![test_cid("node1")?],
            package_names: vec!["Governance Core".to_string()],
            contract_intents: Vec::new(),
            workflow_instance: Some("dec-contracts-1".to_string()),
        };
        let bytes = serde_json::to_vec(&payload)?;
        let back: ContractsInvitePayload = serde_json::from_slice(&bytes)?;
        assert_eq!(back.package_names, vec!["Governance Core"]);
        assert_eq!(back.participants.len(), 1);
        // A minimal payload (only the required dec party) still decodes, so an
        // older coordinator stays compatible.
        let minimal: ContractsInvitePayload =
            serde_json::from_str(&format!(r#"{{"dec_party_id":"{}"}}"#, test_cid("dec")?))?;
        assert!(minimal.participants.is_empty());
        assert!(minimal.package_names.is_empty());
        Ok(())
    }

    /// A run with nothing to create would invite every peer and then fail on
    /// each of them at signing, so it is refused before anything is persisted
    /// or sent.
    #[actix_web::test]
    async fn start_contracts_refuses_a_request_with_no_contracts() -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(AppState::for_test(None).await?)
                .service(start_contracts),
        )
        .await;
        let party = pid(9)?;
        let request = actix_test::TestRequest::post()
            .uri("/contracts")
            .set_json(serde_json::json!({
                "decentralized_party_id": party.to_string(),
                "participant_ids": [pid(10)?.to_string()],
                "participant_parties": [pid(11)?.to_string()],
                "operator_party": party.to_string(),
                "contracts": [],
            }))
            .to_request();
        request.extensions_mut().insert(Principal {
            sub: "admin".to_string(),
            issuer: "test".to_string(),
            roles: Vec::new(),
            email: None,
        });

        let response = actix_test::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }
}
