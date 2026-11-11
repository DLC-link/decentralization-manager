//! The change-threshold workflow, and the confirmation threshold check it shares with external parties.

use std::{collections::HashSet, time::Duration};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use anyhow::Context;
use sqlx::SqlitePool;

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
            ChangeThresholdInvitePayload, ChangeThresholdRequest, ErrorResponse, MessageResponse,
            WorkflowGuard, WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowResponse,
            WorkflowRole, WorkflowStatusResponse,
        },
    },
    workflow::{self, ChangeThresholdStep, thresholds::check_confirmation_threshold},
};

use super::{
    cancel::cancel_workflow_state,
    onboarding::interpret_invite_reply,
    preflight::{
        find_inprogress_run_for_party, format_incompatible_peers, preflight_incompatible_peers,
    },
    runs::{kind_status, mark_run_completed, mark_run_failed, now_secs, register_and_persist},
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
    let party_member_ids: HashSet<CantonId> = match data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
            .collect(),
        Err(e) => {
            tracing::error!("Failed to load dec party participants for change-threshold: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party members".to_string(),
            });
        }
    };

    if party_member_ids.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "No cached membership for {decentralized_party_id}. Try refreshing \
                 /decentralized-parties first."
            ),
        });
    }
    if !party_member_ids.contains(data.config.participant_id()) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "This node is not a member of {decentralized_party_id}; only an existing \
                 member can coordinate a threshold change"
            ),
        });
    }
    // Need at least one other member to reach a signing quorum — a solo-owner
    // party has a fixed threshold of 1 and nothing to change.
    if party_member_ids.len() < 2 {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "Cannot change threshold: {decentralized_party_id} has only {n} member(s)",
                n = party_member_ids.len()
            ),
        });
    }

    // Bound the new threshold by the member count. The deeper validation
    // against the live DNS owner set (and the "already current" check) happens
    // in ExportState.
    let member_count = party_member_ids.len() as i32;
    if body.new_threshold < 1 || body.new_threshold > member_count {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {member_count} (party member count); got {got}",
                got = body.new_threshold,
            ),
        });
    }
    // Every member hosts the party, so the member count bounds the
    // confirmation threshold too.
    if let Some(confirmation) = body.confirmation_threshold
        && let Err(e) = check_confirmation_threshold(confirmation, party_member_ids.len())
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: e.to_string(),
        });
    }

    // Invitees: every member except self — they all sign.
    let invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| party_member_ids.contains(p) && p != data.config.participant_id())
            .collect(),
        Err(e) => {
            tracing::error!("Failed to load peers for change-threshold: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };

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
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {decentralized_party_id} already has a {kind} workflow in flight \
                 (run {run}); wait for it to finish or cancel it first"
            ),
        });
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &invitees).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    if let Err(error) = register_and_persist(
        &data,
        &instance,
        ChangeThresholdStep::WaitingForPeers,
        &change_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await
    {
        return error.into_response();
    }

    let invitees_for_invites = invitees.clone();
    *change_state.invited_peers.write().await = invitees;

    let config = data.config.clone();
    let db = data.db.clone();
    let change_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // abort_handle/status/error are flipped under simultaneously-held locks —
    // see start_dars for the cancel-race rationale.
    let mut abort_guard = change_state.abort_handle.lock().await;
    let mut status_guard = change_state.status.write().await;
    let mut error_guard = change_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        let invite_result =
            send_change_threshold_invites(&config, &db, &change_config, &invitees_for_invites)
                .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send change-threshold invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = change_state_clone.status.write().await;
                let mut error = change_state_clone.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::ChangeThreshold,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            None, // No dars config
            None, // No add-party config
            Some(change_config),
            None, // No auth registry for change-threshold
            last_seen,
            instance_for_coord,
        )
        .await;

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
                    let mut status = change_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("Change-threshold workflow completed successfully");
            }
            Err(e) => {
                let msg = format!("{e}");
                // Durable row first, in-memory status second — same ordering as the
                // success path above, and for the same reason: a caller that reads
                // Failed from `/…/status` and retries must not be refused by the
                // in-flight guard, which scans the run rows.
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                {
                    let mut status = change_state_clone.status.write().await;
                    let mut error = change_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Change-threshold workflow failed: {e}");
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
        message: "Change-threshold workflow started".to_string(),
        instance_name,
    })
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

/// Send change-threshold invites over Noise to every existing member.
async fn send_change_threshold_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    change_config: &workflow::ChangeThresholdConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    // The card shows the full member set: invitees + this node.
    let mut participants = invitees.to_vec();
    participants.push(current_participant_id.clone());
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
    let invite_message = Message::new(MessageType::InviteChangeThreshold, payload_bytes);

    for peer in &network_config.peers {
        if !invitee_set.contains(&peer.participant_id)
            || peer.participant_id == *current_participant_id
        {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping change-threshold invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }
        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping change-threshold invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending change-threshold invite to {} at {}:{}",
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
                interpret_invite_reply(&peer.participant_id, "change-threshold", &response)?;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send change-threshold invite to {}: {e}",
                    peer.participant_id
                );
            }
        }
    }

    Ok(())
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
