//! The contracts workflow: start, status and invites.

use std::collections::HashSet;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::server::handlers::parties::resolve_owner_keys_from_peers;
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType},
    server::{
        AppState,
        error::ApiError,
        middleware::require_admin,
        types::{
            ContractsInvitePayload, ContractsRequest, ErrorResponse, WorkflowInstance,
            WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole, WorkflowStatusResponse,
        },
    },
    workflow::{self, ContractsStep},
};

use super::{
    invites::{InviteLabels, Recipients, send_invites},
    launch::{CoordinatorRequest, StatusOrder, launch_coordinator},
    preflight::{
        find_inprogress_run_for_party, format_incompatible_peers, preflight_incompatible_peers,
    },
    runs::{kind_status, now_secs, register_and_persist},
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
    start_contracts_run(&data, &body)
        .await
        .unwrap_or_else(ApiError::into_response)
}

/// Check a contracts request, resolve and commit the deployment, then
/// register the run and start it.
async fn start_contracts_run(
    data: &web::Data<AppState>,
    body: &ContractsRequest,
) -> std::result::Result<HttpResponse, ApiError> {
    // Peers sign one prepared create per contract, so an empty run would
    // invite them and then fail on every one of them at signing.
    if body.contracts.is_empty() {
        return Err(ApiError::BadRequest(
            "The request lists no contracts to deploy".to_string(),
        ));
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
        return Err(ApiError::Conflict(format!(
            "Party {} already has a {kind} workflow in flight (run {run}); wait for it \
                 to finish or cancel it first",
            body.decentralized_party_id
        )));
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible =
        preflight_incompatible_peers(&data.config, &data.db, &contracts_invitees).await;
    if !incompatible.is_empty() {
        return Err(ApiError::Conflict(format_incompatible_peers(&incompatible)));
    }

    // Resolve aliases once, before persisting or inviting. Preparation and
    // retries must use the same package IDs the operators approved.
    workflow::contracts::steps::prepare::resolve_package_ids(
        &data.config,
        &mut contracts_config.contracts,
    )
    .await
    .map_err(|error| {
        ApiError::BadRequest(format!("Cannot resolve deployment packages: {error:#}"))
    })?;

    // Commit to the exact creates before persisting or inviting. Peers verify
    // the prepared transactions against these, including on a resumed run.
    let committed =
        workflow::contracts::steps::prepare::commit_deployment(&data.db, &contracts_config)
            .await
            .map_err(|error| {
                ApiError::BadRequest(format!("Cannot commit the deployment: {error:#}"))
            })?;
    contracts_config.committed_deployment = Some(committed);

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    register_and_persist(
        data,
        &instance,
        ContractsStep::WaitingForPeers,
        &contracts_config,
        &contracts_invitees,
        Some(body.decentralized_party_id.clone()),
    )
    .await?;

    let workflow_auth = data.auth.read().await.clone();
    let contracts_invitees_for_invites = contracts_invitees.clone();
    *contracts_state.invited_peers.write().await = contracts_invitees;

    // Send invites to this party's members before starting coordinator workflow
    let invites = {
        let config = data.config.clone();
        let db = data.db.clone();
        let contracts_config = contracts_config.clone();
        Box::pin(async move {
            send_contracts_invites(
                &config,
                &db,
                &contracts_config,
                &contracts_invitees_for_invites,
            )
            .await
        })
    };
    // Refresh dec_party cache to pick up new contracts
    let refresh: Box<dyn FnOnce() + Send> = {
        let config = data.config.clone();
        let db = data.db.clone();
        let auth_lock = data.auth.clone();
        let discovery_gate = super::DiscoveryGate::of(data);
        let party_credentials = data.party_credentials.clone();
        Box::new(move || {
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
                        tracing::warn!("Failed to refresh parties after contract deployment: {e}");
                    }
                }
            });
        })
    };
    launch_coordinator(
        data,
        &instance,
        CoordinatorRequest::Contracts(contracts_config),
        workflow_auth,
        invites,
        StatusOrder::RowFirst,
        Some(refresh),
    )
    .await;

    Ok(HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Contracts workflow started".to_string(),
        instance_name: instance_name_for_run,
    }))
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

/// The contracts invite. It carries the dec party, the member set and the
/// package names, so the peer card renders the same rich summary the
/// coordinator shows (mirrors the Kick invite).
pub(super) fn contracts_invite(
    contracts_config: &workflow::ContractsConfig,
    invitees: &[CantonId],
) -> Result<Message> {
    // Skip empty names, then sort+dedup — `dedup` only removes adjacent
    // duplicates, and multiple contracts can share a package name.
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
    Ok(Message::new(MessageType::InviteContracts, payload_bytes))
}

/// Send contracts invites to this party's members (`invitees`) over Noise —
/// never to every configured peer (see start_contracts).
async fn send_contracts_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    contracts_config: &workflow::ContractsConfig,
    invitees: &[CantonId],
) -> Result {
    send_invites(
        config,
        db,
        Recipients::Members(invitees),
        InviteLabels {
            kind: "contracts",
            skip: "",
            failed: "contracts ",
        },
        || contracts_invite(contracts_config, invitees),
    )
    .await
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

#[cfg(test)]
mod start_contracts_tests {
    //! Pins every answer `POST /contracts` gives before it starts a run: the
    //! status code and the exact JSON error body.

    use actix_web::http::StatusCode;
    use serde_json::json;

    use super::super::{
        preflight::{format_incompatible_peers, preflight_incompatible_peers},
        test_support::{
            Answer, TestNode, break_table, cache_party, closed_port, in_progress_run, insert_peers,
            insert_run, peer, pid, post_as, post_as_admin, test_node,
        },
    };
    use super::*;
    use crate::db::MIGRATOR;

    type Result<T = ()> = anyhow::Result<T>;

    const PACKAGE: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    /// This node is `pid(1)` and hosts the party `pid(9)`, threshold 1,
    /// with its peer `pid(2)`, which has no usable Noise key unless
    /// `reachable_keys`.
    async fn two_host_party(pool: SqlitePool, reachable_keys: bool) -> Result<TestNode> {
        let (p1, p2) = (pid(1)?, pid(2)?);
        cache_party(
            &pool,
            &pid(9)?,
            1,
            &[(&p1, Some("k1")), (&p2, Some("k2"))],
            &[("k1", Some(&p1)), ("k2", Some(&p2))],
        )
        .await?;
        insert_peers(&pool, &[peer(&p2, closed_port()?, reachable_keys)]).await?;
        test_node(pool, &p1).await
    }

    fn deploy(package_id: &str) -> Result<serde_json::Value> {
        Ok(json!({
            "decentralized_party_id": pid(9)?.to_string(),
            "participant_ids": [pid(1)?.to_string(), pid(2)?.to_string()],
            "participant_parties": [pid(11)?.to_string(), pid(12)?.to_string()],
            "operator_party": pid(9)?.to_string(),
            "contracts": [{
                "id": "rules",
                "name": "Governance Core",
                "package_id": package_id,
                "module_name": "Governance.Rules",
                "entity_name": "GovernanceRules",
                "fields": [],
            }],
        }))
    }

    async fn post(node: &TestNode, request: serde_json::Value) -> Answer {
        post_as_admin(&node.data, start_contracts, "/contracts", request).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_anonymous_caller_is_unauthorized(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        post_as(
            &node.data,
            start_contracts,
            "/contracts",
            deploy(PACKAGE)?,
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_request_without_contracts_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        let mut request = deploy(PACKAGE)?;
        request["contracts"] = json!([]);
        post(&node, request).await.assert_error(
            StatusCode::BAD_REQUEST,
            "The request lists no contracts to deploy",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_with_a_run_in_flight_is_a_conflict(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        let node = two_host_party(pool.clone(), false).await?;
        insert_run(&pool, &in_progress_run("busy-run", &party)).await?;
        post(&node, deploy(PACKAGE)?).await.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "Party {party} already has a Contracts workflow in flight (run busy-run); \
                 wait for it to finish or cancel it first"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unverifiable_invitee_is_a_conflict(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), true).await?;
        let incompatible = preflight_incompatible_peers(&node.data.config, &pool, &[pid(2)?]).await;
        assert_eq!(incompatible.len(), 1, "{incompatible:?}");
        post(&node, deploy(PACKAGE)?).await.assert_error(
            StatusCode::CONFLICT,
            &format_incompatible_peers(&incompatible),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unresolvable_package_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        post(&node, deploy("not-a-package")?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "Cannot resolve deployment packages: Invalid package ID not-a-package",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_uncached_party_cannot_commit_the_deployment(pool: SqlitePool) -> Result {
        let node = test_node(pool, &pid(1)?).await?;
        let party = pid(9)?;
        post(&node, deploy(PACKAGE)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "Cannot commit the deployment: Decentralized party {party} not found in \
                 cache; refresh /decentralized-parties before deploying contracts"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_write_is_a_conflict_and_leaves_no_run(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), false).await?;
        break_table(&pool, "workflow_runs").await?;
        let answer = post(&node, deploy(PACKAGE)?).await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Failed to start contracts workflow: ")
                && error.contains("workflow_runs"),
            "{error}"
        );
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
        assert!(node.data.workflows.snapshot().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_valid_deployment_is_accepted(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        let answer = post(&node, deploy(PACKAGE)?).await;
        assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let instance = body["instance_name"].as_str().unwrap_or_default();
        assert!(instance.starts_with("validator-9-contracts-"), "{body}");
        assert_eq!(
            answer.body,
            format!(
                r#"{{"status":"inprogress","message":"Contracts workflow started","instance_name":"{instance}"}}"#
            )
        );
        Ok(())
    }
}
