//! Workflow runs and their artefacts.

use sqlx::SqlitePool;

use crate::db::sqlite::connect::MIGRATOR;
use crate::{
    canton_id::CantonId,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    server::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
    workflow::storage::WorkflowStorage,
};

use super::fixtures::TEST_NS;

// ====================================================================
// Workflow runs + artefacts
// ====================================================================

pub(super) fn test_run(instance: &str, kind: &str, role: &str) -> WorkflowRun {
    WorkflowRun {
        instance_name: instance.to_string(),
        kind: kind.parse().unwrap(),
        role: role.parse().unwrap(),
        status: WorkflowProgress::InProgress,
        current_step: "WaitingForPeers".to_string(),
        step_index: 0,
        step_total: 7,
        config_json: r#"{"foo":"bar"}"#.to_string(),
        coordinator_pubkey: Some("aaaa".to_string()),
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: vec![
            CantonId::parse(&format!("a::{TEST_NS}")).unwrap(),
            CantonId::parse(&format!("b::{TEST_NS}")).unwrap(),
        ],
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
        created_at: 1000,
        updated_at: 1000,
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_workflow_run_coordinator_instance_roundtrip(pool: SqlitePool) -> Result {
    // Migration 000014: a peer row's `coordinator_instance` (the
    // coordinator run it belongs to) must survive a persist/load
    // round-trip — instance-scoped CancelInvite/RetryWorkflow and
    // peer-resume routing key off it.
    let mut run = test_run("peer-onboarding-abc-1", "Onboarding", "Peer");
    run.coordinator_instance = Some("test32-creation".to_string());

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    Commitable::commit(tx).await?;

    let loaded = pool.get_workflow_run(&run.instance_name).await?.unwrap();
    assert_eq!(
        loaded.coordinator_instance.as_deref(),
        Some("test32-creation")
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_workflow_runs_lifecycle(pool: SqlitePool) -> Result {
    let run = test_run("party-a-creation", "Onboarding", "Coordinator");
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    Commitable::commit(tx).await?;

    let active = pool
        .get_active_workflow_run(WorkflowKind::Onboarding, WorkflowRole::Coordinator)
        .await?;
    assert!(active.is_some());

    // Advance step
    let mut tx = pool.begin_transaction().await?;
    let completed = vec![CantonId::parse(&format!("a::{TEST_NS}")).unwrap()];
    tx.update_workflow_run_step(&run.instance_name, "SignDns", 3, &completed, 2000)
        .await?;
    Commitable::commit(tx).await?;

    let loaded = pool.get_workflow_run(&run.instance_name).await?.unwrap();
    assert_eq!(loaded.current_step, "SignDns");
    assert_eq!(loaded.step_index, 3);
    assert_eq!(loaded.completed_peers, completed);

    // Onboarding resolves its party ID during the workflow. Persisting it
    // on the run must survive the terminal cleanup below so exact topology
    // discovery can still include the newly-created party.
    let dec_party_id = CantonId::parse(&format!("party-a::{TEST_NS}")).unwrap();
    pool.write_run_party_id(&run.instance_name, &dec_party_id)
        .await?;

    // Seed an artefact so we can verify the terminal-state cleanup wipes it.
    let mut tx = pool.begin_transaction().await?;
    tx.write_workflow_artifact(&run.instance_name, "dns_proto", None, b"some-bytes")
        .await?;
    Commitable::commit(tx).await?;

    // Mark completed → workflow_artifacts for this instance should drop.
    let mut tx = pool.begin_transaction().await?;
    tx.set_workflow_run_status(&run.instance_name, WorkflowProgress::Completed, None, 3000)
        .await?;
    Commitable::commit(tx).await?;

    let leftover = pool
        .read_workflow_artifact(&run.instance_name, "dns_proto", None)
        .await?;
    assert!(
        leftover.is_none(),
        "workflow_artifacts should be cleaned up on terminal status"
    );

    // Visible feed: still here because not dismissed.
    let visible = pool.get_visible_workflow_runs().await?;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].status, WorkflowProgress::Completed);
    assert_eq!(visible[0].dec_party_id.as_ref(), Some(&dec_party_id));

    // Dismiss → vanishes from feed.
    let mut tx = pool.begin_transaction().await?;
    tx.dismiss_workflow_run(&run.instance_name).await?;
    Commitable::commit(tx).await?;

    let visible = pool.get_visible_workflow_runs().await?;
    assert!(visible.is_empty());

    // A Failed run KEEPS its artefacts for post-mortem (unlike the
    // Completed path above); only `dismiss` releases them.
    let failed = test_run("party-b-failed", "Onboarding", "Coordinator");
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&failed).await?;
    tx.write_workflow_artifact(&failed.instance_name, "dns_proto", None, b"keep-me")
        .await?;
    Commitable::commit(tx).await?;

    let mut tx = pool.begin_transaction().await?;
    tx.set_workflow_run_status(
        &failed.instance_name,
        WorkflowProgress::Failed,
        Some("boom"),
        4000,
    )
    .await?;
    Commitable::commit(tx).await?;

    let kept = pool
        .read_workflow_artifact(&failed.instance_name, "dns_proto", None)
        .await?;
    assert!(
        kept.is_some(),
        "Failed runs must keep artefacts for post-mortem"
    );

    // Dismissing the Failed run finally releases the artefacts.
    let mut tx = pool.begin_transaction().await?;
    tx.dismiss_workflow_run(&failed.instance_name).await?;
    Commitable::commit(tx).await?;
    let after_dismiss = pool
        .read_workflow_artifact(&failed.instance_name, "dns_proto", None)
        .await?;
    assert!(
        after_dismiss.is_none(),
        "dismiss must drop a Failed run's artefacts"
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_workflow_runs_concurrent_inprogress_same_kind(pool: SqlitePool) -> Result {
    // Migration 000013 dropped the partial unique index that previously
    // forbade two InProgress runs of the same (kind, role). The DB now
    // accepts them; runtime concurrency is governed by the in-memory
    // `WorkflowRegistry`, each run addressed by its own `instance_name`.
    let a = test_run("alpha", "Onboarding", "Coordinator");
    let b = test_run("beta", "Onboarding", "Coordinator");

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&a).await?;
    Commitable::commit(tx).await?;

    // A second InProgress run of the same (kind, role) must now succeed.
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&b).await?;
    Commitable::commit(tx).await?;

    let visible = pool.get_visible_workflow_runs().await?;
    let inprogress = visible
        .iter()
        .filter(|r| {
            r.kind == WorkflowKind::Onboarding
                && r.role == WorkflowRole::Coordinator
                && r.status == WorkflowProgress::InProgress
        })
        .count();
    assert_eq!(
        inprogress, 2,
        "both concurrent InProgress onboarding coordinator runs must persist"
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_workflow_artifacts_roundtrip_and_cascade(pool: SqlitePool) -> Result {
    let run = test_run("art-test", "Onboarding", "Coordinator");
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    // Shared artefact (no peer).
    tx.write_workflow_artifact(&run.instance_name, "dns_proto", None, b"shared-proto-bytes")
        .await?;
    // Per-peer artefacts.
    tx.write_workflow_artifact(
        &run.instance_name,
        "signed_dns_proposal",
        Some("a::1220aa"),
        b"sig-from-a",
    )
    .await?;
    tx.write_workflow_artifact(
        &run.instance_name,
        "signed_dns_proposal",
        Some("b::1220bb"),
        b"sig-from-b",
    )
    .await?;
    Commitable::commit(tx).await?;

    let proto = pool
        .read_workflow_artifact(&run.instance_name, "dns_proto", None)
        .await?
        .unwrap();
    assert_eq!(proto, b"shared-proto-bytes");

    let listed = pool
        .list_workflow_artifacts(&run.instance_name, "signed_dns_proposal")
        .await?;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].0, "a::1220aa");
    assert_eq!(listed[0].1, b"sig-from-a");

    // CASCADE: deleting the run drops the artefacts.
    sqlx::query("DELETE FROM workflow_runs WHERE instance_name = ?")
        .bind(&run.instance_name)
        .execute(&pool)
        .await?;
    let listed = pool
        .list_workflow_artifacts(&run.instance_name, "signed_dns_proposal")
        .await?;
    assert!(listed.is_empty());

    Ok(())
}
