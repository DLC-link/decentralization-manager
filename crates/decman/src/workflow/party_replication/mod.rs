//! Party replication, independent of what kind of party is being replicated.
//!
//! Moving a party onto a participant that does not yet hold it is the same
//! sequence of Canton calls whoever owns the party: capture a ledger offset
//! before the topology moves, export the ACS scoped to the target, import it on
//! the target across a synchronizer disconnect, then clear Canton's
//! `HostingParticipant.Onboarding` marker so the party goes live there.
//!
//! None of that depends on a `DecentralizedNamespaceDefinition`. It was written
//! inside the decentralized-party add-party workflow and took an
//! [`AddPartyConfig`](crate::workflow::add_party::AddPartyConfig), which tied it
//! to decparties for no reason other than where it happened to live. This module
//! is the same code addressed by a [`ReplicationTarget`] instead — a party, a
//! participant, and the artifact keys one run's durable markers live under.
//!
//! What stays outside: deciding the topology and getting it authorized. That is
//! genuinely party-type specific — a decparty needs owner-threshold signatures
//! over a DNS, an external party needs its own key — and it lives with each
//! workflow.

pub mod acs;
pub mod offset;
pub mod onboarding_flag;

use sqlx::SqlitePool;

use crate::{canton_id::CantonId, error::Result, workflow::storage::WorkflowStorage};

pub use acs::{collect_party_package_ids, export_party_acs, import_party_acs};
pub use offset::{capture_offset_once, current_ledger_offset};
pub use onboarding_flag::{
    ClearOutcome, clear_onboarding_flag, has_onboarding_marker, wait_for_flag_cleared,
};

/// The artifact keys one replication run reads and writes.
///
/// Passed in rather than fixed so each workflow keeps its own key namespace.
/// The add-party run's keys are unchanged by the extraction, so no persisted
/// run is disturbed and a workflow interrupted before it still resumes after.
#[derive(Clone, Copy, Debug)]
pub struct ReplicationArtifacts {
    /// Unscoped. The source's ledger offset, captured before the topology
    /// change so `ExportPartyAcs` can find the party's activation on the target
    /// after it.
    pub export_offset: &'static str,
    /// Scoped to the target participant. Its own pre-activation offset, which
    /// `ClearPartyOnboardingFlag` searches forward from.
    pub pre_activation_offset: &'static str,
    /// Unscoped, durable, never cleared. Written before the synchronizer
    /// disconnect so a retry after a crash knows the participant was left
    /// mid-window and recovers it before touching anything.
    pub import_inflight: &'static str,
}

/// Where a replication's durable artefacts live.
///
/// `workflow_artifacts.instance_name` is a foreign key into `workflow_runs`,
/// which suits the Noise workflows — every artefact belongs to a run the
/// coordinator persisted. The tenant API has no run, so its artefacts have
/// nothing to point at and the foreign key refuses them. They go to a
/// table of the same shape without one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactStore {
    /// Backed by a persisted `workflow_runs` row.
    WorkflowRun,
    /// Standalone, for the run-less tenant API.
    Tenant,
}

/// One replication: which party moves onto which participant, and where this
/// run's durable markers live.
#[derive(Clone, Debug)]
pub struct ReplicationTarget {
    /// The party being replicated. Any party — decentralized, external, or
    /// local; the ACS path does not care who owns it.
    pub party_id: CantonId,
    /// The participant gaining the party. Carries Canton's `Onboarding` marker
    /// until the import lands.
    pub target_participant_id: CantonId,
    /// The workflow run these artifacts belong to.
    pub instance_name: String,
    /// The artifact keys above.
    pub artifacts: ReplicationArtifacts,
    /// Which table those keys live in.
    pub store: ArtifactStore,
}

impl ReplicationTarget {
    /// Read one of this replication's artefacts from whichever table backs it.
    ///
    /// # Errors
    /// Propagates the database error.
    pub async fn read_artifact(
        &self,
        db: &SqlitePool,
        kind: &str,
        scope: Option<&str>,
    ) -> Result<Option<Vec<u8>>> {
        match self.store {
            ArtifactStore::WorkflowRun => db.read_artifact(&self.instance_name, kind, scope).await,
            ArtifactStore::Tenant => {
                let scope = scope.unwrap_or_default();
                let row: Option<(Vec<u8>,)> = sqlx::query_as(
                    "SELECT payload FROM tenant_replication_artifacts \
                     WHERE instance_name = ?1 AND artifact_kind = ?2 AND attestor_id = ?3",
                )
                .bind(&self.instance_name)
                .bind(kind)
                .bind(scope)
                .fetch_optional(db)
                .await?;
                Ok(row.map(|(payload,)| payload))
            }
        }
    }

    /// Write one of this replication's artefacts only if it is not already
    /// there, reporting whether this call was the one that wrote it.
    ///
    /// The offset capture is "first capture wins", and a read-then-write cannot
    /// enforce that: two concurrent prepares both see nothing staged, and the
    /// slower one overwrites the first — possibly after the topology activated,
    /// leaving an offset that `ExportPartyAcs` and `ClearPartyOnboardingFlag`
    /// will search forward from in vain. The database decides instead.
    ///
    /// # Errors
    /// Propagates the database error.
    pub async fn write_artifact_if_absent(
        &self,
        db: &SqlitePool,
        kind: &str,
        scope: Option<&str>,
        payload: &[u8],
    ) -> Result<bool> {
        match self.store {
            ArtifactStore::WorkflowRun => {
                // The Noise workflows serialise their steps through the
                // coordinator, so a read-then-write is not racing anything.
                if db
                    .read_artifact(&self.instance_name, kind, scope)
                    .await?
                    .is_some()
                {
                    return Ok(false);
                }
                db.write_artifact(&self.instance_name, kind, scope, payload)
                    .await?;
                Ok(true)
            }
            ArtifactStore::Tenant => {
                let scope = scope.unwrap_or_default();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or_default();
                let result = sqlx::query(
                    "INSERT INTO tenant_replication_artifacts \
                     (instance_name, artifact_kind, attestor_id, payload, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(instance_name, artifact_kind, attestor_id) DO NOTHING",
                )
                .bind(&self.instance_name)
                .bind(kind)
                .bind(scope)
                .bind(payload)
                .bind(now)
                .execute(db)
                .await?;
                Ok(result.rows_affected() > 0)
            }
        }
    }

    /// Write one of this replication's artefacts, replacing any existing value.
    ///
    /// # Errors
    /// Propagates the database error.
    pub async fn write_artifact(
        &self,
        db: &SqlitePool,
        kind: &str,
        scope: Option<&str>,
        payload: &[u8],
    ) -> Result {
        match self.store {
            ArtifactStore::WorkflowRun => {
                db.write_artifact(&self.instance_name, kind, scope, payload)
                    .await
            }
            ArtifactStore::Tenant => {
                let scope = scope.unwrap_or_default();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or_default();
                sqlx::query(
                    "INSERT INTO tenant_replication_artifacts \
                     (instance_name, artifact_kind, attestor_id, payload, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(instance_name, artifact_kind, attestor_id) \
                     DO UPDATE SET payload = excluded.payload",
                )
                .bind(&self.instance_name)
                .bind(kind)
                .bind(scope)
                .bind(payload)
                .bind(now)
                .execute(db)
                .await?;
                Ok(())
            }
        }
    }
}

impl ReplicationTarget {
    /// Build a target for `party_id` moving onto `target_participant_id`.
    pub fn new(
        party_id: CantonId,
        target_participant_id: CantonId,
        instance_name: String,
        artifacts: ReplicationArtifacts,
        store: ArtifactStore,
    ) -> Self {
        Self {
            party_id,
            target_participant_id,
            instance_name,
            artifacts,
            store,
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use crate::db::MIGRATOR;

    use super::*;

    const ARTIFACTS: ReplicationArtifacts = ReplicationArtifacts {
        export_offset: "test_export_offset",
        pre_activation_offset: "test_pre_activation_offset",
        import_inflight: "test_import_inflight",
    };

    fn canton_id(prefix: &str, tag: u8) -> CantonId {
        let namespace = format!("1220{}", format!("{tag:02x}").repeat(32));
        match CantonId::parse(&format!("{prefix}::{namespace}")) {
            Ok(id) => id,
            Err(e) => panic!("test id must parse: {e}"),
        }
    }

    fn tenant_target() -> ReplicationTarget {
        ReplicationTarget::new(
            canton_id("alice", 1),
            canton_id("participant-3", 3),
            "tenant-add-hosts:alice:participant-3".to_string(),
            ARTIFACTS,
            ArtifactStore::Tenant,
        )
    }

    /// The bug this guards: `workflow_artifacts.instance_name` is a foreign key
    /// into `workflow_runs`, so a tenant replication — which has no run behind
    /// it — could not persist its offsets at all. Every write failed with
    /// `FOREIGN KEY constraint failed`, and the whole add-hosts flow died at
    /// its first call.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn tenant_artifacts_persist_without_a_workflow_run(db: SqlitePool) -> anyhow::Result<()> {
        let target = tenant_target();

        target
            .write_artifact(&db, ARTIFACTS.export_offset, None, b"42")
            .await?;
        let read = target
            .read_artifact(&db, ARTIFACTS.export_offset, None)
            .await?;
        assert_eq!(read.as_deref(), Some(b"42".as_slice()));

        Ok(())
    }

    /// Scoped and unscoped artefacts of the same kind must not collide: the
    /// export offset is unscoped, the pre-activation offset is keyed by the
    /// participant that captured it, and both live under one instance name.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn tenant_artifacts_separate_scoped_from_unscoped(db: SqlitePool) -> anyhow::Result<()> {
        let target = tenant_target();

        target
            .write_artifact(&db, ARTIFACTS.pre_activation_offset, None, b"1")
            .await?;
        target
            .write_artifact(
                &db,
                ARTIFACTS.pre_activation_offset,
                Some("PAR::three"),
                b"2",
            )
            .await?;

        let unscoped = target
            .read_artifact(&db, ARTIFACTS.pre_activation_offset, None)
            .await?;
        let scoped = target
            .read_artifact(&db, ARTIFACTS.pre_activation_offset, Some("PAR::three"))
            .await?;
        assert_eq!(unscoped.as_deref(), Some(b"1".as_slice()));
        assert_eq!(scoped.as_deref(), Some(b"2".as_slice()));

        Ok(())
    }

    /// A missing artefact reads as `None`, not an error — the offset capture
    /// relies on that to decide it is the first caller.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_missing_tenant_artifact_reads_as_none(db: SqlitePool) -> anyhow::Result<()> {
        let read = tenant_target()
            .read_artifact(&db, ARTIFACTS.import_inflight, None)
            .await?;
        assert!(read.is_none());
        Ok(())
    }
}
