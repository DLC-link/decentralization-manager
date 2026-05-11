//! SQLite read helpers for integration tests that need to inspect
//! `workflow_runs`, `workflow_artifacts`, and `dec_party_identity` directly.
//!
//! Mirrors the bash tests' `sqlite3 "$DEV_DIR/participant-N/data/decpm.db"`
//! invocations. All readers open a fresh connection per call so tests don't
//! have to share a pool across phases (the queries are infrequent).

use std::path::{Path, PathBuf};

use anyhow::Context;
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

use super::Fixture;

impl Fixture {
    pub fn db_path(&self, participant: u8) -> PathBuf {
        self.dev_dir
            .join(format!("participant-{participant}"))
            .join("data")
            .join("decpm.db")
    }
}

async fn open(path: &Path) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    SqlitePool::connect_with(opts)
        .await
        .with_context(|| format!("opening sqlite at {}", path.display()))
}

pub async fn count_workflow_runs_inprogress(
    db_path: &Path,
    kind: &str,
    role: &str,
) -> anyhow::Result<i64> {
    let pool = open(db_path).await?;
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflow_runs WHERE kind = ?1 AND role = ?2 AND status = 'inprogress'",
    )
    .bind(kind)
    .bind(role)
    .fetch_one(&pool)
    .await
    .context("count_workflow_runs_inprogress")?;
    pool.close().await;
    Ok(n)
}

/// Resolve the peer-side instance_name for the current inprogress run
/// of `kind`. Peers mint their own synthetic instance_name on accept
/// (e.g. `peer-onboarding-<pubkey>-<epoch>`), so chaos phases can't
/// guess it from the coordinator's prefix. The partial unique index
/// `(kind, role) WHERE status='inprogress'` guarantees at most one match.
pub async fn current_inprogress_peer_instance(
    db_path: &Path,
    kind: &str,
) -> anyhow::Result<Option<String>> {
    let pool = open(db_path).await?;
    let v: Option<String> = sqlx::query_scalar(
        "SELECT instance_name FROM workflow_runs \
         WHERE kind = ?1 AND role = 'Peer' AND status = 'inprogress' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_optional(&pool)
    .await
    .context("current_inprogress_peer_instance")?;
    pool.close().await;
    Ok(v)
}

/// Most recent peer-side instance_name of `kind` regardless of status.
/// Used by chaos phases that need the row identity *after* it's flipped to a
/// terminal state (failed/cancelled), where the inprogress lookup can no
/// longer find it.
pub async fn latest_peer_instance(db_path: &Path, kind: &str) -> anyhow::Result<Option<String>> {
    let pool = open(db_path).await?;
    let v: Option<String> = sqlx::query_scalar(
        "SELECT instance_name FROM workflow_runs \
         WHERE kind = ?1 AND role = 'Peer' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_optional(&pool)
    .await
    .context("latest_peer_instance")?;
    pool.close().await;
    Ok(v)
}

pub async fn workflow_run_status(
    db_path: &Path,
    instance_name: &str,
    role: &str,
) -> anyhow::Result<Option<String>> {
    let pool = open(db_path).await?;
    let s: Option<String> = sqlx::query_scalar(
        "SELECT status FROM workflow_runs WHERE instance_name = ?1 AND role = ?2",
    )
    .bind(instance_name)
    .bind(role)
    .fetch_optional(&pool)
    .await
    .context("workflow_run_status")?;
    pool.close().await;
    Ok(s)
}

pub async fn workflow_run_dismissed(
    db_path: &Path,
    instance_name: &str,
    role: &str,
) -> anyhow::Result<Option<bool>> {
    let pool = open(db_path).await?;
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT dismissed FROM workflow_runs WHERE instance_name = ?1 AND role = ?2",
    )
    .bind(instance_name)
    .bind(role)
    .fetch_optional(&pool)
    .await
    .context("workflow_run_dismissed")?;
    pool.close().await;
    Ok(v.map(|n| n != 0))
}

pub async fn workflow_run_created_at(
    db_path: &Path,
    instance_name: &str,
    role: &str,
) -> anyhow::Result<Option<i64>> {
    let pool = open(db_path).await?;
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT created_at FROM workflow_runs WHERE instance_name = ?1 AND role = ?2",
    )
    .bind(instance_name)
    .bind(role)
    .fetch_optional(&pool)
    .await
    .context("workflow_run_created_at")?;
    pool.close().await;
    Ok(v)
}

pub async fn count_workflow_run_rows(
    db_path: &Path,
    instance_name: &str,
    role: &str,
) -> anyhow::Result<i64> {
    let pool = open(db_path).await?;
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflow_runs WHERE instance_name = ?1 AND role = ?2",
    )
    .bind(instance_name)
    .bind(role)
    .fetch_one(&pool)
    .await
    .context("count_workflow_run_rows")?;
    pool.close().await;
    Ok(n)
}

pub async fn count_completed_runs(db_path: &Path, kind: &str, role: &str) -> anyhow::Result<i64> {
    let pool = open(db_path).await?;
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflow_runs WHERE kind = ?1 AND role = ?2 AND status = 'completed'",
    )
    .bind(kind)
    .bind(role)
    .fetch_one(&pool)
    .await
    .context("count_completed_runs")?;
    pool.close().await;
    Ok(n)
}

pub async fn count_artifacts(db_path: &Path, instance_name: &str) -> anyhow::Result<i64> {
    let pool = open(db_path).await?;
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflow_artifacts WHERE instance_name = ?1")
            .bind(instance_name)
            .fetch_one(&pool)
            .await
            .context("count_artifacts")?;
    pool.close().await;
    Ok(n)
}

pub async fn count_dec_party_identity(db_path: &Path, dec_party_id: &str) -> anyhow::Result<i64> {
    let pool = open(db_path).await?;
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM dec_party_identity WHERE dec_party_id = ?1")
            .bind(dec_party_id)
            .fetch_one(&pool)
            .await
            .context("count_dec_party_identity")?;
    pool.close().await;
    Ok(n)
}

/// Then-style probe over the persisted workflow_runs row. Returns
///  - `None`            when the row hasn't reached a terminal state yet,
///  - `Some(Ok(()))`    when it's `completed`,
///  - `Some(Err(_))`    when it's `failed` or `cancelled`.
///
/// Useful in chaos tests where the in-memory `<Kind>WorkflowState` can lag
/// behind the DB after a restart, but the persisted row is the durable signal.
pub async fn probe_db_run_status(
    db_path: &Path,
    instance_name: &str,
    role: &str,
) -> Option<anyhow::Result<()>> {
    let s = workflow_run_status(db_path, instance_name, role)
        .await
        .ok()
        .flatten()?;
    match s.as_str() {
        "completed" => Some(Ok(())),
        "failed" | "cancelled" => Some(Err(anyhow::anyhow!(
            "{instance_name} ({role}) reached terminal status: {s}"
        ))),
        _ => None,
    }
}

pub async fn list_undismissed_terminal_runs(
    db_path: &Path,
    kinds: &[&str],
    role: &str,
) -> anyhow::Result<Vec<String>> {
    let pool = open(db_path).await?;
    let placeholders = (1..=kinds.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let role_idx = kinds.len() + 1;
    let sql = format!(
        "SELECT instance_name FROM workflow_runs \
         WHERE kind IN ({placeholders}) AND role = ?{role_idx} \
         AND status IN ('cancelled', 'failed') AND dismissed = 0",
    );
    let mut q = sqlx::query_scalar::<_, String>(&sql);
    for k in kinds {
        q = q.bind(*k);
    }
    q = q.bind(role);
    let rows = q
        .fetch_all(&pool)
        .await
        .context("list_undismissed_terminal_runs")?;
    pool.close().await;
    Ok(rows)
}
