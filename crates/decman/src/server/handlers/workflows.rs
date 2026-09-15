//! Workflow start, status, cancel, retry, dismiss, and list handlers.
//!
//! Every start handler validates the request, then hands a
//! [`StartRequest`](crate::onledger::StartRequest) to
//! [`onledger::start_run`], which runs the preflight gates (design D3, section
//! 6), creates the `WorkflowProposal`, and persists the coordinator row. The
//! observer loop drives the run from there; nothing here spawns a task per
//! run (design D11). Cancel and retry are row operations on the engine.

use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, http::StatusCode, post, web};
use sqlx::SqlitePool;

use crate::{
    canton_id::{CantonId, validate_party_id_prefix},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    onledger::{self, PreflightRejected, StartRequest, dars},
    server::{
        AppState,
        middleware::require_admin,
        types::{
            AcsTransferProgress, AddPartyRequest, ChangeThresholdRequest, ContractsRequest,
            DarsRequest, ErrorResponse, ExternalPartiesResponse, ExternalPartyHost,
            ExternalPartyInfo, KickRequest, MessageResponse, OnboardingRequest, SuccessResponse,
            WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole, WorkflowRun,
            WorkflowRunsResponse, WorkflowStatusResponse, permission_from_proto,
        },
    },
    workflow::{self, storage::WorkflowStorage},
};

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ============================================================================
// Shared start plumbing
// ============================================================================

/// The HTTP status a failed `start_run` maps to: a preflight refusal is a
/// 409 that names the peers or the threshold at fault, a missing node
/// identity is a 409 the operator resolves with `PUT /node-identity`, and
/// everything else is a 500.
fn start_error_status(e: &anyhow::Error) -> StatusCode {
    if e.downcast_ref::<PreflightRejected>().is_some() {
        return StatusCode::CONFLICT;
    }
    let message = format!("{e:#}").to_ascii_lowercase();
    if message.contains("node identity") || message.contains("already exists") {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// Run `start_run` and turn the outcome into the HTTP response every start
/// handler shares: `202` with the run's `instance_name`, or the error status
/// of [`start_error_status`] with the engine's message.
async fn start(data: &web::Data<AppState>, req: StartRequest, label: &str) -> HttpResponse {
    let kind = req.kind();
    match onledger::start_run(&data.onledger, req).await {
        Ok(started) => {
            tracing::info!(
                kind = %kind,
                instance = %started.instance_name,
                proposal = %started.proposal_cid,
                "{label} workflow started"
            );
            HttpResponse::Accepted().json(WorkflowResponse {
                status: WorkflowProgress::InProgress,
                message: format!("{label} workflow started"),
                instance_name: started.instance_name,
            })
        }
        Err(e) => {
            let status = start_error_status(&e);
            if status == StatusCode::INTERNAL_SERVER_ERROR {
                tracing::error!(kind = %kind, error = %format!("{e:#}"), "{label} start failed");
            } else {
                tracing::info!(kind = %kind, error = %format!("{e:#}"), "{label} start refused");
            }
            HttpResponse::build(status).json(ErrorResponse {
                error: format!("Failed to start {label} workflow: {e}"),
            })
        }
    }
}

/// The cached member set of a party, as participant ids. Empty when nothing
/// is cached or no cached uid parses.
async fn cached_members(
    db: &SqlitePool,
    party: &CantonId,
) -> std::result::Result<HashSet<CantonId>, Box<HttpResponse>> {
    match db.get_dec_party_participants(party).await {
        Ok(rows) => Ok(rows
            .iter()
            .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
            .collect()),
        Err(e) => {
            tracing::error!("Failed to load dec party participants: {e}");
            // Boxed: an `HttpResponse` is far larger than the `Ok` value, and
            // clippy's `result_large_err` refuses to let every caller carry it.
            Err(Box::new(HttpResponse::InternalServerError().json(
                ErrorResponse {
                    error: "Failed to load decentralized party members".to_string(),
                },
            )))
        }
    }
}

/// The 409 for a party that already has a run in flight on this node.
async fn refuse_second_run_for_party(db: &SqlitePool, party: &CantonId) -> Option<HttpResponse> {
    let (run, kind) = find_inprogress_run_for_party(db, party).await?;
    Some(HttpResponse::Conflict().json(ErrorResponse {
        error: format!(
            "Party {party} already has a {kind} workflow in flight (run {run}); wait for it to \
             finish or cancel it first"
        ),
    }))
}

// ============================================================================
// Kick Workflow
// ============================================================================

/// A party's member count, as a threshold bound has to see it.
///
/// `members` is the cached membership and `self_id` is this node. The
/// coordinator signs the proposals, so it is necessarily a member — but it does
/// not appear in its own `peers` table, so a member set derived from that list
/// omits it. Counting it explicitly keeps the bound right however `members` was
/// built, including the fallback that substitutes the configured peer set when
/// nothing is cached.
fn party_member_count(members: &HashSet<CantonId>, self_id: &CantonId) -> usize {
    members.len() + usize::from(!members.contains(self_id))
}

/// Start a kick workflow to remove a participant from a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = KickRequest,
    responses(
        (status = 202, description = "Kick workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress, a peer is not ready for on-ledger coordination, or the threshold is out of range", body = ErrorResponse),
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
        "Kick request received: party={}, participant_to_kick={}, threshold={}",
        body.decentralized_party_id,
        body.participant_id,
        body.new_threshold
    );

    let decentralized_party_id = body.decentralized_party_id.clone();
    let participant_id = body.participant_id.clone();

    // Prevent kicking ourselves
    if participant_id == *data.config.participant_id() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot kick yourself".to_string(),
        });
    }

    // Bound `new_threshold` by the cached membership when one exists. The
    // engine re-checks against the live topology (a 409), so this is the
    // fast 400 for a request the UI built from a stale party view.
    let party_member_ids = match cached_members(&data.db, &decentralized_party_id).await {
        Ok(ids) => ids,
        Err(resp) => return *resp,
    };
    if !party_member_ids.is_empty() {
        if !party_member_ids.contains(&participant_id) {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!(
                    "Cannot kick {participant_id}: not a member of this decentralized party"
                ),
            });
        }
        let self_id = data.config.participant_id().clone();
        let post_kick_member_count = party_member_count(&party_member_ids, &self_id) as i32 - 1;
        if body.new_threshold < 1 || body.new_threshold > post_kick_member_count {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!(
                    "new_threshold must be between 1 and {post_kick_member_count} \
                     (party member count {n}, minus the participant being kicked); got {got}",
                    n = party_member_count(&party_member_ids, &self_id),
                    got = body.new_threshold,
                ),
            });
        }
    } else if body.new_threshold < 1 {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be at least 1; got {}",
                body.new_threshold
            ),
        });
    }

    // Refuse a second workflow targeting the SAME decentralized party — the
    // party's topology mutations must not interleave.
    if let Some(resp) = refuse_second_run_for_party(&data.db, &decentralized_party_id).await {
        return resp;
    }

    let instance_name = format!("{}-kick-{}", decentralized_party_id.prefix, now_secs());
    start(
        &data,
        StartRequest::Kick {
            dec_party_id: decentralized_party_id,
            participant_id,
            new_threshold: body.new_threshold,
            previous_threshold: body.previous_threshold,
            instance_name,
        },
        "Kick",
    )
    .await
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

/// Summarize the status of a coordinator run of `kind` for the legacy
/// per-kind `/{kind}/status` endpoints: the in-progress run with the lowest
/// `instance_name` when one exists, else the newest visible run of that
/// kind. With concurrent runs of one kind these endpoints are coarse; callers
/// that want per-instance detail use `GET /workflows`.
async fn kind_status(data: &web::Data<AppState>, kind: WorkflowKind) -> WorkflowStatusResponse {
    if let Ok(Some(run)) = data
        .db
        .get_active_workflow_run(kind, WorkflowRole::Coordinator)
        .await
    {
        return WorkflowStatusResponse {
            status: run.status,
            error: run.error,
        };
    }
    if let Ok(runs) = SchemaRead::get_visible_workflow_runs(&data.db).await
        && let Some(run) = runs
            .into_iter()
            .filter(|r| r.kind == kind && r.role == WorkflowRole::Coordinator)
            .max_by_key(|r| r.created_at)
    {
        return WorkflowStatusResponse {
            status: run.status,
            error: run.error,
        };
    }
    WorkflowStatusResponse {
        status: WorkflowProgress::default(),
        error: None,
    }
}

// ============================================================================
// Add-party Workflow
// ============================================================================

/// Start an add-party workflow to add a participant to a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = AddPartyRequest,
    responses(
        (status = 202, description = "Add-party workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress, the participant is already a member, or a peer is not ready", body = ErrorResponse),
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

    tracing::info!(
        "Add-party request received: party={}, new_participant={}, threshold={}",
        body.decentralized_party_id,
        body.new_participant_id,
        body.new_threshold
    );

    let decentralized_party_id = body.decentralized_party_id.clone();
    let new_participant_id = body.new_participant_id.clone();

    if new_participant_id == *data.config.participant_id() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot add yourself as the new participant".to_string(),
        });
    }

    let peers = match data.db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to load peers for add-party: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };

    // The new member must be a configured peer with a node party: the
    // proposal names its node party as observer (design D6).
    match peers
        .iter()
        .find(|p| p.participant_id == new_participant_id)
    {
        None => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!(
                    "Participant {new_participant_id} is not a configured peer of this node"
                ),
            });
        }
        Some(peer) if peer.party.is_none() => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!(
                    "Peer {new_participant_id} has no node party in the network config; add \
                     its `participant_id,node_party_id,name` line first"
                ),
            });
        }
        Some(_) => {}
    }

    let party_member_ids = match cached_members(&data.db, &decentralized_party_id).await {
        Ok(ids) => ids,
        Err(resp) => return *resp,
    };
    if party_member_ids.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "No cached membership for {decentralized_party_id}. Try refreshing \
                 /decentralized-parties first."
            ),
        });
    }
    if party_member_ids.contains(&new_participant_id) {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Participant {new_participant_id} is already a member of \
                 {decentralized_party_id}"
            ),
        });
    }
    if !party_member_ids.contains(data.config.participant_id()) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "This node is not a member of {decentralized_party_id}; only an existing \
                 member can coordinate adding one"
            ),
        });
    }

    // Bound the new threshold by the members that can actually sign, which
    // excludes the one being added. It carries Canton's onboarding marker until
    // its ACS import completes, so it confirms nothing — and a party mapping
    // added at threshold = the post-add member count never becomes effective,
    // which stalls the run with no signal saying why.
    //
    // Raising the threshold to include the new member is a second step, after
    // its marker clears, via the change-threshold workflow. The engine
    // re-checks against the live owner set.
    let signing_member_count = party_member_ids.len() as i32;
    if body.new_threshold < 1 || body.new_threshold > signing_member_count {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {signing_member_count} (the members that \
                 can sign; the member being added cannot until its ACS import completes); got \
                 {got}. To include it, run change-threshold once its onboarding marker clears",
                got = body.new_threshold,
            ),
        });
    }

    if let Some(resp) = refuse_second_run_for_party(&data.db, &decentralized_party_id).await {
        return resp;
    }

    let instance_name = format!("{}-add-party-{}", decentralized_party_id.prefix, now_secs());
    start(
        &data,
        StartRequest::AddParty {
            dec_party_id: decentralized_party_id,
            new_participant_id,
            new_threshold: body.new_threshold,
            previous_threshold: body.previous_threshold,
            instance_name,
        },
        "Add-party",
    )
    .await
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

// ============================================================================
// Change-threshold Workflow
// ============================================================================

/// Start a change-threshold workflow for a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = ChangeThresholdRequest,
    responses(
        (status = 202, description = "Change-threshold workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress or a peer is not ready", body = ErrorResponse),
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
        "Change-threshold request received: party={}, threshold={}->{}",
        body.decentralized_party_id,
        body.previous_threshold,
        body.new_threshold
    );

    let decentralized_party_id = body.decentralized_party_id.clone();

    let party_member_ids = match cached_members(&data.db, &decentralized_party_id).await {
        Ok(ids) => ids,
        Err(resp) => return *resp,
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
    let member_count = party_member_ids.len() as i32;
    if body.new_threshold < 1 || body.new_threshold > member_count {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {member_count} (party member count); got {got}",
                got = body.new_threshold,
            ),
        });
    }

    if let Some(resp) = refuse_second_run_for_party(&data.db, &decentralized_party_id).await {
        return resp;
    }

    let instance_name = format!(
        "{}-change-threshold-{}",
        decentralized_party_id.prefix,
        now_secs()
    );
    start(
        &data,
        StartRequest::ChangeThreshold {
            dec_party_id: decentralized_party_id,
            new_threshold: body.new_threshold,
            previous_threshold: body.previous_threshold,
            instance_name,
        },
        "Change-threshold",
    )
    .await
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

/// Bound an explicit confirmation threshold by the host count: at most
/// `num_hosts - 1` so one host can still leave, and at least 1.
pub(crate) fn validate_confirmation_threshold(
    threshold: Option<u32>,
    num_hosts: usize,
) -> std::result::Result<(), String> {
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

// ============================================================================
// Onboarding Workflow
// ============================================================================

/// Start an onboarding workflow to create a new decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = OnboardingRequest,
    responses(
        (status = 202, description = "Onboarding workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress, duplicate prefix, or a peer is not ready for on-ledger coordination", body = ErrorResponse)
    )
)]
#[post("/onboarding")]
pub async fn start_onboarding(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<OnboardingRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    // Reject an invalid party prefix up-front: it becomes the identifier part
    // of the Canton party id (`<prefix>::<namespace>`), and a bad character
    // would otherwise fail deep in the workflow as an opaque Canton proto
    // deserialization error. Fail fast with a clear 400 instead.
    if let Err(msg) = validate_party_id_prefix(&body.party_id_prefix) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }
    if body.peer_ids.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must name at least one other participant".to_string(),
        });
    }
    if body.peer_ids.contains(data.config.participant_id()) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must not contain this node; it joins as the coordinator".to_string(),
        });
    }

    let party_id_prefix = body.party_id_prefix.clone();
    let instance_name = format!("{party_id_prefix}-creation");

    // If the operator set an explicit initial threshold, bound it by the owner
    // count (invited peers + this coordinator). Omitted => the engine uses the
    // majority default once the owner set is resolved.
    let owner_count = body.peer_ids.len() as i32 + 1;
    if let Some(t) = body.threshold
        && !(1..=owner_count).contains(&t)
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "threshold must be between 1 and {owner_count} (invited peers + this node); got {t}"
            ),
        });
    }

    // The duplicate-prefix refusal lives in `Onboarding::preflight`, which
    // `start_run` runs before it creates the proposal. It reaches the operator
    // as the same 409 through `start_error_status`.
    start(
        &data,
        StartRequest::Onboarding {
            party_id_prefix,
            peer_ids: body.peer_ids.clone(),
            threshold: body.threshold,
            instance_name,
        },
        "Onboarding",
    )
    .await
}

/// Get the current status of the onboarding workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Onboarding workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/onboarding/status")]
pub async fn get_onboarding_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Onboarding).await)
}

// ============================================================================
// Contracts Workflow
// ============================================================================

/// Start a contracts workflow to deploy contracts for a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = ContractsRequest,
    responses(
        (status = 202, description = "Contracts workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress or a peer is not ready", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
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
    if body.participant_ids.len() != body.participant_parties.len() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "participant_ids ({}) and participant_parties ({}) must have the same length",
                body.participant_ids.len(),
                body.participant_parties.len()
            ),
        });
    }

    if let Some(resp) = refuse_second_run_for_party(&data.db, &body.decentralized_party_id).await {
        return resp;
    }

    let instance_name = format!(
        "{}-contracts-{}",
        body.decentralized_party_id.prefix,
        now_secs()
    );
    start(
        &data,
        StartRequest::Contracts {
            dec_party_id: body.decentralized_party_id.clone(),
            participant_ids: body.participant_ids.clone(),
            participant_parties: body.participant_parties.clone(),
            operator_party: body.operator_party.clone(),
            contracts: body.contracts.clone(),
            instance_name,
        },
        "Contracts",
    )
    .await
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

// ============================================================================
// DARs Upload (Local)
// ============================================================================

/// The HTTP status for a pinned upload that failed, from the words
/// `onledger::dars` uses: no proposal for the run is 404, an unaccepted
/// invitation or a file that matches no pin is 409, anything else 500.
fn pinned_upload_status(message: &str) -> StatusCode {
    let lower = message.to_ascii_lowercase();
    if lower.contains("no active dars workflowproposal") || lower.contains("does not name") {
        StatusCode::NOT_FOUND
    } else if lower.contains("not accepted")
        || lower.contains("matches no pin")
        || lower.contains("no pin")
        || lower.contains("more than one")
        || lower.contains("node identity")
    {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// Upload DAR files to this participant. With `pin_instance` (design D8)
/// every file must match a pin of that `Dars` run's `WorkflowProposal`, and
/// the upload is pinned to the pin's main package id; the observer completes
/// the run when it sees the vetting. Without it, a plain local upload.
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 200, description = "DARs uploaded to local node", body = SuccessResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "pin_instance names no active Dars proposal", body = ErrorResponse),
        (status = 409, description = "A file matches no pin, or the invitation is not accepted", body = ErrorResponse),
        (status = 500, description = "Upload failed", body = ErrorResponse)
    )
)]
#[post("/dars/upload")]
pub async fn upload_dars_local(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let Some(pin_instance) = body.pin_instance.as_deref().filter(|s| !s.is_empty()) else {
        return match workflow::contracts::upload_dars(&data.config, &body.dar_files).await {
            Ok(()) => {
                tracing::info!(
                    "Uploaded {} DAR file(s) to local node",
                    body.dar_files.len()
                );
                HttpResponse::Ok().json(SuccessResponse { success: true })
            }
            Err(e) => {
                tracing::error!("Failed to upload DARs to local node: {e}");
                HttpResponse::InternalServerError().json(ErrorResponse {
                    error: format!("Failed to upload DARs: {e}"),
                })
            }
        };
    };

    let files = match dars::decode_dar_files(&body.dar_files) {
        Ok(files) => files,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("{e}"),
            });
        }
    };
    for (filename, bytes) in &files {
        match dars::upload_pinned(&data.onledger, pin_instance, filename, bytes).await {
            Ok(uploaded) => tracing::info!(
                run = %uploaded.run_id,
                file = %uploaded.pin.filename,
                main_package_id = %uploaded.pin.main_package_id,
                "pinned DAR uploaded"
            ),
            Err(e) => {
                let message = format!("{e:#}");
                let status = pinned_upload_status(&message);
                tracing::warn!(error = %message, file = %filename, "pinned DAR upload refused");
                return HttpResponse::build(status).json(ErrorResponse {
                    error: format!("Failed to upload {filename} for run {pin_instance}: {e}"),
                });
            }
        }
    }
    HttpResponse::Ok().json(SuccessResponse { success: true })
}

// ============================================================================
// DARs Distribution Workflow
// ============================================================================

/// Distribute DARs: pin them on a `WorkflowProposal(kind = Dars)` so every
/// invited operator uploads the same files locally (design D8).
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 202, description = "DARs distribution workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request (e.g. empty peer_ids)", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "A peer is not ready for on-ledger coordination", body = ErrorResponse)
    )
)]
#[post("/dars/distribute")]
pub async fn start_dars(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if body.peer_ids.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must contain at least one peer".to_string(),
        });
    }
    if body.dar_files.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "dar_files must contain at least one file".to_string(),
        });
    }
    // The pins carry the hash of these bytes; a file that does not decode
    // fails the run before a proposal exists.
    if let Err(e) = dars::decode_dar_files(&body.dar_files) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("{e}"),
        });
    }

    let instance_name = format!("dars-distribute-{}", now_secs());
    start(
        &data,
        StartRequest::Dars {
            dar_files: body.dar_files.clone(),
            peer_ids: body.peer_ids.clone(),
            instance_name,
        },
        "DARs distribution",
    )
    .await
}

/// Get the current status of the DARs distribution workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "DARs distribution workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/dars/distribute/status")]
pub async fn get_dars_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Dars).await)
}

// ============================================================================
// Cancel
// ============================================================================

/// Cancel the coordinator run of `kind` the legacy per-kind endpoint acts on:
/// the in-progress run with the lowest `instance_name`, the same choice
/// `kind_status` makes.
async fn cancel_workflow_state(
    data: &web::Data<AppState>,
    label: &str,
    kind: WorkflowKind,
) -> HttpResponse {
    let run = match data
        .db
        .get_active_workflow_run(kind, WorkflowRole::Coordinator)
        .await
    {
        Ok(Some(run)) => run,
        Ok(None) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No {label} workflow in progress"),
            });
        }
        Err(e) => {
            tracing::error!("Failed to load the {label} run for cancel: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to cancel {label} workflow (database error) — try again"),
            });
        }
    };
    cancel_run(data, &run.instance_name, label).await
}

/// Cancel one run through the engine (design D10): the row is marked
/// `Cancelled` first, then the coordinator withdraws its `WorkflowProposal`.
async fn cancel_run(data: &web::Data<AppState>, instance_name: &str, label: &str) -> HttpResponse {
    match onledger::cancel_run(&data.onledger, instance_name).await {
        Ok(()) => {
            tracing::info!("{label} workflow {instance_name} cancelled");
            HttpResponse::Ok().json(MessageResponse {
                message: format!("{label} workflow cancelled"),
            })
        }
        Err(e) => {
            let message = format!("{e:#}");
            let status = if message.contains("not in progress") || message.contains("not found") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            tracing::warn!(error = %message, "{label} cancel of {instance_name} refused");
            HttpResponse::build(status).json(ErrorResponse {
                error: format!("Failed to cancel {label} workflow: {e}"),
            })
        }
    }
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/onboarding/cancel")]
pub async fn cancel_onboarding(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Onboarding", WorkflowKind::Onboarding).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/kick/cancel")]
pub async fn cancel_kick(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Kick", WorkflowKind::Kick).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/contracts/cancel")]
pub async fn cancel_contracts(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Contracts", WorkflowKind::Contracts).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/dars/cancel")]
pub async fn cancel_dars(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "DARs", WorkflowKind::Dars).await
}

/// Cancel one specific in-flight coordinator run by its `instance_name`. With
/// concurrent runs of the same kind, this is the only unambiguous cancel — the
/// legacy per-kind `/{kind}/cancel` endpoints pick a run deterministically but
/// cannot target a chosen card.
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No in-flight coordinator run with this instance_name", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/cancel")]
pub async fn cancel_workflow_instance(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();
    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(run)) if run.status == WorkflowProgress::InProgress => run,
        Ok(_) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No in-flight workflow run named {instance_name}"),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load workflow run: {e}"),
            });
        }
    };
    if run.role != WorkflowRole::Coordinator {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Run {instance_name} is not coordinated by this node"),
        });
    }
    cancel_run(&data, &instance_name, run.kind.as_str()).await
}

// ============================================================================
// Generic workflow_runs endpoints (used by the unified notifications feed)
// ============================================================================

/// List every workflow run that should appear in the notifications feed:
/// every InProgress run on this node + any terminal run the operator hasn't
/// dismissed yet. `coordinator_name` is joined from the peers table and
/// `connected_peers` (the invitees that accepted) from the observer's last
/// proposal snapshot.
#[utoipa::path(
    tag = "Workflows",
    responses((status = 200, description = "Visible workflow runs", body = WorkflowRunsResponse))
)]
#[get("/workflows")]
pub async fn list_workflows(data: web::Data<AppState>) -> impl Responder {
    let runs = match data.db.get_visible_workflow_runs().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to list workflow runs: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to list workflow runs: {e}"),
            });
        }
    };

    // Resolve coordinator names from the peers table — same pattern get_invitations uses.
    let names: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.participant_id.to_string(), p.name))
        .collect();

    let mut resolved: Vec<WorkflowRun> = Vec::with_capacity(runs.len());
    for mut r in runs {
        if let Some(coordinator) = r.coordinator_participant.as_deref() {
            r.coordinator_name = names.get(coordinator).cloned();
        }
        enrich_from_config_json(&mut r);
        if let Some(cid) = r.proposal_cid.as_deref() {
            r.connected_peers = data.onledger.accepted_participants(cid).await;
        }
        // The ACS transfer records its progress as an artefact on both sides.
        r.acs_progress = read_acs_progress(&data.db, &r.instance_name).await;
        resolved.push(r);
    }

    HttpResponse::Ok().json(WorkflowRunsResponse { runs: resolved })
}

/// Read the last recorded ACS transfer sample for a run.
///
/// Display only: a missing or unparseable sample means the card shows no
/// transfer line, never an error.
async fn read_acs_progress(db: &SqlitePool, instance_name: &str) -> Option<AcsTransferProgress> {
    let raw = db
        .read_artifact(
            instance_name,
            workflow::storage::artifact_kinds::ADD_PARTY_ACS_PROGRESS,
            None,
        )
        .await
        .ok()
        .flatten()?;
    serde_json::from_slice(&raw).ok()
}

/// List the external parties this participant currently hosts, read from its own
/// Canton topology (an authorized `PartyToParticipant` naming this participant
/// with Confirmation and a self-owned single-key namespace). Wallet-driven
/// onboarding keeps no local run row, so there is nothing DB-side to read.
#[utoipa::path(
    tag = "Workflows",
    responses((status = 200, description = "External parties", body = ExternalPartiesResponse))
)]
#[get("/external-parties")]
pub async fn list_external_parties(data: web::Data<AppState>) -> impl Responder {
    match workflow::external_party::steps::list_hosted_external_parties(&data.config).await {
        Ok(hosted) => {
            let parties = hosted
                .into_iter()
                .map(|p| ExternalPartyInfo {
                    party_id: p.party_id,
                    fingerprint: p.fingerprint,
                    threshold: p.threshold,
                    host_count: p.host_count,
                    created_at: p.created_at,
                    onboarding: p.onboarding,
                    hosts: p
                        .hosts
                        .into_iter()
                        .map(|h| ExternalPartyHost {
                            participant_uid: h.participant_uid,
                            permission: permission_from_proto(h.permission),
                        })
                        .collect(),
                })
                .collect();
            HttpResponse::Ok().json(ExternalPartiesResponse { parties })
        }
        Err(e) => {
            tracing::error!("Failed to list external parties from topology: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to list external parties: {e}"),
            })
        }
    }
}

/// Pull `prefix` + `participants` out of the run's `config_json` and lift
/// them onto the response struct so the frontend can show them without
/// parsing JSON blobs. Coordinator configs spell the prefix field
/// `party_id_prefix` while the peer-side payload uses `prefix`; we accept
/// either. For participants we fall back to `expected_peers` when the config
/// doesn't carry a list of its own.
fn enrich_from_config_json(run: &mut WorkflowRun) {
    // A contract entry in a coordinator-side contracts config. We only need
    // its human-readable `name` for the card's "Packages" row.
    #[derive(serde::Deserialize)]
    struct ContractNameShape {
        #[serde(default)]
        name: String,
    }
    // A DAR entry in a coordinator-side DARs config.
    #[derive(serde::Deserialize)]
    struct DarFileShape {
        #[serde(default)]
        filename: String,
    }
    #[derive(serde::Deserialize)]
    struct ConfigShape {
        #[serde(default)]
        prefix: Option<String>,
        #[serde(default)]
        party_id_prefix: Option<String>,
        #[serde(default)]
        participants: Vec<CantonId>,
        // Kick + AddParty configs only.
        #[serde(default)]
        new_threshold: Option<i32>,
        #[serde(default)]
        previous_threshold: Option<i32>,
        // Onboarding config only: the initial threshold (no previous).
        #[serde(default)]
        threshold: Option<i32>,
        // Kick configs only.
        #[serde(default)]
        participant_id: Option<CantonId>,
        // AddParty configs only.
        #[serde(default)]
        new_participant_id: Option<CantonId>,
        // AddParty + Kick configs: derive a display prefix from the party id.
        #[serde(default)]
        decentralized_party_id: Option<CantonId>,
        // Contracts: `package_names` is the peer's flat list (from the
        // proposal); `contracts[].name` is the coordinator's config.
        #[serde(default)]
        package_names: Vec<String>,
        #[serde(default)]
        contracts: Vec<ContractNameShape>,
        // Dars: `dar_filenames` is the flat list on both sides now;
        // `dar_files[].filename` is the shape legacy rows from before the
        // 2.0 upgrade carry.
        #[serde(default)]
        dar_filenames: Vec<String>,
        #[serde(default)]
        dar_files: Vec<DarFileShape>,
    }
    if let Ok(shape) = serde_json::from_str::<ConfigShape>(&run.config_json) {
        let prefix = shape
            .prefix
            .or(shape.party_id_prefix)
            .or_else(|| shape.decentralized_party_id.map(|p| p.prefix));
        if let Some(p) = prefix
            && !p.is_empty()
        {
            run.prefix = Some(p);
        }
        if !shape.participants.is_empty() {
            run.participants = shape.participants;
        }
        // Onboarding stores the initial threshold under `threshold`; kick /
        // add-party use `new_threshold`. Either populates the card's value.
        run.new_threshold = shape.new_threshold.or(shape.threshold);
        // Only surface a previous threshold when one was known (0 means
        // "unknown", render as new-only).
        run.previous_threshold = shape.previous_threshold.filter(|t| *t > 0);
        run.kicked_participant = shape.participant_id;
        run.added_participant = shape.new_participant_id;
        // Package names: peer's flat list wins; otherwise derive from the
        // coordinator's contract definitions. Both converge to the same set.
        if !shape.package_names.is_empty() {
            run.package_names = shape.package_names;
        } else if !shape.contracts.is_empty() {
            run.package_names = shape
                .contracts
                .into_iter()
                .map(|c| c.name)
                .filter(|n| !n.is_empty())
                .collect();
        }
        // DAR filenames: same flat-list-vs-config convergence.
        if !shape.dar_filenames.is_empty() {
            run.dar_filenames = shape.dar_filenames;
        } else if !shape.dar_files.is_empty() {
            run.dar_filenames = shape
                .dar_files
                .into_iter()
                .map(|d| d.filename)
                .filter(|n| !n.is_empty())
                .collect();
        }
    }
    // Fallback: if config_json didn't expose a participants list, surface the
    // run's `expected_peers` instead so the card still shows who was involved.
    if run.participants.is_empty() && !run.expected_peers.is_empty() {
        run.participants = run.expected_peers.clone();
    }
}

/// Mark a terminal-state workflow run as dismissed so it disappears from the
/// notifications feed. Returns 409 if the run is still InProgress.
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Run dismissed", body = MessageResponse),
        (status = 404, description = "Run not found", body = ErrorResponse),
        (status = 409, description = "Run is still in progress", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/dismiss")]
pub async fn dismiss_workflow(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();

    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("workflow run {instance_name} not found"),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load workflow run: {e}"),
            });
        }
    };

    if run.status == WorkflowProgress::InProgress {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "{} workflow {instance_name} is still in progress — cancel it first",
                run.kind
            ),
        });
    }

    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to begin tx: {e}"),
            });
        }
    };
    if let Err(e) = tx.dismiss_workflow_run(&instance_name).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to dismiss workflow run: {e}"),
        });
    }
    if let Err(e) = Commitable::commit(tx).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to commit dismiss: {e}"),
        });
    }

    HttpResponse::Ok().json(MessageResponse {
        message: format!("workflow {instance_name} dismissed"),
    })
}

/// Retry a Failed coordinator-side workflow run from where it left off
/// (design D10): the row flips back to `inprogress` and the observer's
/// ensure semantics take over. Nothing is broadcast. Peer rows are not
/// retried here: the peer re-issues its co-signature when the coordinator's
/// proposal is back.
#[utoipa::path(
    tag = "Workflows",
    params(
        ("instance_name" = String, Path, description = "Workflow run identifier")
    ),
    responses(
        (status = 200, description = "Retry started", body = MessageResponse),
        (status = 404, description = "Run not found", body = ErrorResponse),
        (status = 409, description = "Run is not in a retryable state", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/retry")]
pub async fn retry_workflow(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();

    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("workflow run {instance_name} not found"),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load workflow run: {e}"),
            });
        }
    };

    if run.role != WorkflowRole::Coordinator {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: "Retry must be initiated from the coordinator side. A peer row follows the \
                    coordinator's proposal — wait for that or dismiss the row."
                .to_string(),
        });
    }
    if run.status != WorkflowProgress::Failed {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Cannot retry a workflow in status {:?}; only Failed runs can be retried",
                run.status
            ),
        });
    }

    match onledger::retry_run(&data.onledger, &instance_name).await {
        Ok(()) => HttpResponse::Ok().json(MessageResponse {
            message: format!(
                "Retrying workflow {instance_name} from step {}",
                run.current_step
            ),
        }),
        Err(e) => {
            let message = format!("{e:#}");
            tracing::warn!(error = %message, "retry of {instance_name} refused");
            HttpResponse::Conflict().json(ErrorResponse {
                error: format!("Cannot retry workflow {instance_name}: {e}"),
            })
        }
    }
}

/// Any in-progress run on this node (either role) already targeting the given
/// decentralized party. Two workflows must not mutate the same party's
/// topology concurrently (two kicks, or a kick racing a contracts
/// deployment). Local-node guard only — two DIFFERENT nodes coordinating
/// conflicting workflows on the same party is a distributed conflict Canton
/// itself surfaces.
async fn find_inprogress_run_for_party(
    db: &SqlitePool,
    party: &CantonId,
) -> Option<(String, WorkflowKind)> {
    SchemaRead::get_in_progress_workflow_runs(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.dec_party_id.as_ref() == Some(party))
        .map(|r| (r.instance_name, r.kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MIGRATOR;

    /// A minimal coordinator run row for `party`, in progress.
    fn in_progress_run(instance: &str, party: &CantonId) -> WorkflowRun {
        WorkflowRun {
            instance_name: instance.to_string(),
            kind: WorkflowKind::Contracts,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "SubmitProposals".to_string(),
            step_index: 0,
            step_total: 1,
            config_json: "{}".to_string(),
            coordinator_participant: None,
            coordinator_party: None,
            proposal_cid: None,
            member_variant: None,
            topology_hashes: Default::default(),
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: Vec::new(),
            completed_peers: Vec::new(),
            connected_peers: Vec::new(),
            acs_progress: None,
            dec_party_id: Some(party.clone()),
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
            created_at: 0,
            updated_at: 0,
        }
    }

    /// The invariant the completion ordering rests on: once a run row is
    /// terminal, the in-flight guard must not see it.
    ///
    /// This is the half that made the ordering bug reachable. `/…/status` reads
    /// the in-memory status while this guard reads the run rows, so marking the
    /// row terminal *after* flipping memory left a window where the API reported
    /// a run finished and refused the next workflow for the same party in the
    /// same breath. The handlers now write the row first; this pins what that
    /// write has to achieve.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completed_run_is_not_in_flight_for_its_party(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let party = pid(7)?;
        let run = in_progress_run("contracts-1", &party);
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;

        // While it runs, a second workflow for the same party is refused — the
        // guard's whole purpose.
        let found = find_inprogress_run_for_party(&pool, &party).await;
        assert_eq!(
            found.map(|(run, _)| run),
            Some("contracts-1".to_string()),
            "an in-progress run must be reported as in flight"
        );

        crate::onledger::engine::complete_run(&pool, &run).await?;

        assert!(
            find_inprogress_run_for_party(&pool, &party).await.is_none(),
            "a completed run must not block the next workflow for its party"
        );
        Ok(())
    }

    /// A failed run must not block the party either: the operator's next move
    /// after reading Failed is to retry, and a guard that still counted it would
    /// refuse that retry.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_is_not_in_flight_for_its_party(pool: SqlitePool) -> anyhow::Result<()> {
        let party = pid(8)?;
        let run = in_progress_run("contracts-2", &party);
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;

        crate::onledger::engine::fail_run(&pool, &run, "boom").await?;

        assert!(
            find_inprogress_run_for_party(&pool, &party).await.is_none(),
            "a failed run must not block the retry it invites"
        );
        Ok(())
    }

    fn pid(tag: u8) -> anyhow::Result<CantonId> {
        let ns = format!("1220{:0>64}", format!("{tag:02x}"));
        CantonId::parse(&format!("validator-{tag}::{ns}"))
    }

    /// The coordinator signs the proposals, so it is a member of the party it
    /// is changing — but it is absent from its own `peers` table. A member set
    /// derived from that list must still count it, or every threshold bound
    /// computed from it is one too low.
    #[test]
    fn party_member_count_counts_this_node_when_the_set_omits_it() -> anyhow::Result<()> {
        let me = pid(1)?;
        let others: HashSet<CantonId> = [pid(2)?, pid(3)?, pid(4)?].into_iter().collect();

        // The shape that caused the bug: three other members cached, self absent.
        assert_eq!(party_member_count(&others, &me), 4);
        Ok(())
    }

    /// When the cache does contain this node — the normal case, since the
    /// refresh writes the chain's participant list verbatim — it must not be
    /// counted twice.
    #[test]
    fn party_member_count_does_not_double_count_a_present_self() -> anyhow::Result<()> {
        let me = pid(1)?;
        let all: HashSet<CantonId> = [me.clone(), pid(2)?, pid(3)?, pid(4)?]
            .into_iter()
            .collect();

        assert_eq!(party_member_count(&all, &me), 4);
        Ok(())
    }

    /// A one-member party is this node alone, whichever way the set was built.
    #[test]
    fn party_member_count_handles_a_lone_member() -> anyhow::Result<()> {
        let me = pid(1)?;
        assert_eq!(party_member_count(&HashSet::new(), &me), 1);
        assert_eq!(
            party_member_count(&[me.clone()].into_iter().collect(), &me),
            1
        );
        Ok(())
    }

    /// The bug in the numbers that produced it: a 4-member party at threshold
    /// 3 could not be kicked, because the bound said 2 while the peers demanded
    /// 3. With self counted the bound is 3 and the kick is expressible.
    #[test]
    fn kick_bound_admits_the_threshold_the_peers_require() -> anyhow::Result<()> {
        let me = pid(1)?;
        let cached_without_self: HashSet<CantonId> =
            [pid(2)?, pid(3)?, pid(4)?].into_iter().collect();

        let bound = party_member_count(&cached_without_self, &me) as i32 - 1;
        assert_eq!(bound, 3, "a 4-member party leaves 3 after a kick");
        assert!(bound >= 3, "threshold 3 must be expressible");
        Ok(())
    }

    /// The design D3 gate: a preflight refusal is a 409 that carries the
    /// engine's message, a missing node identity is a 409 the operator can
    /// fix, and anything else is a 500.
    #[test]
    fn start_errors_map_preflight_and_identity_to_409() -> anyhow::Result<()> {
        let rejected: anyhow::Error = PreflightRejected::peers(vec![(
            pid(2)?,
            "has not vetted the coordination package".into(),
        )])
        .into();
        assert_eq!(start_error_status(&rejected), StatusCode::CONFLICT);
        assert!(rejected.to_string().contains("validator-2"));

        let wrapped = rejected.context("starting the kick");
        assert_eq!(
            start_error_status(&wrapped),
            StatusCode::CONFLICT,
            "a context wrapper must not hide the refusal"
        );

        let no_identity =
            anyhow::anyhow!("node identity not configured; set one with PUT /node-identity first");
        assert_eq!(start_error_status(&no_identity), StatusCode::CONFLICT);

        let duplicate = anyhow::anyhow!("a workflow run named cbtc-creation already exists");
        assert_eq!(start_error_status(&duplicate), StatusCode::CONFLICT);

        let ledger = anyhow::anyhow!("CommandService.submit_and_wait: transport error");
        assert_eq!(
            start_error_status(&ledger),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        Ok(())
    }

    #[test]
    fn pinned_upload_errors_map_to_the_operator_facing_status() {
        assert_eq!(
            pinned_upload_status("no active Dars WorkflowProposal has run id dars-1"),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            pinned_upload_status(
                "run dars-1 invites this node but the invitation is not accepted yet"
            ),
            StatusCode::CONFLICT
        );
        assert_eq!(
            pinned_upload_status("uploaded DAR matches no pin of run dars-1"),
            StatusCode::CONFLICT
        );
        assert_eq!(
            pinned_upload_status("UploadDar: transport error"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

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

    fn test_cid(prefix: &str) -> anyhow::Result<CantonId> {
        let ns = format!("1220{:0>64}", "a");
        CantonId::parse(&format!("{prefix}::{ns}"))
    }

    fn enrich_run(config_json: &str, expected_peers: Vec<CantonId>) -> WorkflowRun {
        WorkflowRun {
            instance_name: "t".to_string(),
            kind: WorkflowKind::Contracts,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "Active".to_string(),
            step_index: 0,
            step_total: 5,
            config_json: config_json.to_string(),
            coordinator_participant: None,
            coordinator_party: None,
            proposal_cid: None,
            member_variant: None,
            topology_hashes: Default::default(),
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers,
            completed_peers: Vec::new(),
            connected_peers: Vec::new(),
            acs_progress: None,
            dec_party_id: None,
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
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn enrich_contracts_coordinator_surfaces_packages() -> anyhow::Result<()> {
        let peers = vec![test_cid("node1")?, test_cid("node2")?];
        let mut run = enrich_run(
            r#"{"contracts":[{"name":"Governance Core"},{"name":"Token Custody"}]}"#,
            peers.clone(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.package_names, vec!["Governance Core", "Token Custody"]);
        // Member set comes from expected_peers when config_json has no list.
        assert_eq!(run.participants, peers);
        Ok(())
    }

    #[test]
    fn enrich_contracts_peer_surfaces_packages_and_participants() -> anyhow::Result<()> {
        let peers = vec![test_cid("node1")?, test_cid("node2")?];
        let peers_json = serde_json::to_string(&peers)?;
        let mut run = enrich_run(
            &format!(r#"{{"package_names":["Governance Core"],"participants":{peers_json}}}"#),
            Vec::new(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.package_names, vec!["Governance Core"]);
        assert_eq!(run.participants, peers);
        Ok(())
    }

    #[test]
    fn enrich_dars_surfaces_filenames_without_dec_party() -> anyhow::Result<()> {
        let mut run = enrich_run(
            r#"{"dar_files":[{"filename":"app.dar"},{"filename":"lib.dar"}]}"#,
            Vec::new(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.dar_filenames, vec!["app.dar", "lib.dar"]);
        assert!(run.dec_party_id.is_none());

        // The on-ledger coordinator row carries the flat list directly.
        let mut run = enrich_run(r#"{"dar_filenames":["app.dar"]}"#, Vec::new());
        enrich_from_config_json(&mut run);
        assert_eq!(run.dar_filenames, vec!["app.dar"]);
        Ok(())
    }
}
