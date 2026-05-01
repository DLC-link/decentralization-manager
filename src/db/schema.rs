use super::rows::{
    ChainAuditCacheRow, DecPartyContractRow, DecPartyParticipantRow, DecPartyRow,
    GovernanceAuditRow,
};
use crate::{
    config::{PartyCredentials, Peer},
    error::Result,
    participant_id::CantonId,
    server::{PendingInvitation, WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
};

/// Read operations on the database
#[allow(async_fn_in_trait)]
pub trait SchemaRead {
    /// Get all peers
    async fn get_all_peers(&self) -> Result<Vec<Peer>>;

    /// Get a peer by participant ID
    async fn get_peer(&self, participant_id: &str) -> Result<Option<Peer>>;

    /// Get the number of peers
    async fn get_peer_count(&self) -> Result<i64>;

    /// Get all party credentials
    async fn get_all_party_credentials(&self) -> Result<Vec<PartyCredentials>>;

    /// Get a peer by its Noise public key
    async fn get_peer_by_public_key(&self, public_key: &str) -> Result<Option<Peer>>;

    /// Get party credentials by decentralized party ID
    async fn get_party_credentials(&self, dec_party_id: &str) -> Result<Option<PartyCredentials>>;

    /// Get cached decentralized parties by prefix
    async fn get_dec_parties_by_prefix(&self, prefix: &str) -> Result<Vec<DecPartyRow>>;

    /// Get owner keys for a decentralized party
    async fn get_dec_party_owners(&self, party_id: &str) -> Result<Vec<String>>;

    /// Get participants for a decentralized party
    async fn get_dec_party_participants(
        &self,
        party_id: &str,
    ) -> Result<Vec<DecPartyParticipantRow>>;

    /// Get the owner key for a specific participant in a decentralized party.
    /// Returns `None` if the row is missing or the `owner_key` column is NULL.
    async fn get_dec_party_participant_owner_key(
        &self,
        party_id: &str,
        participant_uid: &str,
    ) -> Result<Option<String>>;

    /// Get contracts for a decentralized party
    async fn get_dec_party_contracts(&self, party_id: &str) -> Result<Vec<DecPartyContractRow>>;

    /// Get all owners for parties matching a prefix (bulk query)
    async fn get_all_dec_party_owners(&self, prefix: &str) -> Result<Vec<(String, String)>>;

    /// Get all participants for parties matching a prefix (bulk query)
    async fn get_all_dec_party_participants(
        &self,
        prefix: &str,
    ) -> Result<Vec<DecPartyParticipantRow>>;

    /// Get all contracts for parties matching a prefix (bulk query)
    async fn get_all_dec_party_contracts(&self, prefix: &str) -> Result<Vec<DecPartyContractRow>>;

    /// Get paginated governance audit entries for a party, newest first
    async fn get_governance_audit(
        &self,
        party_id: &CantonId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<GovernanceAuditRow>>;

    /// Get cached chain audit entries for a party, newest first
    async fn get_chain_audit_cache(
        &self,
        party_id: &str,
        limit: i64,
    ) -> Result<Vec<ChainAuditCacheRow>>;

    /// Get all persisted pending invitations
    async fn get_all_pending_invitations(&self) -> Result<Vec<PendingInvitation>>;

    /// Get every workflow run that's currently in progress (any role).
    /// Used at startup to drive recovery.
    async fn get_in_progress_workflow_runs(&self) -> Result<Vec<WorkflowRun>>;

    /// Get a single workflow run by its instance name.
    async fn get_workflow_run(&self, instance_name: &str) -> Result<Option<WorkflowRun>>;

    /// Look up the in-progress run for a given (kind, role) on this node.
    /// Returns at most one row thanks to the partial unique index.
    async fn get_active_workflow_run(
        &self,
        kind: WorkflowKind,
        role: WorkflowRole,
    ) -> Result<Option<WorkflowRun>>;

    /// Get all workflow runs that should appear in the notification feed:
    /// every InProgress run plus any terminal run the user hasn't dismissed
    /// yet. Newest-updated first.
    async fn get_visible_workflow_runs(&self) -> Result<Vec<WorkflowRun>>;

    /// Read a single artefact for a workflow run. `attestor` may be None for
    /// shared artefacts (proposals, namespace defs).
    async fn read_workflow_artifact(
        &self,
        instance_name: &str,
        artifact_kind: &str,
        attestor: Option<&str>,
    ) -> Result<Option<Vec<u8>>>;

    /// List all artefacts of a given kind for a workflow run, returning
    /// `(attestor_id, payload)` pairs. Used when the coordinator needs to
    /// gather one-per-attestor artefacts (signatures, attestor pubkeys).
    async fn list_workflow_artifacts(
        &self,
        instance_name: &str,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>>;

    /// Read an identity artefact for a dec party. Used by post-onboarding
    /// workflows (contracts, kick) that need participant_id / signing-key
    /// material that survives the originating onboarding run's dismissal.
    async fn read_dec_party_identity(
        &self,
        dec_party_id: &str,
        artifact_kind: &str,
        attestor_id: &str,
    ) -> Result<Option<Vec<u8>>>;

    /// List every identity artefact of a given kind for a dec party,
    /// returning `(attestor_id, payload)` pairs sorted by attestor_id.
    async fn list_dec_party_identity(
        &self,
        dec_party_id: &str,
        artifact_kind: &str,
    ) -> Result<Vec<(String, Vec<u8>)>>;
}

/// Write operations on the database
#[allow(async_fn_in_trait)]
pub trait SchemaWrite {
    type Transaction: Commitable + Send + Sync;

    /// Begin a new database transaction
    async fn begin_transaction(&self) -> Result<Self::Transaction>;
}

/// A transaction that can be committed
#[allow(async_fn_in_trait)]
pub trait Commitable {
    /// Commit the transaction
    async fn commit(self) -> Result;

    /// Delete all peers
    async fn delete_all_peers(&mut self) -> Result;

    /// Insert a single peer
    async fn insert_peer(&mut self, peer: &Peer) -> Result;

    /// Insert or replace party credentials
    async fn upsert_party_credentials(&mut self, creds: &PartyCredentials) -> Result;

    /// Upsert a decentralized party
    async fn upsert_dec_party(&mut self, row: &DecPartyRow) -> Result;

    /// Replace all owners for a decentralized party
    async fn replace_dec_party_owners(&mut self, party_id: &str, owners: &[String]) -> Result;

    /// Replace all participants for a decentralized party
    async fn replace_dec_party_participants(
        &mut self,
        party_id: &str,
        participants: &[DecPartyParticipantRow],
    ) -> Result;

    /// Replace all contracts for a decentralized party
    async fn replace_dec_party_contracts(
        &mut self,
        party_id: &str,
        contracts: &[DecPartyContractRow],
    ) -> Result;

    /// Delete decentralized parties by prefix (cascades to owners, participants, contracts)
    async fn delete_dec_parties_by_prefix(&mut self, prefix: &str) -> Result;

    /// Delete decentralized parties not in the given set of IDs (within a prefix scope)
    async fn delete_stale_dec_parties(&mut self, prefix: &str, fresh_ids: &[String]) -> Result;

    /// Update the owner key for a specific participant in a decentralized party
    async fn update_participant_owner_key(
        &mut self,
        party_id: &str,
        participant_uid: &str,
        owner_key: &str,
    ) -> Result;

    /// Insert or replace a pending invitation
    async fn upsert_pending_invitation(&mut self, inv: &PendingInvitation) -> Result;

    /// Delete a pending invitation by its id (no-op if absent)
    async fn delete_pending_invitation(&mut self, id: &str) -> Result;

    /// Delete every pending invitation matching a coordinator's Noise pubkey
    async fn delete_pending_invitations_by_coordinator(
        &mut self,
        coordinator_pubkey: &str,
    ) -> Result;

    /// Insert or replace a workflow run. Used on initial start, on every
    /// state-machine advance, and on resume.
    async fn upsert_workflow_run(&mut self, run: &WorkflowRun) -> Result;

    /// Update the per-step progress fields without touching status.
    async fn update_workflow_run_step(
        &mut self,
        instance_name: &str,
        current_step: &str,
        step_index: i64,
        completed_attestors: &[CantonId],
        updated_at: i64,
    ) -> Result;

    /// Flip the status (and optional error) of a workflow run.
    async fn set_workflow_run_status(
        &mut self,
        instance_name: &str,
        status: WorkflowProgress,
        error: Option<&str>,
        updated_at: i64,
    ) -> Result;

    /// Mark a terminal-state run as dismissed by the operator, hiding it
    /// from the notification feed. No-op when the run is still InProgress.
    async fn dismiss_workflow_run(&mut self, instance_name: &str) -> Result;

    /// Write (insert or replace) a single artefact blob.
    async fn write_workflow_artifact(
        &mut self,
        instance_name: &str,
        artifact_kind: &str,
        attestor: Option<&str>,
        payload: &[u8],
    ) -> Result;

    /// Insert/replace a single identity artefact for a dec party.
    async fn write_dec_party_identity(
        &mut self,
        dec_party_id: &str,
        artifact_kind: &str,
        attestor_id: &str,
        payload: &[u8],
    ) -> Result;
}
