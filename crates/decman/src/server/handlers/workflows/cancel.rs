//! Cancelling a workflow run.

use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, Responder, post, web};

use super::{
    preflight::send_cancel_invites,
    runs::{mark_run_cancelled, pick_coordinator_run},
};
use crate::{
    db::schema::SchemaRead,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ErrorResponse, MessageResponse, WorkflowInstance, WorkflowKind, WorkflowProgress,
            WorkflowRole,
        },
    },
};

/// Shared cancel logic for the legacy per-kind endpoints. Picks the coordinator
/// run of `kind` via [`pick_coordinator_run`] (deterministic lowest
/// instance_name), so `/{kind}/status` and `/{kind}/cancel` always act on the
/// same instance. Callers that know which run they mean should use
/// `POST /workflows/{instance}/cancel`. With no in-memory run, falls back to the
/// persisted row (see [`cancel_persisted_run`]) so a stuck run stays cancellable.
pub(super) async fn cancel_workflow_state(
    data: &web::Data<AppState>,
    label: &str,
    kind: WorkflowKind,
) -> HttpResponse {
    if let Some(instance) = pick_coordinator_run(data, kind) {
        return cancel_instance(data, &instance, label).await;
    }
    cancel_persisted_run(data, label, kind).await
}

/// Cancel a run with a persisted `inprogress` row but no in-memory instance
/// (nothing to abort): notify the row's peers, flip it to Cancelled, 409 if none.
async fn cancel_persisted_run(
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
            tracing::error!("Failed to load persisted {label} run for cancel: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to cancel {label} workflow (database error) — try again"),
            });
        }
    };

    if !run.expected_peers.is_empty()
        && let Err(e) = send_cancel_invites(
            &data.config,
            &data.db,
            &run.expected_peers,
            &run.instance_name,
        )
        .await
    {
        tracing::warn!("send_cancel_invites failed during {label} cancel: {e}");
    }

    // On write failure the row stays InProgress (recovery may resume it) — 500.
    match mark_run_cancelled(&data.db, kind, &run.instance_name).await {
        Ok(true) => {}
        // Another write ended the run after it was loaded, and no task is left
        // to stop, so this cancel did nothing.
        Ok(false) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No {label} workflow in progress"),
            });
        }
        Err(e) => {
            tracing::error!(
                "Failed to persist cancel for stuck {label} run {}: {e:#}",
                run.instance_name
            );
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!(
                    "Failed to cancel {label} workflow (database error); the run is still \
                     active — try again"
                ),
            });
        }
    }

    tracing::info!(
        "{label} workflow {} cancelled off the persisted row",
        run.instance_name
    );
    HttpResponse::Ok().json(MessageResponse {
        message: format!("{label} workflow cancelled"),
    })
}

/// Cancel exactly one registered coordinator run: abort its task, notify its
/// invitees with an instance-stamped CancelInvite (sibling runs on the peers
/// survive), and mirror Cancelled onto the persisted row. A row that already
/// records an outcome keeps it, and the task is still stopped.
async fn cancel_instance(
    data: &web::Data<AppState>,
    instance: &Arc<WorkflowInstance>,
    label: &str,
) -> HttpResponse {
    let state = &instance.http;
    {
        let status = state.status.read().await;
        if *status != WorkflowProgress::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No {label} workflow in progress"),
            });
        }
    }

    // Take the abort handle FIRST. If it's None we're racing with a start path that
    // hasn't finished setting itself up yet; refuse the cancel rather than mark the
    // workflow Cancelled while the spawned task is still alive. Start paths always
    // populate abort_handle before they let any await reach this point.
    let Some(handle) = state.abort_handle.lock().await.take() else {
        tracing::warn!(
            "{label} cancel arrived before the workflow finished initializing — refusing"
        );
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("{label} workflow is still initializing — try again in a moment"),
        });
    };

    // Persist Cancelled BEFORE aborting. Recovery resumes every InProgress
    // coordinator row on restart, so if we aborted first and this status write
    // then failed transiently, the row would stay InProgress and
    // respawn_coordinator would re-launch — re-sending invites, re-submitting
    // topology — for a run the operator explicitly cancelled. So on a write
    // failure we DON'T abort: restore the handle so the run stays cancellable and
    // live, and surface 500 so the operator retries against consistent state.
    // Once the row is durably Cancelled, a crash before the abort is safe —
    // recovery only resumes InProgress rows.
    match mark_run_cancelled(&data.db, instance.kind, &instance.instance_name).await {
        Ok(true) => {}
        // The row already records an outcome, which it keeps, but the task can
        // still be alive: a declined run idles in WaitingForPeers. Operators reap
        // that task with this cancel, so it stops the task and answers like a
        // normal cancel. Its peers either got the decline's CancelInvite or
        // finished, so none is sent.
        Ok(false) => {
            handle.abort();
            tracing::info!(
                "{label} workflow {} had already ended; stopped its task",
                instance.instance_name
            );
            return HttpResponse::Ok().json(MessageResponse {
                message: format!("{label} workflow had already ended; its task is stopped"),
            });
        }
        Err(e) => {
            tracing::error!(
                "Failed to persist cancel for {}: {e:#}; leaving the run InProgress and \
                 active rather than aborting a run we can't durably mark cancelled",
                instance.instance_name
            );
            *state.abort_handle.lock().await = Some(handle);
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!(
                    "Failed to cancel {label} workflow (database error); the run is still \
                     active — try again"
                ),
            });
        }
    }

    // Durably Cancelled — now stop the task and notify invitees.
    // NOTE: an abort landing in the narrow window between the workflow task's
    // Ok(result) and its mark_completed write can leave the row Cancelled for a
    // run that de-facto finished — rare and benign (the work itself is already
    // committed on-ledger; only the feed status differs).
    handle.abort();

    let invitees = state.invited_peers.read().await.clone();
    if !invitees.is_empty()
        && let Err(e) =
            send_cancel_invites(&data.config, &data.db, &invitees, &instance.instance_name).await
    {
        tracing::warn!("send_cancel_invites failed during {label} cancel: {e}");
    }

    {
        let mut status = state.status.write().await;
        *status = WorkflowProgress::Cancelled;
    }
    {
        let mut error = state.error.write().await;
        *error = None;
    }

    tracing::info!("{label} workflow {} cancelled", instance.instance_name);
    HttpResponse::Ok().json(MessageResponse {
        message: format!("{label} workflow cancelled"),
    })
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
        (status = 409, description = "No in-flight run with this instance_name", body = ErrorResponse)
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
    let Some(instance) = data.workflows.get(&instance_name) else {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("No in-flight workflow run named {instance_name}"),
        });
    };
    if instance.role != WorkflowRole::Coordinator {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Run {instance_name} is not coordinated by this node"),
        });
    }
    let label = instance.kind.as_str();
    cancel_instance(&data, &instance, label).await
}
