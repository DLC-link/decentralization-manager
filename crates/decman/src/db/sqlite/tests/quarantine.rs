//! The ACS import quarantine table.

use sqlx::SqlitePool;

use crate::db::sqlite::connect::MIGRATOR;
use crate::{
    canton_id::CantonId,
    db::schema::{SchemaRead, SchemaWrite},
    error::Result,
};

/// A valid Canton id for tests. The namespace must be exactly
/// `NAMESPACE_LENGTH` bytes, so build it the way `test_peer` does rather
/// than writing short hex.
fn qid(prefix: &str, tag: u8) -> Result<CantonId> {
    let ns = format!("1220{:0>64}", format!("{tag:02x}"));
    CantonId::parse(&format!("{prefix}::{ns}"))
        .map_err(|e| anyhow::anyhow!("bad test id {prefix}: {e}"))
}

/// The quarantine records a fact about a *participant*, so it must outlive
/// the workflow run that discovered it. This is the bypass the review
/// found: a marker in `workflow_artifacts` is keyed by instance_name and
/// cascades away with its run, so a fresh add-party would not see it and
/// would import onto a partially populated participant.
#[sqlx::test(migrator = "MIGRATOR")]
async fn acs_quarantine_outlives_the_run_that_set_it(pool: SqlitePool) -> Result {
    let party = qid("cbtc-network", 0xaa)?;
    let target = qid("participant-3", 0xbb)?;

    assert!(
        pool.get_acs_import_quarantine(&party, &target)
            .await?
            .is_none(),
        "a clean participant must not be quarantined"
    );

    pool.quarantine_acs_import(&party, &target, "failed after 500 blocks", 2_097_152)
        .await?;

    // No workflow run exists at all here, which is the point: nothing about
    // this row is tied to one.
    let reason = pool.get_acs_import_quarantine(&party, &target).await?;
    assert_eq!(reason.as_deref(), Some("failed after 500 blocks"));

    Ok(())
}

/// Scoped to the pair. Quarantining one participant must not block
/// replicating the same party onto a different one, or the same
/// participant for an unrelated party.
#[sqlx::test(migrator = "MIGRATOR")]
async fn acs_quarantine_is_scoped_to_party_and_participant(pool: SqlitePool) -> Result {
    let party = qid("cbtc-network", 0xaa)?;
    let other_party = qid("other-network", 0xcc)?;
    let target = qid("participant-3", 0xbb)?;
    let other_target = qid("participant-4", 0xdd)?;

    pool.quarantine_acs_import(&party, &target, "partial import", 1)
        .await?;

    assert!(
        pool.get_acs_import_quarantine(&party, &target)
            .await?
            .is_some()
    );
    assert!(
        pool.get_acs_import_quarantine(&party, &other_target)
            .await?
            .is_none(),
        "a different participant is unaffected"
    );
    assert!(
        pool.get_acs_import_quarantine(&other_party, &target)
            .await?
            .is_none(),
        "the same participant is unaffected for a different party"
    );
    Ok(())
}

/// Re-tripping the guard keeps the first reason, which describes the
/// failure that actually reached Canton. Lifting is idempotent so an
/// operator can run it without checking first.
#[sqlx::test(migrator = "MIGRATOR")]
async fn acs_quarantine_keeps_the_first_reason_and_lifts_once(pool: SqlitePool) -> Result {
    let party = qid("cbtc-network", 0xaa)?;
    let target = qid("participant-3", 0xbb)?;

    pool.quarantine_acs_import(&party, &target, "first failure", 100)
        .await?;
    pool.quarantine_acs_import(&party, &target, "second failure", 200)
        .await?;
    assert_eq!(
        pool.get_acs_import_quarantine(&party, &target)
            .await?
            .as_deref(),
        Some("first failure"),
        "the original failure is the one that describes what Canton received"
    );

    assert!(pool.clear_acs_import_quarantine(&party, &target).await?);
    assert!(
        pool.get_acs_import_quarantine(&party, &target)
            .await?
            .is_none()
    );
    assert!(
        !pool.clear_acs_import_quarantine(&party, &target).await?,
        "lifting an absent quarantine reports no row removed rather than failing"
    );
    Ok(())
}
