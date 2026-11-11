//! The add-party workflow: start, status, cancel and invites.

use std::collections::HashSet;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use anyhow::Context;
use sqlx::SqlitePool;

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
            AddPartyInvitePayload, AddPartyRequest, ErrorResponse, MessageResponse,
            WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole,
            WorkflowStatusResponse,
        },
    },
    workflow::{self, AddPartyStep},
};

use super::{
    cancel::cancel_workflow_state,
    invites::{InviteLabels, Recipients, invite_log, send_invites},
    kick_target::cached_confirmation_threshold,
    launch::{CoordinatorRequest, RunEnd, StatusOrder, launch_coordinator, run_log},
    preflight::{
        find_inprogress_run_for_party, format_incompatible_peers, preflight_incompatible_peers,
    },
    runs::{kind_status, now_secs, register_and_persist},
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
        (status = 409, description = "Workflow already in progress for this party, or the participant is already a member", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
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
) -> std::result::Result<HttpResponse, ApiError> {
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

    // The new member must be a configured, known peer — invites travel over
    // Noise, so a participant this node can't reach can't join.
    if !peers.iter().any(|p| p.participant_id == new_participant_id) {
        return Err(ApiError::BadRequest(format!(
            "Participant {new_participant_id} is not a configured peer of this node"
        )));
    }

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
    if party_member_ids.is_empty() {
        return Err(ApiError::Conflict(format!(
            "No cached membership for {decentralized_party_id}. Try refreshing \
                 /decentralized-parties first."
        )));
    }
    if party_member_ids.contains(&new_participant_id) {
        return Err(ApiError::Conflict(format!(
            "Participant {new_participant_id} is already a member of \
                 {decentralized_party_id}"
        )));
    }
    if !party_member_ids.contains(data.config.participant_id()) {
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
    if body.new_threshold < 1 || body.new_threshold > signing_member_count {
        return Err(ApiError::BadRequest(format!(
            "new_threshold must be between 1 and {signing_member_count} (the members that \
                 can sign; the member being added cannot until its ACS import completes); got \
                 {got}. To include it, run change-threshold once its onboarding marker clears",
            got = body.new_threshold,
        )));
    }

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

    // Invitees: every existing member except self, plus the new member.
    let mut invitees: Vec<CantonId> = peers
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| party_member_ids.contains(p) && p != data.config.participant_id())
        .collect();
    invitees.push(new_participant_id.clone());

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
    if let Some((run, kind)) =
        find_inprogress_run_for_party(&data.db, &decentralized_party_id).await
    {
        return Err(ApiError::Conflict(format!(
            "Party {decentralized_party_id} already has a {kind} workflow in flight \
                 (run {run}); wait for it to finish or cancel it first"
        )));
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &invitees).await;
    if !incompatible.is_empty() {
        return Err(ApiError::Conflict(format_incompatible_peers(&incompatible)));
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
        },
        invite_log!(),
        || add_party_invite(config.participant_id(), add_party_config, invitees),
    )
    .await
}

#[cfg(test)]
mod start_add_party_tests {
    //! Pins every answer `POST /add-party` gives before it starts a run: the
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

    /// This node is `pid(1)` and hosts the party `pid(9)` with `pid(2)`.
    /// Its peers are `pid(2)` and `pid(4)`, the member to add, without a
    /// usable Noise key unless `reachable_keys`.
    async fn two_host_party(pool: SqlitePool, reachable_keys: bool) -> Result<TestNode> {
        let (p1, p2, p4) = (pid(1)?, pid(2)?, pid(4)?);
        cache_party(
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
        cache_party(
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
                 wait for it to finish or cancel it first"
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

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_write_is_a_conflict_and_leaves_no_run(pool: SqlitePool) -> Result {
        let node = two_host_party(pool.clone(), false).await?;
        break_table(&pool, "workflow_runs").await?;
        let answer = post(&node, add(&pid(4)?, 1)?).await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Failed to start add-party workflow: ")
                && error.contains("workflow_runs"),
            "{error}"
        );
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
        assert!(node.data.workflows.snapshot().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_valid_add_is_accepted(pool: SqlitePool) -> Result {
        let node = two_host_party(pool, false).await?;
        let answer = post(&node, add(&pid(4)?, 1)?).await;
        assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let instance = body["instance_name"].as_str().unwrap_or_default();
        assert!(instance.starts_with("validator-9-add-party-"), "{body}");
        assert_eq!(
            answer.body,
            format!(
                r#"{{"status":"inprogress","message":"Add-party workflow started","instance_name":"{instance}"}}"#
            )
        );
        Ok(())
    }
}
