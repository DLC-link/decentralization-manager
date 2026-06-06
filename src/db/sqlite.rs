use std::{path::Path, str::FromStr, time::Duration};

use anyhow::Context;
use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use super::{
    crypto,
    rows::{
        ChainAuditCacheRow, DecPartyContractRow, DecPartyIdentityRow, DecPartyParticipantRow,
        DecPartyRow, GovernanceAuditRow, PartyCredentialsRow, PeerRow, PendingInvitationRow,
        WorkflowArtifactRow, WorkflowRunRow,
    },
    schema::{Commitable, SchemaRead, SchemaWrite},
};
use crate::{
    canton_id::CantonId,
    config::{PartyCredentials, Peer},
    error::Result,
    server::{
        InvitationType, PendingInvitation, WorkflowKind, WorkflowProgress, WorkflowRole,
        WorkflowRun,
    },
};

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Create a new SQLite connection pool
///
/// # Errors
///
/// Returns an error if the database file cannot be created or opened
pub async fn connect(db_path: &Path) -> Result<SqlitePool> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let db_url = format!("sqlite:{}", db_path.display());

    let options = SqliteConnectOptions::from_str(&db_url)?
        .create_if_missing(true)
        // Enforce foreign keys explicitly. The schema relies on
        // `ON DELETE CASCADE` (e.g. dec_party -> dec_party_owner/participant);
        // sqlx defaults this pragma on, but pinning it here means a future
        // driver/default change can't silently disable cascade enforcement.
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}

impl SchemaRead for SqlitePool {
    async fn get_all_peers(&self) -> Result<Vec<Peer>> {
        let rows = sqlx::query_as::<_, PeerRow>("SELECT * FROM peers ORDER BY name")
            .fetch_all(self)
            .await?;

        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    async fn get_peer(&self, participant_id: &str) -> Result<Option<Peer>> {
        let row = sqlx::query_as::<_, PeerRow>("SELECT * FROM peers WHERE participant_id = ?")
            .bind(participant_id)
            .fetch_optional(self)
            .await?;

        row.map(|r| r.into_domain()).transpose()
    }

    async fn get_peer_count(&self) -> Result<i64> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM peers")
            .fetch_one(self)
            .await?;

        Ok(count.0)
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

    async fn get_dec_party_owners(&self, party_id: &CantonId) -> Result<Vec<String>> {
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT owner_key FROM dec_party_owner WHERE dec_party_id = ?",
        )
        .bind(party_id.to_string())
        .fetch_all(self)
        .await?;

        Ok(rows.into_iter().map(|(k,)| k).collect())
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

    async fn get_dec_party_contracts(
        &self,
        party_id: &CantonId,
    ) -> Result<Vec<DecPartyContractRow>> {
        let rows = sqlx::query_as::<_, DecPartyContractRow>(
            "SELECT * FROM dec_party_contract WHERE dec_party_id = ?",
        )
        .bind(party_id.to_string())
        .fetch_all(self)
        .await?;

        Ok(rows)
    }

    async fn get_all_dec_party_owners(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let rows = if prefix.is_empty() {
            sqlx::query_as::<_, (String, String)>(
                r"
                SELECT o.dec_party_id, o.owner_key
                FROM dec_party_owner o
                INNER JOIN dec_party p ON p.party_id = o.dec_party_id
                ",
            )
            .fetch_all(self)
            .await?
        } else {
            sqlx::query_as::<_, (String, String)>(
                r"
                SELECT o.dec_party_id, o.owner_key
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
            ORDER BY created_at DESC
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

    async fn get_chain_audit_cache(
        &self,
        party_id: &CantonId,
        limit: i64,
    ) -> Result<Vec<ChainAuditCacheRow>> {
        let rows = sqlx::query_as::<_, ChainAuditCacheRow>(
            r"
            SELECT * FROM chain_audit_cache
            WHERE party_id = ?
            ORDER BY offset DESC
            LIMIT ?
            ",
        )
        .bind(party_id.to_string())
        .bind(limit)
        .fetch_all(self)
        .await?;

        Ok(rows)
    }

    async fn get_all_pending_invitations(&self) -> Result<Vec<PendingInvitation>> {
        let rows = sqlx::query_as::<_, PendingInvitationRow>(
            "SELECT * FROM pending_invitations ORDER BY received_at ASC",
        )
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
        let row = sqlx::query_as::<_, WorkflowRunRow>(
            "SELECT * FROM workflow_runs \
             WHERE kind = ? AND role = ? AND status = 'inprogress' \
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

impl SchemaWrite for SqlitePool {
    type Transaction = sqlx::Transaction<'static, sqlx::Sqlite>;

    async fn begin_transaction(&self) -> Result<Self::Transaction> {
        Ok(self.begin().await?)
    }
}

impl Commitable for sqlx::Transaction<'static, sqlx::Sqlite> {
    async fn commit(self) -> Result {
        Ok(self.commit().await?)
    }

    async fn delete_all_peers(&mut self) -> Result {
        sqlx::query("DELETE FROM peers")
            .execute(&mut **self)
            .await?;

        Ok(())
    }

    async fn insert_peer(&mut self, peer: &Peer) -> Result {
        let row = PeerRow::from_domain(peer);

        sqlx::query(
            r"
            INSERT INTO peers (
                participant_id,
                name,
                address,
                port,
                public_key,
                party
            ) VALUES (?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&row.participant_id)
        .bind(&row.name)
        .bind(&row.address)
        .bind(row.port)
        .bind(&row.public_key)
        .bind(&row.party)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn upsert_party_credentials(&mut self, creds: &PartyCredentials) -> Result {
        let row = PartyCredentialsRow::from_domain(creds)?;

        sqlx::query(
            r"
            INSERT OR REPLACE INTO party_credentials (
                dec_party_id,
                member_party_id,
                user_id,
                keycloak_url,
                keycloak_realm,
                keycloak_client_id,
                keycloak_client_secret,
                keycloak_username,
                keycloak_password,
                auth0_domain,
                auth0_audience,
                auth0_client_id,
                auth0_client_secret
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&row.dec_party_id)
        .bind(&row.member_party_id)
        .bind(&row.user_id)
        .bind(&row.keycloak_url)
        .bind(&row.keycloak_realm)
        .bind(&row.keycloak_client_id)
        .bind(&row.keycloak_client_secret)
        .bind(&row.keycloak_username)
        .bind(&row.keycloak_password)
        .bind(&row.auth0_domain)
        .bind(&row.auth0_audience)
        .bind(&row.auth0_client_id)
        .bind(&row.auth0_client_secret)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn upsert_dec_party(&mut self, row: &DecPartyRow) -> Result {
        // ON CONFLICT DO UPDATE preserves the dec_party row's identity, so the
        // ON DELETE CASCADE on dec_party_participant.dec_party_id does NOT fire.
        // Using INSERT OR REPLACE here would delete-and-reinsert the parent,
        // cascading the delete and wiping participant rows — defeating the
        // owner_key-preservation invariant established by
        // replace_dec_party_participants.
        sqlx::query(
            r"
            INSERT INTO dec_party (
                party_id,
                prefix,
                threshold,
                updated_at,
                my_owner_key
            ) VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(party_id) DO UPDATE SET
                prefix = excluded.prefix,
                threshold = excluded.threshold,
                updated_at = excluded.updated_at,
                my_owner_key = excluded.my_owner_key
            ",
        )
        .bind(&row.party_id)
        .bind(&row.prefix)
        .bind(row.threshold)
        .bind(row.updated_at)
        .bind(&row.my_owner_key)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn replace_dec_party_owners(&mut self, party_id: &CantonId, owners: &[String]) -> Result {
        let party_id_str = party_id.to_string();
        sqlx::query("DELETE FROM dec_party_owner WHERE dec_party_id = ?")
            .bind(&party_id_str)
            .execute(&mut **self)
            .await?;

        for owner in owners {
            sqlx::query(
                r"
                INSERT INTO dec_party_owner (dec_party_id, owner_key)
                VALUES (?, ?)
                ",
            )
            .bind(&party_id_str)
            .bind(owner)
            .execute(&mut **self)
            .await?;
        }

        Ok(())
    }

    async fn replace_dec_party_participants(
        &mut self,
        party_id: &CantonId,
        participants: &[DecPartyParticipantRow],
    ) -> Result {
        let party_id_str = party_id.to_string();
        // UPSERT each fresh row. permission may change (e.g., submission ->
        // confirmation); owner_key only ever transitions NULL -> Some, never
        // back to NULL. COALESCE keeps a previously-known fingerprint when
        // the live Canton fetch carries None for it.
        for p in participants {
            sqlx::query(
                r"
                INSERT INTO dec_party_participant (
                    dec_party_id,
                    participant_uid,
                    permission,
                    owner_key
                ) VALUES (?, ?, ?, ?)
                ON CONFLICT(dec_party_id, participant_uid) DO UPDATE SET
                    permission = excluded.permission,
                    owner_key = COALESCE(excluded.owner_key, dec_party_participant.owner_key)
                ",
            )
            .bind(&party_id_str)
            .bind(&p.participant_uid)
            .bind(&p.permission)
            .bind(&p.owner_key)
            .execute(&mut **self)
            .await?;
        }

        // Delete rows for this party that aren't in the fresh set (a
        // participant left the party).
        let fresh_uids: Vec<&str> = participants
            .iter()
            .map(|p| p.participant_uid.as_str())
            .collect();
        if fresh_uids.is_empty() {
            sqlx::query("DELETE FROM dec_party_participant WHERE dec_party_id = ?")
                .bind(&party_id_str)
                .execute(&mut **self)
                .await?;
        } else {
            let placeholders = fresh_uids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let query = format!(
                "DELETE FROM dec_party_participant WHERE dec_party_id = ? \
                 AND participant_uid NOT IN ({placeholders})"
            );
            let mut q = sqlx::query(&query).bind(&party_id_str);
            for uid in fresh_uids {
                q = q.bind(uid);
            }
            q.execute(&mut **self).await?;
        }

        Ok(())
    }

    async fn replace_dec_party_contracts(
        &mut self,
        party_id: &CantonId,
        contracts: &[DecPartyContractRow],
    ) -> Result {
        let party_id_str = party_id.to_string();
        sqlx::query("DELETE FROM dec_party_contract WHERE dec_party_id = ?")
            .bind(&party_id_str)
            .execute(&mut **self)
            .await?;

        for c in contracts {
            sqlx::query(
                r"
                INSERT INTO dec_party_contract (
                    dec_party_id,
                    contract_id,
                    template_id,
                    package_id,
                    package_name,
                    package_version,
                    created_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                ",
            )
            .bind(&party_id_str)
            .bind(&c.contract_id)
            .bind(&c.template_id)
            .bind(&c.package_id)
            .bind(&c.package_name)
            .bind(&c.package_version)
            .bind(&c.created_at)
            .execute(&mut **self)
            .await?;
        }

        Ok(())
    }

    async fn delete_dec_parties_by_prefix(&mut self, prefix: &str) -> Result {
        if prefix.is_empty() {
            sqlx::query("DELETE FROM dec_party")
                .execute(&mut **self)
                .await?;
        } else {
            sqlx::query("DELETE FROM dec_party WHERE prefix = ?")
                .bind(prefix)
                .execute(&mut **self)
                .await?;
        }

        Ok(())
    }

    async fn delete_stale_dec_parties(&mut self, prefix: &str, fresh_ids: &[String]) -> Result {
        if fresh_ids.is_empty() {
            // No fresh parties — delete all for this prefix
            self.delete_dec_parties_by_prefix(prefix).await?;
            return Ok(());
        }

        // Build placeholders for the IN clause
        let placeholders: Vec<&str> = fresh_ids.iter().map(|_| "?").collect();
        let in_clause = placeholders.join(",");

        if prefix.is_empty() {
            let query = format!("DELETE FROM dec_party WHERE party_id NOT IN ({in_clause})");
            let mut q = sqlx::query(&query);
            for id in fresh_ids {
                q = q.bind(id);
            }
            q.execute(&mut **self).await?;
        } else {
            let query =
                format!("DELETE FROM dec_party WHERE prefix = ? AND party_id NOT IN ({in_clause})");
            let mut q = sqlx::query(&query);
            q = q.bind(prefix);
            for id in fresh_ids {
                q = q.bind(id);
            }
            q.execute(&mut **self).await?;
        }

        Ok(())
    }

    async fn update_participant_owner_key(
        &mut self,
        party_id: &CantonId,
        participant_uid: &str,
        owner_key: &str,
    ) -> Result {
        sqlx::query(
            r"
            UPDATE dec_party_participant
            SET owner_key = ?
            WHERE dec_party_id = ? AND participant_uid = ?
            ",
        )
        .bind(owner_key)
        .bind(party_id.to_string())
        .bind(participant_uid)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn upsert_pending_invitation(&mut self, inv: &PendingInvitation) -> Result {
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
                kicked_participant,
                new_threshold,
                previous_threshold,
                dec_party_id,
                package_names,
                workflow_instance
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&row.id)
        .bind(&row.invitation_type)
        .bind(&row.coordinator_pubkey)
        .bind(row.received_at)
        .bind(&row.prefix)
        .bind(&row.participants)
        .bind(&row.dar_filenames)
        .bind(&row.kicked_participant)
        .bind(row.new_threshold)
        .bind(row.previous_threshold)
        .bind(&row.dec_party_id)
        .bind(&row.package_names)
        .bind(&row.workflow_instance)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn delete_pending_invitation(&mut self, id: &str) -> Result {
        sqlx::query("DELETE FROM pending_invitations WHERE id = ?")
            .bind(id)
            .execute(&mut **self)
            .await?;

        Ok(())
    }

    async fn delete_pending_invitations_by_coordinator(
        &mut self,
        coordinator_pubkey: &str,
    ) -> Result {
        sqlx::query("DELETE FROM pending_invitations WHERE coordinator_pubkey = ?")
            .bind(coordinator_pubkey)
            .execute(&mut **self)
            .await?;

        Ok(())
    }

    async fn delete_pending_invitations_by_type_and_coordinator(
        &mut self,
        invitation_type: InvitationType,
        coordinator_pubkey: &str,
    ) -> Result {
        sqlx::query(
            "DELETE FROM pending_invitations WHERE invitation_type = ? AND coordinator_pubkey = ?",
        )
        .bind(invitation_type.to_string())
        .bind(coordinator_pubkey)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn upsert_workflow_run(&mut self, run: &WorkflowRun) -> Result {
        let row = WorkflowRunRow::from_domain(run)?;

        // ON CONFLICT(instance_name) so re-saving the same run (resume,
        // step advance, etc.) replaces in place. Conflicts on the partial
        // unique index `idx_workflow_runs_inprogress_per_kind` propagate as
        // an error — that's what enforces "one InProgress run per (kind, role)".
        sqlx::query(
            r"
            INSERT INTO workflow_runs (
                instance_name,
                kind,
                role,
                status,
                current_step,
                step_index,
                step_total,
                config_json,
                coordinator_pubkey,
                expected_peers_json,
                completed_peers_json,
                dec_party_id,
                error,
                dismissed,
                created_at,
                updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(instance_name) DO UPDATE SET
                kind                     = excluded.kind,
                role                     = excluded.role,
                status                   = excluded.status,
                current_step             = excluded.current_step,
                step_index               = excluded.step_index,
                step_total               = excluded.step_total,
                config_json              = excluded.config_json,
                coordinator_pubkey       = excluded.coordinator_pubkey,
                expected_peers_json  = excluded.expected_peers_json,
                completed_peers_json = excluded.completed_peers_json,
                dec_party_id             = excluded.dec_party_id,
                error                    = excluded.error,
                dismissed                = excluded.dismissed,
                updated_at               = excluded.updated_at
            ",
        )
        .bind(&row.instance_name)
        .bind(&row.kind)
        .bind(&row.role)
        .bind(&row.status)
        .bind(&row.current_step)
        .bind(row.step_index)
        .bind(row.step_total)
        .bind(&row.config_json)
        .bind(&row.coordinator_pubkey)
        .bind(&row.expected_peers_json)
        .bind(&row.completed_peers_json)
        .bind(&row.dec_party_id)
        .bind(&row.error)
        .bind(row.dismissed)
        .bind(row.created_at)
        .bind(row.updated_at)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn update_workflow_run_step(
        &mut self,
        instance_name: &str,
        current_step: &str,
        step_index: i64,
        completed_peers: &[CantonId],
        updated_at: i64,
    ) -> Result {
        let completed_json =
            serde_json::to_string(completed_peers).context("encode completed_peers")?;

        sqlx::query(
            r"
            UPDATE workflow_runs
            SET current_step = ?,
                step_index = ?,
                completed_peers_json = ?,
                updated_at = ?
            WHERE instance_name = ?
            ",
        )
        .bind(current_step)
        .bind(step_index)
        .bind(&completed_json)
        .bind(updated_at)
        .bind(instance_name)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn set_workflow_run_status(
        &mut self,
        instance_name: &str,
        status: WorkflowProgress,
        error: Option<&str>,
        updated_at: i64,
    ) -> Result {
        let status_str = match status {
            WorkflowProgress::Idle => "idle",
            WorkflowProgress::InProgress => "inprogress",
            WorkflowProgress::Completed => "completed",
            WorkflowProgress::Failed => "failed",
            WorkflowProgress::Cancelled => "cancelled",
        };

        sqlx::query(
            r"
            UPDATE workflow_runs
            SET status = ?,
                error = ?,
                updated_at = ?
            WHERE instance_name = ?
            ",
        )
        .bind(status_str)
        .bind(error)
        .bind(updated_at)
        .bind(instance_name)
        .execute(&mut **self)
        .await?;

        // Clean up `workflow_artifacts` rows when the run reaches a clean
        // terminal state — they're transient working data, the long-lived
        // identity material lives in `dec_party_identity`. Failed runs keep
        // their artefacts so the operator can post-mortem before dismissing.
        if matches!(
            status,
            WorkflowProgress::Completed | WorkflowProgress::Cancelled
        ) {
            sqlx::query("DELETE FROM workflow_artifacts WHERE instance_name = ?")
                .bind(instance_name)
                .execute(&mut **self)
                .await?;
        }

        Ok(())
    }

    async fn dismiss_workflow_run(&mut self, instance_name: &str) -> Result {
        sqlx::query(
            r"
            UPDATE workflow_runs
            SET dismissed = 1
            WHERE instance_name = ? AND status != 'inprogress'
            ",
        )
        .bind(instance_name)
        .execute(&mut **self)
        .await?;

        // Drop any leftover artefacts. Completed/Cancelled runs already had
        // their artefacts cleared at terminal time; Failed runs kept theirs
        // for post-mortem and now release them on dismiss.
        sqlx::query("DELETE FROM workflow_artifacts WHERE instance_name = ?")
            .bind(instance_name)
            .execute(&mut **self)
            .await?;

        Ok(())
    }

    async fn write_workflow_artifact(
        &mut self,
        instance_name: &str,
        artifact_kind: &str,
        peer: Option<&str>,
        payload: &[u8],
    ) -> Result {
        let encrypted = crypto::encrypt_bytes(payload)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        sqlx::query(
            r"
            INSERT OR REPLACE INTO workflow_artifacts
                (instance_name, artifact_kind, peer_id, payload, created_at)
            VALUES (?, ?, ?, ?, ?)
            ",
        )
        .bind(instance_name)
        .bind(artifact_kind)
        .bind(peer.unwrap_or(""))
        .bind(&encrypted)
        .bind(now)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn write_dec_party_identity(
        &mut self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
        peer_id: &str,
        payload: &[u8],
    ) -> Result {
        let encrypted = crypto::encrypt_bytes(payload)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        sqlx::query(
            r"
            INSERT OR REPLACE INTO dec_party_identity
                (dec_party_id, artifact_kind, peer_id, payload, created_at)
            VALUES (?, ?, ?, ?, ?)
            ",
        )
        .bind(dec_party_id.to_string())
        .bind(artifact_kind)
        .bind(peer_id)
        .bind(&encrypted)
        .bind(now)
        .execute(&mut **self)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use crate::{
        canton_id::CantonId,
        config::{Auth0M2MConfig, KeycloakConfig, PackageConfig, PartyCredentials, Peer},
        db::{
            rows::{DecPartyContractRow, DecPartyParticipantRow, DecPartyRow},
            schema::{Commitable, SchemaRead, SchemaWrite},
        },
        error::Result,
        server::{
            InvitationType, PendingInvitation, WorkflowKind, WorkflowProgress, WorkflowRole,
            WorkflowRun,
        },
    };

    use super::MIGRATOR;

    const TEST_NS: &str = "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn test_peer(index: u8) -> Peer {
        let ns = format!("1220{:0>64}", format!("{index:02x}"));
        Peer {
            participant_id: CantonId::parse(&format!("node{index}::{ns}")).unwrap(),
            name: format!("Node {index}"),
            address: format!("10.0.0.{index}"),
            port: 9000 + index as u16,
            public_key: format!("03{index:02x}abcdef"),
            party: None,
        }
    }

    fn test_creds(prefix: &str) -> PartyCredentials {
        PartyCredentials {
            dec_party_id: CantonId::parse(&format!("{prefix}::{TEST_NS}")).unwrap(),
            member_party_id: CantonId::parse(&format!("member::{TEST_NS}")).unwrap(),
            user_id: "test-user".to_string(),
            keycloak: KeycloakConfig {
                url: "https://kc.example.com".to_string(),
                realm: "test".to_string(),
                client_id: "client-1".to_string(),
                client_secret: Some("secret".to_string()),
                username: None,
                password: None,
            },
            auth0: None,
            packages: PackageConfig {
                governance_action: Some("#gov-action".to_string()),
                governance_core: Some("#gov-core".to_string()),
                governance_token_custody: None,
                governance_utility_credential: None,
                governance_utility_onboarding: None,
                utility_credential: None,
                utility_registry: None,
                vault: None,
                vault_governance: None,
            },
        }
    }

    fn test_dec_party(prefix: &str) -> DecPartyRow {
        DecPartyRow {
            party_id: format!("{prefix}::{TEST_NS}"),
            prefix: prefix.to_string(),
            threshold: 2,
            updated_at: 1000,
            my_owner_key: None,
        }
    }

    // ====================================================================
    // Peers
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_insert_and_get_peers(pool: SqlitePool) -> Result {
        assert_eq!(pool.get_peer_count().await?, 0);

        let mut tx = pool.begin_transaction().await?;
        tx.insert_peer(&test_peer(1)).await?;
        tx.insert_peer(&test_peer(2)).await?;
        Commitable::commit(tx).await?;

        assert_eq!(pool.get_peer_count().await?, 2);

        let peers = pool.get_all_peers().await?;
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].name, "Node 1");
        assert_eq!(peers[1].port, 9002);

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_peer_by_id(pool: SqlitePool) -> Result {
        let peer = test_peer(1);
        let id = peer.participant_id.to_string();

        let mut tx = pool.begin_transaction().await?;
        tx.insert_peer(&peer).await?;
        Commitable::commit(tx).await?;

        assert!(pool.get_peer(&id).await?.is_some());
        assert!(pool.get_peer("nonexistent::1220bb").await?.is_none());

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_peer_by_public_key(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.insert_peer(&test_peer(1)).await?;
        tx.insert_peer(&test_peer(2)).await?;
        Commitable::commit(tx).await?;

        let found = pool.get_peer_by_public_key("0301abcdef").await?;
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "Node 1");

        assert!(pool.get_peer_by_public_key("nonexistent").await?.is_none());

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_delete_all_peers(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.insert_peer(&test_peer(1)).await?;
        tx.insert_peer(&test_peer(2)).await?;
        Commitable::commit(tx).await?;

        assert_eq!(pool.get_peer_count().await?, 2);

        let mut tx = pool.begin_transaction().await?;
        tx.delete_all_peers().await?;
        Commitable::commit(tx).await?;

        assert_eq!(pool.get_peer_count().await?, 0);

        Ok(())
    }

    // ====================================================================
    // Party Credentials
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_upsert_and_get_party_credentials(pool: SqlitePool) -> Result {
        let creds = test_creds("party-a");

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_party_credentials(&creds).await?;
        Commitable::commit(tx).await?;

        let all = pool.get_all_party_credentials().await?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].user_id, "test-user");
        assert_eq!(all[0].keycloak.client_secret, Some("secret".to_string()));

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_upsert_party_credentials_updates(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_party_credentials(&test_creds("party-a")).await?;
        Commitable::commit(tx).await?;

        let mut updated = test_creds("party-a");
        updated.user_id = "updated-user".to_string();
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_party_credentials(&updated).await?;
        Commitable::commit(tx).await?;

        let all = pool.get_all_party_credentials().await?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].user_id, "updated-user");

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_party_credentials_by_id(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_party_credentials(&test_creds("party-a")).await?;
        tx.upsert_party_credentials(&test_creds("party-b")).await?;
        Commitable::commit(tx).await?;

        let dec_id = CantonId::parse(&format!("party-a::{TEST_NS}")).unwrap();
        assert!(pool.get_party_credentials(&dec_id).await?.is_some());
        let nonexistent = CantonId::parse(&format!("nonexistent::{TEST_NS}")).unwrap();
        assert!(pool.get_party_credentials(&nonexistent).await?.is_none());

        Ok(())
    }

    // ====================================================================
    // Decentralized Parties
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_insert_and_get_dec_parties(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        tx.upsert_dec_party(&test_dec_party("net-b")).await?;
        Commitable::commit(tx).await?;

        let all = pool.get_dec_parties_by_prefix("").await?;
        assert_eq!(all.len(), 2);

        let filtered = pool.get_dec_parties_by_prefix("net-a").await?;
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].threshold, 2);

        assert!(
            pool.get_dec_parties_by_prefix("nonexistent")
                .await?
                .is_empty()
        );

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_dec_party_owners(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();
        let owners = vec!["owner-key-1".to_string(), "owner-key-2".to_string()];
        tx.replace_dec_party_owners(&party_id, &owners).await?;
        Commitable::commit(tx).await?;

        let result = pool.get_dec_party_owners(&party_id).await?;
        assert_eq!(result.len(), 2);
        assert!(result.contains(&"owner-key-1".to_string()));
        assert!(result.contains(&"owner-key-2".to_string()));

        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_owners(&party_id, &["only-owner".to_string()])
            .await?;
        Commitable::commit(tx).await?;

        let result = pool.get_dec_party_owners(&party_id).await?;
        assert_eq!(result.len(), 1);

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_dec_party_participants(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();
        let participants = vec![
            DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fingerprint-1".to_string()),
            },
            DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node2::1220bb".to_string(),
                permission: "confirmation".to_string(),
                owner_key: None,
            },
        ];
        tx.replace_dec_party_participants(&party_id, &participants)
            .await?;
        Commitable::commit(tx).await?;

        let result = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(result.len(), 2);
        // Assert by participant_uid, not positional index: the backing query
        // has no ORDER BY, so row order is not contractual and must not be
        // relied on.
        let node1 = result.iter().find(|p| p.participant_uid == "node1::1220aa");
        let node2 = result.iter().find(|p| p.participant_uid == "node2::1220bb");
        assert!(
            matches!(node1, Some(p) if p.permission == "submission"
                && p.owner_key.as_deref() == Some("fingerprint-1")),
            "node1 row missing or has wrong permission/owner_key"
        );
        assert!(
            matches!(node2, Some(p) if p.permission == "confirmation" && p.owner_key.is_none()),
            "node2 row missing or has wrong permission/owner_key"
        );

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_replace_preserves_owner_key_when_incoming_is_null(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();

        // First write — owner_key is known.
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fingerprint-1".to_string()),
            }],
        )
        .await?;
        Commitable::commit(tx).await?;

        // Second write — same row, owner_key NULL (simulates a cache refresh
        // that hasn't yet been followed by `resolve_owner_keys_from_peers`).
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: None,
            }],
        )
        .await?;
        Commitable::commit(tx).await?;

        let result = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].owner_key,
            Some("fingerprint-1".to_string()),
            "owner_key should be preserved across a refresh that brings a NULL value"
        );

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_upsert_dec_party_preserves_participant_rows(pool: SqlitePool) -> Result {
        // Regression for the cascading-delete bug: INSERT OR REPLACE on the
        // parent `dec_party` row would fire ON DELETE CASCADE on
        // dec_party_participant, wiping owner_key rows before the participant
        // UPSERT could COALESCE them back.
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fingerprint-1".to_string()),
            }],
        )
        .await?;
        Commitable::commit(tx).await?;

        // Re-upsert the parent dec_party row. With ON CONFLICT DO UPDATE
        // (not INSERT OR REPLACE), this must NOT cascade-delete the
        // participant row.
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        Commitable::commit(tx).await?;

        let result = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(
            result.len(),
            1,
            "participant row was wiped by cascading delete on parent upsert — \
             INSERT OR REPLACE regression"
        );
        assert_eq!(
            result[0].owner_key,
            Some("fingerprint-1".to_string()),
            "owner_key was wiped despite participant row surviving"
        );

        // Now run the full sequence: upsert dec_party + replace participants
        // with NULL owner_key. End-to-end this is what `store_parties_to_db`
        // does on every cache refresh.
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: None,
            }],
        )
        .await?;
        Commitable::commit(tx).await?;

        let result = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].owner_key,
            Some("fingerprint-1".to_string()),
            "owner_key not preserved through the full upsert-party + \
             replace-participants sequence"
        );

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_replace_dec_party_participants_removes_stale(pool: SqlitePool) -> Result {
        // Covers the `NOT IN` delete branch and the empty-list branch of
        // `replace_dec_party_participants` — i.e. participants that have left
        // the party must be removed from the cache.
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();
        let p1 = "node1::1220aa";
        let p2 = "node2::1220bb";
        tx.replace_dec_party_participants(
            &party_id,
            &[
                DecPartyParticipantRow {
                    dec_party_id: party_id_str.clone(),
                    participant_uid: p1.to_string(),
                    permission: "submission".to_string(),
                    owner_key: Some("fp-1".to_string()),
                },
                DecPartyParticipantRow {
                    dec_party_id: party_id_str.clone(),
                    participant_uid: p2.to_string(),
                    permission: "submission".to_string(),
                    owner_key: Some("fp-2".to_string()),
                },
            ],
        )
        .await?;
        Commitable::commit(tx).await?;
        assert_eq!(pool.get_dec_party_participants(&party_id).await?.len(), 2);

        // Replace with only p1 — p2 must be removed by the NOT IN branch.
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: p1.to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fp-1".to_string()),
            }],
        )
        .await?;
        Commitable::commit(tx).await?;
        let remaining = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].participant_uid, p1);

        // Replace with empty list — p1 must be removed by the empty-list branch.
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(&party_id, &[]).await?;
        Commitable::commit(tx).await?;
        assert!(pool.get_dec_party_participants(&party_id).await?.is_empty());

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_dec_party_participant_owner_key(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();
        tx.replace_dec_party_participants(
            &party_id,
            &[
                DecPartyParticipantRow {
                    dec_party_id: party_id_str.clone(),
                    participant_uid: "node1::1220aa".to_string(),
                    permission: "submission".to_string(),
                    owner_key: Some("fingerprint-1".to_string()),
                },
                DecPartyParticipantRow {
                    dec_party_id: party_id_str.clone(),
                    participant_uid: "node2::1220bb".to_string(),
                    permission: "confirmation".to_string(),
                    owner_key: None,
                },
            ],
        )
        .await?;
        Commitable::commit(tx).await?;

        assert_eq!(
            pool.get_dec_party_participant_owner_key(&party_id, "node1::1220aa")
                .await?,
            Some("fingerprint-1".to_string())
        );
        assert_eq!(
            pool.get_dec_party_participant_owner_key(&party_id, "node2::1220bb")
                .await?,
            None
        );
        assert_eq!(
            pool.get_dec_party_participant_owner_key(&party_id, "missing::1220cc")
                .await?,
            None
        );

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_dec_party_contracts(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();
        let contracts = vec![
            DecPartyContractRow {
                dec_party_id: party_id_str.clone(),
                contract_id: "contract-1".to_string(),
                template_id: "Governance:GovernanceRules".to_string(),
                package_id: "#gov-core".to_string(),
                package_name: "governance-core-v0-rc3".to_string(),
                package_version: "0.1.0".to_string(),
                created_at: "2026-04-28T11:07:59.073177Z".to_string(),
            },
            DecPartyContractRow {
                dec_party_id: party_id_str.clone(),
                contract_id: "contract-2".to_string(),
                template_id: "Vault:Vault".to_string(),
                package_id: "#vault".to_string(),
                package_name: "vault".to_string(),
                package_version: "0.1.0".to_string(),
                created_at: "2026-04-28T11:08:00.000000Z".to_string(),
            },
        ];
        tx.replace_dec_party_contracts(&party_id, &contracts)
            .await?;
        Commitable::commit(tx).await?;

        let result = pool.get_dec_party_contracts(&party_id).await?;
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].template_id, "Governance:GovernanceRules");

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_delete_dec_parties_cascades(pool: SqlitePool) -> Result {
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str).unwrap();

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        tx.replace_dec_party_owners(&party_id, &["owner-1".to_string()])
            .await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node1".to_string(),
                permission: "submission".to_string(),
                owner_key: None,
            }],
        )
        .await?;
        tx.replace_dec_party_contracts(
            &party_id,
            &[DecPartyContractRow {
                dec_party_id: party_id_str.clone(),
                contract_id: "c1".to_string(),
                template_id: "t1".to_string(),
                package_id: "p1".to_string(),
                package_name: String::new(),
                package_version: String::new(),
                created_at: String::new(),
            }],
        )
        .await?;
        Commitable::commit(tx).await?;

        assert_eq!(pool.get_dec_party_owners(&party_id).await?.len(), 1);
        assert_eq!(pool.get_dec_party_participants(&party_id).await?.len(), 1);
        assert_eq!(pool.get_dec_party_contracts(&party_id).await?.len(), 1);

        let mut tx = pool.begin_transaction().await?;
        tx.delete_dec_parties_by_prefix("net-a").await?;
        Commitable::commit(tx).await?;

        assert!(pool.get_dec_parties_by_prefix("net-a").await?.is_empty());
        assert!(pool.get_dec_party_owners(&party_id).await?.is_empty());
        assert!(pool.get_dec_party_participants(&party_id).await?.is_empty());
        assert!(pool.get_dec_party_contracts(&party_id).await?.is_empty());

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_delete_all_dec_parties(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        tx.upsert_dec_party(&test_dec_party("net-b")).await?;
        Commitable::commit(tx).await?;

        assert_eq!(pool.get_dec_parties_by_prefix("").await?.len(), 2);

        let mut tx = pool.begin_transaction().await?;
        tx.delete_dec_parties_by_prefix("").await?;
        Commitable::commit(tx).await?;

        assert!(pool.get_dec_parties_by_prefix("").await?.is_empty());

        Ok(())
    }

    // ====================================================================
    // Governance Audit
    // ====================================================================

    async fn insert_audit_entry(
        pool: &SqlitePool,
        party_id: &str,
        event_type: &str,
        status: &str,
        created_at: i64,
    ) -> Result {
        sqlx::query(
            r"
            INSERT INTO governance_audit (
                timestamp, event_type, party_id, member_party_id,
                governance_type, action_summary, details, status,
                error_message, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(created_at)
        .bind(event_type)
        .bind(party_id)
        .bind("member::1220aa")
        .bind("vault")
        .bind("governance_add_member")
        .bind(r#"{"type":"governance_add_member"}"#)
        .bind(status)
        .bind(None::<String>)
        .bind(created_at)
        .execute(pool)
        .await?;

        Ok(())
    }

    fn test_party_id(prefix: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::{TEST_NS}")).unwrap()
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_governance_audit_insert_and_query(pool: SqlitePool) -> Result {
        let party_id = test_party_id("party-a");
        let party_id_str = party_id.to_string();

        // Empty initially
        let entries = pool.get_governance_audit(&party_id, 50, 0).await?;
        assert!(entries.is_empty());

        // Insert entries
        insert_audit_entry(&pool, &party_id_str, "propose", "success", 1000).await?;
        insert_audit_entry(&pool, &party_id_str, "confirm", "success", 2000).await?;
        insert_audit_entry(&pool, &party_id_str, "execute", "failed", 3000).await?;

        // Query all
        let entries = pool.get_governance_audit(&party_id, 50, 0).await?;
        assert_eq!(entries.len(), 3);
        // Newest first
        assert_eq!(entries[0].event_type, "execute");
        assert_eq!(entries[1].event_type, "confirm");
        assert_eq!(entries[2].event_type, "propose");

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_governance_audit_pagination(pool: SqlitePool) -> Result {
        let party_id = test_party_id("party-a");
        let party_id_str = party_id.to_string();

        for i in 0..5 {
            insert_audit_entry(&pool, &party_id_str, "confirm", "success", 1000 + i).await?;
        }

        // Limit
        let entries = pool.get_governance_audit(&party_id, 2, 0).await?;
        assert_eq!(entries.len(), 2);

        // Offset
        let entries = pool.get_governance_audit(&party_id, 2, 3).await?;
        assert_eq!(entries.len(), 2);

        // Beyond end
        let entries = pool.get_governance_audit(&party_id, 50, 5).await?;
        assert!(entries.is_empty());

        Ok(())
    }

    // ====================================================================
    // Pending invitations
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_pending_invitations_roundtrip(pool: SqlitePool) -> Result {
        assert!(pool.get_all_pending_invitations().await?.is_empty());

        let inv_a = PendingInvitation {
            id: "onboarding-aaaaaaaaaaaaaaaa".to_string(),
            invitation_type: InvitationType::Onboarding,
            coordinator_pubkey: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            coordinator_name: None,
            received_at: 1000,
            prefix: Some("my-party".to_string()),
            participants: vec![
                CantonId::parse(&format!("node1::{TEST_NS}")).unwrap(),
                CantonId::parse(&format!("node2::{TEST_NS}")).unwrap(),
            ],
            dar_filenames: Vec::new(),
            kicked_participant: None,
            new_threshold: None,
            previous_threshold: None,
            dec_party_id: None,
            package_names: Vec::new(),
            workflow_instance: Some("my-party-creation".to_string()),
        };
        let inv_b = PendingInvitation {
            id: "kick-bbbbbbbbbbbbbbbb".to_string(),
            invitation_type: InvitationType::Kick,
            coordinator_pubkey: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            coordinator_name: None,
            received_at: 2000,
            prefix: None,
            participants: Vec::new(),
            dar_filenames: Vec::new(),
            kicked_participant: Some(CantonId::parse(&format!("kicked::{TEST_NS}")).unwrap()),
            new_threshold: Some(2),
            previous_threshold: Some(3),
            dec_party_id: Some(CantonId::parse(&format!("dec::{TEST_NS}")).unwrap()),
            package_names: Vec::new(),
            workflow_instance: None,
        };

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_pending_invitation(&inv_a).await?;
        tx.upsert_pending_invitation(&inv_b).await?;
        Commitable::commit(tx).await?;

        let inv_c = PendingInvitation {
            id: "dars-cccccccccccccccc".to_string(),
            invitation_type: InvitationType::Dars,
            coordinator_pubkey: "cccccccccccccccccccccccccccccccc".to_string(),
            coordinator_name: None,
            received_at: 3000,
            prefix: None,
            participants: Vec::new(),
            dar_filenames: vec!["app.dar".to_string(), "lib.dar".to_string()],
            kicked_participant: None,
            new_threshold: None,
            previous_threshold: None,
            dec_party_id: None,
            package_names: Vec::new(),
            workflow_instance: None,
        };
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_pending_invitation(&inv_c).await?;
        Commitable::commit(tx).await?;

        let loaded = pool.get_all_pending_invitations().await?;
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].id, inv_a.id);
        assert_eq!(loaded[0].prefix.as_deref(), Some("my-party"));
        assert_eq!(loaded[0].participants.len(), 2);
        assert_eq!(loaded[1].invitation_type, InvitationType::Kick);
        assert!(loaded[1].prefix.is_none());
        assert!(loaded[1].participants.is_empty());
        assert_eq!(loaded[2].invitation_type, InvitationType::Dars);
        assert_eq!(loaded[2].dar_filenames, vec!["app.dar", "lib.dar"]);

        let mut tx = pool.begin_transaction().await?;
        tx.delete_pending_invitation(&inv_a.id).await?;
        Commitable::commit(tx).await?;
        assert_eq!(pool.get_all_pending_invitations().await?.len(), 2);

        let mut tx = pool.begin_transaction().await?;
        tx.delete_pending_invitations_by_coordinator(&inv_b.coordinator_pubkey)
            .await?;
        Commitable::commit(tx).await?;
        assert_eq!(pool.get_all_pending_invitations().await?.len(), 1);

        let mut tx = pool.begin_transaction().await?;
        tx.delete_pending_invitations_by_coordinator(&inv_c.coordinator_pubkey)
            .await?;
        Commitable::commit(tx).await?;
        assert!(pool.get_all_pending_invitations().await?.is_empty());

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_contracts_pending_invitation_roundtrip(pool: SqlitePool) -> Result {
        // A Contracts invite carries dec party + member set + package names so
        // the peer card is as rich as the coordinator's. Confirm all three
        // round-trip through the DB (validates migration 000011).
        let inv = PendingInvitation {
            id: "contracts-dddddddddddddddd".to_string(),
            invitation_type: InvitationType::Contracts,
            coordinator_pubkey: "dddddddddddddddddddddddddddddddd".to_string(),
            coordinator_name: None,
            received_at: 4000,
            prefix: None,
            participants: vec![
                CantonId::parse(&format!("node1::{TEST_NS}")).unwrap(),
                CantonId::parse(&format!("node2::{TEST_NS}")).unwrap(),
            ],
            dar_filenames: Vec::new(),
            kicked_participant: None,
            new_threshold: None,
            previous_threshold: None,
            dec_party_id: Some(CantonId::parse(&format!("dec::{TEST_NS}")).unwrap()),
            package_names: vec!["Governance Core".to_string(), "Token Custody".to_string()],
            workflow_instance: Some("dec-contracts-4000".to_string()),
        };

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_pending_invitation(&inv).await?;
        Commitable::commit(tx).await?;

        let loaded = pool.get_all_pending_invitations().await?;
        assert_eq!(loaded.len(), 1);
        let got = &loaded[0];
        assert_eq!(got.invitation_type, InvitationType::Contracts);
        assert_eq!(got.participants.len(), 2);
        assert_eq!(
            got.dec_party_id.as_ref().map(CantonId::to_string),
            Some(format!("dec::{TEST_NS}"))
        );
        assert_eq!(got.package_names, vec!["Governance Core", "Token Custody"]);
        assert_eq!(got.workflow_instance.as_deref(), Some("dec-contracts-4000"));

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_governance_audit_filters_by_party(pool: SqlitePool) -> Result {
        let party_a = test_party_id("party-a");
        let party_b = test_party_id("party-b");

        insert_audit_entry(&pool, &party_a.to_string(), "propose", "success", 1000).await?;
        insert_audit_entry(&pool, &party_b.to_string(), "confirm", "success", 2000).await?;

        let entries = pool.get_governance_audit(&party_a, 50, 0).await?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event_type, "propose");

        let entries = pool.get_governance_audit(&party_b, 50, 0).await?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event_type, "confirm");

        Ok(())
    }

    // ====================================================================
    // Workflow runs + artefacts
    // ====================================================================

    fn test_run(instance: &str, kind: &str, role: &str) -> WorkflowRun {
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
            coordinator_name: None,
            expected_peers: vec![
                CantonId::parse(&format!("a::{TEST_NS}")).unwrap(),
                CantonId::parse(&format!("b::{TEST_NS}")).unwrap(),
            ],
            completed_peers: Vec::new(),
            dec_party_id: None,
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 1000,
            updated_at: 1000,
        }
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
    async fn test_workflow_runs_unique_inprogress_per_kind(pool: SqlitePool) -> Result {
        let mut a = test_run("alpha", "Onboarding", "Coordinator");
        let mut b = test_run("beta", "Onboarding", "Coordinator");

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&a).await?;
        Commitable::commit(tx).await?;

        // A second InProgress run of the same (kind, role) must fail — and
        // specifically with a UNIQUE constraint violation, not merely any
        // error. A malformed row or a JSON-encode failure would also be
        // `is_err()` and would let a dropped partial index slip through.
        let mut tx = pool.begin_transaction().await?;
        let Err(err) = tx.upsert_workflow_run(&b).await else {
            panic!("expected a UNIQUE constraint violation, got Ok");
        };
        let full = format!("{err:#}");
        assert!(
            full.contains("UNIQUE constraint"),
            "expected a UNIQUE constraint violation, got: {full}"
        );
        // tx is poisoned — drop it
        drop(tx);

        // Once A is terminal, B can start.
        let mut tx = pool.begin_transaction().await?;
        tx.set_workflow_run_status(&a.instance_name, WorkflowProgress::Completed, None, 2000)
            .await?;
        Commitable::commit(tx).await?;

        a.status = WorkflowProgress::Completed;
        b.status = WorkflowProgress::InProgress;
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&b).await?;
        Commitable::commit(tx).await?;

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

    // ====================================================================
    // delete_stale_dec_parties
    // ====================================================================

    /// Build a `DecPartyRow` with a caller-chosen `party_id` while keeping a
    /// fixed `prefix`. `test_dec_party` ties the party_id to the prefix, so it
    /// cannot produce several distinct parties under the same prefix — which is
    /// exactly what the stale-delete prefix-scoping test needs.
    fn dec_party_row(prefix: &str, party_id: &str) -> DecPartyRow {
        DecPartyRow {
            party_id: party_id.to_string(),
            prefix: prefix.to_string(),
            threshold: 2,
            updated_at: 1000,
            my_owner_key: None,
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_delete_stale_dec_parties_prefix_scoped(pool: SqlitePool) -> Result {
        // Three parties under `net-a`, one under `net-b`.
        let a1 = format!("a1::{TEST_NS}");
        let a2 = format!("a2::{TEST_NS}");
        let a3 = format!("a3::{TEST_NS}");
        let b1 = format!("b1::{TEST_NS}");

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&dec_party_row("net-a", &a1)).await?;
        tx.upsert_dec_party(&dec_party_row("net-a", &a2)).await?;
        tx.upsert_dec_party(&dec_party_row("net-a", &a3)).await?;
        tx.upsert_dec_party(&dec_party_row("net-b", &b1)).await?;
        Commitable::commit(tx).await?;

        assert_eq!(pool.get_dec_parties_by_prefix("net-a").await?.len(), 3);
        assert_eq!(pool.get_dec_parties_by_prefix("net-b").await?.len(), 1);

        // Keep only a1 fresh under net-a → a2/a3 must go, a1 stays, and the
        // net-b party must be untouched (proves the `prefix = ?` scoping).
        let mut tx = pool.begin_transaction().await?;
        tx.delete_stale_dec_parties("net-a", std::slice::from_ref(&a1))
            .await?;
        Commitable::commit(tx).await?;

        let net_a = pool.get_dec_parties_by_prefix("net-a").await?;
        assert_eq!(net_a.len(), 1);
        assert_eq!(net_a[0].party_id, a1);
        let net_b = pool.get_dec_parties_by_prefix("net-b").await?;
        assert_eq!(net_b.len(), 1);
        assert_eq!(net_b[0].party_id, b1);

        // Empty fresh set for net-a → the early-return prefix-wide delete fires;
        // every net-a party goes while net-b still survives.
        let mut tx = pool.begin_transaction().await?;
        tx.delete_stale_dec_parties("net-a", &[]).await?;
        Commitable::commit(tx).await?;

        assert!(pool.get_dec_parties_by_prefix("net-a").await?.is_empty());
        let net_b = pool.get_dec_parties_by_prefix("net-b").await?;
        assert_eq!(net_b.len(), 1);
        assert_eq!(net_b[0].party_id, b1);

        Ok(())
    }

    // ====================================================================
    // dec_party_identity
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_dec_party_identity_roundtrip(pool: SqlitePool) -> Result {
        let party_id = test_party_id("net-a");
        let kind = "peer_public_keys";

        // Peers deliberately written out of order; ids sort as a, b, c.
        let mut tx = pool.begin_transaction().await?;
        tx.write_dec_party_identity(&party_id, kind, "c::1220cc", b"payload-c")
            .await?;
        tx.write_dec_party_identity(&party_id, kind, "a::1220aa", b"payload-a")
            .await?;
        tx.write_dec_party_identity(&party_id, kind, "b::1220bb", b"payload-b")
            .await?;
        Commitable::commit(tx).await?;

        // read returns each peer's exact bytes.
        assert_eq!(
            pool.read_dec_party_identity(&party_id, kind, "a::1220aa")
                .await?,
            Some(b"payload-a".to_vec())
        );
        assert_eq!(
            pool.read_dec_party_identity(&party_id, kind, "b::1220bb")
                .await?,
            Some(b"payload-b".to_vec())
        );
        assert_eq!(
            pool.read_dec_party_identity(&party_id, kind, "c::1220cc")
                .await?,
            Some(b"payload-c".to_vec())
        );

        // A missing peer → None.
        assert_eq!(
            pool.read_dec_party_identity(&party_id, kind, "missing::1220dd")
                .await?,
            None
        );
        // A different artifact_kind on a known peer → None.
        assert_eq!(
            pool.read_dec_party_identity(&party_id, "other_kind", "a::1220aa")
                .await?,
            None
        );

        // list returns rows ordered a, b, c with payloads paired correctly.
        let listed = pool.list_dec_party_identity(&party_id, kind).await?;
        assert_eq!(
            listed,
            vec![
                ("a::1220aa".to_string(), b"payload-a".to_vec()),
                ("b::1220bb".to_string(), b"payload-b".to_vec()),
                ("c::1220cc".to_string(), b"payload-c".to_vec()),
            ]
        );

        // INSERT OR REPLACE: re-writing the same (party, kind, peer) replaces
        // the payload rather than inserting a duplicate row.
        let mut tx = pool.begin_transaction().await?;
        tx.write_dec_party_identity(&party_id, kind, "a::1220aa", b"payload-a-v2")
            .await?;
        Commitable::commit(tx).await?;

        assert_eq!(
            pool.read_dec_party_identity(&party_id, kind, "a::1220aa")
                .await?,
            Some(b"payload-a-v2".to_vec())
        );
        let listed = pool.list_dec_party_identity(&party_id, kind).await?;
        assert_eq!(listed.len(), 3);
        assert_eq!(
            listed[0],
            ("a::1220aa".to_string(), b"payload-a-v2".to_vec())
        );

        Ok(())
    }

    // ====================================================================
    // Party credentials — Auth0 round-trip
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_party_credentials_auth0_roundtrip(pool: SqlitePool) -> Result {
        let mut creds = test_creds("party-a");
        creds.auth0 = Some(Auth0M2MConfig {
            domain: "tenant.us.auth0.com".to_string(),
            audience: "https://api.example.com".to_string(),
            client_id: "auth0-client-id".to_string(),
            client_secret: "auth0-client-secret".to_string(),
        });

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_party_credentials(&creds).await?;
        Commitable::commit(tx).await?;

        let assert_auth0 = |c: &PartyCredentials| {
            let Some(a) = c.auth0.as_ref() else {
                panic!("auth0 config did not round-trip");
            };
            assert_eq!(a.domain, "tenant.us.auth0.com");
            assert_eq!(a.audience, "https://api.example.com");
            assert_eq!(a.client_id, "auth0-client-id");
            assert_eq!(a.client_secret, "auth0-client-secret");
        };

        let all = pool.get_all_party_credentials().await?;
        assert_eq!(all.len(), 1);
        assert_auth0(&all[0]);

        let Some(by_id) = pool.get_party_credentials(&creds.dec_party_id).await? else {
            panic!("credentials not found by dec_party_id");
        };
        assert_auth0(&by_id);

        Ok(())
    }

    // ====================================================================
    // get_all_dec_party_* — INNER JOIN + prefix scoping
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_all_dec_party_joins_and_prefix(pool: SqlitePool) -> Result {
        let party_a = test_party_id("net-a");
        let party_a_str = party_a.to_string();
        let party_b = test_party_id("net-b");
        let party_b_str = party_b.to_string();

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        tx.upsert_dec_party(&test_dec_party("net-b")).await?;

        // net-a: 2 owners, 1 participant. net-b: 1 owner, 2 participants.
        tx.replace_dec_party_owners(
            &party_a,
            &["a-owner-1".to_string(), "a-owner-2".to_string()],
        )
        .await?;
        tx.replace_dec_party_owners(&party_b, &["b-owner-1".to_string()])
            .await?;

        tx.replace_dec_party_participants(
            &party_a,
            &[DecPartyParticipantRow {
                dec_party_id: party_a_str.clone(),
                participant_uid: "a-node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: None,
            }],
        )
        .await?;
        tx.replace_dec_party_participants(
            &party_b,
            &[
                DecPartyParticipantRow {
                    dec_party_id: party_b_str.clone(),
                    participant_uid: "b-node1::1220bb".to_string(),
                    permission: "submission".to_string(),
                    owner_key: None,
                },
                DecPartyParticipantRow {
                    dec_party_id: party_b_str.clone(),
                    participant_uid: "b-node2::1220cc".to_string(),
                    permission: "confirmation".to_string(),
                    owner_key: None,
                },
            ],
        )
        .await?;
        Commitable::commit(tx).await?;

        // Empty prefix → every JOIN-matched row across both parties.
        let all_owners = pool.get_all_dec_party_owners("").await?;
        assert_eq!(all_owners.len(), 3);
        let all_participants = pool.get_all_dec_party_participants("").await?;
        assert_eq!(all_participants.len(), 3);

        // Specific prefix → only that party's rows.
        let a_owners = pool.get_all_dec_party_owners("net-a").await?;
        assert_eq!(a_owners.len(), 2);
        assert!(a_owners.iter().all(|(id, _)| *id == party_a_str));
        let b_owners = pool.get_all_dec_party_owners("net-b").await?;
        assert_eq!(b_owners.len(), 1);
        assert_eq!(b_owners[0], (party_b_str.clone(), "b-owner-1".to_string()));

        let a_participants = pool.get_all_dec_party_participants("net-a").await?;
        assert_eq!(a_participants.len(), 1);
        assert_eq!(a_participants[0].participant_uid, "a-node1::1220aa");
        let b_participants = pool.get_all_dec_party_participants("net-b").await?;
        assert_eq!(b_participants.len(), 2);
        assert!(b_participants.iter().all(|p| p.dec_party_id == party_b_str));

        Ok(())
    }

    // ====================================================================
    // delete_pending_invitations_by_type_and_coordinator
    // ====================================================================

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_delete_pending_invitations_by_type_and_coordinator(pool: SqlitePool) -> Result {
        let coordinator = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string();

        // Two invites from the SAME coordinator, DIFFERENT invitation_type.
        let onboarding = PendingInvitation {
            id: "onboarding-eeeeeeeeeeeeeeee".to_string(),
            invitation_type: InvitationType::Onboarding,
            coordinator_pubkey: coordinator.clone(),
            coordinator_name: None,
            received_at: 1000,
            prefix: Some("my-party".to_string()),
            participants: Vec::new(),
            dar_filenames: Vec::new(),
            kicked_participant: None,
            new_threshold: None,
            previous_threshold: None,
            dec_party_id: None,
            package_names: Vec::new(),
            workflow_instance: None,
        };
        let dars = PendingInvitation {
            id: "dars-eeeeeeeeeeeeeeee".to_string(),
            invitation_type: InvitationType::Dars,
            coordinator_pubkey: coordinator.clone(),
            coordinator_name: None,
            received_at: 2000,
            prefix: None,
            participants: Vec::new(),
            dar_filenames: vec!["app.dar".to_string()],
            kicked_participant: None,
            new_threshold: None,
            previous_threshold: None,
            dec_party_id: None,
            package_names: Vec::new(),
            workflow_instance: None,
        };

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_pending_invitation(&onboarding).await?;
        tx.upsert_pending_invitation(&dars).await?;
        Commitable::commit(tx).await?;
        assert_eq!(pool.get_all_pending_invitations().await?.len(), 2);

        // Delete only the Onboarding type → the Dars invite from the same
        // coordinator must survive (proves the `AND invitation_type` clause).
        let mut tx = pool.begin_transaction().await?;
        tx.delete_pending_invitations_by_type_and_coordinator(
            InvitationType::Onboarding,
            &coordinator,
        )
        .await?;
        Commitable::commit(tx).await?;

        let remaining = pool.get_all_pending_invitations().await?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, dars.id);
        assert_eq!(remaining[0].invitation_type, InvitationType::Dars);

        Ok(())
    }
}
