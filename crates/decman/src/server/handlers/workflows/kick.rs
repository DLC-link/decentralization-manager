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
    kick_signers::{
        KickSigners, check_kick_survivor_keys, kick_key_adoption_refusal, kick_key_check_failure,
        kick_quorum_refusal, kick_signers,
    },
    kick_target::{
        cached_confirmation_threshold, cached_namespace_threshold, kick_threshold_bound,
        party_member_count, resolve_kick_target,
    },
    onboarding::interpret_invite_reply,
    preflight::{
        claim_party_for_start, format_incompatible_peers, format_peers_lacking_owner_only_kick,
        incompatible_versions, lacking_feature, probe_invitee_health,
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
        (status = 409, description = "Workflow already in progress, owner key not yet resolved for the target participant, or two remaining members claim one signing key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 503, description = "A remaining member's signing key is not on record, or the party's signing keys cannot be read right now", body = ErrorResponse)
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

    // The peer list decides which members the kick can invite. A DB error
    // here is fatal, because without it the signer count is unknown.
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
    // The target was checked against the membership above. Its peer entry
    // does not matter, because the kick never contacts it. Each member that
    // stays has to sign, and the kick reaches a member only through its peer
    // entry.
    let peer_ids: HashSet<CantonId> = peers.into_iter().map(|p| p.participant_id).collect();
    let signers = kick_signers(&party_member_ids, &peer_ids, &self_id, kicked_host.as_ref());
    let member_count = party_member_count(&party_member_ids, &self_id);
    let namespace_threshold =
        match cached_namespace_threshold(&data.db, &decentralized_party_id).await {
            Ok(threshold) => threshold,
            Err(e) => {
                tracing::error!("Failed to read the cached threshold for kick: {e}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "Failed to read the party's cached thresholds".to_string(),
                });
            }
        };

    // Only the hosts sign. An owner that hosts nothing cannot be asked, so
    // the namespace threshold in force must be reachable without it.
    if !target.hosts
        && let Some(threshold) = namespace_threshold
        && threshold > member_count
    {
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

    // Validate `new_threshold` before persisting anything. Negative or zero
    // values corrupt topology submission: DNS `authorize()` accepts a bare
    // i32 while the subsequent P2P proposal converts via `try_into()` to
    // u32 and fails partway, leaving the DNS write committed with no
    // rollback. The upper bound is the post-kick member count — there
    // must be at least as many remaining signers as the threshold needs.
    //
    // Counted from the party's OWN member set, not from the invitees. They
    // exclude this node, which is itself a member — it signs the proposals.
    // Bounding by them undercounts by one, and a party whose threshold
    // already equals its member count then has no acceptable value at all:
    // the peers reject anything below the current threshold, and this check
    // rejects anything at or above it. The add-party and change-threshold
    // bounds below already count the party set.
    //
    // The party's signing threshold follows the namespace one and counts the
    // hosts' keys, so the hosts left after the kick bound it as well as the
    // owners left. An owner that hosts nothing leaves every host in place.
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

    // The DNS changes first. Its signatures satisfy the current threshold,
    // and the following P2P update must satisfy the proposed threshold too.
    if let Some(error) =
        kick_quorum_refusal(namespace_threshold, body.new_threshold as usize, &signers)
    {
        return HttpResponse::BadRequest().json(ErrorResponse { error });
    }
    if !signers.not_peers.is_empty() {
        tracing::info!(
            "Kick of {decentralized_party_id} proceeds without members that are not \
             configured peers: {ids}",
            ids = signers
                .not_peers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
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

    // Fixed before the InProgress flip so a concurrent /kick/cancel cannot
    // observe InProgress while we're still preparing.
    let invitees = signers.invitees.clone();

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
    let _start = match claim_party_for_start(&data, &decentralized_party_id).await {
        Ok(claim) => claim,
        Err(response) => return response,
    };

    let removed = kick_config.participant_id.as_ref().map_or_else(
        || kick_config.namespace_fingerprint.clone(),
        ToString::to_string,
    );
    if let Err(e) = check_kick_survivor_keys(
        &data.db,
        &decentralized_party_id,
        &self_id,
        &signers,
        &removed,
    )
    .await
    {
        return kick_key_check_failure(&decentralized_party_id, &e);
    }
    match kick_key_adoption_refusal(
        &data.config,
        &data.db,
        &decentralized_party_id,
        &signers.not_peers,
        &removed,
        kick_config.participant_id.as_ref(),
    )
    .await
    {
        Ok(Some(error)) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
        Ok(None) => {}
        Err(e) => return kick_key_check_failure(&decentralized_party_id, &e),
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
        let invite_result = send_kick_invites(&config, &db, &kick_config, &signers).await;
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
            None, // No rotate-key config
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

/// Pin the complete surviving host set in the accepted invitation. Members
/// that are not peers remain on the topology even though they cannot sign.
fn kick_invite_payload(
    kick_config: &workflow::KickConfig,
    signers: &KickSigners,
    coordinator: &CantonId,
) -> KickInvitePayload {
    let mut participants: Vec<CantonId> = signers
        .invitees
        .iter()
        .chain(&signers.not_peers)
        .cloned()
        .collect();
    participants.push(coordinator.clone());
    participants.sort();
    participants.dedup();
    KickInvitePayload {
        dec_party_id: kick_config.decentralized_party_id.clone(),
        kicked_participant: kick_config.participant_id.clone(),
        kicked_owner_key: Some(kick_config.namespace_fingerprint.clone()),
        new_threshold: kick_config.new_threshold,
        previous_threshold: kick_config.previous_threshold,
        confirmation_threshold: kick_config.confirmation_threshold,
        participants,
        workflow_instance: Some(kick_config.instance_name.clone()),
    }
}

/// Send kick invites only to the reachable members selected for this run.
async fn send_kick_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    kick_config: &workflow::KickConfig,
    signers: &KickSigners,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let kicked_participant = kick_config.participant_id.as_ref();
    let invitee_set: HashSet<&CantonId> = signers.invitees.iter().collect();
    let payload = kick_invite_payload(kick_config, signers, current_participant_id);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};

    use crate::{
        db::{
            MIGRATOR,
            schema::{Commitable, SchemaWrite},
        },
        server::PendingInvitation,
        workflow::{
            kick::prune_cached_membership, thresholds::PartyThresholds,
            validation::PeerExpectations,
        },
    };

    use super::super::{
        super::invitations::insert_peer_run,
        runs::insert_coordinator_run,
        test_support::{
            admin, cache_kick_party, cache_party, host_row, ids, pid, record_signing_keys,
            state_on, status_and_body,
        },
    };
    use super::*;

    /// [`state_on`], with `pid(1)` as this node.
    async fn kick_state_on(pool: SqlitePool) -> Result<web::Data<AppState>> {
        let state = Arc::try_unwrap(state_on(pool).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(pid(1)?);
        Ok(web::Data::new(AppState { config, ..state }))
    }

    /// A party hosted by this node and `pid(2..=4)` at namespace threshold 3.
    /// Each host holds its own owner key. Only `pid(2)` is a configured peer.
    async fn cache_party_with_one_peer(pool: &SqlitePool) -> Result<CantonId> {
        let party = pid(40)?;
        cache_party(pool, &party, 3, None).await?;
        let keys: Vec<String> = (1..=4).map(|n| format!("k{n}")).collect();
        let hosts = (1..=4)
            .zip(&keys)
            .map(|(tag, key)| Ok(host_row(&pid(tag)?, Some(key))))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(&party, &hosts).await?;
        tx.replace_dec_party_owners(&party, &keys).await?;
        tx.insert_peer(&crate::config::Peer {
            participant_id: pid(2)?,
            name: "peer-2".to_string(),
            address: "127.0.0.1".to_string(),
            port: 1,
            public_key: String::new(),
            party: None,
        })
        .await?;
        Commitable::commit(tx).await?;
        Ok(party)
    }

    /// A kick of `target` at threshold 2.
    fn kick_request(party: &CantonId, target: &CantonId) -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri("/kick")
            .set_json(serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": target,
                "new_threshold": 2,
            }))
    }

    /// The kicked host has no peer entry, which a kick does not need. The
    /// refusal is about the survivor that is not a peer, so it names that
    /// survivor. A participant outside the party is still refused as one.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_refused_for_quorum_names_the_survivor_not_the_target(
        pool: SqlitePool,
    ) -> Result {
        let party = cache_party_with_one_peer(&pool).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(kick_state_on(pool).await?)
                .service(start_kick),
        )
        .await;

        let request = kick_request(&party, &pid(4)?).to_request();
        request.extensions_mut().insert(admin());
        let response = actix_test::call_service(&app, request).await;
        let (status, body) = status_and_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains(&format!(
                "{} is a member of the party but is not a configured peer of this node",
                pid(3)?
            )),
            "{body}"
        );
        assert!(
            !body.contains("not a member of this decentralized party"),
            "{body}"
        );

        let request = kick_request(&party, &pid(9)?).to_request();
        request.extensions_mut().insert(admin());
        let response = actix_test::call_service(&app, request).await;
        let (status, body) = status_and_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("not a member of this decentralized party"),
            "{body}"
        );
        Ok(())
    }

    async fn post_kick(pool: SqlitePool, body: serde_json::Value) -> Result<(StatusCode, String)> {
        let app = actix_test::init_service(
            App::new()
                .app_data(kick_state_on(pool).await?)
                .service(start_kick),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri("/kick")
            .set_json(body)
            .to_request();
        request.extensions_mut().insert(admin());
        Ok(status_and_body(actix_test::call_service(&app, request).await).await)
    }

    /// Issue #617: the operator removed a lost host from the peer list, then
    /// kicked it. The remaining members still reach the threshold, so the
    /// kick must pass every check. It reaches the in-flight guard, the last
    /// step before invitations.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_host_removed_from_the_peer_list_can_still_be_kicked(pool: SqlitePool) -> Result {
        let party = pid(41)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3], &[], &[2]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 2,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    /// The bound includes every surviving host, but a higher threshold
    /// still needs enough configured peers to sign the P2P update.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_raised_threshold_needs_enough_reachable_signers(pool: SqlitePool) -> Result {
        let party = pid(42)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 3]).await?;
        let kick = |new_threshold: i64| -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": new_threshold,
            }))
        };

        let (status, body) = post_kick(pool.clone(), kick(4)?).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("new_threshold must be between 1 and 3"),
            "{body}"
        );

        let (status, body) = post_kick(pool.clone(), kick(3)?).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("the requested new_threshold is 3. Only 2 members can sign"),
            "{body}"
        );
        assert!(body.contains(&pid(4)?.to_string()), "{body}");
        assert!(body.contains("Add it as a peer, then retry."), "{body}");
        assert!(!body.contains(&pid(3)?.to_string()), "{body}");

        let (status, body) = post_kick(pool, kick(2)?).await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_raised_threshold_at_reachable_quorum_passes(pool: SqlitePool) -> Result {
        let party = pid(44)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 4]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 3,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_lowered_kick_threshold_allows_an_immediate_second_kick(pool: SqlitePool) -> Result {
        let party = pid(45)?;
        cache_kick_party(&pool, &party, 3, &[1, 2, 3, 4], &[], &[2, 3, 4]).await?;

        prune_cached_membership(
            &pool,
            &party,
            Some(&pid(4)?),
            "k4",
            crate::db::rows::DecPartyTopologyVersion {
                namespace_serial: 2,
                p2p_serial: 2,
            },
            PartyThresholds::new(2, None),
        )
        .await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 2,
            }),
        )
        .await?;
        // The second kick passes quorum and reaches the no-invitations
        // in-flight guard. A stale cached threshold of 3 refuses it first.
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_quorum_kick_advertises_non_peers_without_waiting_for_them(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(46)?;
        cache_party(&pool, &party, 2, None).await?;
        let signers = kick_signers(
            &ids(&[1, 2, 3, 4])?,
            &ids(&[2, 5])?,
            &pid(1)?,
            Some(&pid(4)?),
        );
        assert_eq!(signers.invitees, vec![pid(2)?]);
        assert_eq!(signers.not_peers, vec![pid(3)?]);
        let config = workflow::KickConfig::new(
            party.clone(),
            Some(pid(4)?),
            "k4".to_string(),
            2,
            None,
            2,
            "quorum-kick".to_string(),
        );
        let payload = kick_invite_payload(&config, &signers, &pid(1)?);
        // Exercise the existing wire schema; no new capability is needed.
        let payload: KickInvitePayload = serde_json::from_slice(&serde_json::to_vec(&payload)?)?;
        assert_eq!(
            payload.participants.iter().cloned().collect::<HashSet<_>>(),
            ids(&[1, 2, 3])?
        );

        insert_coordinator_run(
            &pool,
            &config.instance_name,
            WorkflowKind::Kick,
            KickStep::WaitingForPeers,
            &config,
            &signers.invitees,
            Some(party),
        )
        .await?;
        let run = pool
            .get_workflow_run(&config.instance_name)
            .await?
            .context("coordinator run missing")?;
        assert_eq!(run.expected_peers, vec![pid(2)?]);
        let state = workflow::WorkflowState::new(
            pool.clone(),
            run.instance_name,
            KickStep::WaitingForPeers,
            run.expected_peers,
            Some(1),
        );
        state.peer_connected(pid(2)?).await;
        assert_eq!(state.current_step().await, KickStep::ExportState);

        let mut received = serde_json::to_value(payload)?;
        received["id"] = "kick-invite".into();
        received["invitation_type"] = "Kick".into();
        received["coordinator_pubkey"] = "a1".repeat(32).into();
        received["received_at"] = 1000.into();
        let invitation: PendingInvitation = serde_json::from_value(received)?;
        let peer_instance = insert_peer_run(&pool, &invitation)
            .await
            .context("accepted peer run missing")?;
        let mut node = NodeConfig::default();
        node.node.participant_id = Some(pid(2)?);
        let accepted = PeerExpectations::load(&pool, &peer_instance, &node, &pid(1)?).await?;
        assert_eq!(
            accepted.members.into_iter().collect::<HashSet<_>>(),
            ids(&[1, 2, 3])?
        );
        assert_eq!(accepted.kicked_participant, Some(pid(4)?));
        assert_eq!(accepted.threshold, Some(2));
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unreadable_key_adoption_preflight_creates_no_kick_run(pool: SqlitePool) -> Result {
        let party = pid(47)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2]).await?;
        record_signing_keys(&pool, &party, &[(2, 2), (3, 3)]).await?;
        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 4).await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            error_text(&body)?,
            "Cannot check the party's signing keys right now. Retry later."
        );
        assert_no_kick_started(&pool, &state).await
    }

    /// Posts a kick of `pid(target)` at threshold 2 once the party's run in
    /// flight is gone. Canton is unreachable, so a ledger read fails at once.
    async fn post_kick_without_run_in_flight(
        pool: &SqlitePool,
        party: &CantonId,
        target: u8,
    ) -> Result<(web::Data<AppState>, StatusCode, String)> {
        sqlx::query("DELETE FROM workflow_runs WHERE instance_name = 'contracts-in-flight'")
            .execute(pool)
            .await?;
        let state = Arc::try_unwrap(kick_state_on(pool.clone()).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("fresh state has one owner"))?;
        let mut config = state.config.clone();
        // Invalid URI fails at the Canton boundary without opening a socket.
        config.canton.admin_api_host = "[".to_string();
        let state = web::Data::new(AppState { config, ..state });
        let app =
            actix_test::init_service(App::new().app_data(state.clone()).service(start_kick)).await;
        let request = kick_request(party, &pid(target)?).to_request();
        request.extensions_mut().insert(admin());
        let (status, body) = status_and_body(actix_test::call_service(&app, request).await).await;
        Ok((state, status, body))
    }

    /// The `error` field of an error response body.
    fn error_text(body: &str) -> Result<String> {
        serde_json::from_str::<serde_json::Value>(body)?["error"]
            .as_str()
            .map(ToString::to_string)
            .with_context(|| format!("no error text in {body}"))
    }

    async fn assert_no_kick_started(pool: &SqlitePool, state: &web::Data<AppState>) -> Result {
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::Kick)
                .await?
                .is_empty()
        );
        assert!(state.workflows.snapshot().is_empty());
        assert!(state.pending_invitations.read().await.is_empty());
        Ok(())
    }

    /// Every remaining host's key must be on record, or the proposal cannot
    /// keep it. The refusal comes before any run exists. It names the member,
    /// and a member that is not a peer is told to become one.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_needs_every_remaining_hosts_key_before_any_run(pool: SqlitePool) -> Result {
        let party = pid(50)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2]).await?;
        record_signing_keys(&pool, &party, &[(2, 2)]).await?;

        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 4).await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let error = error_text(&body)?;
        assert_eq!(
            error,
            format!(
                "Cannot kick: this node has no record of the signing key of {p3}. The kick \
                 must keep the key of each member that stays. {p3} is not a configured peer \
                 of this node, so a refresh cannot read its key. Add it as a peer, refresh \
                 /decentralized-parties, then retry.",
                p3 = pid(3)?
            )
        );
        assert_no_kick_started(&pool, &state).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_peer_without_a_recorded_key_is_asked_to_refresh(pool: SqlitePool) -> Result {
        let party = pid(51)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3], &[], &[2, 3]).await?;
        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 3).await?;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let error = error_text(&body)?;
        assert!(error.contains(&pid(2)?.to_string()), "{error}");
        assert!(
            error.ends_with(
                " Refresh /decentralized-parties so each peer reports its key, then retry."
            ),
            "{error}"
        );
        assert!(!error.contains("Add it as a peer"), "{error}");
        assert_no_kick_started(&pool, &state).await
    }

    /// Two remaining members that claim one key are a conflict no retry can
    /// settle. The refusal is a 409 and comes before any run exists.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn two_survivors_that_claim_one_key_are_refused_with_a_conflict(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(52)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 3]).await?;
        record_signing_keys(&pool, &party, &[(2, 2), (3, 2), (4, 4)]).await?;

        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 4).await?;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let error = error_text(&body)?;
        assert!(error.contains("both claim signing key"), "{error}");
        assert!(error.contains(&pid(2)?.to_string()), "{error}");
        assert!(error.contains(&pid(3)?.to_string()), "{error}");
        assert_no_kick_started(&pool, &state).await
    }

    /// A host kick at a threshold equal to the host count can never collect
    /// enough signatures, because the kicked host does not sign. The refusal
    /// comes before any invitation. It is about the signers, so it does not
    /// name the target, whose peer entry the kick does not need.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_short_of_signers_does_not_blame_the_target(pool: SqlitePool) -> Result {
        let party = pid(43)?;
        cache_kick_party(&pool, &party, 3, &[1, 2, 3], &[], &[2]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 2,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("current namespace threshold is 3. Only 2 members can sign"),
            "{body}"
        );
        assert!(body.contains("Change Threshold"), "{body}");
        assert!(!body.contains(&pid(3)?.to_string()), "{body}");
        Ok(())
    }

    /// An owner-only kick removes no host, so every host stays and has to
    /// sign. A host that is not a peer cannot be invited. When that leaves
    /// the run short of the threshold, the refusal names that host.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_only_kick_short_of_signers_names_the_host_that_is_not_a_peer(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(44)?;
        cache_kick_party(&pool, &party, 3, &[1, 2, 3], &["k5"], &[2]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "owner_key": "k5",
                "new_threshold": 2,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains(&format!(
                "{} is a member of the party but is not a configured peer of this node",
                pid(3)?
            )),
            "{body}"
        );
        assert!(body.contains("namespace threshold is 3"), "{body}");
        Ok(())
    }
}
