//! Commitable writes inside one SQLite transaction.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;

use super::write::{owner_key_refusal, write_invitation};
use crate::{
    canton_id::{CantonId, Fingerprint},
    config::{PartyCredentials, Peer},
    db::{
        crypto,
        rows::{
            DecPartyContractRow, DecPartyParticipantRow, DecPartyRow, OwnerKeyRefusal,
            PartyCredentialsRow, PeerRow, WorkflowRunRow,
        },
        schema::Commitable,
    },
    error::Result,
    server::{InvitationOutcome, PendingInvitation, WorkflowProgress, WorkflowRun},
};

/// The refusal of a workflow run that would become in progress during a purge
/// of former-party contracts. The purge takes the participant off its
/// synchronizer, so a run would fail underneath it.
const WORKFLOW_BLOCKED_BY_PURGE: &str = "A purge of former-party contracts is running on this \
     node. Start the workflow again after the purge finishes";

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
                my_owner_key,
                confirmation_threshold
            ) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(party_id) DO UPDATE SET
                prefix = excluded.prefix,
                threshold = excluded.threshold,
                updated_at = excluded.updated_at,
                my_owner_key = excluded.my_owner_key,
                confirmation_threshold = excluded.confirmation_threshold
            ",
        )
        .bind(&row.party_id)
        .bind(&row.prefix)
        .bind(row.threshold)
        .bind(row.updated_at)
        .bind(&row.my_owner_key)
        .bind(row.confirmation_threshold)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn replace_dec_party_owners(&mut self, party_id: &CantonId, owners: &[String]) -> Result {
        let party_id_str = party_id.to_string();
        // An owner that stays keeps its claim. The live Canton fetch carries
        // no claims, and a refresh that wiped them would hide every owner that
        // does not host the party until the next OwnerKeys round.
        if owners.is_empty() {
            sqlx::query("DELETE FROM dec_party_owner WHERE dec_party_id = ?")
                .bind(&party_id_str)
                .execute(&mut **self)
                .await?;
        } else {
            let placeholders = owners.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let query = format!(
                "DELETE FROM dec_party_owner WHERE dec_party_id = ? \
                 AND owner_key NOT IN ({placeholders})"
            );
            let mut q = sqlx::query(&query).bind(&party_id_str);
            for owner in owners {
                q = q.bind(owner);
            }
            q.execute(&mut **self).await?;
        }

        for owner in owners {
            sqlx::query(
                r"
                INSERT INTO dec_party_owner (dec_party_id, owner_key)
                VALUES (?, ?)
                ON CONFLICT(dec_party_id, owner_key) DO NOTHING
                ",
            )
            .bind(&party_id_str)
            .bind(owner)
            .execute(&mut **self)
            .await?;
        }

        Ok(())
    }

    async fn delete_dec_party_owner(&mut self, party_id: &CantonId, owner_key: &str) -> Result {
        sqlx::query("DELETE FROM dec_party_owner WHERE dec_party_id = ? AND owner_key = ?")
            .bind(party_id.to_string())
            .bind(owner_key)
            .execute(&mut **self)
            .await?;
        Ok(())
    }

    async fn update_owner_claim(
        &mut self,
        party_id: &CantonId,
        owner_key: &str,
        participant_uid: &str,
    ) -> Result<Option<OwnerKeyRefusal>> {
        let party_id = party_id.to_string();
        if let Some(refusal) =
            owner_key_refusal(self, &party_id, owner_key, participant_uid).await?
        {
            return Ok(Some(refusal));
        }
        sqlx::query(
            r"
            UPDATE dec_party_owner
            SET participant_uid = ?
            WHERE dec_party_id = ? AND owner_key = ?
            ",
        )
        .bind(participant_uid)
        .bind(&party_id)
        .bind(owner_key)
        .execute(&mut **self)
        .await?;
        Ok(None)
    }

    async fn replace_dec_party_participants(
        &mut self,
        party_id: &CantonId,
        participants: &[DecPartyParticipantRow],
    ) -> Result {
        let party_id_str = party_id.to_string();
        // UPSERT each fresh row. permission may change (e.g., submission ->
        // confirmation); owner_key and signing_key only ever transition
        // NULL -> Some, never back to NULL. COALESCE keeps a previously-known
        // fingerprint when the live Canton fetch carries None for it.
        for p in participants {
            sqlx::query(
                r"
                INSERT INTO dec_party_participant (
                    dec_party_id,
                    participant_uid,
                    permission,
                    owner_key,
                    signing_key
                ) VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(dec_party_id, participant_uid) DO UPDATE SET
                    permission = excluded.permission,
                    owner_key = COALESCE(excluded.owner_key, dec_party_participant.owner_key),
                    signing_key =
                        COALESCE(excluded.signing_key, dec_party_participant.signing_key)
                ",
            )
            .bind(&party_id_str)
            .bind(&p.participant_uid)
            .bind(&p.permission)
            .bind(&p.owner_key)
            .bind(&p.signing_key)
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

    async fn delete_dec_party_participant(
        &mut self,
        party_id: &CantonId,
        participant_uid: &str,
    ) -> Result {
        sqlx::query(
            "DELETE FROM dec_party_participant WHERE dec_party_id = ? AND participant_uid = ?",
        )
        .bind(party_id.to_string())
        .bind(participant_uid)
        .execute(&mut **self)
        .await?;
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
    ) -> Result<Option<OwnerKeyRefusal>> {
        let party_id = party_id.to_string();
        if let Some(refusal) =
            owner_key_refusal(self, &party_id, owner_key, participant_uid).await?
        {
            return Ok(Some(refusal));
        }
        sqlx::query(
            r"
            UPDATE dec_party_participant
            SET owner_key = ?
            WHERE dec_party_id = ? AND participant_uid = ?
            ",
        )
        .bind(owner_key)
        .bind(&party_id)
        .bind(participant_uid)
        .execute(&mut **self)
        .await?;

        Ok(None)
    }

    async fn update_participant_signing_key(
        &mut self,
        party_id: &CantonId,
        participant_uid: &str,
        signing_key: &Fingerprint,
    ) -> Result {
        sqlx::query(
            r"
            UPDATE dec_party_participant
            SET signing_key = ?
            WHERE dec_party_id = ? AND participant_uid = ?
            ",
        )
        .bind(signing_key.as_str())
        .bind(party_id.to_string())
        .bind(participant_uid)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn upsert_pending_invitation(&mut self, inv: &PendingInvitation) -> Result {
        write_invitation(self, inv, "pending", None).await
    }

    async fn insert_handled_invitation(
        &mut self,
        inv: &PendingInvitation,
        outcome: InvitationOutcome,
        handled_at: i64,
    ) -> Result {
        write_invitation(self, inv, outcome.as_str(), Some(handled_at)).await
    }

    async fn delete_pending_invitation(&mut self, id: &str) -> Result {
        sqlx::query("DELETE FROM pending_invitations WHERE id = ? AND status = 'pending'")
            .bind(id)
            .execute(&mut **self)
            .await?;

        Ok(())
    }

    async fn delete_pending_invitations_by_coordinator(
        &mut self,
        coordinator_pubkey: &str,
    ) -> Result {
        sqlx::query(
            "DELETE FROM pending_invitations WHERE coordinator_pubkey = ? AND status = 'pending'",
        )
        .bind(coordinator_pubkey)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn mark_invitation_handled(
        &mut self,
        id: &str,
        outcome: InvitationOutcome,
        handled_at: i64,
    ) -> Result<bool> {
        let result = sqlx::query(
            r"
            UPDATE pending_invitations SET status = ?, handled_at = ?
            WHERE id = ? AND status = 'pending'
            ",
        )
        .bind(outcome.as_str())
        .bind(handled_at)
        .bind(id)
        .execute(&mut **self)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    async fn prune_handled_invitations(&mut self, keep: i64) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM pending_invitations
            WHERE status != 'pending' AND id NOT IN (
                SELECT id FROM pending_invitations
                WHERE status != 'pending'
                ORDER BY handled_at DESC, id ASC
                LIMIT ?
            )
            ",
        )
        .bind(keep.max(0))
        .execute(&mut **self)
        .await?;

        Ok(result.rows_affected())
    }

    async fn upsert_workflow_run(&mut self, run: &WorkflowRun) -> Result {
        let row = WorkflowRunRow::from_domain(run)?;

        // ON CONFLICT(instance_name) so re-saving the same run (resume,
        // step advance, etc.) replaces in place. Migration 000013 dropped the
        // partial unique index that once forbade two InProgress runs per
        // (kind, role) — concurrency is governed by the in-memory registry.
        // The WHERE refuses an in-progress run while a purge runs.
        let result = sqlx::query(
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
                coordinator_instance,
                expected_peers_json,
                completed_peers_json,
                dec_party_id,
                error,
                dismissed,
                created_at,
                updated_at
            )
            SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
            WHERE ? != 'inprogress'
               OR NOT EXISTS (SELECT 1 FROM former_dec_party WHERE purge_status = 'running')
            ON CONFLICT(instance_name) DO UPDATE SET
                kind                     = excluded.kind,
                role                     = excluded.role,
                status                   = excluded.status,
                current_step             = excluded.current_step,
                step_index               = excluded.step_index,
                step_total               = excluded.step_total,
                config_json              = excluded.config_json,
                coordinator_pubkey       = excluded.coordinator_pubkey,
                coordinator_instance     = excluded.coordinator_instance,
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
        .bind(&row.coordinator_instance)
        .bind(&row.expected_peers_json)
        .bind(&row.completed_peers_json)
        .bind(&row.dec_party_id)
        .bind(&row.error)
        .bind(row.dismissed)
        .bind(row.created_at)
        .bind(row.updated_at)
        .bind(&row.status)
        .execute(&mut **self)
        .await?;

        anyhow::ensure!(result.rows_affected() > 0, WORKFLOW_BLOCKED_BY_PURGE);
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

        let result = sqlx::query(
            r"
            UPDATE workflow_runs
            SET status = ?,
                error = ?,
                updated_at = ?
            WHERE instance_name = ?
              AND (? != 'inprogress'
                   OR NOT EXISTS (SELECT 1 FROM former_dec_party WHERE purge_status = 'running'))
            ",
        )
        .bind(status_str)
        .bind(error)
        .bind(updated_at)
        .bind(instance_name)
        .bind(status_str)
        .execute(&mut **self)
        .await?;
        if result.rows_affected() == 0 && status == WorkflowProgress::InProgress {
            // No row changed: either the run is missing, which stays a no-op,
            // or a purge refused it.
            let (purging,): (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM former_dec_party WHERE purge_status = 'running')",
            )
            .fetch_one(&mut **self)
            .await?;
            anyhow::ensure!(!purging, WORKFLOW_BLOCKED_BY_PURGE);
        }

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

    async fn finish_workflow_run(
        &mut self,
        instance_name: &str,
        status: WorkflowProgress,
        error: Option<&str>,
        updated_at: i64,
    ) -> Result<bool> {
        // Failed keeps its artefacts for a post-mortem, as in `set_workflow_run_status`.
        let clears_artifacts = match status {
            WorkflowProgress::Completed | WorkflowProgress::Cancelled => true,
            WorkflowProgress::Failed => false,
            WorkflowProgress::Idle | WorkflowProgress::InProgress => {
                anyhow::bail!("{status} is not a terminal workflow status")
            }
        };

        let moved = sqlx::query(
            r"
            UPDATE workflow_runs
            SET status = ?,
                error = ?,
                updated_at = ?
            WHERE instance_name = ?
              AND status IN ('idle', 'inprogress')
            ",
        )
        .bind(status.as_str())
        .bind(error)
        .bind(updated_at)
        .bind(instance_name)
        .execute(&mut **self)
        .await?
        .rows_affected()
            == 1;

        if moved && clears_artifacts {
            sqlx::query("DELETE FROM workflow_artifacts WHERE instance_name = ?")
                .bind(instance_name)
                .execute(&mut **self)
                .await?;
        }

        Ok(moved)
    }

    async fn set_workflow_run_dec_party_id(
        &mut self,
        instance_name: &str,
        dec_party_id: &CantonId,
    ) -> Result {
        sqlx::query("UPDATE workflow_runs SET dec_party_id = ? WHERE instance_name = ?")
            .bind(dec_party_id.to_string())
            .bind(instance_name)
            .execute(&mut **self)
            .await?;
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
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
