//! Storage backend for workflow runtime artefacts.
//!
//! Step code reads/writes artefacts (signed proposals, namespace defs, signatures,
//! prepared submissions, …) through this trait instead of the file system. The
//! production impl is backed by SQLite — both control-plane state and data-plane
//! blobs live in the same DB so the run survives a node restart.
//!
//! Artefacts come in two shapes:
//! - **shared**: one per workflow run (e.g. `dns_proto`, `namespace_def`). Pass
//!   `attestor = None`.
//! - **per-attestor**: one per (run, attestor) (e.g. `signed_dns_proposal`,
//!   `submission_signatures`). Pass `attestor = Some(canton_id_string)`.

use sqlx::SqlitePool;

use crate::{
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    participant_id::CantonId,
};

/// Stable string identifiers for every artefact kind we persist. Step code
/// passes one of these into `WorkflowStorage::read_artifact` / `write_artifact`.
/// Keeping them here so the taxonomy is grep-able and we don't sprinkle
/// stringly-typed kinds across the step modules.
pub mod artifact_kinds {
    // Onboarding (workflow_artifacts during a run; identity ones get copied
    // into dec_party_identity at workflow completion)
    pub const DNS_PROTO: &str = "dns_proto";
    pub const P2P_PROTO: &str = "p2p_proto";
    pub const NAMESPACE_DEF: &str = "namespace_def";
    pub const SIGNED_DNS_PROPOSAL: &str = "signed_dns_proposal";
    pub const SIGNED_P2P_PROPOSAL: &str = "signed_p2p_proposal";
    pub const ATTESTOR_PUBLIC_KEYS: &str = "attestor_public_keys";
    pub const PARTICIPANT_ID: &str = "participant_id";
    /// Resolved decentralized party id (`{prefix}::{namespace_fingerprint}`)
    /// produced by onboarding's CreateProposals once all attestor namespace
    /// keys are aggregated. Plaintext UTF-8. Read by the coordinator's HTTP
    /// path after the workflow finishes so it can return the new party id to
    /// the UI without round-tripping through the file system.
    pub const PARTY_ID: &str = "party_id";

    // Contracts (workflow_artifacts during a run)
    pub const PREPARED_SUBMISSION: &str = "prepared_submission";
    pub const SUBMISSION_SIGNATURES: &str = "submission_signatures";

    // Kick (workflow_artifacts during a run)
    /// Current (pre-kick) decentralized namespace definition, exported from
    /// the topology snapshot in the ExportState step. Stored as a single
    /// length-prefixed protobuf message, shared across the run.
    pub const KICK_NAMESPACE_DEF: &str = "kick_namespace_def";
    /// Namespace fingerprint (DNS owner key hex) being kicked. Stored as
    /// plaintext UTF-8 (a trailing newline is fine; readers trim).
    pub const KICK_TARGET_NAMESPACE: &str = "kick_target_namespace";
    /// Canton participant id of the participant being kicked. Plaintext UTF-8.
    pub const KICK_TARGET_PARTICIPANT: &str = "kick_target_participant";
    /// New DNS threshold to apply after the kick. Plaintext UTF-8 integer.
    pub const KICK_NEW_THRESHOLD: &str = "kick_new_threshold";
    /// Unsigned DNS kick proposal protobuf (`SignedTopologyTransaction`)
    /// produced by the coordinator in CreateProposals. Length-prefixed proto.
    pub const KICK_DNS_PROPOSAL: &str = "kick_dns_proposal";
    /// Unsigned P2P kick proposal protobuf produced alongside the DNS one.
    pub const KICK_P2P_PROPOSAL: &str = "kick_p2p_proposal";
    /// Post-kick `DecentralizedNamespaceDefinition` protobuf — used by submit
    /// to wait for the new state to land in the topology.
    pub const KICK_NEW_NAMESPACE_DEF: &str = "kick_new_namespace_def";
    /// Full decentralized party id (`{prefix}::{namespace_hex}`). Plaintext.
    pub const KICK_PARTY_ID: &str = "kick_party_id";
    /// Per-attestor signed DNS kick proposal — a single length-prefixed
    /// `SignedTopologyTransaction` protobuf containing only that attestor's
    /// signature contribution.
    pub const SIGNED_KICK_DNS: &str = "signed_kick_dns";
    /// Per-attestor signed P2P kick proposal — same shape as `SIGNED_KICK_DNS`.
    pub const SIGNED_KICK_P2P: &str = "signed_kick_p2p";
}

/// Identity-table artefact kinds. These survive the originating workflow run's
/// dismissal and are read by post-onboarding workflows (contracts, kick, …).
pub mod identity_kinds {
    /// Each attestor's exported namespace + DAML signing keys. Stored under
    /// `(dec_party_id, attestor_id)` — the local node's own row is the
    /// canonical source for sign.rs's "load my DAML signing key" lookup.
    pub const ATTESTOR_PUBLIC_KEYS: &str = "attestor_public_keys";
    /// Each attestor's `participant_id` file content. Coordinator's row set
    /// holds one per attestor; an attestor's row set holds just their own.
    pub const PARTICIPANT_ID: &str = "participant_id";
}

/// Read/write surface for workflow runtime artefacts. Replaces the file-backed
/// `<kind>::Dirs` helpers that used to write under `workflow-data/<instance>/`.
///
/// All methods take `&self` and manage their own transactions internally — step
/// code stays linear and doesn't have to thread a transaction through.
#[allow(async_fn_in_trait)]
pub trait WorkflowStorage: Send + Sync {
    /// Read a single artefact. Returns `None` if it doesn't exist (the step
    /// caller decides whether that's an error or a "haven't generated yet"
    /// signal).
    async fn read_artifact(
        &self,
        instance_name: &str,
        artifact_kind: &str,
        attestor: Option<&str>,
    ) -> Result<Option<Vec<u8>>>;

    /// List every per-attestor artefact of a given kind for a run, returning
    /// `(attestor_id, payload)` pairs sorted by attestor id. Used by the
    /// coordinator to gather signatures, attestor pubkeys, etc.
    async fn list_artifacts(
        &self,
        instance_name: &str,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>>;

    /// Write (insert or replace) an artefact. Idempotent — re-running a step
    /// after a restart overwrites the previous payload.
    async fn write_artifact(
        &self,
        instance_name: &str,
        artifact_kind: &str,
        attestor: Option<&str>,
        payload: &[u8],
    ) -> Result;

    /// Read a per-(dec_party, attestor) identity artefact. Returns `None` if
    /// no row exists for that combination.
    async fn read_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
        attestor_id: &str,
    ) -> Result<Option<Vec<u8>>>;

    /// List every identity artefact of a given kind for a dec party,
    /// returning `(attestor_id, payload)` pairs sorted by attestor id.
    async fn list_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>>;

    /// Write an identity artefact. Used at onboarding completion to copy
    /// `participant_id` and `attestor_public_keys` rows from
    /// `workflow_artifacts` into `dec_party_identity` keyed by the resolved
    /// dec_party_id.
    async fn write_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
        attestor_id: &str,
        payload: &[u8],
    ) -> Result;
}

impl WorkflowStorage for SqlitePool {
    async fn read_artifact(
        &self,
        instance_name: &str,
        artifact_kind: &str,
        attestor: Option<&str>,
    ) -> Result<Option<Vec<u8>>> {
        SchemaRead::read_workflow_artifact(self, instance_name, artifact_kind, attestor).await
    }

    async fn list_artifacts(
        &self,
        instance_name: &str,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        SchemaRead::list_workflow_artifacts(self, instance_name, artifact_kind).await
    }

    async fn write_artifact(
        &self,
        instance_name: &str,
        artifact_kind: &str,
        attestor: Option<&str>,
        payload: &[u8],
    ) -> Result {
        let mut tx = self.begin_transaction().await?;
        tx.write_workflow_artifact(instance_name, artifact_kind, attestor, payload)
            .await?;
        Commitable::commit(tx).await
    }

    async fn read_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
        attestor_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        SchemaRead::read_dec_party_identity(self, dec_party_id, artifact_kind, attestor_id).await
    }

    async fn list_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        SchemaRead::list_dec_party_identity(self, dec_party_id, artifact_kind).await
    }

    async fn write_identity(
        &self,
        dec_party_id: &CantonId,
        artifact_kind: &str,
        attestor_id: &str,
        payload: &[u8],
    ) -> Result {
        let mut tx = self.begin_transaction().await?;
        tx.write_dec_party_identity(dec_party_id, artifact_kind, attestor_id, payload)
            .await?;
        Commitable::commit(tx).await
    }
}
