//! The add-party workflow: start, status, cancel and invites.

use std::{collections::HashSet, future::Future, time::Duration};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use anyhow::Context;
use sqlx::SqlitePool;

use super::{
    cancel::cancel_workflow_state,
    invites::{InviteLabels, Recipients, invite_log, send_invites},
    kick_target::cached_confirmation_threshold,
    launch::{CoordinatorRequest, RunEnd, StatusOrder, launch_coordinator, run_log},
    preflight::{claim_party_for_start, format_incompatible_peers, preflight_incompatible_peers},
    runs::{kind_status, register_and_persist},
};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, Peer},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType},
    server::{
        AppState,
        error::ApiError,
        middleware::require_admin,
        types::{
            AddPartyInvitePayload, AddPartyRequest, ErrorResponse, MessageResponse,
            WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole,
            WorkflowStatusResponse,
        },
    },
    utils::now_secs,
    workflow::{
        self, AddPartyStep,
        add_party::{MissingPackages, packages::check_new_member_packages},
    },
};

// ============================================================================
// Add-Party Workflow
// ============================================================================

/// Start an add-party workflow to add a new member to a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = AddPartyRequest,
    responses(
        (status = 202, description = "Add-party workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress for this party, the participant is already a member, or it has not vetted a package the party's contracts need", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 503, description = "The new member's packages could not be checked", body = ErrorResponse)
    )
)]
#[post("/add-party")]
pub async fn start_add_party(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<AddPartyRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    start_add_party_run(&data, &body)
        .await
        .unwrap_or_else(ApiError::into_response)
}

/// Check an add-party request against the peers and the cached party, then
/// register the run and start it.
async fn start_add_party_run(
    data: &web::Data<AppState>,
    body: &AddPartyRequest,
) -> Result<HttpResponse, ApiError> {
    tracing::info!(
        "Add-party request received: party={}, new_participant={}, threshold={}",
        body.decentralized_party_id,
        body.new_participant_id,
        body.new_threshold
    );

    let decentralized_party_id = body.decentralized_party_id.clone();
    let new_participant_id = body.new_participant_id.clone();

    if new_participant_id == *data.config.participant_id() {
        return Err(ApiError::BadRequest(
            "Cannot add yourself as the new participant".to_string(),
        ));
    }

    let peers = data.db.get_all_peers().await.map_err(|e| {
        tracing::error!("Failed to load peers for add-party: {e}");
        ApiError::Internal("Failed to load peers".to_string())
    })?;

    refuse_unknown_peer(&peers, &new_participant_id)?;

    // Scope the existing-member set to the party's cached membership (same
    // rationale as start_kick — the party can be a strict subset of the
    // configured mesh).
    let party_member_ids: HashSet<CantonId> = data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to load dec party participants for add-party: {e}");
            ApiError::Internal("Failed to load decentralized party members".to_string())
        })?
        .iter()
        .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
        .collect();
    let invitees = plan_add_party(body, data.config.participant_id(), &party_member_ids, peers)?;

    let confirmation_threshold = cached_confirmation_threshold(
        &data.db,
        &decentralized_party_id,
        party_member_ids.len() + 1,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to read the cached thresholds for add-party: {e}");
        ApiError::Internal("Failed to read the party's cached thresholds".to_string())
    })?;

    let timestamp = now_secs();
    let instance_name = format!("{}-add-party-{timestamp}", decentralized_party_id.prefix);

    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::AddParty,
        WorkflowRole::Coordinator,
    );
    let add_party_state = &instance.http;

    let add_party_config = workflow::AddPartyConfig::new(
        decentralized_party_id.clone(),
        new_participant_id.clone(),
        body.new_threshold,
        confirmation_threshold,
        body.previous_threshold,
        instance_name.clone(),
    );

    // Refuse a second workflow targeting the SAME decentralized party (see
    // start_kick for the rationale — topology mutations must not interleave).
    let _start = claim_party_for_start(data, &decentralized_party_id).await?;

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &invitees).await;
    if !incompatible.is_empty() {
        return Err(ApiError::Conflict(format_incompatible_peers(&incompatible)));
    }

    // A missing DAR must stop the add before any invite goes out, so no peer
    // waits and the new member generates no keys for a run that cannot finish.
    let ledger_token = {
        let auth = data.auth.read().await.clone();
        workflow::add_party::resolve_ledger_token(&auth, &decentralized_party_id).await
    };
    let check = check_new_member_packages(&data.config, &add_party_config, ledger_token.as_deref());
    if let Err(e) = bounded_package_check(ADD_PARTY_PACKAGE_CHECK_TIMEOUT, check).await {
        return Err(add_party_package_check_failure(&decentralized_party_id, &e));
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    register_and_persist(
        data,
        &instance,
        AddPartyStep::WaitingForPeers,
        &add_party_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await?;

    let invitees_for_invites = invitees.clone();
    *add_party_state.invited_peers.write().await = invitees;

    // Ledger token source for the begin-offset capture (see
    // `current_ledger_offset`); the workflow proceeds without it.
    let workflow_auth = data.auth.read().await.clone();

    let invites = {
        let config = data.config.clone();
        let db = data.db.clone();
        let add_party_config = add_party_config.clone();
        Box::pin(async move {
            send_add_party_invites(&config, &db, &add_party_config, &invitees_for_invites).await
        })
    };
    launch_coordinator(
        data,
        &instance,
        CoordinatorRequest::AddParty(add_party_config),
        workflow_auth,
        invites,
        RunEnd {
            order: StatusOrder::RowFirst,
            log: run_log!(),
            after_success: None,
        },
    )
    .await;

    Ok(HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Add-party workflow started".to_string(),
        instance_name,
    }))
}

/// Refuse a new member that is not a configured peer. The new member must
/// be a configured, known peer — invites travel over Noise, so a
/// participant this node can't reach can't join.
fn refuse_unknown_peer(peers: &[Peer], new_participant_id: &CantonId) -> Result<(), ApiError> {
    if !peers
        .iter()
        .any(|p| p.participant_id == *new_participant_id)
    {
        return Err(ApiError::BadRequest(format!(
            "Participant {new_participant_id} is not a configured peer of this node"
        )));
    }
    Ok(())
}

/// Check an add-party request against the party's cached members, and work
/// out whom to invite. It reads only its inputs.
fn plan_add_party(
    request: &AddPartyRequest,
    self_id: &CantonId,
    party_member_ids: &HashSet<CantonId>,
    peers: Vec<Peer>,
) -> Result<Vec<CantonId>, ApiError> {
    let decentralized_party_id = &request.decentralized_party_id;
    let new_participant_id = &request.new_participant_id;
    if party_member_ids.is_empty() {
        return Err(ApiError::Conflict(format!(
            "No cached membership for {decentralized_party_id}. Try refreshing \
                 /decentralized-parties first."
        )));
    }
    if party_member_ids.contains(new_participant_id) {
        return Err(ApiError::Conflict(format!(
            "Participant {new_participant_id} is already a member of \
                 {decentralized_party_id}"
        )));
    }
    if !party_member_ids.contains(self_id) {
        return Err(ApiError::BadRequest(format!(
            "This node is not a member of {decentralized_party_id}; only an existing \
                 member can coordinate adding one"
        )));
    }

    // Bound the new threshold by the members that can actually sign, which
    // excludes the one being added. It carries Canton's onboarding marker until
    // its ACS import completes, so it confirms nothing — and a P2P add at
    // threshold = the post-add member count is the known full-threshold bug:
    // the write never becomes effective, and the run stalls with no signal
    // saying why.
    //
    // Raising the threshold to include the new member is a second step, after
    // its marker clears, via the change-threshold workflow. The deeper
    // validation against the live DNS owner set happens in ExportState.
    let signing_member_count = party_member_ids.len() as i32;
    if request.new_threshold < 1 || request.new_threshold > signing_member_count {
        return Err(ApiError::BadRequest(format!(
            "new_threshold must be between 1 and {signing_member_count} (the members that \
                 can sign; the member being added cannot until its ACS import completes); got \
                 {got}. To include it, run change-threshold once its onboarding marker clears",
            got = request.new_threshold,
        )));
    }

    // Invitees: every existing member except self, plus the new member.
    let mut invitees: Vec<CantonId> = peers
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| party_member_ids.contains(p) && p != self_id)
        .collect();
    invitees.push(new_participant_id.clone());

    Ok(invitees)
}

/// How long an add-party start waits for the package check. The check streams
/// the party's active contracts, so a large party needs time. A Canton call
/// that stalls must still not leave the Add Member dialog waiting forever.
const ADD_PARTY_PACKAGE_CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// Run the add-party package check under `limit`. A check that runs out of
/// time is a check that could not run.
async fn bounded_package_check(limit: Duration, check: impl Future<Output = Result>) -> Result {
    match tokio::time::timeout(limit, check).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "the check did not finish within {secs}s",
            secs = limit.as_secs()
        ),
    }
}

/// The response to a failed package check of an add-party start. A missing
/// package says what to do, so it goes back as it is. Any other failure means
/// the check could not run, so the add is refused until it can.
fn add_party_package_check_failure(party: &CantonId, e: &anyhow::Error) -> ApiError {
    match e.downcast_ref::<MissingPackages>() {
        Some(missing) => {
            tracing::warn!("Add-party to {party} refused: {missing}");
            ApiError::Conflict(missing.to_string())
        }
        None => {
            tracing::error!("Cannot check the new member's packages for {party}: {e:#}");
            ApiError::ServiceUnavailable(format!("Cannot check the new member's packages: {e:#}"))
        }
    }
}

/// Get the current status of the add-party workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Add-party workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/add-party/status")]
pub async fn get_add_party_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::AddParty).await)
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/add-party/cancel")]
pub async fn cancel_add_party(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Add-party", WorkflowKind::AddParty).await
}

/// The add-party invite. Its card shows the full post-add member set: the
/// invitees and this node, `self_id`.
pub(super) fn add_party_invite(
    self_id: &CantonId,
    add_party_config: &workflow::AddPartyConfig,
    invitees: &[CantonId],
) -> Result<Message> {
    let mut participants = invitees.to_vec();
    participants.push(self_id.clone());
    let payload = AddPartyInvitePayload {
        dec_party_id: add_party_config.decentralized_party_id.clone(),
        new_participant: add_party_config.new_participant_id.clone(),
        new_threshold: add_party_config.new_threshold,
        previous_threshold: add_party_config.previous_threshold,
        confirmation_threshold: add_party_config.confirmation_threshold,
        participants,
        workflow_instance: Some(add_party_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode AddPartyInvitePayload")?;
    Ok(Message::new(MessageType::InviteAddParty, payload_bytes))
}

/// Send add-party invites over Noise to the existing members and the new
/// member (`invitees` carries both).
async fn send_add_party_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    add_party_config: &workflow::AddPartyConfig,
    invitees: &[CantonId],
) -> Result {
    send_invites(
        config,
        db,
        Recipients::Members(invitees),
        InviteLabels {
            kind: "add-party",
            skip: "add-party ",
            failed: "add-party ",
            address: true,
            empty_key_warning: true,
        },
        invite_log!(),
        || add_party_invite(config.participant_id(), add_party_config, invitees),
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};

    use super::{
        super::test_support::{
            admin, cache_party, host_row, in_progress_run, insert_run, pid, state_on,
            status_and_body,
        },
        *,
    };
    use crate::db::{
        MIGRATOR,
        schema::{Commitable, SchemaWrite},
    };

    async fn error_body(response: HttpResponse) -> anyhow::Result<String> {
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let body: serde_json::Value = serde_json::from_slice(&body)?;
        body["error"]
            .as_str()
            .map(str::to_string)
            .context("the response has no error text")
    }

    /// A missing package goes back as a conflict that names the ids, because
    /// the operator fixes it by uploading the DAR.
    #[actix_web::test]
    async fn a_missing_package_refuses_the_add_with_the_missing_ids() -> anyhow::Result<()> {
        let error = anyhow::Error::from(workflow::add_party::MissingPackages {
            participant: pid(3)?,
            missing: vec!["pkg-a".to_string(), "pkg-b".to_string()],
        });

        let response = add_party_package_check_failure(&pid(1)?, &error).into_response();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let error = error_body(response).await?;
        assert!(
            error.contains(&pid(3)?.to_string()),
            "names the new member: {error}"
        );
        assert!(
            error.contains(r#"Missing package ids: ["pkg-a", "pkg-b"]"#),
            "lists the missing ids: {error}"
        );
        // The add-member guide tells the operator to search for this text.
        assert!(
            error.contains("has not vetted 2 package(s)"),
            "counts the missing ids: {error}"
        );
        Ok(())
    }

    /// A Canton call that stalls ends in a 503, not in a request that never
    /// answers.
    #[actix_web::test]
    async fn a_stalled_package_check_refuses_the_add_as_unavailable() -> anyhow::Result<()> {
        let stalled = std::future::pending::<Result>();

        let error = bounded_package_check(Duration::from_millis(10), stalled)
            .await
            .expect_err("a check that never finishes must fail");
        let response = add_party_package_check_failure(&pid(1)?, &error).into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error = error_body(response).await?;
        assert!(error.contains("did not finish within"), "{error}");
        Ok(())
    }

    /// A check that finishes in time passes its own result through, so a
    /// missing package is still a 409 and not a timeout.
    #[actix_web::test]
    async fn a_check_that_finishes_in_time_keeps_its_own_answer() -> anyhow::Result<()> {
        bounded_package_check(Duration::from_secs(5), async { Ok(()) }).await?;

        let missing = anyhow::Error::from(workflow::add_party::MissingPackages {
            participant: pid(3)?,
            missing: vec!["pkg-a".to_string()],
        });
        let error = bounded_package_check(Duration::from_secs(5), async { Err(missing) })
            .await
            .expect_err("the missing package must come through");
        let response = add_party_package_check_failure(&pid(1)?, &error).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        Ok(())
    }

    /// A check that could not run refuses the add as well. Letting it through
    /// unchecked would send the invites the check exists to hold back.
    #[actix_web::test]
    async fn a_check_that_cannot_run_refuses_the_add_as_unavailable() -> anyhow::Result<()> {
        let error = anyhow::anyhow!("ledger API unreachable");

        let response = add_party_package_check_failure(&pid(1)?, &error).into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error = error_body(response).await?;
        assert!(
            error.contains("ledger API unreachable"),
            "carries the cause: {error}"
        );
        Ok(())
    }

    /// [`state_on`], with `pid(1)` as this node. Its Ledger and Admin API
    /// addresses do not parse, so every Canton read fails at once.
    async fn add_party_state_on(pool: SqlitePool) -> Result<web::Data<AppState>> {
        let state = Arc::try_unwrap(state_on(pool).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(pid(1)?);
        config.canton.ledger_api_host = "[".to_string();
        config.canton.admin_api_host = "[".to_string();
        Ok(web::Data::new(AppState { config, ..state }))
    }

    /// Caches `party` hosted by this node and `pid(2)` at threshold 1. Both
    /// `pid(2)` and `pid(3)` are configured peers, so `pid(3)` passes every
    /// request check as the member to add.
    async fn cache_party_to_grow(pool: &SqlitePool, party: &CantonId) -> Result {
        cache_party(pool, party, 1, None).await?;
        let keys = ["k1".to_string(), "k2".to_string()];
        let hosts = [
            host_row(&pid(1)?, Some("k1")),
            host_row(&pid(2)?, Some("k2")),
        ];
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(party, &hosts).await?;
        tx.replace_dec_party_owners(party, &keys).await?;
        for tag in [2, 3] {
            tx.insert_peer(&Peer {
                participant_id: pid(tag)?,
                name: format!("peer-{tag}"),
                address: "127.0.0.1".to_string(),
                port: 1,
                public_key: String::new(),
                party: None,
            })
            .await?;
        }
        Commitable::commit(tx).await
    }

    async fn post_add_party(
        data: web::Data<AppState>,
        party: &CantonId,
        new_member: &CantonId,
    ) -> Result<(StatusCode, String)> {
        let app =
            actix_test::init_service(App::new().app_data(data).service(start_add_party)).await;
        let request = actix_test::TestRequest::post()
            .uri("/add-party")
            .set_json(serde_json::json!({
                "decentralized_party_id": party,
                "new_participant_id": new_member,
                "new_threshold": 1,
                "previous_threshold": 1,
            }))
            .to_request();
        request.extensions_mut().insert(admin());
        Ok(status_and_body(actix_test::call_service(&app, request).await).await)
    }

    /// The add passes every request check, but Canton cannot be read. The
    /// start request must refuse it with 503 and leave no run behind. A run
    /// is what sends the invites, so no peer is asked to join unchecked.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_add_whose_package_check_cannot_run_starts_no_run(pool: SqlitePool) -> Result {
        let party = pid(60)?;
        cache_party_to_grow(&pool, &party).await?;
        let data = add_party_state_on(pool.clone()).await?;

        let (status, body) = post_add_party(data.clone(), &party, &pid(3)?).await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(
            body.contains("Cannot check the new member's packages"),
            "{body}"
        );
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::AddParty)
                .await?
                .is_empty(),
            "a refused add must not persist a run"
        );
        assert!(
            data.workflows.snapshot().is_empty(),
            "a refused add must not register a run"
        );
        let (retry_status, retry_body) = post_add_party(data, &party, &pid(3)?).await?;
        assert_eq!(
            retry_status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{retry_body}"
        );
        assert!(retry_body.contains("Cannot check the new member's packages"));
        Ok(())
    }

    /// The request checks run before the package check, which reads the
    /// party's whole ACS. Canton cannot be read here, so a 503 would mean the
    /// package check ran first. Each refusal must come from its request check.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_request_checks_refuse_an_add_before_the_package_check(pool: SqlitePool) -> Result {
        let party = pid(61)?;
        cache_party_to_grow(&pool, &party).await?;
        let data = add_party_state_on(pool.clone()).await?;

        let (status, body) = post_add_party(data.clone(), &party, &pid(2)?).await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("is already a member of"), "{body}");

        // The in-flight guard is the last request check that needs no peer.
        // Only the peer version gate runs between it and the package check.
        insert_run(&pool, &in_progress_run("contracts-in-flight", &party)).await?;
        let (status, body) = post_add_party(data, &party, &pid(3)?).await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            body.contains("already has a Contracts workflow in flight"),
            "{body}"
        );
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::AddParty)
                .await?
                .is_empty()
        );
        Ok(())
    }
}

#[cfg(test)]
mod start_add_party_tests {
    //! Pins every answer `POST /add-party` gives before it starts a run: the
    //! status code and the exact JSON error body.

    use actix_web::http::StatusCode;
    use serde_json::json;

    use super::{
        super::{
            preflight::{format_incompatible_peers, preflight_incompatible_peers},
            test_support::{
                Answer, TestNode, break_table, cache_hosted_party, closed_port, in_progress_run,
                insert_peers, insert_run, peer, pid, post_as, post_as_admin, test_node,
            },
        },
        *,
    };
    use crate::db::MIGRATOR;

    type Result<T = ()> = anyhow::Result<T>;

    /// This node is `pid(1)` and hosts the party `pid(9)` with `pid(2)`.
    /// Its peers are `pid(2)` and `pid(4)`, the member to add, without a
    /// usable Noise key unless `reachable_keys`.
    async fn two_host_party(pool: SqlitePool, reachable_keys: bool) -> Result<TestNode> {
        let (p1, p2, p4) = (pid(1)?, pid(2)?, pid(4)?);
        cache_hosted_party(
            &pool,
            &pid(9)?,
            1,
            &[(&p1, Some("k1")), (&p2, Some("k2"))],
            &[("k1", Some(&p1)), ("k2", Some(&p2))],
        )
        .await?;
        let port = closed_port()?;
        insert_peers(
            &pool,
            &[
                peer(&p2, port, reachable_keys),
                peer(&p4, port, reachable_keys),
            ],
        )
        .await?;
        test_node(pool, &p1).await
    }

    fn add(new_participant: &CantonId, new_threshold: i32) -> Result<serde_json::Value> {
        Ok(json!({
            "decentralized_party_id": pid(9)?.to_string(),
            "new_participant_id": new_participant.to_string(),
            "new_threshold": new_threshold,
        }))
    }

    async fn post(node: &TestNode, request: serde_json::Value) -> Answer {
        post_as_admin(&node.data, start_add_party, "/add-party", request).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_anonymous_caller_is_unauthorized(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        post_as(
            &node.data,
            start_add_party,
            "/add-party",
            add(&pid(4)?, 1)?,
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn adding_this_node_is_refused(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        post(&node, add(&pid(1)?, 1)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "Cannot add yourself as the new participant",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_peer_read_answers_500(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), false).await?;
        break_table(&pool, "peers").await?;
        post(&node, add(&pid(4)?, 1)?)
            .await
            .assert_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load peers");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_new_member_must_be_a_peer(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        let p5 = pid(5)?;
        post(&node, add(&p5, 1)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            &format!("Participant {p5} is not a configured peer of this node"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_member_read_answers_500(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), false).await?;
        break_table(&pool, "dec_party_participant").await?;
        post(&node, add(&pid(4)?, 1)?).await.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load decentralized party members",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_without_cached_members_is_a_conflict(pool: SqlitePool) -> Result {
        insert_peers(&pool, &[peer(&pid(4)?, closed_port()?, false)]).await?;
        let node = test_node(pool, &pid(1)?).await?;
        let party = pid(9)?;
        post(&node, add(&pid(4)?, 1)?).await.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "No cached membership for {party}. Try refreshing /decentralized-parties first."
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_current_member_is_a_conflict(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        let (p2, party) = (pid(2)?, pid(9)?);
        post(&node, add(&p2, 1)?).await.assert_error(
            StatusCode::CONFLICT,
            &format!("Participant {p2} is already a member of {party}"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn only_a_member_coordinates(pool: SqlitePool) -> Result {
        let (p2, p3, p4) = (pid(2)?, pid(3)?, pid(4)?);
        cache_hosted_party(
            &pool,
            &pid(9)?,
            1,
            &[(&p2, Some("k2")), (&p3, Some("k3"))],
            &[("k2", Some(&p2)), ("k3", Some(&p3))],
        )
        .await?;
        insert_peers(&pool, &[peer(&p4, closed_port()?, false)]).await?;
        let node = test_node(pool, &pid(1)?).await?;
        let party = pid(9)?;
        post(&node, add(&p4, 1)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "This node is not a member of {party}; only an existing member can \
                 coordinate adding one"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_threshold_out_of_range_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        post(&node, add(&pid(4)?, 3)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "new_threshold must be between 1 and 2 (the members that can sign; the member \
             being added cannot until its ACS import completes); got 3. To include it, run \
             change-threshold once its onboarding marker clears",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_threshold_read_answers_500(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), false).await?;
        break_table(&pool, "dec_party").await?;
        post(&node, add(&pid(4)?, 1)?).await.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to read the party's cached thresholds",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_with_a_run_in_flight_is_a_conflict(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        let node = two_host_party(pool.clone(), false).await?;
        insert_run(&pool, &in_progress_run("busy-run", &party)).await?;
        post(&node, add(&pid(4)?, 1)?).await.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "Party {party} already has a Contracts workflow in flight (run busy-run); \
                 wait for it to finish or cancel it from its coordinator"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_another_request_is_starting_is_a_conflict(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        let node = two_host_party(pool, false).await?;
        let _claim = node
            .data
            .workflows
            .claim_start(&party)
            .ok_or_else(|| anyhow::anyhow!("a fresh registry has no claims"))?;
        post(&node, add(&pid(4)?, 1)?).await.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "Another request is starting a workflow for {party}; retry once it has started"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unverifiable_invitee_is_a_conflict(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), true).await?;
        let incompatible =
            preflight_incompatible_peers(&node.data.config, &pool, &[pid(2)?, pid(4)?]).await;
        assert_eq!(incompatible.len(), 2, "{incompatible:?}");
        post(&node, add(&pid(4)?, 1)?).await.assert_error(
            StatusCode::CONFLICT,
            &format_incompatible_peers(&incompatible),
        );
        Ok(())
    }

    /// Main's package check reads the party's contracts from Canton before
    /// the run write. The test node's Canton ports are closed, so every add
    /// that passes the request checks answers 503 and starts no run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_add_past_the_request_checks_meets_the_package_check(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), false).await?;
        let answer = post(&node, add(&pid(4)?, 1)?).await;
        assert_eq!(
            answer.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            answer.body
        );
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Cannot check the new member's packages: "),
            "{error}"
        );
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
        assert!(node.data.workflows.snapshot().is_empty());
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::AddParty)
                .await?
                .is_empty()
        );
        Ok(())
    }
}

#[cfg(test)]
mod plan_add_party_tests {
    //! Calls the pure add-party checks with members and peers, and pins each
    //! refusal text.

    use super::{
        super::test_support::{peer, pid},
        *,
    };

    type Result<T = ()> = anyhow::Result<T>;

    fn request(new_participant: &CantonId, new_threshold: i32) -> Result<AddPartyRequest> {
        Ok(AddPartyRequest {
            decentralized_party_id: pid(9)?,
            new_participant_id: new_participant.clone(),
            new_threshold,
            previous_threshold: 0,
        })
    }

    fn ids(ids: &[&CantonId]) -> HashSet<CantonId> {
        ids.iter().map(|id| (*id).clone()).collect()
    }

    fn peers(ids: &[&CantonId]) -> Vec<Peer> {
        ids.iter().map(|id| peer(id, 1, false)).collect()
    }

    #[test]
    fn a_new_member_must_be_a_peer() -> Result {
        let (p2, p4) = (pid(2)?, pid(4)?);
        assert_eq!(
            refuse_unknown_peer(&peers(&[&p2]), &p4),
            Err(ApiError::BadRequest(format!(
                "Participant {p4} is not a configured peer of this node"
            )))
        );
        assert_eq!(refuse_unknown_peer(&peers(&[&p2, &p4]), &p4), Ok(()));
        Ok(())
    }

    /// This node is `pid(1)`; `pid(4)` joins.
    #[test]
    fn each_refusal_keeps_its_text() -> Result {
        let (me, p2, p3, p4, party) = (pid(1)?, pid(2)?, pid(3)?, pid(4)?, pid(9)?);
        let cases = [
            (
                ids(&[]),
                1,
                ApiError::Conflict(format!(
                    "No cached membership for {party}. Try refreshing /decentralized-parties \
                     first."
                )),
            ),
            (
                ids(&[&me, &p2, &p4]),
                1,
                ApiError::Conflict(format!("Participant {p4} is already a member of {party}")),
            ),
            (
                ids(&[&p2, &p3]),
                1,
                ApiError::BadRequest(format!(
                    "This node is not a member of {party}; only an existing member can \
                     coordinate adding one"
                )),
            ),
            (
                ids(&[&me, &p2, &p3]),
                4,
                ApiError::BadRequest(
                    "new_threshold must be between 1 and 3 (the members that can sign; the \
                     member being added cannot until its ACS import completes); got 4. To \
                     include it, run change-threshold once its onboarding marker clears"
                        .to_string(),
                ),
            ),
        ];
        for (members, threshold, refusal) in cases {
            assert_eq!(
                plan_add_party(
                    &request(&p4, threshold)?,
                    &me,
                    &members,
                    peers(&[&p2, &p3, &p4])
                ),
                Err(refusal)
            );
        }
        Ok(())
    }

    /// The invite goes to the members this node peers with, never to a peer
    /// outside the party, and last to the new member.
    #[test]
    fn a_plan_invites_the_members_and_the_new_one() -> Result {
        let (me, p2, p3, p4, p7) = (pid(1)?, pid(2)?, pid(3)?, pid(4)?, pid(7)?);
        let invitees = plan_add_party(
            &request(&p4, 3)?,
            &me,
            &ids(&[&me, &p2, &p3]),
            peers(&[&p7, &p3, &p4, &p2]),
        )?;
        assert_eq!(invitees, vec![p3, p2, p4]);
        Ok(())
    }
}
