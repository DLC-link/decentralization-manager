//! The `external_party_unhost_audit` table: one row per attempt to make this
//! node leave an external party.
//!
//! [`super::unhost`] inserts the row before it reads anything and updates it as
//! it moves, so a crash leaves an `in_progress` row naming the step it reached.
//! The next attempt reads that step to decide where to resume.

use common::api::UnhostAuditEntry;
use sqlx::SqlitePool;

use crate::error::Result;

/// Which entry point asked for the unhost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedVia {
    /// An admin through `POST /external-parties/{party_id}/unhost`.
    Operator,
    /// A wallet provider through `POST /v0/tenant/unhost`.
    Tenant,
}

impl RequestedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Tenant => "tenant",
        }
    }
}

/// How far an attempt got. Stored as text so the audit reads without a lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnhostStep {
    Preconditions,
    SpliceCleanup,
    Topology,
    Done,
}

impl UnhostStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preconditions => "preconditions",
            Self::SpliceCleanup => "splice_cleanup",
            Self::Topology => "topology",
            Self::Done => "done",
        }
    }
}

/// An attempt's outcome as the row records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditStatus {
    InProgress,
    Completed,
    Failed,
}

impl AuditStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// What a new row starts with.
pub struct NewAttempt<'a> {
    pub party_id: &'a str,
    pub participant_id: &'a str,
    pub requested_via: RequestedVia,
    pub requested_by: &'a str,
    pub splice_cleanup: bool,
}

/// One row, as stored.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct UnhostAuditRow {
    pub id: i64,
    pub party_id: String,
    pub participant_id: String,
    pub requested_via: String,
    pub requested_by: String,
    pub serial_before: Option<i64>,
    pub serial_after: Option<i64>,
    pub validator_rights_archived: String,
    pub preapprovals_cancelled: String,
    pub splice_cleanup: bool,
    pub step: String,
    pub resumed_from: Option<i64>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub error: Option<String>,
}

impl UnhostAuditRow {
    /// The wire shape. A contract-id list that does not parse reads as empty
    /// rather than failing the whole audit read.
    pub fn into_entry(self) -> UnhostAuditEntry {
        UnhostAuditEntry {
            id: self.id,
            party_id: self.party_id,
            participant_id: self.participant_id,
            requested_via: self.requested_via,
            requested_by: self.requested_by,
            serial_before: self.serial_before.and_then(|s| u32::try_from(s).ok()),
            serial_after: self.serial_after.and_then(|s| u32::try_from(s).ok()),
            validator_rights_archived: serde_json::from_str(&self.validator_rights_archived)
                .unwrap_or_default(),
            preapprovals_cancelled: serde_json::from_str(&self.preapprovals_cancelled)
                .unwrap_or_default(),
            splice_cleanup: self.splice_cleanup,
            step: self.step,
            resumed_from: self.resumed_from,
            started_at: self.started_at,
            finished_at: self.finished_at,
            status: self.status,
            error: self.error,
        }
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Insert the row for a new attempt and return its id. It starts
/// `in_progress` at the preconditions step.
///
/// # Errors
/// Returns an error if the insert fails.
pub async fn insert_attempt(db: &SqlitePool, attempt: &NewAttempt<'_>) -> Result<i64> {
    let id = sqlx::query(
        "INSERT INTO external_party_unhost_audit \
         (party_id, participant_id, requested_via, requested_by, splice_cleanup, step, \
          started_at, status) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(attempt.party_id)
    .bind(attempt.participant_id)
    .bind(attempt.requested_via.as_str())
    .bind(attempt.requested_by)
    .bind(attempt.splice_cleanup)
    .bind(UnhostStep::Preconditions.as_str())
    .bind(now_secs())
    .bind(AuditStatus::InProgress.as_str())
    .execute(db)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// Record the head serial the attempt read before it wrote anything.
///
/// # Errors
/// Returns an error if the update fails.
pub async fn set_serial_before(db: &SqlitePool, id: i64, serial: u32) -> Result {
    sqlx::query("UPDATE external_party_unhost_audit SET serial_before = ? WHERE id = ?")
        .bind(i64::from(serial))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record that this attempt took over from an earlier one.
///
/// # Errors
/// Returns an error if the update fails.
pub async fn set_resumed_from(db: &SqlitePool, id: i64, earlier: i64) -> Result {
    sqlx::query("UPDATE external_party_unhost_audit SET resumed_from = ? WHERE id = ?")
        .bind(earlier)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record the step the attempt has reached.
///
/// # Errors
/// Returns an error if the update fails.
pub async fn set_step(db: &SqlitePool, id: i64, step: UnhostStep) -> Result {
    sqlx::query("UPDATE external_party_unhost_audit SET step = ? WHERE id = ?")
        .bind(step.as_str())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record the Splice contracts archived so far. Called after each archive, so a
/// crash mid-cleanup still leaves the ids that went.
///
/// # Errors
/// Returns an error if the update fails.
pub async fn set_cleanup(
    db: &SqlitePool,
    id: i64,
    validator_rights_archived: &[String],
    preapprovals_cancelled: &[String],
) -> Result {
    sqlx::query(
        "UPDATE external_party_unhost_audit \
         SET validator_rights_archived = ?, preapprovals_cancelled = ? WHERE id = ?",
    )
    .bind(serde_json::to_string(validator_rights_archived)?)
    .bind(serde_json::to_string(preapprovals_cancelled)?)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Close the attempt.
///
/// `InProgress` here means the topology write was accepted but is not
/// effective yet, so the row stays open without a finish time.
///
/// # Errors
/// Returns an error if the update fails.
pub async fn finish(
    db: &SqlitePool,
    id: i64,
    status: AuditStatus,
    serial_after: Option<u32>,
    error: Option<&str>,
) -> Result {
    let finished_at = (status != AuditStatus::InProgress).then(now_secs);
    sqlx::query(
        "UPDATE external_party_unhost_audit \
         SET status = ?, serial_after = COALESCE(?, serial_after), finished_at = ?, error = ? \
         WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(serial_after.map(i64::from))
    .bind(finished_at)
    .bind(error)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// The newest attempt for this party on this participant that got past its
/// preconditions, whatever its status.
///
/// An attempt refused at its preconditions changed nothing, so it says nothing
/// about where a new attempt should start.
///
/// # Errors
/// Returns an error if the read fails.
pub async fn latest_with_work(
    db: &SqlitePool,
    party_id: &str,
    participant_id: &str,
) -> Result<Option<UnhostAuditRow>> {
    Ok(sqlx::query_as::<_, UnhostAuditRow>(
        "SELECT * FROM external_party_unhost_audit \
         WHERE party_id = ? AND participant_id = ? AND step != ? \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(party_id)
    .bind(participant_id)
    .bind(UnhostStep::Preconditions.as_str())
    .fetch_optional(db)
    .await?)
}

/// Close every other attempt still marked `in_progress` for this party.
///
/// The caller holds the in-process guard for the party, so no other attempt can
/// be running: an open row is one a crash or a restart left behind.
///
/// # Errors
/// Returns an error if the update fails.
pub async fn close_interrupted(
    db: &SqlitePool,
    party_id: &str,
    participant_id: &str,
    current: i64,
) -> Result<u64> {
    let closed = sqlx::query(
        "UPDATE external_party_unhost_audit \
         SET status = ?, finished_at = ?, \
             error = COALESCE(error, 'interrupted before it finished; attempt ' || ? || ' took over') \
         WHERE party_id = ? AND participant_id = ? AND status = ? AND id != ?",
    )
    .bind(AuditStatus::Failed.as_str())
    .bind(now_secs())
    .bind(current)
    .bind(party_id)
    .bind(participant_id)
    .bind(AuditStatus::InProgress.as_str())
    .bind(current)
    .execute(db)
    .await?
    .rows_affected();
    Ok(closed)
}

/// Every attempt for a party on this node, newest first.
///
/// # Errors
/// Returns an error if the read fails.
pub async fn list_for_party(
    db: &SqlitePool,
    party_id: &str,
    limit: i64,
) -> Result<Vec<UnhostAuditRow>> {
    Ok(sqlx::query_as::<_, UnhostAuditRow>(
        "SELECT * FROM external_party_unhost_audit WHERE party_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(party_id)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MIGRATOR;

    const PARTY: &str = "alice::1220aa";
    const ME: &str = "participant-3::1220bb";

    fn attempt(via: RequestedVia) -> NewAttempt<'static> {
        NewAttempt {
            party_id: PARTY,
            participant_id: ME,
            requested_via: via,
            requested_by: "tenant-key-1",
            splice_cleanup: true,
        }
    }

    async fn only_row(db: &SqlitePool) -> Result<UnhostAuditEntry> {
        let mut rows = list_for_party(db, PARTY, 10).await?;
        assert_eq!(rows.len(), 1, "expected exactly one row");
        Ok(rows.remove(0).into_entry())
    }

    /// A row is visible from the moment the attempt starts, so a crash before
    /// any other write still leaves a trace.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_new_attempt_is_open_at_its_preconditions(db: SqlitePool) -> Result {
        insert_attempt(&db, &attempt(RequestedVia::Tenant)).await?;

        let row = only_row(&db).await?;
        assert_eq!(row.status, "in_progress");
        assert_eq!(row.step, "preconditions");
        assert_eq!(row.requested_via, "tenant");
        assert_eq!(row.requested_by, "tenant-key-1");
        assert!(row.splice_cleanup);
        assert!(row.validator_rights_archived.is_empty());
        assert!(row.preapprovals_cancelled.is_empty());
        assert_eq!(row.serial_before, None);
        assert_eq!(row.finished_at, None);
        Ok(())
    }

    /// The full shape the issue asks for, written the way the workflow writes it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completed_attempt_records_every_field(db: SqlitePool) -> Result {
        let id = insert_attempt(&db, &attempt(RequestedVia::Operator)).await?;
        set_serial_before(&db, id, 7).await?;
        set_step(&db, id, UnhostStep::SpliceCleanup).await?;
        set_cleanup(&db, id, &["vr-1".to_string()], &["tp-1".to_string()]).await?;
        set_step(&db, id, UnhostStep::Done).await?;
        finish(&db, id, AuditStatus::Completed, Some(8), None).await?;

        let row = only_row(&db).await?;
        assert_eq!(row.id, id);
        assert_eq!(row.party_id, PARTY);
        assert_eq!(row.participant_id, ME);
        assert_eq!(row.requested_via, "operator");
        assert_eq!(row.serial_before, Some(7));
        assert_eq!(row.serial_after, Some(8));
        assert_eq!(row.validator_rights_archived, vec!["vr-1".to_string()]);
        assert_eq!(row.preapprovals_cancelled, vec!["tp-1".to_string()]);
        assert_eq!(row.step, "done");
        assert_eq!(row.status, "completed");
        assert!(row.finished_at.is_some());
        assert_eq!(row.error, None);
        Ok(())
    }

    /// An accepted but not yet effective write keeps the row open: a finish time
    /// would claim an outcome nobody has seen.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_attempt_awaiting_topology_stays_open(db: SqlitePool) -> Result {
        let id = insert_attempt(&db, &attempt(RequestedVia::Tenant)).await?;
        finish(&db, id, AuditStatus::InProgress, Some(8), None).await?;

        let row = only_row(&db).await?;
        assert_eq!(row.status, "in_progress");
        assert_eq!(row.serial_after, Some(8));
        assert_eq!(row.finished_at, None);
        Ok(())
    }

    /// A refusal at the preconditions changed nothing, so it must not decide
    /// where the next attempt starts.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_refused_attempt_is_not_work_to_resume(db: SqlitePool) -> Result {
        let worked = insert_attempt(&db, &attempt(RequestedVia::Tenant)).await?;
        set_step(&db, worked, UnhostStep::Topology).await?;
        finish(&db, worked, AuditStatus::Failed, None, Some("boom")).await?;
        let refused = insert_attempt(&db, &attempt(RequestedVia::Tenant)).await?;
        finish(&db, refused, AuditStatus::Failed, None, Some("stale")).await?;

        let latest = latest_with_work(&db, PARTY, ME).await?;
        assert_eq!(latest.map(|row| row.id), Some(worked));
        assert!(
            latest_with_work(&db, PARTY, "participant-9::1220cc")
                .await?
                .is_none()
        );
        Ok(())
    }

    /// An open row left by a crash is closed when the next attempt starts, and
    /// the attempt that took over is named. The new attempt's own row stays open.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_new_attempt_closes_rows_a_crash_left_open(db: SqlitePool) -> Result {
        let crashed = insert_attempt(&db, &attempt(RequestedVia::Tenant)).await?;
        let current = insert_attempt(&db, &attempt(RequestedVia::Tenant)).await?;

        assert_eq!(close_interrupted(&db, PARTY, ME, current).await?, 1);

        let rows = list_for_party(&db, PARTY, 10).await?;
        let by_id = |id| rows.iter().find(|row| row.id == id).cloned();
        let Some(crashed) = by_id(crashed) else {
            panic!("the crashed row must still be there");
        };
        assert_eq!(crashed.status, "failed");
        assert!(crashed.finished_at.is_some());
        assert!(
            crashed
                .error
                .as_deref()
                .is_some_and(|e| e.contains(&current.to_string())),
            "{:?}",
            crashed.error
        );
        assert_eq!(
            by_id(current).map(|row| row.status),
            Some("in_progress".into())
        );
        Ok(())
    }

    /// The audit is newest first and scoped to the party asked about.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_audit_lists_one_party_newest_first(db: SqlitePool) -> Result {
        let first = insert_attempt(&db, &attempt(RequestedVia::Tenant)).await?;
        let second = insert_attempt(&db, &attempt(RequestedVia::Operator)).await?;
        insert_attempt(
            &db,
            &NewAttempt {
                party_id: "bob::1220dd",
                ..attempt(RequestedVia::Tenant)
            },
        )
        .await?;

        let ids: Vec<i64> = list_for_party(&db, PARTY, 10)
            .await?
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, vec![second, first]);
        Ok(())
    }
}
