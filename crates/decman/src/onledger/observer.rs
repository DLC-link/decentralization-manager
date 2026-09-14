//! The observer loop (design D5, D11): the one background task of on-ledger
//! coordination.
//!
//! Every `DECPM_OBSERVER_POLL_SECS` the loop refreshes the registry, keeps
//! this node's `DecmanNode` current, reads every coordination contract once,
//! projects invitations, and drives every in-progress run through its
//! [`KindDriver`](super::engine::KindDriver). Every minute it also refreshes
//! the unsolicited-proposal list and archives this node's stale acceptances
//! and declines.
//!
//! The loop never panics. A stage that fails is logged and counted, and the
//! tick moves on; the next tick retries. Ticks do not overlap
//! (`MissedTickBehavior::Skip`), and a per-run `try_lock` keeps any other
//! caller from driving the same run at the same time.

use std::{
    collections::HashSet,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use futures::future::join_all;
use prometheus::{Gauge, Histogram, IntCounter, IntCounterVec};
use tokio::{sync::Mutex, task::JoinHandle};

use crate::{consts, db::schema::SchemaRead, utils};

use super::{
    OnLedger,
    engine::{self, Driven, ProposalSnapshot, TickCtx},
    now_micros, proposals, topology,
};

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

static TICKS: LazyLock<IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(
        "decman_observer_tick_total",
        "Observer ticks started, one per poll interval while the node lives."
    )
    .expect("metric name is a unique literal")
});

static TICK_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
    prometheus::register_histogram!(
        "decman_observer_tick_seconds",
        "Wall-clock duration of one observer tick."
    )
    .expect("metric name is a unique literal")
});

static LAST_TICK_SECONDS: LazyLock<Gauge> = LazyLock::new(|| {
    prometheus::register_gauge!(
        "decman_observer_last_tick_seconds",
        "Wall-clock duration of the most recent observer tick."
    )
    .expect("metric name is a unique literal")
});

static RUNS_DRIVEN: LazyLock<IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(
        "decman_observer_runs_driven_total",
        "In-progress workflow runs handed to a kind driver."
    )
    .expect("metric name is a unique literal")
});

static ERRORS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_observer_errors_total",
        "Observer stages that ended in an error, by stage.",
        &["stage"]
    )
    .expect("metric name is a unique literal")
});

static NO_IDENTITY: LazyLock<IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(
        "decman_observer_no_identity_total",
        "Observer ticks skipped because no node identity is configured."
    )
    .expect("metric name is a unique literal")
});

/// Register every family at startup, so a family exists before its first
/// event.
pub(crate) fn register_metrics() {
    LazyLock::force(&TICKS);
    LazyLock::force(&TICK_SECONDS);
    LazyLock::force(&LAST_TICK_SECONDS);
    LazyLock::force(&RUNS_DRIVEN);
    LazyLock::force(&ERRORS);
    LazyLock::force(&NO_IDENTITY);
    for stage in [
        "identity",
        "registry",
        "runs",
        "proposals",
        "projection",
        "synchronizer",
        "drive",
        "unsolicited",
        "archive",
    ] {
        ERRORS.with_label_values(&[stage]);
    }
}

/// Log and count a failed stage, returning its value when it succeeded.
fn note<T>(stage: &'static str, result: Result<T>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            ERRORS.with_label_values(&[stage]).inc();
            tracing::warn!(stage, error = %format!("{e:#}"), "observer stage failed");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Loop
// ---------------------------------------------------------------------------

/// Start the observer loop on the runtime and return its handle. The loop
/// runs until the process exits.
pub fn spawn_observer(ol: Arc<OnLedger>) -> JoinHandle<()> {
    register_metrics();
    tokio::spawn(run_observer_loop(ol))
}

/// Cross-tick state.
#[derive(Debug, Default)]
pub struct ObserverState {
    last_unsolicited_scan: Option<Instant>,
}

/// The loop body of [`spawn_observer`]; public so a test harness can run it
/// inline.
pub async fn run_observer_loop(ol: Arc<OnLedger>) {
    let poll = Duration::from_secs(consts::observer_poll_secs());
    let mut interval = tokio::time::interval(poll);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut state = ObserverState::default();
    tracing::info!(poll_secs = poll.as_secs(), "observer loop started");
    loop {
        interval.tick().await;
        TICKS.inc();
        let started = Instant::now();
        tick(&ol, &mut state).await;
        let elapsed = started.elapsed().as_secs_f64();
        TICK_SECONDS.observe(elapsed);
        LAST_TICK_SECONDS.set(elapsed);
    }
}

/// What one tick did, for logs and tests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TickReport {
    pub had_identity: bool,
    pub runs_seen: usize,
    pub runs_driven: usize,
    pub runs_busy: usize,
    pub runs_stopped: usize,
    pub invitations: usize,
}

/// One observer tick. Never panics; every stage is logged and counted.
pub async fn tick(ol: &OnLedger, state: &mut ObserverState) -> TickReport {
    let mut report = TickReport::default();

    // 1. Identity. Without one there is nothing to sign with; re-load it
    //    each tick so `PUT /node-identity` takes effect without a restart.
    let identity = match ol.identity().await {
        Some(i) => i,
        None => match note("identity", ol.reload_identity().await) {
            Some(Some(i)) => i,
            _ => {
                NO_IDENTITY.inc();
                tracing::debug!("observer idle: no node identity configured");
                return report;
            }
        },
    };
    report.had_identity = true;
    let Some(client) = note("identity", ol.client().await) else {
        return report;
    };
    let db = ol.db();

    // 2. Registry: snapshot for the status handlers and the preflight gate,
    //    own entry current, heartbeat when due.
    note("registry", ol.refresh_registry().await);
    if let Some(outcome) = note("registry", ol.publish_registry_entry().await) {
        match outcome {
            super::PublishOutcome::Created(cid) => {
                tracing::info!(contract_id = %cid, "published DecmanNode")
            }
            super::PublishOutcome::Updated(cid) => {
                tracing::info!(contract_id = %cid, "updated DecmanNode")
            }
            super::PublishOutcome::Unchanged(_) => {}
        }
    }
    if let Some(Some(cid)) = note("registry", ol.heartbeat_if_due().await) {
        tracing::debug!(contract_id = %cid, "heartbeat sent");
    }
    let registry = ol.registry_snapshot().await;

    // 3. Runs first, then the ledger: a row exists only after its proposal
    //    was committed, so a proposal missing from a later snapshot is a
    //    real loss and not a race.
    let Some(runs) = note("runs", db.get_in_progress_workflow_runs().await) else {
        return report;
    };
    report.runs_seen = runs.len();
    let Some(snapshot) = note("proposals", ProposalSnapshot::read(&client, db).await) else {
        return report;
    };

    // 4. Invitations: undecided proposals that name this node.
    let now = now_micros();
    if let Some(peers) = note("projection", db.get_all_peers().await) {
        let for_me = snapshot.for_me(client.node_party());
        if let Some(list) = note(
            "projection",
            proposals::project_pending_invitations(db, &for_me, &snapshot.decisions, &peers, now)
                .await,
        ) {
            report.invitations = list.len();
            ol.set_pending_invitations(list).await;
        }
    }

    // 5. Drive every on-ledger run, at most one driver per run at a time.
    let Some(sync_id) = note(
        "synchronizer",
        utils::get_synchronizer_id(ol.config()).await,
    ) else {
        return report;
    };
    let ctx = TickCtx {
        ol,
        client: &client,
        identity: &identity,
        sync_id,
        participant_id: identity.participant_id.clone(),
        proposals: &snapshot,
        registry: &registry,
        now_micros: now,
    };
    let mut locked = Vec::with_capacity(runs.len());
    for run in &runs {
        if engine::read_run_meta(run).is_none() {
            continue;
        }
        locked.push((run, ol.run_lock(&run.instance_name).await));
    }
    let outcomes = join_all(
        locked
            .iter()
            .map(|(run, lock)| drive_one(&ctx, run, lock.clone())),
    )
    .await;
    for outcome in outcomes {
        match outcome {
            None => report.runs_busy += 1,
            Some(Ok(Driven::Ticked)) => {
                report.runs_driven += 1;
                RUNS_DRIVEN.inc();
            }
            Some(Ok(Driven::Stopped)) => report.runs_stopped += 1,
            Some(Ok(Driven::Skipped)) => {}
            Some(Err(e)) => {
                ERRORS.with_label_values(&["drive"]).inc();
                tracing::warn!(error = %format!("{e:#}"), "driving a run failed; retry next tick");
            }
        }
    }
    let live: HashSet<String> = runs.iter().map(|r| r.instance_name.clone()).collect();
    ol.prune_run_locks(&live).await;

    // 6. Slow work: the UI's unsolicited list and housekeeping.
    let due = state
        .last_unsolicited_scan
        .is_none_or(|t| t.elapsed() >= Duration::from_secs(consts::UNSOLICITED_SCAN_INTERVAL_SECS));
    if due {
        state.last_unsolicited_scan = Some(Instant::now());
        if let Some(list) = note(
            "unsolicited",
            topology::scan_unsolicited(ol.config(), &ctx.sync_id).await,
        ) {
            ol.set_unsolicited(list).await;
        }
        if let Some(swept) = note(
            "archive",
            proposals::archive_sweep(&client, &snapshot.active_cids()).await,
        ) && (swept.acceptances > 0 || swept.declines > 0)
        {
            tracing::info!(
                acceptances = swept.acceptances,
                declines = swept.declines,
                "archived stale coordination contracts"
            );
        }
    }

    tracing::debug!(?report, "observer tick done");
    report
}

/// Drive one run under its lock; `None` when another caller holds it.
async fn drive_one(
    ctx: &TickCtx<'_>,
    run: &common::types::WorkflowRun,
    lock: Arc<Mutex<()>>,
) -> Option<Result<Driven>> {
    let Ok(_guard) = lock.try_lock() else {
        tracing::debug!(instance = %run.instance_name, "run busy; skipped this tick");
        return None;
    };
    Some(engine::drive(ctx, run).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_register_once_and_expose_every_stage() {
        register_metrics();
        register_metrics();
        let families = prometheus::gather();
        let names: Vec<&str> = families.iter().map(|f| f.name()).collect();
        for expected in [
            "decman_observer_tick_total",
            "decman_observer_tick_seconds",
            "decman_observer_last_tick_seconds",
            "decman_observer_runs_driven_total",
            "decman_observer_errors_total",
            "decman_observer_no_identity_total",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        let errors = families
            .iter()
            .find(|f| f.name() == "decman_observer_errors_total")
            .expect("errors family");
        assert!(errors.get_metric().len() >= 9, "one series per stage");
    }

    #[test]
    fn note_counts_and_swallows_errors() {
        register_metrics();
        let before = ERRORS.with_label_values(&["drive"]).get();
        assert_eq!(note("drive", Ok::<u8, anyhow::Error>(7)), Some(7));
        assert_eq!(note("drive", Err::<u8, _>(anyhow::anyhow!("boom"))), None);
        assert_eq!(ERRORS.with_label_values(&["drive"]).get(), before + 1);
    }

    #[tokio::test]
    async fn a_tick_without_identity_reports_idle_and_does_not_panic() {
        let ol = OnLedger::placeholder();
        let mut state = ObserverState::default();
        let report = tick(&ol, &mut state).await;
        assert!(!report.had_identity);
        assert_eq!(report.runs_seen, 0);
    }
}
