//! Workflow run rows: persistence, status lookups and run metrics.

use std::{
    sync::{Arc, LazyLock},
    time::{SystemTime, UNIX_EPOCH},
};

use actix_web::web;

use prometheus::IntCounterVec;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    server::{
        AppState,
        error::ApiError,
        types::{
            WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun,
            WorkflowStatusResponse,
        },
    },
    workflow::state::WorkflowStep,
};

// ============================================================================
// Workflow State Types
// ============================================================================

// Per-kind workflow state is now tracked per-instance in
// `AppState.workflows` (`WorkflowRegistry`) rather than via singleton
// `HttpWorkflowState<S>` aliases.

// ============================================================================
// Workflow run persistence helpers
// ============================================================================

pub(super) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Insert the coordinator-side `workflow_runs` row for a freshly-started run.
/// Duplicate-instance protection lives in the registry insert that precedes
/// this call (migration 000013 dropped the one-InProgress-per-(kind, role)
/// unique index — concurrent same-kind runs are allowed); a failure here is a
/// genuine persistence error and bubbles back as 409.
async fn insert_coordinator_run<S, C>(
    db: &SqlitePool,
    instance_name: &str,
    kind: WorkflowKind,
    initial_step: S,
    config: &C,
    invitees: &[CantonId],
    dec_party_id: Option<CantonId>,
) -> Result
where
    S: WorkflowStep,
    C: serde::Serialize,
{
    let now = now_secs();
    let run = WorkflowRun {
        instance_name: instance_name.to_string(),
        kind,
        role: WorkflowRole::Coordinator,
        status: WorkflowProgress::InProgress,
        current_step: initial_step.step_name().to_string(),
        step_index: initial_step.step_index(),
        step_total: S::step_total(),
        config_json: serde_json::to_string(config)
            .map_err(|e| anyhow::anyhow!("encode workflow config: {e}"))?,
        coordinator_pubkey: None,
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: invitees.to_vec(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        dec_party_id,
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
        created_at: now,
        updated_at: now,
    };

    let mut tx = db.begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    Commitable::commit(tx).await
}

/// Register a freshly-built coordinator run in the shared registry and persist
/// its `workflow_runs` row — the register-or-409 / persist-or-unregister
/// contract every `start_*` handler needs, kept in one place so a change to it
/// can't drift between the four handlers.
///
/// The registry insert is the dedup gate: a duplicate `instance_name` is
/// rejected here BEFORE we persist, so a racing same-instance request can't
/// upsert (and clobber) the in-flight run's row. If the persist then fails the
/// registry entry is removed, so a run that never started can't leak and block
/// future starts. On either failure returns the 409 the handler should answer
/// with; on success the run is both registered and persisted.
pub(super) async fn register_and_persist<S, C>(
    data: &web::Data<AppState>,
    instance: &Arc<WorkflowInstance>,
    initial_step: S,
    config: &C,
    invitees: &[CantonId],
    dec_party_id: Option<CantonId>,
) -> std::result::Result<(), ApiError>
where
    S: WorkflowStep,
    C: serde::Serialize,
{
    let kind = instance.kind;
    let instance_name = &instance.instance_name;
    let label = match kind {
        WorkflowKind::Onboarding => "onboarding",
        WorkflowKind::Kick => "kick",
        WorkflowKind::Contracts => "contracts",
        WorkflowKind::Dars => "DARs",
        WorkflowKind::AddParty => "add-party",
        WorkflowKind::ChangeThreshold => "change-threshold",
    };

    if !data.workflows.insert(instance.clone()) {
        return Err(ApiError::Conflict(format!(
            "A workflow with instance {instance_name} is already running"
        )));
    }

    if let Err(e) = insert_coordinator_run(
        &data.db,
        instance_name,
        kind,
        initial_step,
        config,
        invitees,
        dec_party_id,
    )
    .await
    {
        data.workflows.remove(instance_name);
        tracing::warn!("Failed to persist {label} workflow run: {e:#}");
        return Err(ApiError::Conflict(format!(
            "Failed to start {label} workflow: {e}"
        )));
    }

    count_run_started(kind);
    Ok(())
}

/// Flip the persisted run's status to Completed. Errors are logged and
/// swallowed — the spawned task can't usefully react to a DB error here.
pub(crate) async fn mark_run_completed(db: &SqlitePool, kind: WorkflowKind, instance_name: &str) {
    match finish_run(db, instance_name, WorkflowProgress::Completed, None).await {
        Ok(true) => count_run_finished(kind, RunOutcome::Completed),
        Ok(false) => {}
        Err(e) => tracing::warn!("Failed to mark run {instance_name} completed: {e:#}"),
    }
}

/// Flip the persisted run's status to Failed with a message.
pub(crate) async fn mark_run_failed(
    db: &SqlitePool,
    kind: WorkflowKind,
    instance_name: &str,
    error: &str,
) {
    match finish_run(db, instance_name, WorkflowProgress::Failed, Some(error)).await {
        Ok(true) => count_run_finished(kind, RunOutcome::Failed),
        Ok(false) => {}
        Err(e) => tracing::warn!("Failed to mark run {instance_name} failed: {e:#}"),
    }
}

/// Flip the persisted run's status to Cancelled. `Ok(false)` means the run
/// had already ended, so nothing was cancelled.
pub(super) async fn mark_run_cancelled(
    db: &SqlitePool,
    kind: WorkflowKind,
    instance_name: &str,
) -> Result<bool> {
    let cancelled = finish_run(db, instance_name, WorkflowProgress::Cancelled, None).await?;
    if cancelled {
        count_run_finished(kind, RunOutcome::Cancelled);
    }
    Ok(cancelled)
}

/// Record a terminal status on a run that has not ended yet. The first terminal
/// write wins, so a run counts under one outcome only. A failed write counts
/// nothing and leaves the row in progress, which recovery resumes on restart.
async fn finish_run(
    db: &SqlitePool,
    instance_name: &str,
    status: WorkflowProgress,
    error: Option<&str>,
) -> Result<bool> {
    let mut tx = db.begin_transaction().await?;
    let finished = tx
        .finish_workflow_run(instance_name, status, error, now_secs())
        .await?;
    Commitable::commit(tx).await?;
    if !finished {
        tracing::warn!(
            instance_name,
            %status,
            "run already has a terminal status; this status was not recorded"
        );
    }
    Ok(finished)
}

// ============================================================================
// Workflow run metrics
// ============================================================================

// Coordinator runs only. A peer row can read Cancelled and then Failed for the
// same run, so peer runs have no single point that sees one outcome per run.
// A peer's decline records Failed through `mark_run_failed`, so the run's later
// end records and counts nothing.

static RUNS_STARTED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_workflow_runs_started_total",
        "Workflow runs this node started as coordinator. A retry counts as a new start. A run resumed after a restart does not.",
        &["kind"]
    )
    .expect("metric name is a unique literal")
});

static RUNS_FINISHED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_workflow_runs_finished_total",
        "Workflow runs this node coordinated that ended, by outcome: completed, failed or cancelled.",
        &["kind", "outcome"]
    )
    .expect("metric name is a unique literal")
});

/// Every kind, for exporting each series at zero. Extend it with
/// [`run_kind_label`], whose match fails to compile for a new kind.
const RUN_KINDS: [WorkflowKind; 6] = [
    WorkflowKind::Onboarding,
    WorkflowKind::Kick,
    WorkflowKind::Contracts,
    WorkflowKind::Dars,
    WorkflowKind::AddParty,
    WorkflowKind::ChangeThreshold,
];

/// The `kind` label, fixed per variant so the series stay bounded.
fn run_kind_label(kind: WorkflowKind) -> &'static str {
    match kind {
        WorkflowKind::Onboarding => "onboarding",
        WorkflowKind::Kick => "kick",
        WorkflowKind::Contracts => "contracts",
        WorkflowKind::Dars => "dars",
        WorkflowKind::AddParty => "add_party",
        WorkflowKind::ChangeThreshold => "change_threshold",
    }
}

#[derive(Clone, Copy)]
enum RunOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl RunOutcome {
    const ALL: [Self; 3] = [Self::Completed, Self::Failed, Self::Cancelled];

    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

pub(super) fn count_run_started(kind: WorkflowKind) {
    RUNS_STARTED
        .with_label_values(&[run_kind_label(kind)])
        .inc();
}

fn count_run_finished(kind: WorkflowKind, outcome: RunOutcome) {
    RUNS_FINISHED
        .with_label_values(&[run_kind_label(kind), outcome.label()])
        .inc();
}

/// Registers both families at startup and exports every kind and outcome at
/// zero, so a dashboard reads zero rather than no data before the first run.
pub(crate) fn register_run_metrics() {
    for kind in RUN_KINDS {
        RUNS_STARTED
            .with_label_values(&[run_kind_label(kind)])
            .inc_by(0);
        for outcome in RunOutcome::ALL {
            RUNS_FINISHED
                .with_label_values(&[run_kind_label(kind), outcome.label()])
                .inc_by(0);
        }
    }
}

/// Pick the coordinator run of `kind` that the legacy per-kind `/{kind}/status`
/// and `/{kind}/cancel` endpoints act on. `snapshot()` is HashMap-ordered, so we
/// sort by `instance_name` and take the lowest — a stable choice so repeated
/// polls and cancels don't flip between concurrent same-kind runs from call to
/// call. Callers that mean a specific run use the per-instance endpoints. Shared
/// by `kind_status` and `cancel_workflow_state` so both always pick the same
/// instance.
pub(super) fn pick_coordinator_run(
    data: &web::Data<AppState>,
    kind: WorkflowKind,
) -> Option<Arc<WorkflowInstance>> {
    let mut runs: Vec<_> = data
        .workflows
        .snapshot()
        .into_iter()
        .filter(|i| i.kind == kind && i.role == WorkflowRole::Coordinator)
        .collect();
    runs.sort_by(|a, b| a.instance_name.cmp(&b.instance_name));
    runs.into_iter().next()
}

/// Summarize the status of a coordinator run of `kind` for the legacy
/// per-kind `/{kind}/status` endpoints.
///
/// While a run is live it is in the registry, so report its in-memory status.
/// But the registry entry is removed the moment a run reaches a terminal
/// status (its `WorkflowGuard` drops), so once finished there is nothing in the
/// registry — fall back to the **latest persisted `workflow_runs` row** of this
/// kind, which carries the terminal Completed/Failed/Cancelled status (and is
/// the same source the `/workflows` feed uses). Without this fallback a poller
/// watching `/{kind}/status` would never observe completion. With concurrent
/// runs of one kind these endpoints are necessarily coarse — callers wanting
/// per-instance detail should use `GET /workflows`.
pub(super) async fn kind_status(
    data: &web::Data<AppState>,
    kind: WorkflowKind,
) -> WorkflowStatusResponse {
    if let Some(inst) = pick_coordinator_run(data, kind) {
        return WorkflowStatusResponse {
            status: *inst.http.status.read().await,
            error: inst.http.error.read().await.clone(),
        };
    }
    // No live run registered — report the latest persisted coordinator run of
    // this kind (terminal status survives in the DB after deregistration).
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{admin, in_progress_run, pid};
    use super::super::{cancel::cancel_workflow_instance, listing::retry_workflow};
    use super::*;
    use crate::workflow::{self, AddPartyStep, OnboardingStep};
    use crate::{db::MIGRATOR, server::AppState};
    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};
    use std::collections::HashSet;

    fn started(kind: WorkflowKind) -> u64 {
        RUNS_STARTED
            .with_label_values(&[run_kind_label(kind)])
            .get()
    }

    fn finished(kind: WorkflowKind, outcome: RunOutcome) -> u64 {
        RUNS_FINISHED
            .with_label_values(&[run_kind_label(kind), outcome.label()])
            .get()
    }

    async fn insert_run(pool: &SqlitePool, run: &WorkflowRun) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(run).await?;
        Commitable::commit(tx).await
    }

    /// `AppState::for_test` on a migrated database.
    async fn state_on(pool: SqlitePool) -> Result<web::Data<AppState>> {
        let state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
        Ok(web::Data::new(AppState { db: pool, ..state }))
    }

    // The tests below share one process-wide metrics registry and run in
    // parallel, so each owns the kind and outcome it asserts on.

    #[test]
    fn each_kind_has_a_fixed_snake_case_label() {
        let labels: Vec<&str> = RUN_KINDS.into_iter().map(run_kind_label).collect();
        assert_eq!(
            labels,
            [
                "onboarding",
                "kick",
                "contracts",
                "dars",
                "add_party",
                "change_threshold"
            ]
        );
    }

    #[test]
    fn every_kind_and_outcome_is_exported_before_the_first_run() {
        register_run_metrics();
        let families = prometheus::gather();
        let series = |name: &str| -> HashSet<Vec<String>> {
            families
                .iter()
                .filter(|f| f.name() == name)
                .flat_map(|f| f.get_metric())
                .map(|m| {
                    m.get_label()
                        .iter()
                        .map(|l| l.value().to_string())
                        .collect()
                })
                .collect()
        };
        let started_series = series("decman_workflow_runs_started_total");
        let finished_series = series("decman_workflow_runs_finished_total");

        for kind in RUN_KINDS.map(run_kind_label) {
            assert!(
                started_series.contains(&vec![kind.to_string()]),
                "no started series for {kind}"
            );
            for outcome in RunOutcome::ALL.map(RunOutcome::label) {
                assert!(
                    finished_series.contains(&vec![kind.to_string(), outcome.to_string()]),
                    "no finished series for {kind} {outcome}"
                );
            }
        }
    }

    /// A duplicate instance is refused with 409, so only the first registration
    /// starts a run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_registered_run_counts_as_started_once(pool: SqlitePool) -> Result {
        let data = state_on(pool).await?;
        let instance = WorkflowInstance::new(
            "onboarding-1".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let before = started(WorkflowKind::Onboarding);

        let config = serde_json::json!({});
        let register = || {
            register_and_persist(
                &data,
                &instance,
                OnboardingStep::WaitingForPeers,
                &config,
                &[],
                None,
            )
        };
        let first = register().await;
        let again = register().await;

        assert!(first.is_ok());
        assert!(again.is_err(), "a duplicate instance must be refused");
        assert_eq!(started(WorkflowKind::Onboarding), before + 1);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completed_run_counts_once(pool: SqlitePool) -> Result {
        insert_run(&pool, &in_progress_run("kick-1", &pid(12)?)).await?;
        let before = finished(WorkflowKind::Kick, RunOutcome::Completed);

        mark_run_completed(&pool, WorkflowKind::Kick, "kick-1").await;

        assert_eq!(
            finished(WorkflowKind::Kick, RunOutcome::Completed),
            before + 1
        );
        Ok(())
    }

    /// A failed write leaves the row in progress, so the outcome is not counted
    /// yet. A database with no tables makes the write fail.
    #[tokio::test]
    async fn a_failed_status_write_counts_nothing() -> Result {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let before = finished(WorkflowKind::Dars, RunOutcome::Failed);

        mark_run_failed(&pool, WorkflowKind::Dars, "dars-1", "boom").await;

        assert_eq!(finished(WorkflowKind::Dars, RunOutcome::Failed), before);
        Ok(())
    }

    async fn status_of(pool: &SqlitePool, instance: &str) -> Result<Option<WorkflowProgress>> {
        Ok(pool.get_workflow_run(instance).await?.map(|run| run.status))
    }

    /// A completion that lands after a cancel loses: the row stays Cancelled
    /// and the run counts as cancelled only.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cancel_that_lands_first_wins(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::Onboarding;
        insert_run(&pool, &in_progress_run("onboarding-2", &pid(15)?)).await?;
        let cancelled = finished(kind, RunOutcome::Cancelled);
        let completed = finished(kind, RunOutcome::Completed);
        let failed = finished(kind, RunOutcome::Failed);

        assert!(mark_run_cancelled(&pool, kind, "onboarding-2").await?);
        mark_run_completed(&pool, kind, "onboarding-2").await;
        mark_run_failed(&pool, kind, "onboarding-2", "late").await;

        assert_eq!(
            status_of(&pool, "onboarding-2").await?,
            Some(WorkflowProgress::Cancelled)
        );
        assert_eq!(finished(kind, RunOutcome::Cancelled), cancelled + 1);
        assert_eq!(finished(kind, RunOutcome::Completed), completed);
        assert_eq!(finished(kind, RunOutcome::Failed), failed);
        Ok(())
    }

    /// A cancel that lands after the run completed loses: the row stays
    /// Completed and the run counts as completed only.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completion_that_lands_first_wins(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::Dars;
        insert_run(&pool, &in_progress_run("dars-2", &pid(16)?)).await?;
        let completed = finished(kind, RunOutcome::Completed);
        let cancelled = finished(kind, RunOutcome::Cancelled);

        mark_run_completed(&pool, kind, "dars-2").await;
        let cancel_won = mark_run_cancelled(&pool, kind, "dars-2").await?;

        assert!(!cancel_won, "a cancel after completion must not win");
        assert_eq!(
            status_of(&pool, "dars-2").await?,
            Some(WorkflowProgress::Completed)
        );
        assert_eq!(finished(kind, RunOutcome::Completed), completed + 1);
        assert_eq!(finished(kind, RunOutcome::Cancelled), cancelled);
        Ok(())
    }

    /// A failed cancel write leaves the run live, so only a written cancel
    /// counts.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cancel_counts_only_once_it_is_written(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::ChangeThreshold;
        insert_run(&pool, &in_progress_run("change-threshold-2", &pid(13)?)).await?;
        let before = finished(kind, RunOutcome::Cancelled);
        assert!(mark_run_cancelled(&pool, kind, "change-threshold-2").await?);
        assert_eq!(finished(kind, RunOutcome::Cancelled), before + 1);

        let no_tables = SqlitePool::connect("sqlite::memory:").await?;
        let before = finished(WorkflowKind::Kick, RunOutcome::Cancelled);
        assert!(
            mark_run_cancelled(&no_tables, WorkflowKind::Kick, "kick-2")
                .await
                .is_err()
        );
        assert_eq!(finished(WorkflowKind::Kick, RunOutcome::Cancelled), before);
        Ok(())
    }

    /// A retry puts a Failed run back in progress, so it counts as a new start.
    /// This run's config does not parse, so its resume fails at once, through
    /// the same counted helper as every other coordinator run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_retry_counts_as_a_new_start(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::ChangeThreshold;
        let run = WorkflowRun {
            kind,
            status: WorkflowProgress::Failed,
            config_json: "not json".to_string(),
            ..in_progress_run("change-threshold-1", &pid(14)?)
        };
        insert_run(&pool, &run).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(state_on(pool).await?)
                .service(retry_workflow),
        )
        .await;
        let started_before = started(kind);
        let failed_before = finished(kind, RunOutcome::Failed);

        let request = actix_test::TestRequest::post()
            .uri("/workflows/change-threshold-1/retry")
            .to_request();
        request.extensions_mut().insert(admin());
        let response = actix_test::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(started(kind), started_before + 1);
        assert_eq!(finished(kind, RunOutcome::Failed), failed_before + 1);
        Ok(())
    }

    /// POSTs the per-instance cancel for a live coordinator run. Its in-memory
    /// status reads InProgress and its task never ends on its own, like a
    /// declined run idling in WaitingForPeers. Returns the status and the task.
    async fn cancel_live_run(
        pool: SqlitePool,
        instance: &str,
        kind: WorkflowKind,
    ) -> Result<(StatusCode, tokio::task::JoinHandle<()>)> {
        let data = state_on(pool).await?;
        let run = WorkflowInstance::new(instance.to_string(), kind, WorkflowRole::Coordinator);
        *run.http.status.write().await = WorkflowProgress::InProgress;
        let task = tokio::spawn(std::future::pending::<()>());
        *run.http.abort_handle.lock().await = Some(task.abort_handle());
        anyhow::ensure!(
            data.workflows.insert(run),
            "{instance} is already registered"
        );
        let app =
            actix_test::init_service(App::new().app_data(data).service(cancel_workflow_instance))
                .await;
        let request = actix_test::TestRequest::post()
            .uri(&format!("/workflows/{instance}/cancel"))
            .to_request();
        request.extensions_mut().insert(admin());
        Ok((actix_test::call_service(&app, request).await.status(), task))
    }

    /// A run whose own outcome landed first keeps it. The cancel still stops
    /// the run's task and answers like a normal cancel.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn cancelling_a_finished_run_stops_its_task_and_keeps_its_outcome(
        pool: SqlitePool,
    ) -> Result {
        let kind = WorkflowKind::Contracts;
        let run = WorkflowRun {
            status: WorkflowProgress::Completed,
            ..in_progress_run("contracts-3", &pid(17)?)
        };
        insert_run(&pool, &run).await?;
        let before = finished(kind, RunOutcome::Cancelled);

        let (status, task) = cancel_live_run(pool.clone(), "contracts-3", kind).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            status_of(&pool, "contracts-3").await?,
            Some(WorkflowProgress::Completed)
        );
        assert_eq!(finished(kind, RunOutcome::Cancelled), before);
        assert!(task.await.is_err_and(|e| e.is_cancelled()));
        Ok(())
    }

    /// A peer's decline records Failed while the run's task idles. The
    /// operator's cancel then reaps the task: it answers 200, keeps the row
    /// Failed and counts nothing more.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cancel_after_a_decline_reaps_the_task_and_keeps_failed(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::AddParty;
        let run = WorkflowRun {
            kind,
            ..in_progress_run("add-party-2", &pid(18)?)
        };
        insert_run(&pool, &run).await?;
        let failed = finished(kind, RunOutcome::Failed);
        let cancelled = finished(kind, RunOutcome::Cancelled);

        let state = workflow::state::WorkflowState::new(
            pool.clone(),
            "add-party-2".to_string(),
            AddPartyStep::WaitingForPeers,
            Vec::new(),
            None,
        );
        state.mark_failed("Peer p3 declined the invitation").await;
        assert_eq!(
            status_of(&pool, "add-party-2").await?,
            Some(WorkflowProgress::Failed)
        );
        assert_eq!(finished(kind, RunOutcome::Failed), failed + 1);

        let (status, task) = cancel_live_run(pool.clone(), "add-party-2", kind).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            status_of(&pool, "add-party-2").await?,
            Some(WorkflowProgress::Failed)
        );
        assert_eq!(finished(kind, RunOutcome::Failed), failed + 1);
        assert_eq!(finished(kind, RunOutcome::Cancelled), cancelled);
        assert!(task.await.is_err_and(|e| e.is_cancelled()));
        Ok(())
    }
}
