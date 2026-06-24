//! Regression for the uncancellable stuck workflow: a contracts run that the
//! UI lists as "in progress" (a persisted `workflow_runs` row) must be
//! cancellable even when the in-memory `<Kind>WorkflowState` is Idle — the
//! divergence a node restart / failed startup-recovery leaves behind.
//!
//! We reproduce that divergence directly: inject an `inprogress` Contracts
//! coordinator row into P1's DB that the running node has no in-memory state
//! for. Before the fix, POST /contracts/cancel gated on the in-memory status
//! and returned 409 "No Contracts workflow in progress" (exactly the operator's
//! report). With the fix, cancel falls back to the persisted row, so it returns
//! 200 and the row becomes `cancelled`.

use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tokio::time::sleep;
use tracing::info;

use crate::common::{Fixture, chaos, db, types::WorkflowRunsResponse};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: cancel_stuck_contracts");
    chaos::ensure_nodes_healthy(f).await?;

    let instance = chaos::fresh_prefix("stuck-contracts");
    let p1_db = f.db_path(1);

    // Reproduce "DB says InProgress, in-memory is Idle": a coordinator row P1
    // never started in memory (as if a restart's recovery failed to restore it).
    db::inject_inprogress_coordinator_run(&p1_db, "Contracts", &instance, "SignSubmissions")
        .await
        .context("inject stuck contracts run")?;

    // Sanity: the feed / UI lists it as in progress (this is the card the
    // operator sees with a CANCEL WORKFLOW button).
    let runs: WorkflowRunsResponse = f.get_json(f.p1.http, "/workflows").await?;
    anyhow::ensure!(
        runs.runs
            .iter()
            .any(|w| w.instance_name == instance && w.status == "inprogress"),
        "injected run not visible as inprogress in /workflows"
    );

    // The fix: cancel must succeed off the persisted row (pre-fix this 409'd
    // with "No Contracts workflow in progress").
    let (status, body) = f
        .post_expect_status(f.p1.http, "/contracts/cancel", &json!({}))
        .await?;
    anyhow::ensure!(
        status.as_u16() == 200,
        "/contracts/cancel returned {status}: {body} (expected 200 for a stuck/recovered run)"
    );

    // The persisted row must now be cancelled.
    sleep(Duration::from_secs(2)).await;
    let row_status = db::workflow_run_status(&p1_db, &instance, "Coordinator").await?;
    anyhow::ensure!(
        row_status.as_deref() == Some("cancelled"),
        "stuck run not cancelled in DB (status={row_status:?})"
    );
    info!("stuck contracts run cancelled via the persisted row");

    // Clean up so it doesn't linger in the feed.
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}
