//! The kick workflow: start, status and invites.

use std::{collections::HashSet, time::Duration};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    consts::OWNER_ONLY_KICK,
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ErrorResponse, KickInvitePayload, KickRequest, KickResponse, KickStatus, WorkflowGuard,
            WorkflowInstance, WorkflowKind, WorkflowResponse, WorkflowRole, WorkflowStatusResponse,
        },
    },
    workflow::{self, KickStep},
};

use super::{
    kick_target::{
        cached_confirmation_threshold, cached_namespace_threshold, kick_threshold_bound,
        party_member_count, resolve_kick_target,
    },
    onboarding::interpret_invite_reply,
    preflight::{
        find_inprogress_run_for_party, format_incompatible_peers,
        format_peers_lacking_owner_only_kick, incompatible_versions, lacking_feature,
        probe_invitee_health,
    },
    runs::{kind_status, mark_run_completed, mark_run_failed, now_secs, register_and_persist},
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
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot kick yourself".to_string(),
        });
    }

    let member_rows = match data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("Failed to load dec party participants for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party members".to_string(),
            });
        }
    };
    let owner_rows = match data.db.get_dec_party_owners(&decentralized_party_id).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("Failed to load dec party owners for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party owners".to_string(),
            });
        }
    };

    // The server works out the owner key from the cache rather than taking
    // it from the client, so an empty prefill becomes a clear error.
    let target = match resolve_kick_target(
        body.participant_id.as_ref(),
        body.owner_key.as_deref(),
        &member_rows,
        &owner_rows,
    ) {
        Ok(target) => target,
        Err(e) => return e.into_response(),
    };
    if target.participant.as_ref() == Some(&self_id) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot kick yourself".to_string(),
        });
    }
    // The participant that leaves the hosting mapping, if the member hosts.
    let kicked_host = target.participant.clone().filter(|_| target.hosts);

    // Load peers once: we use the count to bound `new_threshold` per the
    // audit finding, then filter into `invitees`. A DB error here is fatal
    // — we can't safely proceed without knowing how many signers remain.
    let peers = match data.db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to load peers for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };

    // Scope the kick to this dec party's actual members, not every peer this
    // node has configured. A party can be a strict subset of the configured
    // mesh (see start_onboarding), and basing the signer set / threshold bound
    // / invites on the full mesh would invite outsiders and stall
    // WaitingForPeers on peers that were never part of the party. Fall back to
    // the full peer set only if no cached row yields a usable id — membership
    // not cached yet, or every `participant_uid` in a legacy/invalid format —
    // since scoping to an empty member set would reject the kick with a
    // misleading "need at least 2 party members" error.
    let party_member_ids: HashSet<CantonId> = {
        let parsed: HashSet<CantonId> = member_rows
            .iter()
            .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
            .collect();
        if parsed.is_empty() {
            tracing::warn!(
                "No usable cached participants for {decentralized_party_id} \
                 ({count} rows); falling back to all configured peers for \
                 kick scoping",
                count = member_rows.len()
            );
            peers.iter().map(|p| p.participant_id.clone()).collect()
        } else {
            parsed
        }
    };
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
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "Cannot kick: need at least one other hosting member besides this node \
                 to co-sign, have {co_signers}"
            ),
        });
    }
    if let Some(host) = &kicked_host
        && !peers.iter().any(|p| p.participant_id == *host)
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("Cannot kick {host}: not a member of this decentralized party"),
        });
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
    let member_count = party_member_count(&party_member_ids, &self_id);
    let post_kick_member_count = member_count - usize::from(target.hosts);
    let max_threshold = kick_threshold_bound(target.hosts, member_count, owner_rows.len());
    if body.new_threshold < 1 || body.new_threshold > max_threshold {
        let reason = if target.hosts {
            format!("party member count {member_count}, minus the participant being kicked")
        } else {
            format!(
                "the party keeps its {member_count} hosts and {owners} owners",
                owners = owner_rows.len().saturating_sub(1)
            )
        };
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {max_threshold} ({reason}); got {got}",
                got = body.new_threshold,
            ),
        });
    }

    // Only the hosts sign. An owner that hosts nothing cannot be asked, so
    // the namespace threshold in force must be reachable without it.
    if !target.hosts {
        match cached_namespace_threshold(&data.db, &decentralized_party_id).await {
            Ok(Some(threshold)) if threshold > member_count => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!(
                        "Cannot kick owner key {key}: the party's namespace threshold is \
                         {threshold}, but only its {member_count} hosting members can sign. \
                         The owner being removed would have to sign too, and a kick cannot \
                         ask it",
                        key = target.owner_key
                    ),
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!("Failed to read the cached threshold for kick: {e}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "Failed to read the party's cached thresholds".to_string(),
                });
            }
        }
    }

    let confirmation_threshold = match cached_confirmation_threshold(
        &data.db,
        &decentralized_party_id,
        post_kick_member_count,
    )
    .await
    {
        Ok(threshold) => threshold,
        Err(e) => {
            tracing::error!("Failed to read the cached thresholds for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the party's cached thresholds".to_string(),
            });
        }
    };

    // Compute peers we're going to invite — every peer except self + the kicked participant.
    // Done before the InProgress flip so a concurrent /kick/cancel cannot observe
    // InProgress while we're still preparing.
    let invitees: Vec<CantonId> = peers
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| *p != self_id && Some(p) != kicked_host.as_ref())
        .collect();

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
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {decentralized_party_id} already has a {kind} workflow in flight \
                 (run {run}); wait for it to finish or cancel it first"
            ),
        });
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let health = probe_invitee_health(&data.config, &data.db, &invitees).await;
    let incompatible = incompatible_versions(&health);
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }
    // An older peer refuses an owner-only kick with an error that does not
    // say why, so the run would fail after the invites went out.
    if !target.hosts {
        let lacking = lacking_feature(&health, OWNER_ONLY_KICK);
        if !lacking.is_empty() {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format_peers_lacking_owner_only_kick(&lacking),
            });
        }
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        KickStep::WaitingForPeers,
        &kick_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await
    {
        return resp;
    }

    let invitees_for_invites = invitees.clone();
    *kick_state.invited_peers.write().await = invitees;

    // Spawn the kick workflow in the background
    let config = data.config.clone();
    let db = data.db.clone();
    let kick_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent /kick/cancel
    // can never observe "status=InProgress + abort_handle=None" and bail.
    let mut abort_guard = kick_state.abort_handle.lock().await;
    let mut status_guard = kick_state.status.write().await;
    let mut error_guard = kick_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        // Removes this run from the registry on return (success/failure/abort).
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send kick invites to the surviving party members before starting workflow
        let invite_result =
            send_kick_invites(&config, &db, &kick_config, &invitees_for_invites).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send kick invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = kick_state_clone.status.write().await;
                let mut error = kick_state_clone.error.write().await;
                *status = KickStatus::Failed;
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
            workflow::WorkflowType::Kick,
            None, // No onboarding config
            Some(kick_config),
            None, // No contracts config
            None, // No dars config
            None, // No add-party config
            None, // No change-threshold config
            None, // No auth registry for kick
            last_seen,
            instance_for_coord,
        )
        .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /kick/status acquires a read lock to serve
        // every poll; if a writer holds the lock during the DB write, every
        // concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                {
                    let mut status = kick_state_clone.status.write().await;
                    *status = KickStatus::Completed;
                }
                tracing::info!("Kick workflow completed successfully");
                mark_run_completed(&db, kind, &instance_for_task).await;
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = kick_state_clone.status.write().await;
                    let mut error = kick_state_clone.error.write().await;
                    *status = KickStatus::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Kick workflow failed: {e}");
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = KickStatus::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(KickResponse {
        status: KickStatus::InProgress,
        message: "Kick workflow started".to_string(),
        instance_name,
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

/// Send kick invites to all peers using Noise protocol (excluding the peer being kicked)
async fn send_kick_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    kick_config: &workflow::KickConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let kicked_participant = kick_config.participant_id.as_ref();
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    let payload = KickInvitePayload {
        dec_party_id: kick_config.decentralized_party_id.clone(),
        kicked_participant: kicked_participant.cloned(),
        kicked_owner_key: Some(kick_config.namespace_fingerprint.clone()),
        new_threshold: kick_config.new_threshold,
        previous_threshold: kick_config.previous_threshold,
        confirmation_threshold: kick_config.confirmation_threshold,
        participants: invitees.to_vec(),
        workflow_instance: Some(kick_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode KickInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteKick, payload_bytes);

    tracing::info!(
        "Kick invites: self={current_participant_id}, kicked={kicked_participant:?}, \
         owner key={}",
        kick_config.namespace_fingerprint
    );

    for peer in &network_config.peers {
        tracing::debug!(
            "Checking peer {}: self_match={}, kicked_match={}",
            peer.participant_id,
            peer.participant_id == *current_participant_id,
            Some(&peer.participant_id) == kicked_participant
        );

        // Only invite this run's surviving party members (see start_kick). The
        // self/kicked skips below are now subsumed by this but kept for clarity.
        if !invitee_set.contains(&peer.participant_id) {
            continue;
        }

        // Skip self
        if peer.participant_id == *current_participant_id {
            tracing::debug!("Skipping {} - this is self", peer.participant_id);
            continue;
        }

        // Skip the peer being kicked (they won't participate in the kick workflow)
        if Some(&peer.participant_id) == kicked_participant {
            tracing::info!(
                "Skipping invite to {} - they are being kicked",
                peer.participant_id
            );
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
            "Sending kick invite to {} at {}:{}",
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
                interpret_invite_reply(&peer.participant_id, "kick", &response)?;
            }
            Err(e) => {
                tracing::error!("Failed to send kick invite to {}: {e}", peer.participant_id);
            }
        }
    }

    Ok(())
}
