//! Former parties and their purge state.

use sqlx::SqlitePool;

use super::{fixtures::TEST_NS, runs::test_run};
use crate::{
    canton_id::CantonId,
    db::{
        schema::{Commitable, SchemaRead, SchemaWrite},
        sqlite::connect::MIGRATOR,
    },
    error::Result,
    server::WorkflowProgress,
};

fn former(prefix: &str) -> CantonId {
    CantonId::parse(&format!("{prefix}::{TEST_NS}")).unwrap()
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn former_party_keeps_its_first_detection(pool: SqlitePool) -> Result {
    let party = former("left");
    assert!(pool.record_former_dec_party(&party, 100).await?);
    let detected_at = pool
        .get_former_dec_party(&party)
        .await?
        .unwrap()
        .detected_at;
    assert!(
        !pool.record_former_dec_party(&party, 100).await?,
        "a later refresh of the same removal changes nothing"
    );
    assert!(!pool.record_former_dec_party(&party, 50).await?);

    let rows = pool.get_former_dec_parties().await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].party_id, party.to_string());
    assert_eq!(rows[0].removed_at, 100);
    assert_eq!(rows[0].detected_at, detected_at);
    assert_eq!(rows[0].purge_status, "none");
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_newer_removal_starts_the_party_over(pool: SqlitePool) -> Result {
    let party = former("left");
    pool.record_former_dec_party(&party, 100).await?;
    pool.claim_former_party_purge(&party).await?;
    pool.complete_former_party_purge(&party, 7, 3).await?;
    pool.dismiss_former_dec_party(&party).await?;

    // A rehost and a second removal between two refreshes.
    assert!(pool.record_former_dec_party(&party, 200).await?);
    let row = pool.get_former_dec_party(&party).await?.unwrap();
    assert_eq!(row.removed_at, 200);
    assert_eq!(row.purge_status, "none");
    assert_eq!(
        (row.purged_count, row.kept_count, row.purge_finished_at),
        (None, None, None)
    );
    assert_eq!(
        row.dismissed_at, None,
        "the new leftovers must show again, so the party needs a new purge"
    );

    pool.claim_former_party_purge(&party).await?;
    assert!(
        !pool.record_former_dec_party(&party, 300).await?,
        "a running purge keeps its row until it records its outcome"
    );
    let row = pool.get_former_dec_party(&party).await?.unwrap();
    assert_eq!(
        (row.removed_at, row.purge_status.as_str()),
        (200, "running")
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn only_one_purge_runs_at_a_time(pool: SqlitePool) -> Result {
    let (a, b) = (former("a"), former("b"));
    pool.record_former_dec_party(&a, 1).await?;
    pool.record_former_dec_party(&b, 1).await?;

    assert!(pool.claim_former_party_purge(&a).await?);
    assert!(
        !pool.claim_former_party_purge(&a).await?,
        "a running purge cannot be claimed twice"
    );
    assert!(
        !pool.claim_former_party_purge(&b).await?,
        "a purge disconnects the participant, so a second party must wait"
    );

    pool.complete_former_party_purge(&a, 7, 3).await?;
    assert!(pool.claim_former_party_purge(&b).await?);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_purge_waits_for_running_workflows(pool: SqlitePool) -> Result {
    let party = former("left");
    pool.record_former_dec_party(&party, 1).await?;
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&test_run("kick-1", "Kick", "Peer"))
        .await?;
    Commitable::commit(tx).await?;

    assert!(
        !pool.claim_former_party_purge(&party).await?,
        "the purge would take the participant off the synchronizer under the run"
    );

    let mut tx = pool.begin_transaction().await?;
    tx.set_workflow_run_status("kick-1", WorkflowProgress::Completed, None, 2)
        .await?;
    Commitable::commit(tx).await?;
    assert!(pool.claim_former_party_purge(&party).await?);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn no_workflow_run_starts_during_a_purge(pool: SqlitePool) -> Result {
    let mut failed = test_run("kick-1", "Kick", "Peer");
    failed.status = WorkflowProgress::Failed;
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&failed).await?;
    Commitable::commit(tx).await?;
    let party = former("left");
    pool.record_former_dec_party(&party, 1).await?;
    assert!(pool.claim_former_party_purge(&party).await?);

    let mut tx = pool.begin_transaction().await?;
    let start = tx
        .upsert_workflow_run(&test_run("add-1", "AddParty", "Coordinator"))
        .await;
    assert!(start.is_err(), "a new run must not start during the purge");
    let retry = tx
        .set_workflow_run_status("kick-1", WorkflowProgress::InProgress, None, 2)
        .await;
    assert!(
        retry.is_err(),
        "a failed run must not be retried during the purge"
    );
    // Runs that do not become in progress are still recorded.
    tx.set_workflow_run_status("kick-1", WorkflowProgress::Cancelled, None, 3)
        .await?;
    Commitable::commit(tx).await?;
    assert!(pool.get_workflow_run("add-1").await?.is_none());
    assert_eq!(
        pool.get_workflow_run("kick-1").await?.unwrap().status,
        WorkflowProgress::Cancelled
    );

    pool.complete_former_party_purge(&party, 1, 0).await?;
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&test_run("add-1", "AddParty", "Coordinator"))
        .await?;
    tx.set_workflow_run_status("kick-1", WorkflowProgress::InProgress, None, 4)
        .await?;
    // Flipping a missing run stays a no-op.
    tx.set_workflow_run_status("missing", WorkflowProgress::InProgress, None, 4)
        .await?;
    Commitable::commit(tx).await?;
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn unknown_party_cannot_be_claimed(pool: SqlitePool) -> Result {
    assert!(!pool.claim_former_party_purge(&former("never")).await?);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn purge_outcomes_are_recorded(pool: SqlitePool) -> Result {
    let party = former("left");
    pool.record_former_dec_party(&party, 1).await?;

    pool.claim_former_party_purge(&party).await?;
    pool.fail_former_party_purge(&party, "participant unhealthy")
        .await?;
    let row = pool.get_former_dec_party(&party).await?.unwrap();
    assert_eq!(row.purge_status, "failed");
    assert_eq!(row.purge_error.as_deref(), Some("participant unhealthy"));

    // A retry clears the previous failure.
    pool.claim_former_party_purge(&party).await?;
    let row = pool.get_former_dec_party(&party).await?.unwrap();
    assert_eq!(row.purge_status, "running");
    assert_eq!(row.purge_error, None);

    pool.complete_former_party_purge(&party, 3028, 11438)
        .await?;
    let row = pool.get_former_dec_party(&party).await?.unwrap();
    assert_eq!(row.purge_status, "succeeded");
    assert_eq!(
        (row.purged_count, row.kept_count, row.purge_error),
        (Some(3028), Some(11438), None)
    );
    assert!(row.purge_finished_at.is_some());
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn restart_fails_only_running_purges(pool: SqlitePool) -> Result {
    let (running, done) = (former("running"), former("done"));
    pool.record_former_dec_party(&done, 1).await?;
    pool.claim_former_party_purge(&done).await?;
    pool.complete_former_party_purge(&done, 1, 0).await?;
    pool.record_former_dec_party(&running, 1).await?;
    pool.claim_former_party_purge(&running).await?;

    assert_eq!(
        pool.fail_interrupted_purges("restarted").await?,
        vec![running.to_string()]
    );
    let row = pool.get_former_dec_party(&running).await?.unwrap();
    assert_eq!(row.purge_status, "failed");
    assert_eq!(row.purge_error.as_deref(), Some("restarted"));
    let row = pool.get_former_dec_party(&done).await?.unwrap();
    assert_eq!(row.purge_status, "succeeded");
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn only_a_purged_party_can_be_dismissed(pool: SqlitePool) -> Result {
    let party = former("left");
    pool.record_former_dec_party(&party, 1).await?;
    assert!(
        !pool.dismiss_former_dec_party(&party).await?,
        "dismissing before the purge would hide stale contracts"
    );

    pool.claim_former_party_purge(&party).await?;
    pool.complete_former_party_purge(&party, 1, 0).await?;
    assert!(pool.dismiss_former_dec_party(&party).await?);
    assert!(pool.get_former_dec_parties().await?.is_empty());
    assert!(
        !pool.record_former_dec_party(&party, 1).await?,
        "a refresh that sees the same removal leaves the party dismissed"
    );
    assert!(pool.get_former_dec_parties().await?.is_empty());
    assert!(
        !pool.claim_former_party_purge(&party).await?,
        "a dismissed party is not purged again"
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn rehosted_party_is_forgotten_unless_purging(pool: SqlitePool) -> Result {
    let (back, purging, other) = (former("back"), former("purging"), former("other"));
    for party in [&back, &purging, &other] {
        pool.record_former_dec_party(party, 1).await?;
    }
    pool.claim_former_party_purge(&purging).await?;

    let removed = pool
        .forget_former_dec_parties(&[back.to_string(), purging.to_string()])
        .await?;
    assert_eq!(removed, 1);
    assert!(pool.get_former_dec_party(&back).await?.is_none());
    assert!(pool.get_former_dec_party(&purging).await?.is_some());
    assert!(pool.get_former_dec_party(&other).await?.is_some());
    assert_eq!(pool.forget_former_dec_parties(&[]).await?, 0);
    Ok(())
}
