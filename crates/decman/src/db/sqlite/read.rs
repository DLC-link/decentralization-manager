//! SchemaRead for the SQLite pool.

use sqlx::SqlitePool;

use crate::db::{
    crypto,
    rows::{
        ChainAuditCacheRow, DecPartyContractRow, DecPartyIdentityRow, DecPartyOwnerRow,
        DecPartyParticipantRow, DecPartyRow, FormerDecPartyRow, GovernanceAuditRow,
        HandledInvitationRow, PartyCredentialsRow, PeerRow, PendingInvitationRow,
        WorkflowArtifactRow, WorkflowRunRow,
    },
    schema::SchemaRead,
};
use crate::{
    canton_id::CantonId,
    config::{PartyCredentials, Peer},
    error::Result,
    server::{HandledInvitation, PendingInvitation, WorkflowKind, WorkflowRole, WorkflowRun},
};

/// The newest audit rows across every party. Shared with the test that checks
/// it walks `idx_governance_audit_created` instead of sorting the table.
pub(super) const RECENT_GOVERNANCE_AUDIT_SQL: &str = r"
    SELECT * FROM governance_audit
    ORDER BY created_at DESC, id DESC
    LIMIT ? OFFSET ?
";

impl SchemaRead for SqlitePool {
    async fn get_all_peers(&self) -> Result<Vec<Peer>> {
        let rows = sqlx::query_as::<_, PeerRow>("SELECT * FROM peers ORDER BY name")
            .fetch_all(self)
            .await?;

        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    async fn get_peer_by_public_key(&self, public_key: &str) -> Result<Option<Peer>> {
        let row = sqlx::query_as::<_, PeerRow>("SELECT * FROM peers WHERE public_key = ?")
            .bind(public_key)
            .fetch_optional(self)
            .await?;

        row.map(|r| r.into_domain()).transpose()
    }

    async fn get_all_party_credentials(&self) -> Result<Vec<PartyCredentials>> {
        let rows = sqlx::query_as::<_, PartyCredentialsRow>("SELECT * FROM party_credentials")
            .fetch_all(self)
            .await?;

        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    async fn get_party_credentials(
        &self,
        dec_party_id: &CantonId,
    ) -> Result<Option<PartyCredentials>> {
        let row = sqlx::query_as::<_, PartyCredentialsRow>(
            "SELECT * FROM party_credentials WHERE dec_party_id = ?",
        )
        .bind(dec_party_id.to_string())
        .fetch_optional(self)
        .await?;

        row.map(|r| r.into_domain()).transpose()
    }

    async fn get_acs_import_quarantine(
        &self,
        party_id: &CantonId,
        participant_id: &CantonId,
    ) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT reason FROM acs_import_quarantine \
             WHERE party_id = ? AND participant_id = ?",
        )
        .bind(party_id.to_string())
        .bind(participant_id.to_string())
        .fetch_optional(self)
        .await?;
        Ok(row.map(|(reason,)| reason))
    }

    async fn get_former_dec_parties(&self) -> Result<Vec<FormerDecPartyRow>> {
        Ok(sqlx::query_as::<_, FormerDecPartyRow>(
            "SELECT * FROM former_dec_party WHERE dismissed_at IS NULL \
             ORDER BY removed_at, party_id",
        )
        .fetch_all(self)
        .await?)
    }

    async fn get_former_dec_party(&self, party_id: &CantonId) -> Result<Option<FormerDecPartyRow>> {
        Ok(sqlx::query_as::<_, FormerDecPartyRow>(
            "SELECT * FROM former_dec_party WHERE party_id = ?",
        )
        .bind(party_id.to_string())
        .fetch_optional(self)
        .await?)
    }

    async fn get_dec_parties_by_prefix(&self, prefix: &str) -> Result<Vec<DecPartyRow>> {
        let rows = if prefix.is_empty() {
            sqlx::query_as::<_, DecPartyRow>("SELECT * FROM dec_party")
                .fetch_all(self)
                .await?
        } else {
            sqlx::query_as::<_, DecPartyRow>("SELECT * FROM dec_party WHERE prefix = ?")
                .bind(prefix)
                .fetch_all(self)
                .await?
        };

        Ok(rows)
    }

    async fn get_dec_party_owners(&self, party_id: &CantonId) -> Result<Vec<DecPartyOwnerRow>> {
        let rows = sqlx::query_as::<_, DecPartyOwnerRow>(
            r"
            SELECT dec_party_id, owner_key, participant_uid
            FROM dec_party_owner WHERE dec_party_id = ?
            ",
        )
        .bind(party_id.to_string())
        .fetch_all(self)
        .await?;

        Ok(rows)
    }

    async fn get_dec_party_participants(
        &self,
        party_id: &CantonId,
    ) -> Result<Vec<DecPartyParticipantRow>> {
        let rows = sqlx::query_as::<_, DecPartyParticipantRow>(
            "SELECT * FROM dec_party_participant WHERE dec_party_id = ?",
        )
        .bind(party_id.to_string())
        .fetch_all(self)
        .await?;

        Ok(rows)
    }

    async fn get_dec_party_participant_owner_key(
        &self,
        party_id: &CantonId,
        participant_uid: &str,
    ) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            r"
            SELECT owner_key FROM dec_party_participant
            WHERE dec_party_id = ? AND participant_uid = ?
            ",
        )
        .bind(party_id.to_string())
        .bind(participant_uid)
        .fetch_optional(self)
        .await?;
        Ok(row.and_then(|(k,)| k))
    }

    async fn get_all_dec_party_owners(&self, prefix: &str) -> Result<Vec<DecPartyOwnerRow>> {
        let rows = if prefix.is_empty() {
            sqlx::query_as::<_, DecPartyOwnerRow>(
                r"
                SELECT o.dec_party_id, o.owner_key, o.participant_uid
                FROM dec_party_owner o
                INNER JOIN dec_party p ON p.party_id = o.dec_party_id
                ",
            )
            .fetch_all(self)
            .await?
        } else {
            sqlx::query_as::<_, DecPartyOwnerRow>(
                r"
                SELECT o.dec_party_id, o.owner_key, o.participant_uid
                FROM dec_party_owner o
                INNER JOIN dec_party p ON p.party_id = o.dec_party_id
                WHERE p.prefix = ?
                ",
            )
            .bind(prefix)
            .fetch_all(self)
            .await?
        };

        Ok(rows)
    }

    async fn get_all_dec_party_participants(
        &self,
        prefix: &str,
    ) -> Result<Vec<DecPartyParticipantRow>> {
        let rows = if prefix.is_empty() {
            sqlx::query_as::<_, DecPartyParticipantRow>(
                r"
                SELECT dp.*
                FROM dec_party_participant dp
                INNER JOIN dec_party p ON p.party_id = dp.dec_party_id
                ",
            )
            .fetch_all(self)
            .await?
        } else {
            sqlx::query_as::<_, DecPartyParticipantRow>(
                r"
                SELECT dp.*
                FROM dec_party_participant dp
                INNER JOIN dec_party p ON p.party_id = dp.dec_party_id
                WHERE p.prefix = ?
                ",
            )
            .bind(prefix)
            .fetch_all(self)
            .await?
        };

        Ok(rows)
    }

    async fn get_all_dec_party_contracts(&self, prefix: &str) -> Result<Vec<DecPartyContractRow>> {
        let rows = if prefix.is_empty() {
            sqlx::query_as::<_, DecPartyContractRow>(
                r"
                SELECT dc.*
                FROM dec_party_contract dc
                INNER JOIN dec_party p ON p.party_id = dc.dec_party_id
                ",
            )
            .fetch_all(self)
            .await?
        } else {
            sqlx::query_as::<_, DecPartyContractRow>(
                r"
                SELECT dc.*
                FROM dec_party_contract dc
                INNER JOIN dec_party p ON p.party_id = dc.dec_party_id
                WHERE p.prefix = ?
                ",
            )
            .bind(prefix)
            .fetch_all(self)
            .await?
        };

        Ok(rows)
    }

    async fn get_governance_audit(
        &self,
        party_id: &CantonId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<GovernanceAuditRow>> {
        let rows = sqlx::query_as::<_, GovernanceAuditRow>(
            r"
            SELECT * FROM governance_audit
            WHERE party_id = ?
            ORDER BY created_at DESC, id DESC
            LIMIT ? OFFSET ?
            ",
        )
        .bind(party_id.to_string())
        .bind(limit)
        .bind(offset)
        .fetch_all(self)
        .await?;

        Ok(rows)
    }

    async fn get_recent_governance_audit(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<GovernanceAuditRow>> {
        let rows = sqlx::query_as::<_, GovernanceAuditRow>(RECENT_GOVERNANCE_AUDIT_SQL)
            .bind(limit)
            .bind(offset)
            .fetch_all(self)
            .await?;

        Ok(rows)
    }

    async fn get_chain_audit_cache(
        &self,
        party_id: &CantonId,
        limit: i64,
        before_offset: Option<i64>,
    ) -> Result<Vec<ChainAuditCacheRow>> {
        // Selects whole offset groups for the newest `limit` distinct offsets
        // rather than the newest `limit` rows. One transaction can produce
        // several entries sharing an offset, and the cursor handed to the
        // client is an offset — a page cut mid-offset would make the next page
        // skip the remainder. Matches how the live path pages.
        let rows = sqlx::query_as::<_, ChainAuditCacheRow>(
            r"
            SELECT * FROM chain_audit_cache
            WHERE party_id = ?1
              AND (?2 IS NULL OR offset < ?2)
              AND offset >= (
                SELECT MIN(offset) FROM (
                  SELECT DISTINCT offset FROM chain_audit_cache
                  WHERE party_id = ?1
                    AND (?2 IS NULL OR offset < ?2)
                  ORDER BY offset DESC
                  LIMIT ?3
                )
              )
            ORDER BY offset DESC
            ",
        )
        .bind(party_id.to_string())
        .bind(before_offset)
        .bind(limit)
        .fetch_all(self)
        .await?;

        Ok(rows)
    }

    async fn get_all_pending_invitations(&self) -> Result<Vec<PendingInvitation>> {
        let rows = sqlx::query_as::<_, PendingInvitationRow>(
            "SELECT * FROM pending_invitations WHERE status = 'pending' ORDER BY received_at ASC",
        )
        .fetch_all(self)
        .await?;

        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    async fn get_handled_invitations(&self, limit: i64) -> Result<Vec<HandledInvitation>> {
        let rows = sqlx::query_as::<_, HandledInvitationRow>(
            r"
            SELECT * FROM pending_invitations
            WHERE status != 'pending' AND handled_at IS NOT NULL
            ORDER BY handled_at DESC, id ASC
            LIMIT ?
            ",
        )
        .bind(limit)
        .fetch_all(self)
        .await?;

        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    async fn get_in_progress_workflow_runs(&self) -> Result<Vec<WorkflowRun>> {
        let rows = sqlx::query_as::<_, WorkflowRunRow>(
            "SELECT * FROM workflow_runs WHERE status = 'inprogress' ORDER BY created_at ASC",
        )
        .fetch_all(self)
        .await?;

        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    async fn get_workflow_run(&self, instance_name: &str) -> Result<Option<WorkflowRun>> {
        let row = sqlx::query_as::<_, WorkflowRunRow>(
            "SELECT * FROM workflow_runs WHERE instance_name = ?",
        )
        .bind(instance_name)
        .fetch_optional(self)
        .await?;

        row.map(|r| r.into_domain()).transpose()
    }

    async fn get_active_workflow_run(
        &self,
        kind: WorkflowKind,
        role: WorkflowRole,
    ) -> Result<Option<WorkflowRun>> {
        // ORDER BY instance_name so that, with concurrent same-kind runs (the
        // per-kind unique index was dropped in migration 000013), this picks the
        // same run as `pick_coordinator_run` (lowest instance_name) — the
        // persisted-row cancel path must not target a different run than the feed.
        let row = sqlx::query_as::<_, WorkflowRunRow>(
            "SELECT * FROM workflow_runs \
             WHERE kind = ? AND role = ? AND status = 'inprogress' \
             ORDER BY instance_name \
             LIMIT 1",
        )
        .bind(kind.as_str())
        .bind(role.as_str())
        .fetch_optional(self)
        .await?;

        row.map(|r| r.into_domain()).transpose()
    }

    async fn get_visible_workflow_runs(&self) -> Result<Vec<WorkflowRun>> {
        let rows = sqlx::query_as::<_, WorkflowRunRow>(
            "SELECT * FROM workflow_runs \
             WHERE status = 'inprogress' OR dismissed = 0 \
             ORDER BY updated_at DESC",
        )
        .fetch_all(self)
        .await?;

        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    async fn read_workflow_artifact(
        &self,
        instance_name: &str,
        artifact_kind: &str,
        peer: Option<&str>,
    ) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, WorkflowArtifactRow>(
            "SELECT * FROM workflow_artifacts \
             WHERE instance_name = ? AND artifact_kind = ? AND peer_id = ?",
        )
        .bind(instance_name)
        .bind(artifact_kind)
        .bind(peer.unwrap_or(""))
        .fetch_optional(self)
        .await?;

        row.map(|r| crypto::decrypt_bytes(&r.payload)).transpose()
    }

    async fn list_workflow_artifacts(
        &self,
        instance_name: &str,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let rows = sqlx::query_as::<_, WorkflowArtifactRow>(
            "SELECT * FROM workflow_artifacts \
             WHERE instance_name = ? AND artifact_kind = ? \
             ORDER BY peer_id ASC",
        )
        .bind(instance_name)
        .bind(artifact_kind)
        .fetch_all(self)
        .await?;

        rows.into_iter()
            .map(|r| Ok((r.peer_id, crypto::decrypt_bytes(&r.payload)?)))
            .collect()
    }

    async fn read_dec_party_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
        peer_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, DecPartyIdentityRow>(
            "SELECT * FROM dec_party_identity \
             WHERE dec_party_id = ? AND artifact_kind = ? AND peer_id = ?",
        )
        .bind(dec_party_id.to_string())
        .bind(artifact_kind)
        .bind(peer_id)
        .fetch_optional(self)
        .await?;

        row.map(|r| crypto::decrypt_bytes(&r.payload)).transpose()
    }

    async fn list_dec_party_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let rows = sqlx::query_as::<_, DecPartyIdentityRow>(
            "SELECT * FROM dec_party_identity \
             WHERE dec_party_id = ? AND artifact_kind = ? \
             ORDER BY peer_id ASC",
        )
        .bind(dec_party_id.to_string())
        .bind(artifact_kind)
        .fetch_all(self)
        .await?;

        rows.into_iter()
            .map(|r| Ok((r.peer_id, crypto::decrypt_bytes(&r.payload)?)))
            .collect()
    }
}
