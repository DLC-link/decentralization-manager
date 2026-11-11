//! The kick workflow: start, status and invites.

use std::collections::HashSet;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use anyhow::Context;
use sqlx::SqlitePool;

use super::{
    invites::{InviteLabels, Recipients, invite_log, send_invites},
    kick_target::{
        KickTarget, cached_confirmation_threshold, cached_namespace_threshold,
        kick_threshold_bound, party_member_count, resolve_kick_target,
    },
    launch::{CoordinatorRequest, RunEnd, StatusOrder, launch_coordinator, run_log},
    preflight::{
        find_inprogress_run_for_party, format_incompatible_peers,
        format_peers_lacking_owner_only_kick, incompatible_versions, lacking_feature,
        probe_invitee_health,
    },
    runs::{kind_status, now_secs, register_and_persist},
};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, Peer},
    consts::OWNER_ONLY_KICK,
    db::{
        rows::{DecPartyOwnerRow, DecPartyParticipantRow},
        schema::SchemaRead,
    },
    error::Result,
    noise::{Message, MessageType},
    server::{
        AppState,
        error::ApiError,
        middleware::require_admin,
        types::{
            ErrorResponse, KickInvitePayload, KickRequest, KickResponse, KickStatus,
            WorkflowInstance, WorkflowKind, WorkflowResponse, WorkflowRole, WorkflowStatusResponse,
        },
    },
    workflow::{self, KickStep},
};

/// Start a kick workflow to remove a participant from a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = KickRequest,
    responses(
        (status = 202, description = "Kick workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress, or owner key not yet resolved for the target participant", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/kick")]
pub async fn start_kick(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<KickRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    start_kick_run(&data, &body)
        .await
        .unwrap_or_else(ApiError::into_response)
}

/// Check a kick request against the cached party and the peers, then
/// register the run and start it.
async fn start_kick_run(
    data: &web::Data<AppState>,
    body: &KickRequest,
) -> std::result::Result<HttpResponse, ApiError> {
    tracing::info!(
        "Kick request received: party={}, participant_to_kick={:?}, owner_key={:?}, \
         threshold={}",
        body.decentralized_party_id,
        body.participant_id,
        body.owner_key,
        body.new_threshold
    );

    // KickRequest's CantonId fields validate during deserialization, so by
    // the time we get here both ids are well-formed.
    let decentralized_party_id = body.decentralized_party_id.clone();
    let self_id = data.config.participant_id().clone();

    if body.participant_id.as_ref() == Some(&self_id) {
        return Err(ApiError::BadRequest("Cannot kick yourself".to_string()));
    }

    let member_rows = data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to load dec party participants for kick: {e}");
            ApiError::Internal("Failed to load decentralized party members".to_string())
        })?;
    let owner_rows = data
        .db
        .get_dec_party_owners(&decentralized_party_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to load dec party owners for kick: {e}");
            ApiError::Internal("Failed to load decentralized party owners".to_string())
        })?;

    let target = kick_target(body, &self_id, &member_rows, &owner_rows)?;

    // Load peers once: we use the count to bound `new_threshold` per the
    // audit finding, then filter into `invitees`. A DB error here is fatal
    // — we can't safely proceed without knowing how many signers remain.
    let peers = data.db.get_all_peers().await.map_err(|e| {
        tracing::error!("Failed to load peers for kick: {e}");
        ApiError::Internal("Failed to load peers".to_string())
    })?;

    let scope = kick_scope(&member_rows, &peers);
    if scope.fell_back {
        tracing::warn!(
            "No usable cached participants for {decentralized_party_id} \
             ({count} rows); falling back to all configured peers for \
             kick scoping",
            count = member_rows.len()
        );
    }
    let KickPlan {
        target,
        member_count,
        post_kick_member_count,
        invitees,
    } = plan_kick(
        body,
        &self_id,
        target,
        &scope.party_member_ids,
        owner_rows.len(),
        peers,
    )?;

    // Only the hosts sign. An owner that hosts nothing cannot be asked, so
    // the namespace threshold in force must be reachable without it.
    if !target.hosts {
        match cached_namespace_threshold(&data.db, &decentralized_party_id).await {
            Ok(Some(threshold)) if threshold > member_count => {
                return Err(ApiError::BadRequest(format!(
                    "Cannot kick owner key {key}: the party's namespace threshold is \
                         {threshold}, but only its {member_count} hosting members can sign. \
                         The owner being removed would have to sign too, and a kick cannot \
                         ask it",
                    key = target.owner_key
                )));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!("Failed to read the cached threshold for kick: {e}");
                return Err(ApiError::Internal(
                    "Failed to read the party's cached thresholds".to_string(),
                ));
            }
        }
    }

    let confirmation_threshold =
        cached_confirmation_threshold(&data.db, &decentralized_party_id, post_kick_member_count)
            .await
            .map_err(|e| {
                tracing::error!("Failed to read the cached thresholds for kick: {e}");
                ApiError::Internal("Failed to read the party's cached thresholds".to_string())
            })?;

    // Compute instance name + config up-front so the persisted workflow_runs row
    // can carry the same identifier the coordinator task will use.
    let timestamp = now_secs();
    let instance_name = format!("{}-kick-{timestamp}", decentralized_party_id.prefix);

    // Register this run in the shared registry. A duplicate `instance_name`
    // (extremely unlikely given the timestamp) is the only rejection — multiple
    // distinct kick runs may now proceed concurrently.
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::Kick,
        WorkflowRole::Coordinator,
    );
    let kick_state = &instance.http;

    let kick_config = workflow::KickConfig::new(
        decentralized_party_id.clone(),
        target.participant,
        target.owner_key,
        body.new_threshold,
        confirmation_threshold,
        body.previous_threshold,
        instance_name.clone(),
    );

    // Refuse a second workflow targeting the SAME decentralized party — the
    // party's topology mutations must not interleave (two kicks, or a kick
    // racing a contracts deployment, conflict at the Canton layer with
    // confusing step failures).
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
    let health = probe_invitee_health(&data.config, &data.db, &invitees).await;
    let incompatible = incompatible_versions(&health);
    if !incompatible.is_empty() {
        return Err(ApiError::Conflict(format_incompatible_peers(&incompatible)));
    }
    // An older peer refuses an owner-only kick with an error that does not
    // say why, so the run would fail after the invites went out.
    if !target.hosts {
        let lacking = lacking_feature(&health, OWNER_ONLY_KICK);
        if !lacking.is_empty() {
            return Err(ApiError::Conflict(format_peers_lacking_owner_only_kick(
                &lacking,
            )));
        }
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    register_and_persist(
        data,
        &instance,
        KickStep::WaitingForPeers,
        &kick_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await?;

    let invitees_for_invites = invitees.clone();
    *kick_state.invited_peers.write().await = invitees;

    // Send kick invites to the surviving party members before starting workflow
    let invites = {
        let config = data.config.clone();
        let db = data.db.clone();
        let kick_config = kick_config.clone();
        Box::pin(async move {
            send_kick_invites(&config, &db, &kick_config, &invitees_for_invites).await
        })
    };
    launch_coordinator(
        data,
        &instance,
        CoordinatorRequest::Kick(kick_config),
        None,
        invites,
        RunEnd {
            order: StatusOrder::MemoryFirst,
            log: run_log!(),
            after_success: None,
        },
    )
    .await;

    Ok(HttpResponse::Accepted().json(KickResponse {
        status: KickStatus::InProgress,
        message: "Kick workflow started".to_string(),
        instance_name,
    }))
}

/// The member a kick request names, from the cached membership.
///
/// The server works out the owner key from the cache rather than taking
/// it from the client, so an empty prefill becomes a clear error.
fn kick_target(
    request: &KickRequest,
    self_id: &CantonId,
    member_rows: &[DecPartyParticipantRow],
    owner_rows: &[DecPartyOwnerRow],
) -> std::result::Result<KickTarget, ApiError> {
    let target = resolve_kick_target(
        request.participant_id.as_ref(),
        request.owner_key.as_deref(),
        member_rows,
        owner_rows,
    )?;
    if target.participant.as_ref() == Some(self_id) {
        return Err(ApiError::BadRequest("Cannot kick yourself".to_string()));
    }
    Ok(target)
}

/// The members a kick is scoped to.
struct KickScope {
    party_member_ids: HashSet<CantonId>,
    /// No cached row gave a usable id, so the scope is every configured
    /// peer.
    fell_back: bool,
}

/// Scope the kick to this dec party's actual members, not every peer this
/// node has configured. A party can be a strict subset of the configured
/// mesh (see start_onboarding), and basing the signer set / threshold bound
/// / invites on the full mesh would invite outsiders and stall
/// WaitingForPeers on peers that were never part of the party. Fall back to
/// the full peer set only if no cached row yields a usable id — membership
/// not cached yet, or every `participant_uid` in a legacy/invalid format —
/// since scoping to an empty member set would reject the kick with a
/// misleading "need at least 2 party members" error.
fn kick_scope(member_rows: &[DecPartyParticipantRow], peers: &[Peer]) -> KickScope {
    let parsed: HashSet<CantonId> = member_rows
        .iter()
        .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
        .collect();
    if parsed.is_empty() {
        KickScope {
            party_member_ids: peers.iter().map(|p| p.participant_id.clone()).collect(),
            fell_back: true,
        }
    } else {
        KickScope {
            party_member_ids: parsed,
            fell_back: false,
        }
    }
}

/// A kick request checked against the party's members and the peers.
struct KickPlan {
    target: KickTarget,
    /// The party's members, counting this node.
    member_count: usize,
    /// The members left once a kicked host leaves.
    post_kick_member_count: usize,
    /// The peers to invite.
    invitees: Vec<CantonId>,
}

/// Check a kick of `target` against the party's members, its owner count
/// and this node's peers, and work out whom to invite. It reads only its
/// inputs.
fn plan_kick(
    request: &KickRequest,
    self_id: &CantonId,
    target: KickTarget,
    party_member_ids: &HashSet<CantonId>,
    owner_count: usize,
    peers: Vec<Peer>,
) -> std::result::Result<KickPlan, ApiError> {
    // The participant that leaves the hosting mapping, if the member hosts.
    let kicked_host = target.participant.clone().filter(|_| target.hosts);
    let peers: Vec<_> = peers
        .into_iter()
        .filter(|p| party_member_ids.contains(&p.participant_id))
        .collect();

    // Preconditions for a kick: there must be at least one peer left
    // after the kick (so the surviving signer set is non-empty), and a host
    // being kicked must be a known peer. Without these checks
    // the `post_kick_member_count` below could go negative and produce
    // a "between 1 and -1" error that obscures the real problem.
    let co_signers = peers
        .iter()
        .filter(|p| Some(&p.participant_id) != kicked_host.as_ref())
        .count();
    if co_signers < 1 {
        return Err(ApiError::BadRequest(format!(
            "Cannot kick: need at least one other hosting member besides this node \
                 to co-sign, have {co_signers}"
        )));
    }
    if let Some(host) = &kicked_host
        && !peers.iter().any(|p| p.participant_id == *host)
    {
        return Err(ApiError::BadRequest(format!(
            "Cannot kick {host}: not a member of this decentralized party"
        )));
    }

    // Validate `new_threshold` before persisting anything. Negative or zero
    // values corrupt topology submission: DNS `authorize()` accepts a bare
    // i32 while the subsequent P2P proposal converts via `try_into()` to
    // u32 and fails partway, leaving the DNS write committed with no
    // rollback. The upper bound is the post-kick member count — there
    // must be at least as many remaining signers as the threshold needs.
    //
    // Counted from the party's OWN member set, not from `peers`. `peers` is
    // scoped to invitees and so excludes this node, which is itself a member —
    // it signs the proposals. Bounding by it undercounts by one, and a party
    // whose threshold already equals its member count then has no acceptable
    // value at all: the peers reject anything below the current threshold, and
    // this check rejects anything at or above it. The add-party and
    // change-threshold bounds below already count the party set.
    //
    // The party's signing threshold follows the namespace one and counts the
    // hosts' keys, so the hosts left after the kick bound it as well as the
    // owners left. An owner that hosts nothing leaves every host in place.
    let member_count = party_member_count(party_member_ids, self_id);
    let post_kick_member_count = member_count - usize::from(target.hosts);
    let max_threshold = kick_threshold_bound(target.hosts, member_count, owner_count);
    if request.new_threshold < 1 || request.new_threshold > max_threshold {
        let reason = if target.hosts {
            format!("party member count {member_count}, minus the participant being kicked")
        } else {
            format!(
                "the party keeps its {member_count} hosts and {owners} owners",
                owners = owner_count.saturating_sub(1)
            )
        };
        return Err(ApiError::BadRequest(format!(
            "new_threshold must be between 1 and {max_threshold} ({reason}); got {got}",
            got = request.new_threshold,
        )));
    }

    // Compute peers we're going to invite — every peer except self + the kicked participant.
    // Done before the InProgress flip so a concurrent /kick/cancel cannot observe
    // InProgress while we're still preparing.
    let invitees: Vec<CantonId> = peers
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| p != self_id && Some(p) != kicked_host.as_ref())
        .collect();

    Ok(KickPlan {
        target,
        member_count,
        post_kick_member_count,
        invitees,
    })
}

/// Get the current status of the kick workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Kick workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/kick/status")]
pub async fn get_kick_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Kick).await)
}

/// The kick invite for the surviving party members.
pub(super) fn kick_invite(
    kick_config: &workflow::KickConfig,
    invitees: &[CantonId],
) -> Result<Message> {
    let payload = KickInvitePayload {
        dec_party_id: kick_config.decentralized_party_id.clone(),
        kicked_participant: kick_config.participant_id.clone(),
        kicked_owner_key: Some(kick_config.namespace_fingerprint.clone()),
        new_threshold: kick_config.new_threshold,
        previous_threshold: kick_config.previous_threshold,
        confirmation_threshold: kick_config.confirmation_threshold,
        participants: invitees.to_vec(),
        workflow_instance: Some(kick_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode KickInvitePayload")?;
    Ok(Message::new(MessageType::InviteKick, payload_bytes))
}

/// Send kick invites to all peers using Noise protocol (excluding the peer being kicked)
async fn send_kick_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    kick_config: &workflow::KickConfig,
    invitees: &[CantonId],
) -> Result {
    let kicked_participant = kick_config.participant_id.as_ref();
    send_invites(
        config,
        db,
        Recipients::KickSurvivors {
            invitees,
            kicked: kicked_participant,
        },
        InviteLabels {
            kind: "kick",
            skip: "",
            failed: "kick ",
        },
        invite_log!(),
        || {
            let invite_message = kick_invite(kick_config, invitees)?;
            tracing::info!(
                "Kick invites: self={}, kicked={kicked_participant:?}, owner key={}",
                config.participant_id(),
                kick_config.namespace_fingerprint
            );
            Ok(invite_message)
        },
    )
    .await
}

#[cfg(test)]
mod start_kick_tests {
    //! Pins every answer `POST /kick` gives before it starts a run: the
    //! status code and the exact JSON error body.

    use actix_web::http::StatusCode;
    use serde_json::json;

    use super::{
        super::{
            preflight::{format_incompatible_peers, incompatible_versions, probe_invitee_health},
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
    /// `pid(3)`. Each host holds the owner key named after it, and has
    /// claimed it. Hosts 2 and 3 are this node's peers, without a usable
    /// Noise key unless `reachable_keys`.
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

    /// As [`three_host_party`], plus owner key `k4`, claimed by `pid(4)`,
    /// which hosts nothing. The party's namespace threshold is `threshold`.
    async fn party_with_an_owner_only_member(pool: SqlitePool, threshold: i64) -> Result<TestNode> {
        let (p1, p2, p3, p4) = (pid(1)?, pid(2)?, pid(3)?, pid(4)?);
        cache_party(
            &pool,
            &pid(9)?,
            threshold,
            &[(&p1, Some("k1")), (&p2, Some("k2")), (&p3, Some("k3"))],
            &[
                ("k1", Some(&p1)),
                ("k2", Some(&p2)),
                ("k3", Some(&p3)),
                ("k4", Some(&p4)),
            ],
        )
        .await?;
        let port = closed_port()?;
        insert_peers(&pool, &[peer(&p2, port, false), peer(&p3, port, false)]).await?;
        test_node(pool, &p1).await
    }

    async fn kick(node: &TestNode, request: serde_json::Value) -> Answer {
        post_as_admin(&node.data, start_kick, "/kick", request).await
    }

    fn kick_host(participant: &CantonId, new_threshold: i32) -> Result<serde_json::Value> {
        Ok(json!({
            "decentralized_party_id": pid(9)?.to_string(),
            "participant_id": participant.to_string(),
            "new_threshold": new_threshold,
        }))
    }

    fn kick_owner_key(owner_key: &str, new_threshold: i32) -> Result<serde_json::Value> {
        Ok(json!({
            "decentralized_party_id": pid(9)?.to_string(),
            "owner_key": owner_key,
            "new_threshold": new_threshold,
        }))
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_anonymous_caller_is_unauthorized(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        post_as(
            &node.data,
            start_kick,
            "/kick",
            kick_host(&pid(2)?, 1)?,
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn kicking_this_node_by_participant_is_refused(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        kick(&node, kick_host(&pid(1)?, 1)?)
            .await
            .assert_error(StatusCode::BAD_REQUEST, "Cannot kick yourself");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_member_read_answers_500(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "dec_party_participant").await?;
        kick(&node, kick_host(&pid(2)?, 1)?).await.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load decentralized party members",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_owner_read_answers_500(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "dec_party_owner").await?;
        kick(&node, kick_host(&pid(2)?, 1)?).await.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load decentralized party owners",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unnamed_member_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        let request = json!({
            "decentralized_party_id": pid(9)?.to_string(),
            "new_threshold": 1,
        });
        kick(&node, request).await.assert_error(
            StatusCode::BAD_REQUEST,
            "Name the member to kick with participant_id, owner_key, or both",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_host_without_a_known_key_is_a_conflict(pool: SqlitePool) -> Result {
        let (p1, p2) = (pid(1)?, pid(2)?);
        cache_party(
            &pool,
            &pid(9)?,
            1,
            &[(&p1, Some("k1")), (&p2, None)],
            &[("k1", Some(&p1)), ("k2", None)],
        )
        .await?;
        let node = test_node(pool, &p1).await?;
        kick(&node, kick_host(&p2, 1)?).await.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "Participant {p2} hosts the party, but its current owner key has not been \
                 resolved yet. Try refreshing /decentralized-parties first."
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn kicking_this_node_by_owner_key_is_refused(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        kick(&node, kick_owner_key("k1", 1)?)
            .await
            .assert_error(StatusCode::BAD_REQUEST, "Cannot kick yourself");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_peer_read_answers_500(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "peers").await?;
        kick(&node, kick_host(&pid(2)?, 1)?)
            .await
            .assert_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load peers");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_needs_another_co_signer(pool: SqlitePool) -> Result {
        let (p1, p2, p3) = (pid(1)?, pid(2)?, pid(3)?);
        cache_party(
            &pool,
            &pid(9)?,
            2,
            &[(&p1, Some("k1")), (&p2, Some("k2")), (&p3, Some("k3"))],
            &[("k1", Some(&p1)), ("k2", Some(&p2)), ("k3", Some(&p3))],
        )
        .await?;
        insert_peers(&pool, &[peer(&p2, closed_port()?, false)]).await?;
        let node = test_node(pool, &p1).await?;
        kick(&node, kick_host(&p2, 1)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "Cannot kick: need at least one other hosting member besides this node to \
             co-sign, have 0",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kicked_host_must_be_a_peer(pool: SqlitePool) -> Result {
        let (p1, p2, p3) = (pid(1)?, pid(2)?, pid(3)?);
        cache_party(
            &pool,
            &pid(9)?,
            2,
            &[(&p1, Some("k1")), (&p2, Some("k2")), (&p3, Some("k3"))],
            &[("k1", Some(&p1)), ("k2", Some(&p2)), ("k3", Some(&p3))],
        )
        .await?;
        insert_peers(&pool, &[peer(&p3, closed_port()?, false)]).await?;
        let node = test_node(pool, &p1).await?;
        kick(&node, kick_host(&p2, 1)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            &format!("Cannot kick {p2}: not a member of this decentralized party"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_host_kick_threshold_out_of_range_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        kick(&node, kick_host(&pid(2)?, 3)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "new_threshold must be between 1 and 2 (party member count 3, minus the \
             participant being kicked); got 3",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_kick_threshold_out_of_range_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = party_with_an_owner_only_member(pool, 2).await?;
        kick(&node, kick_owner_key("k4", 4)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "new_threshold must be between 1 and 3 (the party keeps its 3 hosts and 3 \
             owners); got 4",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_kick_the_hosts_cannot_sign_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = party_with_an_owner_only_member(pool, 4).await?;
        kick(&node, kick_owner_key("k4", 2)?).await.assert_error(
            StatusCode::BAD_REQUEST,
            "Cannot kick owner key k4: the party's namespace threshold is 4, but only its 3 \
             hosting members can sign. The owner being removed would have to sign too, and \
             a kick cannot ask it",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_kick_with_a_failed_threshold_read_answers_500(pool: SqlitePool) -> Result {
        let node = party_with_an_owner_only_member(pool.clone(), 2).await?;
        break_table(&pool, "dec_party").await?;
        kick(&node, kick_owner_key("k4", 2)?).await.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to read the party's cached thresholds",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_host_kick_with_a_failed_threshold_read_answers_500(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "dec_party").await?;
        kick(&node, kick_host(&pid(2)?, 1)?).await.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to read the party's cached thresholds",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_with_a_run_in_flight_is_a_conflict(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        let node = three_host_party(pool.clone(), false).await?;
        insert_run(&pool, &in_progress_run("busy-run", &party)).await?;
        kick(&node, kick_host(&pid(2)?, 1)?).await.assert_error(
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
        let probe = probe_invitee_health(&node.data.config, &pool, &[pid(3)?]).await;
        let expected = format_incompatible_peers(&incompatible_versions(&probe));
        assert!(expected.contains(&pid(3)?.to_string()), "{expected}");
        kick(&node, kick_host(&pid(2)?, 1)?)
            .await
            .assert_error(StatusCode::CONFLICT, &expected);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_write_is_a_conflict_and_leaves_no_run(pool: SqlitePool) -> Result {
        let node = three_host_party(pool.clone(), false).await?;
        break_table(&pool, "workflow_runs").await?;
        let answer = kick(&node, kick_host(&pid(2)?, 1)?).await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Failed to start kick workflow: ") && error.contains("workflow_runs"),
            "{error}"
        );
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
        assert!(node.data.workflows.snapshot().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_valid_kick_is_accepted(pool: SqlitePool) -> Result {
        let node = three_host_party(pool, false).await?;
        let answer = kick(&node, kick_host(&pid(2)?, 1)?).await;
        assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let instance = body["instance_name"].as_str().unwrap_or_default();
        assert!(instance.starts_with("validator-9-kick-"), "{body}");
        assert_eq!(
            answer.body,
            format!(
                r#"{{"status":"inprogress","message":"Kick workflow started","instance_name":"{instance}"}}"#
            )
        );
        Ok(())
    }
}

#[cfg(test)]
mod plan_kick_tests {
    //! Calls the pure kick checks with members, owners and peers, and pins
    //! each refusal text.

    use super::{
        super::test_support::{peer, pid},
        *,
    };

    type Result<T = ()> = anyhow::Result<T>;

    fn request(new_threshold: i32) -> Result<KickRequest> {
        Ok(KickRequest {
            decentralized_party_id: pid(9)?,
            participant_id: None,
            owner_key: None,
            new_threshold,
            previous_threshold: 0,
        })
    }

    fn host(participant: &CantonId) -> KickTarget {
        KickTarget {
            owner_key: "k2".to_string(),
            participant: Some(participant.clone()),
            hosts: true,
        }
    }

    fn owner_only() -> KickTarget {
        KickTarget {
            owner_key: "k4".to_string(),
            participant: None,
            hosts: false,
        }
    }

    fn ids(ids: &[&CantonId]) -> HashSet<CantonId> {
        ids.iter().map(|id| (*id).clone()).collect()
    }

    fn peers(ids: &[&CantonId]) -> Vec<Peer> {
        ids.iter().map(|id| peer(id, 1, false)).collect()
    }

    fn member_row(uid: &str) -> DecPartyParticipantRow {
        DecPartyParticipantRow {
            dec_party_id: "dec".to_string(),
            participant_uid: uid.to_string(),
            permission: "confirmation".to_string(),
            owner_key: None,
            signing_key: None,
        }
    }

    /// This node is `pid(1)`; the party is this node, `pid(2)` and `pid(3)`.
    #[test]
    fn each_refusal_keeps_its_text() -> Result {
        let (me, p2, p3, p5) = (pid(1)?, pid(2)?, pid(3)?, pid(5)?);
        let party = ids(&[&me, &p2, &p3]);
        let cases = [
            (
                host(&p2),
                3,
                peers(&[&p2]),
                1,
                "Cannot kick: need at least one other hosting member besides this node to \
                 co-sign, have 0"
                    .to_string(),
            ),
            (
                host(&p5),
                3,
                peers(&[&p2, &p3]),
                1,
                format!("Cannot kick {p5}: not a member of this decentralized party"),
            ),
            (
                host(&p2),
                3,
                peers(&[&p2, &p3]),
                0,
                "new_threshold must be between 1 and 2 (party member count 3, minus the \
                 participant being kicked); got 0"
                    .to_string(),
            ),
            (
                host(&p2),
                3,
                peers(&[&p2, &p3]),
                3,
                "new_threshold must be between 1 and 2 (party member count 3, minus the \
                 participant being kicked); got 3"
                    .to_string(),
            ),
            (
                owner_only(),
                4,
                peers(&[&p2, &p3]),
                4,
                "new_threshold must be between 1 and 3 (the party keeps its 3 hosts and 3 \
                 owners); got 4"
                    .to_string(),
            ),
        ];
        for (target, owners, peers, threshold, error) in cases {
            let refusal = plan_kick(&request(threshold)?, &me, target, &party, owners, peers)
                .err()
                .ok_or_else(|| anyhow::anyhow!("the kick must be refused: {error}"))?;
            assert_eq!(refusal, ApiError::BadRequest(error));
        }
        Ok(())
    }

    /// A plan invites the party's peers but the kicked host, and never a
    /// peer outside the party.
    #[test]
    fn a_plan_invites_the_members_left() -> Result {
        let (me, p2, p3, p7) = (pid(1)?, pid(2)?, pid(3)?, pid(7)?);
        let party = ids(&[&me, &p2, &p3]);

        let plan = plan_kick(
            &request(2)?,
            &me,
            host(&p2),
            &party,
            3,
            peers(&[&p2, &p3, &p7]),
        )?;
        assert_eq!(plan.invitees, vec![p3.clone()]);
        assert_eq!((plan.member_count, plan.post_kick_member_count), (3, 2));
        assert_eq!(plan.target, host(&p2));

        let plan = plan_kick(
            &request(3)?,
            &me,
            owner_only(),
            &party,
            4,
            peers(&[&p2, &p3, &p7]),
        )?;
        assert_eq!(plan.invitees, vec![p2, p3]);
        assert_eq!((plan.member_count, plan.post_kick_member_count), (3, 3));
        Ok(())
    }

    /// A naming of this node through its owner key is refused like a
    /// naming by participant.
    #[test]
    fn a_target_that_is_this_node_is_refused() -> Result {
        let me = pid(1)?;
        let members = [DecPartyParticipantRow {
            owner_key: Some("k1".to_string()),
            ..member_row(&me.to_string())
        }];
        let owners = [DecPartyOwnerRow {
            dec_party_id: "dec".to_string(),
            owner_key: "k1".to_string(),
            participant_uid: Some(me.to_string()),
        }];
        let request = KickRequest {
            owner_key: Some("k1".to_string()),
            ..request(1)?
        };
        assert_eq!(
            kick_target(&request, &me, &members, &owners).err(),
            Some(ApiError::BadRequest("Cannot kick yourself".to_string()))
        );
        Ok(())
    }

    /// The cached rows scope the kick. With no usable row, every configured
    /// peer does, and the handler logs that.
    #[test]
    fn the_scope_falls_back_to_the_peers() -> Result {
        let (p2, p3) = (pid(2)?, pid(3)?);
        let configured = peers(&[&p2, &p3]);

        let scope = kick_scope(&[member_row(&p2.to_string())], &configured);
        assert_eq!(scope.party_member_ids, ids(&[&p2]));
        assert!(!scope.fell_back);

        let scope = kick_scope(&[member_row("not-a-canton-id")], &configured);
        assert_eq!(scope.party_member_ids, ids(&[&p2, &p3]));
        assert!(scope.fell_back);
        Ok(())
    }
}
