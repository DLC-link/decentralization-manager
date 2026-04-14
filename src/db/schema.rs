use sqlx::SqlitePool;

use crate::{
    config::{PartyCredentials, Peer},
    error::Result,
};

use super::rows::{PartyCredentialsRow, PeerRow};

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
}
