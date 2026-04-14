use crate::{
    config::{PartyCredentials, Peer},
    error::Result,
};

use super::rows::{DecPartyContractRow, DecPartyParticipantRow, DecPartyRow};

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

    /// Get contracts for a decentralized party
    async fn get_dec_party_contracts(&self, party_id: &str) -> Result<Vec<DecPartyContractRow>>;
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

    /// Update the owner key for a specific participant in a decentralized party
    async fn update_participant_owner_key(
        &mut self,
        party_id: &str,
        participant_uid: &str,
        owner_key: &str,
    ) -> Result;
}
