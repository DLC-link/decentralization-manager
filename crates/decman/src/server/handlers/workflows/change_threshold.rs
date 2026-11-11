//! The change-threshold workflow, and the confirmation threshold check it shares with external parties.

use std::collections::HashSet;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use anyhow::Context;
use sqlx::SqlitePool;

use super::{
    cancel::cancel_workflow_state,
    invites::{InviteLabels, Recipients, invite_log, send_invites},
    launch::{CoordinatorRequest, RunEnd, StatusOrder, launch_coordinator, run_log},
    preflight::{
        find_inprogress_run_for_party, format_incompatible_peers, preflight_incompatible_peers,
    },
    runs::{kind_status, now_secs, register_and_persist},
};
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
            ChangeThresholdInvitePayload, ChangeThresholdRequest, ErrorResponse, MessageResponse,
            WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole,
            WorkflowStatusResponse,
        },
    },
    workflow::{self, ChangeThresholdStep, thresholds::check_confirmation_threshold},
};

// ============================================================================
// Change-Threshold Workflow
// ============================================================================

/// Start a change-threshold workflow to change the namespace and
/// confirmation thresholds of an existing decentralized party, keeping its
/// membership.
#[utoipa::path(
    tag = "Workflows",
    request_body = ChangeThresholdRequest,
    responses(
        (status = 202, description = "Change-threshold workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request, such as a threshold or confirmation threshold out of range", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress for this party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/change-threshold")]
pub async fn start_change_threshold(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ChangeThresholdRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    start_change_threshold_run(&data, &body)
        .await
        .unwrap_or_else(ApiError::into_response)
}

/// Check a change-threshold request against the cached party and the peers,
/// then register the run and start it.
async fn start_change_threshold_run(
    data: &web::Data<AppState>,
    body: &ChangeThresholdRequest,
) -> std::result::Result<HttpResponse, ApiError> {
    tracing::info!(
        "Change-threshold request received: party={}, threshold={}->{}, confirmation={:?}",
        body.decentralized_party_id,
        body.previous_threshold,
        body.new_threshold,
        body.confirmation_threshold
    );

    let decentralized_party_id = body.decentralized_party_id.clone();

    // Scope to the party's cached membership (same rationale as start_kick /
    // start_add_party — the party can be a strict subset of the mesh).
    let party_member_ids: HashSet<CantonId> = data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to load dec party participants for change-threshold: {e}");
            ApiError::Internal("Failed to load decentralized party members".to_string())
        })?
        .iter()
        .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
        .collect();

    check_change_threshold(body, data.config.participant_id(), &party_member_ids)?;

    // Invitees: every member except self — they all sign.
    let invitees: Vec<CantonId> = data
        .db
        .get_all_peers()
        .await
        .map_err(|e| {
            tracing::error!("Failed to load peers for change-threshold: {e}");
            ApiError::Internal("Failed to load peers".to_string())
        })?
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| party_member_ids.contains(p) && p != data.config.participant_id())
        .collect();

    let timestamp = now_secs();
    let instance_name = format!(
        "{}-change-threshold-{timestamp}",
        decentralized_party_id.prefix
    );

    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::ChangeThreshold,
        WorkflowRole::Coordinator,
    );
    let change_state = &instance.http;

    let change_config = workflow::ChangeThresholdConfig::new(
        decentralized_party_id.clone(),
        body.new_threshold,
        body.confirmation_threshold,
        body.previous_threshold,
        instance_name.clone(),
    );

    // Refuse a second workflow targeting the SAME dec party (topology mutations
    // must not interleave — see start_kick).
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

    register_and_persist(
        data,
        &instance,
        ChangeThresholdStep::WaitingForPeers,
        &change_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await?;

    let invitees_for_invites = invitees.clone();
    *change_state.invited_peers.write().await = invitees;

    let invites = {
        let config = data.config.clone();
        let db = data.db.clone();
        let change_config = change_config.clone();
        Box::pin(async move {
            send_change_threshold_invites(&config, &db, &change_config, &invitees_for_invites).await
        })
    };
    launch_coordinator(
        data,
        &instance,
        CoordinatorRequest::ChangeThreshold(change_config),
        None,
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
        message: "Change-threshold workflow started".to_string(),
        instance_name,
    }))
}

/// Check a threshold change against the party's cached members. It reads
/// only its inputs.
fn check_change_threshold(
    request: &ChangeThresholdRequest,
    self_id: &CantonId,
    party_member_ids: &HashSet<CantonId>,
) -> std::result::Result<(), ApiError> {
    let decentralized_party_id = &request.decentralized_party_id;
    if party_member_ids.is_empty() {
        return Err(ApiError::Conflict(format!(
            "No cached membership for {decentralized_party_id}. Try refreshing \
                 /decentralized-parties first."
        )));
    }
    if !party_member_ids.contains(self_id) {
        return Err(ApiError::BadRequest(format!(
            "This node is not a member of {decentralized_party_id}; only an existing \
                 member can coordinate a threshold change"
        )));
    }
    // Need at least one other member to reach a signing quorum — a solo-owner
    // party has a fixed threshold of 1 and nothing to change.
    if party_member_ids.len() < 2 {
        return Err(ApiError::BadRequest(format!(
            "Cannot change threshold: {decentralized_party_id} has only {n} member(s)",
            n = party_member_ids.len()
        )));
    }

    // Bound the new threshold by the member count. The deeper validation
    // against the live DNS owner set (and the "already current" check) happens
    // in ExportState.
    let member_count = party_member_ids.len() as i32;
    if request.new_threshold < 1 || request.new_threshold > member_count {
        return Err(ApiError::BadRequest(format!(
            "new_threshold must be between 1 and {member_count} (party member count); got {got}",
            got = request.new_threshold,
        )));
    }
    // Every member hosts the party, so the member count bounds the
    // confirmation threshold too.
    if let Some(confirmation) = request.confirmation_threshold
        && let Err(e) = check_confirmation_threshold(confirmation, party_member_ids.len())
    {
        return Err(ApiError::BadRequest(e.to_string()));
    }

    Ok(())
}

/// Get the current status of the change-threshold workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Change-threshold workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/change-threshold/status")]
pub async fn get_change_threshold_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::ChangeThreshold).await)
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/change-threshold/cancel")]
pub async fn cancel_change_threshold(
    http_req: HttpRequest,
    data: web::Data<AppState>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Change-threshold", WorkflowKind::ChangeThreshold).await
}

/// The change-threshold invite. Its card shows the full member set: the
/// invitees and this node, `self_id`.
pub(super) fn change_threshold_invite(
    self_id: &CantonId,
    change_config: &workflow::ChangeThresholdConfig,
    invitees: &[CantonId],
) -> Result<Message> {
    let mut participants = invitees.to_vec();
    participants.push(self_id.clone());
    let payload = ChangeThresholdInvitePayload {
        dec_party_id: change_config.decentralized_party_id.clone(),
        new_threshold: change_config.new_threshold,
        previous_threshold: change_config.previous_threshold,
        confirmation_threshold: change_config.confirmation_threshold,
        participants,
        workflow_instance: Some(change_config.instance_name.clone()),
    };
    let payload_bytes =
        serde_json::to_vec(&payload).context("encode ChangeThresholdInvitePayload")?;
    Ok(Message::new(
        MessageType::InviteChangeThreshold,
        payload_bytes,
    ))
}

/// Send change-threshold invites over Noise to every existing member.
async fn send_change_threshold_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    change_config: &workflow::ChangeThresholdConfig,
    invitees: &[CantonId],
) -> Result {
    send_invites(
        config,
        db,
        Recipients::Members(invitees),
        InviteLabels {
            kind: "change-threshold",
            skip: "change-threshold ",
            failed: "change-threshold ",
        },
        invite_log!(),
        || change_threshold_invite(config.participant_id(), change_config, invitees),
    )
    .await
}

// ============================================================================
// External-Party Workflow
// ============================================================================

/// Validate the requested confirmation threshold for a decentrally-hosted
/// external party. The hosting set is the local participant plus the hosting
/// peers, and the threshold is capped at `N-1`: Canton requires the threshold to
/// be at most the number of confirming hosts remaining after a change, so a
/// threshold of `N` would block any later unhost. Reject 0 and anything above
/// `N-1` up-front instead of failing deep in a Canton proto error. An unset
/// value passes here and is defaulted to `N-1` by
/// [`prepare_topology`](crate::workflow::external_party::steps::prepare_topology),
/// so it also stays within the cap.
pub(crate) fn validate_confirmation_threshold(
    threshold: Option<u32>,
    num_hosts: usize,
) -> Result<(), String> {
    let Some(t) = threshold else { return Ok(()) };
    let max = num_hosts.saturating_sub(1) as u32;
    if t < 1 || t > max {
        return Err(format!(
            "confirmation_threshold must be between 1 and {max} (one less than the {num_hosts} \
             hosting participants, so a host can still exit); got {t}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_party_threshold_bounded_by_host_count() {
        // 3 hosts (local + 2 peers): capped at N-1 = 2 so a host can still exit.
        assert!(validate_confirmation_threshold(None, 3).is_ok());
        assert!(validate_confirmation_threshold(Some(1), 3).is_ok());
        assert!(validate_confirmation_threshold(Some(2), 3).is_ok());
        // 0 can never reach quorum; N and above would block any unhost.
        assert!(validate_confirmation_threshold(Some(0), 3).is_err());
        assert!(validate_confirmation_threshold(Some(3), 3).is_err());
        assert!(validate_confirmation_threshold(Some(4), 3).is_err());
    }
}

#[cfg(test)]
mod start_change_threshold_tests {
    //! Pins every answer `POST /change-threshold` gives before it starts a
    //! run: the status code and the exact JSON error body.

    use actix_web::http::StatusCode;
    use serde_json::json;

    use super::{
        super::{
            preflight::{format_incompatible_peers, preflight_incompatible_peers},
            test_support::{
                Answer, TestNode, break_table, cache_party, closed_port, in_progress_run,
                insert_peers, insert_run, peer, pid, post_as, post_as_admin, test_node,
            },
        },
        *,
    };
    use crate::db::MIGRATOR;

    type Result<T = ()> = anyhow::Result<T>;

    /// This node is `pid(1)` and hosts the party `pid(9)` with `pid(2)` and
    /// `pid(3)`, its peers, without a usable Noise key unless
    /// `reachable_keys`.
    async fn three_host_party(pool: SqlitePool, reachable_keys: bool) -> Result<TestNode> {
        let (p1, p2, p3) = (pid(1)?, pid(2)?, pid(3)?);
        cache_party(
            &pool,
            &pid(9)?,
            2,
            &[(&p1, Some("k1")), (&p2, Some("k2")), (&p3, Some("k3"))],
            &[("k1", Some(&p1)), ("k2", Some(&p2)), ("k3", Some(&p3))],
        )
        .await?;
        let port = closed_port()?;
        insert_peers(
            &pool,
            &[
                peer(&p2, port, reachable_keys),
                peer(&p3, port, reachable_keys),
            ],
        )
        .await?;
        test_node(pool, &p1).await
    }

    fn change(new_threshold: i32, confirmation: Option<i32>) -> Result<serde_json::Value> {
        Ok(json!({
            "decentralized_party_id": pid(9)?.to_string(),
            "new_threshold": new_threshold,
            "confirmation_threshold": confirmation,
        }))
    }

    async fn post(node: &TestNode, request: serde_json::Value) -> Answer {
        post_as_admin(
            &node.data,
            start_change_threshold,
            "/change-threshold",
            request,
        )
        .await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_anonymous_caller_is_unauthorized(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        post_as(
            &node.data,
            start_change_threshold,
            "/change-threshold",
            change(2, None)?,
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_member_read_answers_500(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "dec_party_participant").await?;
        post(&node, change(2, None)?).await.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load decentralized party members",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_without_cached_members_is_a_conflict(pool: SqlitePool) -> Result {
        let node = test_node(pool, &pid(1)?).await?;
        let party = pid(9)?;
        post(&node, change(2, None)?).await.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "No cached membership for {party}. Try refreshing /decentralized-parties first."
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn only_a_member_coordinates(pool: SqlitePool) -> Result {
        let (p2, p3) = (pid(2)?, pid(3)?);
        cache_party(
            &pool,
            &pid(9)?,
            1,
            &[(&p2, Some("k2")), (&p3, Some("k3"))],
            &[("k2", Some(&p2)), ("k3", Some(&p3))],
        )
        .await?;
        let node = test_node(pool, &pid(1)?).await?;
        let party = pid(9)?;
        post(&node, change(1, None)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "This node is not a member of {party}; only an existing member can \
                 coordinate a threshold change"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_lone_member_has_nothing_to_change(pool: SqlitePool) -> Result {
        let p1 = pid(1)?;
        cache_party(
            &pool,
            &pid(9)?,
            1,
            &[(&p1, Some("k1"))],
            &[("k1", Some(&p1))],
        )
        .await?;
        let node = test_node(pool, &p1).await?;
        let party = pid(9)?;
        post(&node, change(1, None)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            &format!("Cannot change threshold: {party} has only 1 member(s)"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_threshold_out_of_range_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        post(&node, change(4, None)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "new_threshold must be between 1 and 3 (party member count); got 4",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_confirmation_threshold_out_of_range_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        post(&node, change(2, Some(4))?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "confirmation_threshold must be between 1 and 3 (the hosting participants); got 4",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_peer_read_answers_500(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "peers").await?;
        post(&node, change(2, None)?)
            .await
            .assert_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load peers");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_with_a_run_in_flight_is_a_conflict(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        let node = three_host_party(pool.clone(), false).await?;
        insert_run(&pool, &in_progress_run("busy-run", &party)).await?;
        post(&node, change(2, None)?).await.assert_error(
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
        let node = three_host_party(pool.clone(), true).await?;
        let incompatible =
            preflight_incompatible_peers(&node.data.config, &pool, &[pid(2)?, pid(3)?]).await;
        assert_eq!(incompatible.len(), 2, "{incompatible:?}");
        post(&node, change(2, None)?).await.assert_error(
            StatusCode::CONFLICT,
            &format_incompatible_peers(&incompatible),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_write_is_a_conflict_and_leaves_no_run(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "workflow_runs").await?;
        let answer = post(&node, change(2, None)?).await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Failed to start change-threshold workflow: ")
                && error.contains("workflow_runs"),
            "{error}"
        );
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
        assert!(node.data.workflows.snapshot().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_valid_change_is_accepted(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        let answer = post(&node, change(2, None)?).await;
        assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let instance = body["instance_name"].as_str().unwrap_or_default();
        assert!(
            instance.starts_with("validator-9-change-threshold-"),
            "{body}"
        );
        assert_eq!(
            answer.body,
            format!(
                r#"{{"status":"inprogress","message":"Change-threshold workflow started","instance_name":"{instance}"}}"#
            )
        );
        Ok(())
    }
}

#[cfg(test)]
mod check_change_threshold_tests {
    //! Calls the pure threshold-change checks with members, and pins each
    //! refusal text.

    use super::{super::test_support::pid, *};

    type Result<T = ()> = anyhow::Result<T>;

    fn request(new_threshold: i32, confirmation: Option<i32>) -> Result<ChangeThresholdRequest> {
        Ok(ChangeThresholdRequest {
            decentralized_party_id: pid(9)?,
            new_threshold,
            confirmation_threshold: confirmation,
            previous_threshold: 0,
        })
    }

    fn ids(ids: &[&CantonId]) -> HashSet<CantonId> {
        ids.iter().map(|id| (*id).clone()).collect()
    }

    /// This node is `pid(1)`.
    #[test]
    fn each_refusal_keeps_its_text() -> Result {
        let (me, p2, p3, party) = (pid(1)?, pid(2)?, pid(3)?, pid(9)?);
        let three = ids(&[&me, &p2, &p3]);
        let confirmation_refusal = check_confirmation_threshold(4, 3)
            .err()
            .ok_or_else(|| anyhow::anyhow!("4 of 3 hosts must be refused"))?
            .to_string();
        let cases = [
            (
                ids(&[]),
                request(1, None)?,
                ApiError::Conflict(format!(
                    "No cached membership for {party}. Try refreshing /decentralized-parties \
                     first."
                )),
            ),
            (
                ids(&[&p2, &p3]),
                request(1, None)?,
                ApiError::BadRequest(format!(
                    "This node is not a member of {party}; only an existing member can \
                     coordinate a threshold change"
                )),
            ),
            (
                ids(&[&me]),
                request(1, None)?,
                ApiError::BadRequest(format!(
                    "Cannot change threshold: {party} has only 1 member(s)"
                )),
            ),
            (
                three.clone(),
                request(0, None)?,
                ApiError::BadRequest(
                    "new_threshold must be between 1 and 3 (party member count); got 0".to_string(),
                ),
            ),
            (
                three.clone(),
                request(2, Some(4))?,
                ApiError::BadRequest(confirmation_refusal),
            ),
        ];
        for (members, request, refusal) in cases {
            assert_eq!(
                check_change_threshold(&request, &me, &members),
                Err(refusal)
            );
        }
        assert_eq!(
            check_change_threshold(&request(3, Some(2))?, &me, &three),
            Ok(())
        );
        Ok(())
    }
}
