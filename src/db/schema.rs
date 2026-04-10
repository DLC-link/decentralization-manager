use sqlx::SqlitePool;

use crate::{
    config::{PartyCredentials, Peer},
    error::Result,
};

use super::rows::{
    DecPartyContractRow, DecPartyParticipantRow, DecPartyRow, PartyCredentialsRow, PeerRow,
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
}

impl SchemaRead for SqlitePool {
    async fn get_all_peers(&self) -> Result<Vec<Peer>> {
        let rows = sqlx::query_as::<_, PeerRow>("SELECT * FROM peers")
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

    async fn get_party_credentials(&self, dec_party_id: &str) -> Result<Option<PartyCredentials>> {
        let row = sqlx::query_as::<_, PartyCredentialsRow>(
            "SELECT * FROM party_credentials WHERE dec_party_id = ?",
        )
        .bind(dec_party_id)
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

    async fn get_dec_party_owners(&self, party_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT owner_key FROM dec_party_owner WHERE dec_party_id = ?",
        )
        .bind(party_id)
        .fetch_all(self)
        .await?;

        Ok(rows.into_iter().map(|(k,)| k).collect())
    }

    async fn get_dec_party_participants(
        &self,
        party_id: &str,
    ) -> Result<Vec<DecPartyParticipantRow>> {
        let rows = sqlx::query_as::<_, DecPartyParticipantRow>(
            "SELECT * FROM dec_party_participant WHERE dec_party_id = ?",
        )
        .bind(party_id)
        .fetch_all(self)
        .await?;

        Ok(rows)
    }

    async fn get_dec_party_contracts(&self, party_id: &str) -> Result<Vec<DecPartyContractRow>> {
        let rows = sqlx::query_as::<_, DecPartyContractRow>(
            "SELECT * FROM dec_party_contract WHERE dec_party_id = ?",
        )
        .bind(party_id)
        .fetch_all(self)
        .await?;

        Ok(rows)
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
                keycloak_password
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn upsert_dec_party(&mut self, row: &DecPartyRow) -> Result {
        sqlx::query(
            r"
            INSERT OR REPLACE INTO dec_party (
                party_id,
                prefix,
                threshold,
                updated_at
            ) VALUES (?, ?, ?, ?)
            ",
        )
        .bind(&row.party_id)
        .bind(&row.prefix)
        .bind(row.threshold)
        .bind(row.updated_at)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn replace_dec_party_owners(&mut self, party_id: &str, owners: &[String]) -> Result {
        sqlx::query("DELETE FROM dec_party_owner WHERE dec_party_id = ?")
            .bind(party_id)
            .execute(&mut **self)
            .await?;

        for owner in owners {
            sqlx::query(
                r"
                INSERT INTO dec_party_owner (dec_party_id, owner_key)
                VALUES (?, ?)
                ",
            )
            .bind(party_id)
            .bind(owner)
            .execute(&mut **self)
            .await?;
        }

        Ok(())
    }

    async fn replace_dec_party_participants(
        &mut self,
        party_id: &str,
        participants: &[DecPartyParticipantRow],
    ) -> Result {
        sqlx::query("DELETE FROM dec_party_participant WHERE dec_party_id = ?")
            .bind(party_id)
            .execute(&mut **self)
            .await?;

        for p in participants {
            sqlx::query(
                r"
                INSERT INTO dec_party_participant (
                    dec_party_id,
                    participant_uid,
                    permission
                ) VALUES (?, ?, ?)
                ",
            )
            .bind(party_id)
            .bind(&p.participant_uid)
            .bind(&p.permission)
            .execute(&mut **self)
            .await?;
        }

        Ok(())
    }

    async fn replace_dec_party_contracts(
        &mut self,
        party_id: &str,
        contracts: &[DecPartyContractRow],
    ) -> Result {
        sqlx::query("DELETE FROM dec_party_contract WHERE dec_party_id = ?")
            .bind(party_id)
            .execute(&mut **self)
            .await?;

        for c in contracts {
            sqlx::query(
                r"
                INSERT INTO dec_party_contract (
                    dec_party_id,
                    contract_id,
                    template_id,
                    package_id
                ) VALUES (?, ?, ?, ?)
                ",
            )
            .bind(party_id)
            .bind(&c.contract_id)
            .bind(&c.template_id)
            .bind(&c.package_id)
            .execute(&mut **self)
            .await?;
        }

        Ok(())
    }

    async fn delete_dec_parties_by_prefix(&mut self, prefix: &str) -> Result {
        // CASCADE deletes owners, participants, and contracts
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
}
