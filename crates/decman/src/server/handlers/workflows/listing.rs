//! Run listing, external parties, dismiss and retry.

use std::collections::HashMap;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use sqlx::SqlitePool;

use super::{
    preflight::send_retry_workflow,
    runs::{count_run_started, now_secs},
};
use crate::{
    canton_id::CantonId,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    server::{
        AppState, external_parties,
        middleware::require_admin,
        respawn_coordinator,
        types::{
            AcsTransferProgress, ErrorResponse, ExternalPartiesResponse, ExternalPartyHost,
            ExternalPartyInfo, MessageResponse, WorkflowKind, WorkflowProgress, WorkflowRole,
            WorkflowRun, WorkflowRunsResponse, permission_from_proto,
        },
    },
    workflow::{self, storage::WorkflowStorage},
};

// ============================================================================
// Generic workflow_runs endpoints (used by the unified notifications feed)
// ============================================================================

/// List every workflow run that should appear in the notifications feed:
/// every InProgress run on this node + any terminal run the operator hasn't
/// dismissed yet. `coordinator_name` is joined from the peers table.
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
    let pubkey_to_name: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.public_key, p.name))
        .collect();

    let mut resolved: Vec<WorkflowRun> = Vec::with_capacity(runs.len());
    for mut r in runs {
        if let Some(pk) = r.coordinator_pubkey.as_deref() {
            r.coordinator_name = pubkey_to_name.get(pk).cloned();
        }
        enrich_from_config_json(&mut r);
        // Who has joined lives only in the live coordinator's `WorkflowState`;
        // the row itself carries no such column. Merge it in so a run parked on
        // WaitingForPeers can show real progress instead of a permanent zero.
        if let Some(active) = data.workflows.route(&r.instance_name) {
            let mut joined: Vec<CantonId> = active.connected_peers().await.into_iter().collect();
            joined.sort();
            r.connected_peers = joined;
            // The source's progress is live in the open export session, so read
            // it straight from memory rather than from a sampled artefact.
            r.acs_progress = active.acs_export_progress().await;
        }
        if r.acs_progress.is_none() {
            // The target has no registry entry — its side of the transfer runs
            // in a peer job — so it records samples as an artefact instead.
            r.acs_progress = read_acs_progress(&data.db, &r.instance_name).await;
        }
        resolved.push(r);
    }

    HttpResponse::Ok().json(WorkflowRunsResponse { runs: resolved })
}

/// Read the target's last recorded ACS transfer sample for a run.
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

/// List the external parties this participant currently hosts, cached from its own
/// Canton topology (an authorized `PartyToParticipant` naming this participant
/// with Confirmation and a self-owned single-key namespace). Wallet-driven
/// onboarding keeps no local run row, so there is nothing DB-side to read.
///
/// The answer comes from a snapshot, and a stale one starts a background scan.
/// Until the first scan finishes, the response carries no `fetched_at`.
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "External parties", body = ExternalPartiesResponse),
        (status = 503, description = "Discovery failed and no earlier result exists", body = ErrorResponse)
    )
)]
#[get("/external-parties")]
pub async fn list_external_parties(data: web::Data<AppState>) -> impl Responder {
    let (snapshot, refreshing, refresh_error) = match external_parties::read(&data) {
        external_parties::View::Pending => {
            return HttpResponse::Ok().json(ExternalPartiesResponse {
                parties: Vec::new(),
                fetched_at: None,
                refreshing: true,
                refresh_error: None,
            });
        }
        external_parties::View::Failed(error) => {
            return HttpResponse::ServiceUnavailable().json(ErrorResponse { error });
        }
        external_parties::View::Ready {
            snapshot,
            refreshing,
            refresh_error,
        } => (snapshot, refreshing, refresh_error),
    };
    let parties = snapshot
        .parties
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
    HttpResponse::Ok().json(ExternalPartiesResponse {
        parties,
        fetched_at: Some(snapshot.fetched_at.to_rfc3339()),
        refreshing,
        refresh_error,
    })
}

/// Pull `prefix` + `participants` out of the run's `config_json` and lift
/// them onto the response struct so the frontend can show them without
/// parsing JSON blobs. Coordinator configs spell the prefix field
/// `party_id_prefix` (e.g. `OnboardingConfig`) while the peer-side payload
/// uses `prefix`; we accept either. For participants we fall back to
/// `expected_peers` when the config doesn't carry a list of its own — that
/// way Kick / Contracts / Dars runs (whose configs don't include a peer
/// list) still surface their participants.
fn enrich_from_config_json(run: &mut WorkflowRun) {
    // A contract entry in a coordinator-side `ContractsConfig`. We only need
    // its human-readable `name` for the card's "Packages" row.
    #[derive(serde::Deserialize)]
    struct ContractNameShape {
        #[serde(default)]
        name: String,
    }
    // A DAR entry in a coordinator-side `DarsConfig`.
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
        #[serde(default)]
        namespace_fingerprint: Option<String>,
        // AddParty configs only.
        #[serde(default)]
        new_participant_id: Option<CantonId>,
        // AddParty + Kick configs: derive a display prefix from the party id.
        #[serde(default)]
        decentralized_party_id: Option<CantonId>,
        // Contracts: `package_names` is the peer's flat list (from the invite);
        // `contracts[].name` is the coordinator's `ContractsConfig`.
        #[serde(default)]
        package_names: Vec<String>,
        #[serde(default)]
        contracts: Vec<ContractNameShape>,
        // Dars: `dar_filenames` is the peer's flat list (from the invite);
        // `dar_files[].filename` is the coordinator's `DarsConfig`.
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
        // Only surface a previous threshold when an older client actually
        // sent one (it defaults to 0); 0 means "unknown", render as new-only.
        run.previous_threshold = shape.previous_threshold.filter(|t| *t > 0);
        run.kicked_participant = shape.participant_id;
        if run.kind == WorkflowKind::Kick {
            run.kicked_owner_key = shape.namespace_fingerprint;
        }
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
        // DAR filenames: same peer-flat-list-vs-coordinator-config convergence.
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
    // Fallback: if config_json didn't expose a participants list (e.g. Kick /
    // Contracts / Dars), surface the run's `expected_peers` instead so the
    // card still shows who was involved.
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

/// Retry a Failed coordinator-side workflow run from where it left off.
///
/// Flips `status` back to `inprogress`, clears `error`, and re-spawns the
/// coordinator task. `NoiseServer::new` re-hydrates `WorkflowState` from the
/// persisted `current_step`, so the run picks up at the same step that
/// failed. Only valid on Failed coordinator-side rows; peer retry is not
/// supported (the coordinator may already be past the config-bearing
/// command — operator should dismiss the peer row and re-accept the
/// invite instead).
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
            error: "Retry must be initiated from the coordinator side. Peer rows flip back \
                    to InProgress automatically when the coordinator retries — wait for that or \
                    dismiss the row."
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

    // Flip the row back to InProgress before re-spawning the task. Concurrent
    // same-kind runs are allowed now (no unique index); the registry insert in
    // respawn_coordinator still rejects a same-instance duplicate.
    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to begin tx: {e}"),
            });
        }
    };
    if let Err(e) = tx
        .set_workflow_run_status(
            &instance_name,
            WorkflowProgress::InProgress,
            None,
            now_secs(),
        )
        .await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to flip status to InProgress: {e}"),
        });
    }
    if let Err(e) = Commitable::commit(tx).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to commit retry status: {e}"),
        });
    }
    count_run_started(run.kind);

    // Re-load the row (now InProgress), broadcast RetryWorkflow to the
    // peer cohort, and re-spawn the coordinator task. respawn_coordinator
    // updates the matching HttpWorkflowState, stashes a new abort handle,
    // and finalizes the row to Completed/Failed when the task ends.
    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        _ => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Workflow run vanished mid-retry".to_string(),
            });
        }
    };
    // Tell every previously-invited peer to flip their Failed row back to
    // InProgress and re-spin start_peer. Best-effort — peers that are
    // unreachable now will stay Failed; operator can dismiss + re-accept.
    if let Err(e) = send_retry_workflow(
        &data.config,
        &data.db,
        &run.expected_peers,
        &run.instance_name,
    )
    .await
    {
        tracing::warn!("Failed to broadcast RetryWorkflow: {e:#}");
    }
    respawn_coordinator(
        data.db.clone(),
        data.config.clone(),
        &run,
        data.workflows.clone(),
        data.auth.clone(),
        data.last_seen.clone(),
    )
    .await;

    HttpResponse::Ok().json(MessageResponse {
        message: format!(
            "Retrying workflow {instance_name} from step {}",
            run.current_step
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{super::test_support::test_cid, *};

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
            coordinator_pubkey: None,
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
            kicked_owner_key: None,
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
        Ok(())
    }

    /// The feed is polled every two seconds, so a run may put nothing
    /// unbounded on the wire. A Dars config carries each DAR's bytes.
    #[test]
    fn dars_run_keeps_its_dar_bytes_off_the_feed() -> anyhow::Result<()> {
        let payload = "A".repeat(4096);
        let mut run = enrich_run(
            &format!(r#"{{"dar_files":[{{"filename":"app.dar","data":"{payload}"}}]}}"#),
            Vec::new(),
        );
        enrich_from_config_json(&mut run);

        let feed = serde_json::to_string(&WorkflowRunsResponse { runs: vec![run] })?;
        assert!(!feed.contains(&payload), "the feed carries the DAR bytes");
        assert!(feed.contains("app.dar"), "the feed lost the DAR filename");

        // The CLI and the IT harness read this response back, so the field has
        // to default in rather than fail as missing.
        let back: WorkflowRunsResponse = serde_json::from_str(&feed)?;
        assert!(back.runs[0].config_json.is_empty());
        Ok(())
    }
}
