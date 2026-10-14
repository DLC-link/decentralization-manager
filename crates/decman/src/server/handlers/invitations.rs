use std::{collections::HashMap, time::Duration};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    config::Peer,
    consts::{DECLINE_NOTIFY_BACKOFF_SECS, DECLINE_NOTIFY_MAX_ATTEMPTS},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    noise::{
        Message, MessageType, NoiseKeypair, client::NoiseClient, parse_public_key,
        send_noise_message_with_chunked_response,
    },
    server::{
        AppState, mark_failed_via_pool,
        middleware::require_admin,
        types::{
            DeclineInvitationPayload, ErrorResponse, HandledInvitationsResponse,
            InvitationActionRequest, InvitationOutcome, MessageResponse, PeerJob,
            PendingInvitation, PendingInvitationsResponse, RequestDarPayload, WorkflowKind,
            WorkflowProgress, WorkflowRole, WorkflowRun,
        },
    },
    workflow::{
        AddPartyStep, ChangeThresholdStep, ContractsStep, DarsStep, KickStep, OnboardingStep,
        state::WorkflowStep,
    },
};

/// Handled invitations kept as history. Invitations arrive one per workflow
/// run, so this covers months of activity, and it is also the most the
/// notification feed asks for. The bound is a row count, so it holds however
/// fast invitations arrive.
pub(crate) const MAX_HANDLED_INVITATIONS: i64 = 200;

/// Keep an accepted or declined invitation as history instead of deleting it,
/// and trim the history to [`MAX_HANDLED_INVITATIONS`] in the same transaction.
///
/// A database failure is logged and the answer goes ahead, as it did when
/// this step only deleted the row.
async fn persist_handled_invitation(
    data: &web::Data<AppState>,
    invitation: &PendingInvitation,
    outcome: InvitationOutcome,
) {
    let id = &invitation.id;
    let handled_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tx = match data.db.begin_transaction().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::warn!("Failed to begin tx to record invitation {id}: {e}");
            return;
        }
    };
    let marked = match tx.mark_invitation_handled(id, outcome, handled_at).await {
        Ok(marked) => marked,
        Err(e) => {
            tracing::warn!(
                "Failed to record invitation {id} as {}: {e}",
                outcome.as_str()
            );
            return;
        }
    };
    // No pending row: persisting the invitation on receipt failed, and only
    // the in-memory card was left. Write the answered invitation in full.
    if !marked
        && let Err(e) = tx
            .insert_handled_invitation(invitation, outcome, handled_at)
            .await
    {
        tracing::warn!(
            "Failed to record invitation {id} as {}: {e}",
            outcome.as_str()
        );
        return;
    }
    if let Err(e) = tx.prune_handled_invitations(MAX_HANDLED_INVITATIONS).await {
        tracing::warn!("Failed to prune handled invitations: {e}");
        return;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("Failed to commit handled invitation {id}: {e}");
    }
}

/// Put a removed invitation back (memory + DB) after an accept failed
/// part-way: removal happens FIRST (under the write lock, so a double accept
/// 404s), which means a failure after that point would otherwise strand the
/// run — the card is gone, the operator can't retry, and the coordinator's
/// human-paced WaitingForPeers waits forever. The upsert also turns the row
/// recorded as accepted back into a pending one.
async fn restore_invitation(data: &web::Data<AppState>, invitation: &PendingInvitation) {
    match data.db.begin_transaction().await {
        Ok(mut tx) => {
            if let Err(e) = tx.upsert_pending_invitation(invitation).await {
                tracing::warn!("Failed to re-persist restored invitation: {e}");
            } else if let Err(e) = Commitable::commit(tx).await {
                tracing::warn!("Failed to commit restored invitation: {e}");
            }
        }
        Err(e) => tracing::warn!("Failed to begin tx to restore invitation: {e}"),
    }
    let mut invitations = data.pending_invitations.write().await;
    if !invitations.iter().any(|i| i.id == invitation.id) {
        invitations.push(invitation.clone());
    }
}

fn step_total_for(kind: WorkflowKind) -> i64 {
    match kind {
        WorkflowKind::Onboarding => OnboardingStep::step_total(),
        WorkflowKind::Kick => KickStep::step_total(),
        WorkflowKind::Contracts => ContractsStep::step_total(),
        WorkflowKind::Dars => DarsStep::step_total(),
        WorkflowKind::AddParty => AddPartyStep::step_total(),
        WorkflowKind::ChangeThreshold => ChangeThresholdStep::step_total(),
    }
}

/// Insert the peer-side `workflow_runs` row for a freshly-accepted invite.
///
/// The synthetic instance_name is keyed on the coordinator's run instance
/// (`peer-<kind>-<coord_pubkey[..16]>-<workflow_instance>`): with concurrent
/// workflows a node can accept several same-kind invites from the SAME
/// coordinator back to back, and a timestamp suffix at seconds resolution
/// would collide — the second accept's upsert (ON CONFLICT(instance_name) DO
/// UPDATE) silently merging two distinct runs into one row, cross-wiring
/// their artefacts and statuses. Keying on the coordinator run makes the row
/// unique per run and stable across a re-sent invite of the same run (the
/// re-accept resumes the same row instead of duplicating it). Invites from
/// coordinators that predate instance routing carry no `workflow_instance`;
/// they fall back to the old timestamp suffix.
pub(crate) async fn insert_peer_run(
    db: &SqlitePool,
    invitation: &PendingInvitation,
) -> Option<String> {
    let kind: WorkflowKind = invitation.invitation_type.into();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let pubkey_short =
        &invitation.coordinator_pubkey[..invitation.coordinator_pubkey.len().min(16)];
    let run_suffix = match invitation.workflow_instance.as_deref() {
        Some(coord_instance) if !coord_instance.is_empty() => coord_instance.to_string(),
        _ => now.to_string(),
    };
    let instance_name = format!(
        "peer-{}-{}-{}",
        kind.as_str().to_lowercase(),
        pubkey_short,
        run_suffix
    );
    let run = WorkflowRun {
        instance_name: instance_name.clone(),
        kind,
        role: WorkflowRole::Peer,
        status: WorkflowProgress::InProgress,
        // No per-step granularity on the peer side yet — coordinator drives
        // the protocol; we just track "I'm participating".
        current_step: "Active".to_string(),
        step_index: 0,
        step_total: step_total_for(kind),
        // We don't have the coordinator's full config; we have what they sent
        // in the invite payload. Persist that for forensic purposes / future
        // resume.
        config_json: serde_json::json!({
            "prefix": invitation.prefix,
            "participants": invitation.participants,
            "dar_filenames": invitation.dar_filenames,
            "dar_hashes": invitation.dar_hashes,
            "participant_id": invitation.kicked_participant,
            "new_participant_id": invitation.new_participant,
            "new_threshold": invitation.new_threshold,
            "previous_threshold": invitation.previous_threshold,
            "package_names": invitation.package_names,
            "contract_intents": invitation.contract_intents,
        })
        .to_string(),
        coordinator_pubkey: Some(invitation.coordinator_pubkey.clone()),
        // The coordinator run this peer row belongs to — keys instance-scoped
        // CancelInvite/RetryWorkflow and peer-resume routing.
        coordinator_instance: invitation.workflow_instance.clone(),
        coordinator_name: None,
        // The participants list is the authoritative peer set carried in the
        // invite (all four kinds send one).
        expected_peers: invitation.participants.clone(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        // Kick + contracts invites carry the target dec party; others don't.
        dec_party_id: invitation.dec_party_id.clone(),
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        added_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
        error: None,
        dismissed: false,
        created_at: now,
        updated_at: now,
    };

    let mut tx = match db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("peer run: begin_transaction failed: {e}");
            return None;
        }
    };
    if let Err(e) = tx.upsert_workflow_run(&run).await {
        tracing::warn!("peer run: upsert failed: {e}");
        return None;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("peer run: commit failed: {e}");
        return None;
    }
    Some(instance_name)
}

/// Get all pending invitations
#[utoipa::path(
    tag = "Invitations",
    responses(
        (status = 200, description = "Pending invitations", body = PendingInvitationsResponse)
    )
)]
#[get("/invitations")]
pub async fn get_invitations(data: web::Data<AppState>) -> impl Responder {
    let invitations = data.pending_invitations.read().await;

    // Resolve coordinator names from a single DB query
    let pubkey_to_name: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.public_key, p.name))
        .collect();

    let invitations_with_names: Vec<PendingInvitation> = invitations
        .iter()
        .map(|inv| PendingInvitation {
            coordinator_name: pubkey_to_name.get(&inv.coordinator_pubkey).cloned(),
            ..inv.clone()
        })
        .collect();

    HttpResponse::Ok().json(PendingInvitationsResponse {
        invitations: invitations_with_names,
    })
}

/// Query parameters for the handled invitations endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct HandledInvitationsQuery {
    /// Most invitations to return (default 50, at most 200)
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Get recently accepted and declined invitations, newest first
#[utoipa::path(
    tag = "Invitations",
    params(HandledInvitationsQuery),
    responses(
        (status = 200, description = "Handled invitations, newest first", body = HandledInvitationsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/invitations/handled")]
pub async fn list_handled_invitations(
    data: web::Data<AppState>,
    query: web::Query<HandledInvitationsQuery>,
) -> impl Responder {
    let limit = query.limit.unwrap_or(50).clamp(1, MAX_HANDLED_INVITATIONS);
    let handled = match data.db.get_handled_invitations(limit).await {
        Ok(handled) => handled,
        Err(e) => {
            tracing::error!("Failed to read handled invitations: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read handled invitations: {e}"),
            });
        }
    };

    let pubkey_to_name: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.public_key, p.name))
        .collect();
    let invitations = handled
        .into_iter()
        .map(|mut h| {
            h.invitation.coordinator_name = pubkey_to_name
                .get(&h.invitation.coordinator_pubkey)
                .cloned();
            h
        })
        .collect();

    HttpResponse::Ok().json(HandledInvitationsResponse { invitations })
}

/// Accept a pending invitation and trigger the workflow
#[utoipa::path(
    tag = "Invitations",
    request_body = InvitationActionRequest,
    responses(
        (status = 200, description = "Invitation accepted", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "Invitation not found", body = ErrorResponse)
    )
)]
#[post("/invitations/accept")]
pub async fn accept_invitation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let invitation = {
        let mut invitations = data.pending_invitations.write().await;
        let idx = invitations.iter().position(|i| i.id == body.id);
        match idx {
            Some(i) => invitations.remove(i),
            None => {
                return HttpResponse::NotFound().json(serde_json::json!({
                    "error": "Invitation not found"
                }));
            }
        }
    };

    persist_handled_invitation(&data, &invitation, InvitationOutcome::Accepted).await;

    // Refuse a second accept for a run we're already driving: a resurrected
    // card (e.g. an in-flight invite delivered after a scoped cancel, or a
    // re-broadcast) would otherwise upsert the SAME peer row and enqueue a
    // SECOND start_peer loop for it — two loops double-executing commands
    // against one row. Legacy invites without an instance skip the check.
    if let Some(coord_instance) = invitation
        .workflow_instance
        .as_deref()
        .filter(|i| !i.is_empty())
    {
        let already_running = SchemaRead::get_in_progress_workflow_runs(&data.db)
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| {
                r.role == WorkflowRole::Peer
                    && r.coordinator_instance.as_deref() == Some(coord_instance)
                    && r.coordinator_pubkey.as_deref() == Some(&invitation.coordinator_pubkey)
            });
        if already_running {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": format!(
                    "Already participating in workflow run {coord_instance} from this \
                     coordinator"
                )
            }));
        }
    }

    // Persist a peer-side workflow_runs row so the operator's feed shows
    // "I'm participating in <kind>" until completion.
    let Some(peer_instance) = insert_peer_run(&data.db, &invitation).await else {
        // Put the card back so the operator can retry the accept.
        restore_invitation(&data, &invitation).await;
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Failed to record accepted invitation; the invitation was kept"
        }));
    };

    // Enqueue a peer job. Carrying the coordinator's run instance
    // (`workflow_instance` from the invite) lets the peer route its commands
    // back to the right concurrent run; the single queue lets this node accept
    // many invites of any kind at once without racing over a shared slot.
    let job = PeerJob {
        kind: invitation.invitation_type.into(),
        instance_name: peer_instance,
        coordinator_instance: invitation.workflow_instance.clone().unwrap_or_default(),
        coordinator_pubkey: invitation.coordinator_pubkey.clone(),
    };
    tracing::info!(
        "Accepting {:?} invitation; enqueuing peer job {}",
        invitation.invitation_type,
        job.instance_name
    );
    let peer_instance_for_err = job.instance_name.clone();
    if data.peer_job_sender.send(job).is_err() {
        // The row was just persisted InProgress; without a listener nothing
        // will ever drive it, so mark it Failed instead of leaving a stale
        // in-progress card in the feed — and put the invitation back so the
        // operator can retry once the listener recovers.
        mark_failed_via_pool(
            &data.db,
            &peer_instance_for_err,
            "Peer workflow listener unavailable",
        )
        .await;
        restore_invitation(&data, &invitation).await;
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Peer workflow listener unavailable; the invitation was kept"
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({
        "message": "Invitation accepted"
    }))
}

/// Decline a pending invitation
#[utoipa::path(
    tag = "Invitations",
    request_body = InvitationActionRequest,
    responses(
        (status = 200, description = "Invitation declined", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "Invitation not found", body = ErrorResponse)
    )
)]
#[post("/invitations/decline")]
pub async fn decline_invitation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let invitation = {
        let mut invitations = data.pending_invitations.write().await;
        let idx = invitations.iter().position(|i| i.id == body.id);
        match idx {
            Some(i) => invitations.remove(i),
            None => {
                return HttpResponse::NotFound().json(serde_json::json!({
                    "error": "Invitation not found"
                }));
            }
        }
    };

    persist_handled_invitation(&data, &invitation, InvitationOutcome::Declined).await;

    // Best-effort: tell the coordinator we declined so they can fail their
    // matching in-progress run immediately instead of waiting until timeout.
    // If the Noise round-trip fails for any reason we still report success
    // to the operator — the local invitation is gone and the coordinator's
    // existing timeout/cancel paths cover the worst case.
    notify_coordinator_of_decline(&data, &invitation).await;

    tracing::info!("Declined invitation {}", invitation.id);
    HttpResponse::Ok().json(serde_json::json!({
        "message": "Invitation declined"
    }))
}

/// Best-effort: open a Noise client to the coordinator and send a
/// `DeclineInvitation` message. Logs (but does not propagate) failures —
/// callers treat this as fire-and-forget.
async fn notify_coordinator_of_decline(data: &web::Data<AppState>, invitation: &PendingInvitation) {
    let coordinator = match find_coordinator_peer(data, &invitation.coordinator_pubkey).await {
        Some(p) => p,
        None => {
            tracing::warn!(
                "Cannot notify coordinator of decline: no peer record for pubkey {}",
                invitation.coordinator_pubkey
            );
            return;
        }
    };

    let payload = DeclineInvitationPayload {
        kind: invitation.invitation_type.into(),
        reason: None,
        // Echo the coordinator's run identity so it only fails the matching
        // run — a stale card's decline must not kill a newer workflow.
        workflow_instance: invitation.workflow_instance.clone(),
        abandoned: false,
    };
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Failed to encode DeclineInvitationPayload: {e}");
            return;
        }
    };

    // Route the decline to the coordinator's matching run so it fails only that
    // run when multiple workflows are active.
    let route_instance = invitation.workflow_instance.clone().unwrap_or_default();
    let client = match NoiseClient::new(data.config.clone(), coordinator, route_instance).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to build NoiseClient for decline notification: {e}");
            return;
        }
    };

    // Bounded retries: this is the only signal that fails the coordinator's
    // run — its WaitingForPeers is human-paced (no timeout), so a dropped
    // notification leaves that run hanging until a manual cancel. The budget
    // is the dedicated DECLINE_NOTIFY profile, NOT the fast-transport
    // noise_retry one: a decline sent the moment the invite card appears
    // races the coordinator's ~2s init window (invites are sent before the
    // run registers its Noise handle), during which the listener answers 503
    // — the retries must outlive that window (see consts.rs). Spawned: each
    // attempt can block for the full Noise request timeout, and the decline
    // HTTP response must not hang the operator's UI on an unreachable
    // coordinator — the notification is explicitly best-effort.
    tokio::spawn(async move {
        let max_attempts = DECLINE_NOTIFY_MAX_ATTEMPTS.max(1);
        for attempt in 1..=max_attempts {
            match client.send_decline_invitation(payload_bytes.clone()).await {
                Ok(()) => return,
                Err(e) if attempt < max_attempts => {
                    tracing::warn!(
                        "Decline notification to coordinator failed \
                         (attempt {attempt}/{max_attempts}), retrying: {e}"
                    );
                    tokio::time::sleep(Duration::from_secs(DECLINE_NOTIFY_BACKOFF_SECS)).await;
                }
                Err(e) => {
                    tracing::warn!(
                        "Best-effort decline notification to coordinator failed after \
                         {max_attempts} attempts: {e}"
                    );
                }
            }
        }
    });
}

/// Look up the `Peer` record that owns the given Noise public key. The
/// invitation carries the coordinator's pubkey but `get_peer` is keyed by
/// participant id, so iterate the peers table.
async fn find_coordinator_peer(
    data: &web::Data<AppState>,
    coordinator_pubkey: &str,
) -> Option<Peer> {
    data.db
        .get_all_peers()
        .await
        .ok()?
        .into_iter()
        .find(|p| p.public_key == coordinator_pubkey)
}

/// Set on a DAR served from an invite that pins no hashes, so the caller can
/// tell the operator the bytes were not checked.
pub const UNPINNED_DAR_HEADER: &str = "X-Decman-Dar-Unpinned";

/// Fetch one DAR of a pending DARs invitation from the coordinator so the
/// operator can read it before accepting. The bytes are not stored: they are
/// pulled over Noise on demand, checked against the hash the invite pinned,
/// and handed straight to the caller.
#[utoipa::path(
    tag = "Invitations",
    params(
        ("id" = String, Path, description = "Pending invitation id"),
        ("index" = usize, Path, description = "Index into the invitation's dar_filenames"),
    ),
    responses(
        (status = 200, description = "The DAR", content_type = "application/octet-stream"),
        (status = 404, description = "No such invitation or DAR", body = ErrorResponse),
        (status = 422, description = "The invitation pins only some of its DARs", body = ErrorResponse),
        (status = 502, description = "The coordinator did not serve it", body = ErrorResponse),
    )
)]
#[get("/invitations/{id}/dars/{index}")]
pub async fn get_invitation_dar(
    data: web::Data<AppState>,
    path: web::Path<(String, usize)>,
    http_req: HttpRequest,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let (id, index) = path.into_inner();

    let invitation = {
        let invitations = data.pending_invitations.read().await;
        match invitations.iter().find(|i| i.id == id) {
            Some(i) => i.clone(),
            None => {
                return HttpResponse::NotFound().json(ErrorResponse {
                    error: format!("No pending invitation {id}"),
                });
            }
        }
    };

    let Some(filename) = invitation.dar_filenames.get(index).cloned() else {
        return HttpResponse::NotFound().json(ErrorResponse {
            error: format!("Invitation {id} offers no DAR at index {index}"),
        });
    };
    let expected_hash =
        match pinned_dar_hash(&invitation.dar_filenames, &invitation.dar_hashes, index) {
            Ok(hash) => hash.map(str::to_string),
            Err(e) => {
                tracing::warn!("Refused to serve {filename} of invitation {id}: {e}");
                return HttpResponse::UnprocessableEntity().json(ErrorResponse {
                    error: format!("{e}. Do not accept it."),
                });
            }
        };
    let Some(workflow_instance) = invitation.workflow_instance.clone() else {
        return HttpResponse::NotFound().json(ErrorResponse {
            error: "This invitation predates run-scoped DAR reads".to_string(),
        });
    };

    let Some(coordinator) = find_coordinator_peer(&data, &invitation.coordinator_pubkey).await
    else {
        return HttpResponse::BadGateway().json(ErrorResponse {
            error: "No peer record for the coordinator that sent this invitation".to_string(),
        });
    };

    let dar = match fetch_dar_from_coordinator(
        &data,
        &coordinator,
        &RequestDarPayload {
            workflow_instance,
            index,
            filename: filename.clone(),
        },
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("Could not read {filename} from the coordinator: {e:#}");
            return HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Could not read {filename} from the coordinator: {e:#}"),
            });
        }
    };

    let Some(expected) = expected_hash else {
        tracing::warn!(
            "Served {filename} of invitation {id} unpinned: the invite carries no hashes"
        );
        return dar_response(&filename, dar, true);
    };
    let actual = crate::workflow::validation::hash_dar(&dar);
    if actual != expected {
        tracing::warn!(
            "Coordinator served a {filename} that is not the one it invited us to \
             ({actual}, invited {expected})"
        );
        return HttpResponse::BadGateway().json(ErrorResponse {
            error: format!(
                "{filename} does not match the hash this invitation pinned. \
                 Do not accept it."
            ),
        });
    }

    dar_response(&filename, dar, false)
}

fn dar_response(filename: &str, dar: Vec<u8>, unpinned: bool) -> HttpResponse {
    let mut response = HttpResponse::Ok();
    response
        .content_type("application/octet-stream")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}\"", sanitize_filename(filename)),
        ));
    if unpinned {
        response.insert_header((UNPINNED_DAR_HEADER, "true"));
    }
    response.body(dar)
}

/// The hash `dar_hashes` pins for DAR `index`, or `None` when the invite pins
/// none at all (a coordinator that predates the field). A list that covers
/// only some of the filenames is refused, as `validation.rs` refuses it at
/// upload.
fn pinned_dar_hash<'a>(
    dar_filenames: &[String],
    dar_hashes: &'a [String],
    index: usize,
) -> Result<Option<&'a str>, String> {
    if dar_hashes.is_empty() {
        return Ok(None);
    }
    if dar_hashes.len() != dar_filenames.len() {
        return Err(format!(
            "The invitation pins {hashes} DAR hash(es) for {names} filename(s)",
            hashes = dar_hashes.len(),
            names = dar_filenames.len(),
        ));
    }
    dar_hashes
        .get(index)
        .map(|hash| Some(hash.as_str()))
        .ok_or_else(|| format!("The invitation pins no hash for DAR {index}"))
}

/// Quotes and path separators out: this name comes off the wire and goes into
/// a `Content-Disposition` header and the browser's save dialog.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '"' | '\\' | '/' | '\r' | '\n' => '_',
            c => c,
        })
        .collect()
}

/// Ask the coordinator for one DAR over Noise, reassembling a chunked answer.
async fn fetch_dar_from_coordinator(
    data: &web::Data<AppState>,
    coordinator: &Peer,
    request: &RequestDarPayload,
) -> anyhow::Result<Vec<u8>> {
    let peer_pub_key = parse_public_key(&coordinator.public_key)
        .map_err(|e| anyhow::anyhow!("the coordinator's public key does not parse: {e}"))?;
    let keypair = NoiseKeypair::from_file(&data.config.key_file_path()).await?;
    let psk = keypair.derive_psk(&peer_pub_key);
    let identity = data.config.participant_id().to_string();

    let message = Message::new(MessageType::RequestDar, serde_json::to_vec(request)?);
    let response = send_noise_message_with_chunked_response(
        &coordinator.address,
        coordinator.port,
        &psk,
        identity.as_bytes(),
        &message,
        &data.config.noise_retry,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let response = Message::from_bytes(&response)
        .map_err(|_| anyhow::anyhow!("the coordinator's answer does not parse"))?;
    match response.msg_type {
        MessageType::Data => Ok(response.payload),
        MessageType::Error => Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&response.payload)
        )),
        other => Err(anyhow::anyhow!("unexpected answer {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("pkg-{i}.dar")).collect()
    }

    #[test]
    fn full_hash_list_pins_the_index() {
        let hashes = vec!["aa".to_string(), "bb".to_string()];

        let pinned = pinned_dar_hash(&names(2), &hashes, 1);

        assert_eq!(pinned, Ok(Some("bb")));
    }

    #[test]
    fn empty_hash_list_serves_unpinned() {
        let pinned = pinned_dar_hash(&names(2), &[], 0);

        assert_eq!(pinned, Ok(None));
    }

    #[test]
    fn partial_hash_list_is_refused_for_every_index() {
        let hashes = vec!["aa".to_string()];

        for index in 0..2 {
            let pinned = pinned_dar_hash(&names(2), &hashes, index);

            assert!(pinned.is_err(), "index {index} was not refused: {pinned:?}");
        }
    }

    #[test]
    fn longer_hash_list_is_refused() {
        let hashes = vec!["aa".to_string(), "bb".to_string()];

        let pinned = pinned_dar_hash(&names(1), &hashes, 0);

        assert!(pinned.is_err(), "not refused: {pinned:?}");
    }

    #[test]
    fn unpinned_response_carries_the_warning_header() {
        let response = dar_response("pkg.dar", vec![1, 2, 3], true);

        assert_eq!(
            response
                .headers()
                .get(UNPINNED_DAR_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("true")
        );
    }

    #[test]
    fn pinned_response_has_no_warning_header() {
        let response = dar_response("pkg.dar", vec![1, 2, 3], false);

        assert!(response.headers().get(UNPINNED_DAR_HEADER).is_none());
    }
}

#[cfg(test)]
mod handled_invitation_tests {
    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};
    use serde_json::{Value, json};

    use super::*;
    use crate::{auth::Principal, db::MIGRATOR, server::InvitationType};

    fn invitation(id: &str) -> PendingInvitation {
        PendingInvitation {
            id: id.to_string(),
            invitation_type: InvitationType::Onboarding,
            coordinator_pubkey: "c0ffee".repeat(8),
            coordinator_name: None,
            received_at: 1000,
            prefix: Some("party-x".to_string()),
            participants: Vec::new(),
            dar_filenames: Vec::new(),
            dar_hashes: Vec::new(),
            kicked_participant: None,
            new_participant: None,
            new_threshold: None,
            previous_threshold: None,
            dec_party_id: None,
            package_names: Vec::new(),
            contract_intents: Vec::new(),
            workflow_instance: Some("run-1".to_string()),
        }
    }

    /// App state on a migrated database holding `pending` as a received invite.
    async fn state_with(
        pool: &SqlitePool,
        pending: &PendingInvitation,
    ) -> anyhow::Result<web::Data<AppState>> {
        let state = AppState::for_test_on(None, pool.clone());
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_pending_invitation(pending).await?;
        Commitable::commit(tx).await?;
        state
            .pending_invitations
            .write()
            .await
            .push(pending.clone());
        Ok(state)
    }

    /// A POST of `{ "id": id }` to `uri` from an authenticated operator. A
    /// macro because the built request's type is not nameable from here.
    macro_rules! post {
        ($uri:expr, $id:expr) => {{
            let request = actix_test::TestRequest::post()
                .uri($uri)
                .set_json(json!({ "id": $id }))
                .to_request();
            request.extensions_mut().insert(Principal {
                sub: "admin".to_string(),
                issuer: "test".to_string(),
                roles: Vec::new(),
                email: None,
            });
            request
        }};
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_declined_invitation_is_kept_and_cannot_be_answered_again(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = state_with(&pool, &invitation("inv-1")).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .service(get_invitations)
                .service(accept_invitation)
                .service(decline_invitation),
        )
        .await;

        let declined = actix_test::call_service(&app, post!("/invitations/decline", "inv-1")).await;
        assert_eq!(declined.status(), StatusCode::OK);

        let listed: Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/invitations")
                .to_request(),
        )
        .await;
        assert_eq!(listed, json!({ "invitations": [] }));
        let handled = pool.get_handled_invitations(50).await?;
        assert_eq!(handled.len(), 1);
        assert_eq!(handled[0].invitation.id, "inv-1");
        assert_eq!(handled[0].outcome, InvitationOutcome::Declined);

        for uri in ["/invitations/decline", "/invitations/accept"] {
            let again = actix_test::call_service(&app, post!(uri, "inv-1")).await;
            assert_eq!(again.status(), StatusCode::NOT_FOUND, "{uri}");
        }
        assert_eq!(pool.get_handled_invitations(50).await?.len(), 1);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_handled_list_is_newest_first_and_names_the_coordinator(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let older = invitation("inv-1");
        let state = state_with(&pool, &older).await?;
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_pending_invitation(&invitation("inv-2")).await?;
        // Still pending, so not listed.
        tx.upsert_pending_invitation(&invitation("inv-3")).await?;
        tx.mark_invitation_handled("inv-1", InvitationOutcome::Declined, 5000)
            .await?;
        tx.mark_invitation_handled("inv-2", InvitationOutcome::Accepted, 6000)
            .await?;
        tx.insert_peer(&crate::config::Peer {
            participant_id: crate::canton_id::CantonId::parse(&format!(
                "coordinator::1220{}",
                "ab".repeat(32)
            ))?,
            name: "Coordinator One".to_string(),
            address: "localhost".to_string(),
            port: 1,
            public_key: older.coordinator_pubkey.clone(),
            party: None,
        })
        .await?;
        Commitable::commit(tx).await?;
        let app =
            actix_test::init_service(App::new().app_data(state).service(list_handled_invitations))
                .await;

        let all: Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/invitations/handled")
                .to_request(),
        )
        .await;
        let newest: Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/invitations/handled?limit=1")
                .to_request(),
        )
        .await;

        let summary: Vec<(&str, &str, i64)> = all["invitations"]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|h| {
                        (
                            h["invitation"]["id"].as_str().unwrap_or_default(),
                            h["outcome"].as_str().unwrap_or_default(),
                            h["handled_at"].as_i64().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            summary,
            vec![("inv-2", "accepted", 6000), ("inv-1", "declined", 5000)]
        );
        assert_eq!(
            all["invitations"][0]["invitation"]["coordinator_name"],
            json!("Coordinator One")
        );
        assert_eq!(newest["invitations"].as_array().map(Vec::len), Some(1));
        assert_eq!(newest["invitations"][0]["invitation"]["id"], json!("inv-2"));
        Ok(())
    }

    /// A card whose persist failed on receipt lives only in memory. Its
    /// answer must still reach the history.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_answer_to_a_card_kept_only_in_memory_reaches_the_history(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let (state, mut peer_jobs) = AppState::for_test_with_peer_jobs(None, pool.clone());
        state
            .pending_invitations
            .write()
            .await
            .extend([invitation("inv-a"), invitation("inv-d")]);
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .service(accept_invitation)
                .service(decline_invitation)
                .service(list_handled_invitations),
        )
        .await;

        let accepted = actix_test::call_service(&app, post!("/invitations/accept", "inv-a")).await;
        let declined = actix_test::call_service(&app, post!("/invitations/decline", "inv-d")).await;

        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(declined.status(), StatusCode::OK);
        assert!(
            peer_jobs.try_recv().is_ok(),
            "the accept enqueued no peer job"
        );
        let listed: Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/invitations/handled")
                .to_request(),
        )
        .await;
        let mut answers: Vec<(String, String, String)> = listed["invitations"]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|h| {
                        (
                            h["invitation"]["id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            h["outcome"].as_str().unwrap_or_default().to_string(),
                            h["invitation"]["prefix"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        answers.sort();
        assert_eq!(
            answers,
            vec![
                ("inv-a".into(), "accepted".into(), "party-x".into()),
                ("inv-d".into(), "declined".into(), "party-x".into()),
            ]
        );
        assert!(pool.get_all_pending_invitations().await?.is_empty());
        Ok(())
    }

    /// Before history, a failed delete was logged and the decline went ahead.
    /// A failed history write must not fail the decline either.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_database_failure_does_not_fail_the_decline(pool: SqlitePool) -> anyhow::Result<()> {
        let state = AppState::for_test_on(None, pool.clone());
        state
            .pending_invitations
            .write()
            .await
            .push(invitation("inv-1"));
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .service(decline_invitation),
        )
        .await;
        pool.close().await;

        let declined = actix_test::call_service(&app, post!("/invitations/decline", "inv-1")).await;

        assert_eq!(declined.status(), StatusCode::OK);
        assert!(state.pending_invitations.read().await.is_empty());
        Ok(())
    }

    /// The test state has no peer listener, so the accept fails after the
    /// invitation was recorded as accepted. The operator must get the card
    /// back, and the history must not claim an accept that did not happen.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_accept_leaves_the_invitation_pending(pool: SqlitePool) -> anyhow::Result<()> {
        let state = state_with(&pool, &invitation("inv-1")).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .service(accept_invitation),
        )
        .await;

        let accepted = actix_test::call_service(&app, post!("/invitations/accept", "inv-1")).await;

        assert_eq!(accepted.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let pending = pool.get_all_pending_invitations().await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "inv-1");
        assert!(pool.get_handled_invitations(50).await?.is_empty());
        assert_eq!(state.pending_invitations.read().await.len(), 1);
        Ok(())
    }
}
