//! SchemaWrite for the SQLite pool, and the invitation and owner-key writes it shares with transactions.

use sqlx::{SqliteConnection, SqlitePool};

use crate::db::{
    rows::{OwnerKeyRefusal, PendingInvitationRow},
    schema::SchemaWrite,
};
use crate::{canton_id::CantonId, error::Result, server::PendingInvitation};

impl SchemaWrite for SqlitePool {
    type Transaction = sqlx::Transaction<'static, sqlx::Sqlite>;

    async fn begin_transaction(&self) -> Result<Self::Transaction> {
        Ok(self.begin().await?)
    }

    async fn quarantine_acs_import(
        &self,
        party_id: &CantonId,
        participant_id: &CantonId,
        reason: &str,
        bytes_imported: u64,
    ) -> Result<()> {
        // First reason wins: a later attempt that trips the same guard is not
        // new information, and the original is the one describing the failure
        // that actually reached Canton.
        sqlx::query(
            "INSERT OR IGNORE INTO acs_import_quarantine \
             (party_id, participant_id, reason, bytes_imported, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(party_id.to_string())
        .bind(participant_id.to_string())
        .bind(reason)
        .bind(i64::try_from(bytes_imported).unwrap_or(i64::MAX))
        .bind(chrono::Utc::now().timestamp())
        .execute(self)
        .await?;
        Ok(())
    }

    async fn clear_acs_import_quarantine(
        &self,
        party_id: &CantonId,
        participant_id: &CantonId,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM acs_import_quarantine WHERE party_id = ? AND participant_id = ?",
        )
        .bind(party_id.to_string())
        .bind(participant_id.to_string())
        .execute(self)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn record_former_dec_party(&self, party_id: &CantonId, removed_at: i64) -> Result<bool> {
        let result = sqlx::query(
            "INSERT INTO former_dec_party (party_id, removed_at, detected_at) VALUES (?, ?, ?) \
             ON CONFLICT(party_id) DO UPDATE SET \
                 removed_at = excluded.removed_at, detected_at = excluded.detected_at, \
                 purge_status = 'none', purge_started_at = NULL, purge_finished_at = NULL, \
                 purged_count = NULL, kept_count = NULL, purge_error = NULL, \
                 dismissed_at = NULL \
             WHERE excluded.removed_at > former_dec_party.removed_at \
               AND former_dec_party.purge_status != 'running'",
        )
        .bind(party_id.to_string())
        .bind(removed_at)
        .bind(chrono::Utc::now().timestamp())
        .execute(self)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn forget_former_dec_parties(&self, party_ids: &[String]) -> Result<u64> {
        if party_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; party_ids.len()].join(",");
        let query = format!(
            "DELETE FROM former_dec_party \
             WHERE purge_status != 'running' AND party_id IN ({placeholders})"
        );
        let mut q = sqlx::query(&query);
        for party_id in party_ids {
            q = q.bind(party_id);
        }
        Ok(q.execute(self).await?.rows_affected())
    }

    async fn claim_former_party_purge(&self, party_id: &CantonId) -> Result<bool> {
        // One statement, so the "nothing else is running" check and the claim
        // cannot interleave with another purge or with a workflow start.
        let result = sqlx::query(
            "UPDATE former_dec_party \
             SET purge_status = 'running', purge_started_at = ?, purge_finished_at = NULL, \
                 purged_count = NULL, kept_count = NULL, purge_error = NULL \
             WHERE party_id = ? AND dismissed_at IS NULL \
               AND NOT EXISTS (SELECT 1 FROM former_dec_party WHERE purge_status = 'running') \
               AND NOT EXISTS (SELECT 1 FROM workflow_runs WHERE status = 'inprogress')",
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(party_id.to_string())
        .execute(self)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn complete_former_party_purge(
        &self,
        party_id: &CantonId,
        purged: i64,
        kept: i64,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE former_dec_party \
             SET purge_status = 'succeeded', purge_finished_at = ?, purged_count = ?, \
                 kept_count = ?, purge_error = NULL \
             WHERE party_id = ?",
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(purged)
        .bind(kept)
        .bind(party_id.to_string())
        .execute(self)
        .await?;
        Ok(())
    }

    async fn fail_former_party_purge(&self, party_id: &CantonId, error: &str) -> Result<()> {
        sqlx::query(
            "UPDATE former_dec_party \
             SET purge_status = 'failed', purge_finished_at = ?, purge_error = ? \
             WHERE party_id = ?",
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(error)
        .bind(party_id.to_string())
        .execute(self)
        .await?;
        Ok(())
    }

    async fn fail_interrupted_purges(&self, error: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "UPDATE former_dec_party \
             SET purge_status = 'failed', purge_finished_at = ?, purge_error = ? \
             WHERE purge_status = 'running' \
             RETURNING party_id",
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(error)
        .fetch_all(self)
        .await?;
        Ok(rows.into_iter().map(|(party_id,)| party_id).collect())
    }

    async fn dismiss_former_dec_party(&self, party_id: &CantonId) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE former_dec_party SET dismissed_at = ? \
             WHERE party_id = ? AND purge_status = 'succeeded' AND dismissed_at IS NULL",
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(party_id.to_string())
        .execute(self)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}

/// Insert or replace an invitation row with the given status. A pending row
/// has no `handled_at`; an accepted or declined one has the time of the answer.
pub(super) async fn write_invitation(
    conn: &mut SqliteConnection,
    inv: &PendingInvitation,
    status: &str,
    handled_at: Option<i64>,
) -> Result {
    let row = PendingInvitationRow::from_domain(inv)?;

    sqlx::query(
        r"
        INSERT OR REPLACE INTO pending_invitations (
            id,
            invitation_type,
            coordinator_pubkey,
            received_at,
            prefix,
            participants,
            dar_filenames,
            dar_hashes,
            kicked_participant,
            kicked_owner_key,
            new_participant,
            new_threshold,
            previous_threshold,
            confirmation_threshold,
            dec_party_id,
            package_names,
            contract_intents,
            workflow_instance,
            status,
            handled_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&row.id)
    .bind(&row.invitation_type)
    .bind(&row.coordinator_pubkey)
    .bind(row.received_at)
    .bind(&row.prefix)
    .bind(&row.participants)
    .bind(&row.dar_filenames)
    .bind(&row.dar_hashes)
    .bind(&row.kicked_participant)
    .bind(&row.kicked_owner_key)
    .bind(&row.new_participant)
    .bind(row.new_threshold)
    .bind(row.previous_threshold)
    .bind(row.confirmation_threshold)
    .bind(&row.dec_party_id)
    .bind(&row.package_names)
    .bind(&row.contract_intents)
    .bind(&row.workflow_instance)
    .bind(status)
    .bind(handled_at)
    .execute(conn)
    .await?;

    Ok(())
}

/// Why `owner_key` must not go on record for `participant_uid`: another
/// participant has the key on record, as a host's key or as a claim, or
/// `participant_uid` has another key on record that is still an owner.
pub(super) async fn owner_key_refusal(
    conn: &mut SqliteConnection,
    party_id: &str,
    owner_key: &str,
    participant_uid: &str,
) -> Result<Option<OwnerKeyRefusal>> {
    let holder: Option<String> = sqlx::query_scalar(
        r"
        SELECT participant_uid FROM dec_party_participant
        WHERE dec_party_id = ? AND owner_key = ? AND participant_uid != ?
        UNION
        SELECT participant_uid FROM dec_party_owner
        WHERE dec_party_id = ? AND owner_key = ? AND participant_uid != ?
        LIMIT 1
        ",
    )
    .bind(party_id)
    .bind(owner_key)
    .bind(participant_uid)
    .bind(party_id)
    .bind(owner_key)
    .bind(participant_uid)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(holder) = holder {
        return Ok(Some(OwnerKeyRefusal::HeldBy(holder)));
    }

    // Every dec_party_owner row is a current owner of the party.
    let kept: Option<String> = sqlx::query_scalar(
        r"
        SELECT owner.owner_key FROM dec_party_owner AS owner
        WHERE owner.dec_party_id = ?
            AND owner.owner_key != ?
            AND (
                owner.participant_uid = ?
                OR EXISTS (
                    SELECT 1 FROM dec_party_participant AS participant
                    WHERE participant.dec_party_id = owner.dec_party_id
                        AND participant.participant_uid = ?
                        AND participant.owner_key = owner.owner_key
                )
            )
        LIMIT 1
        ",
    )
    .bind(party_id)
    .bind(owner_key)
    .bind(participant_uid)
    .bind(participant_uid)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(kept.map(OwnerKeyRefusal::KeepsKey))
}
