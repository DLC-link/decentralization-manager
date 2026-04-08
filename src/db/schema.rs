use sqlx::SqlitePool;

use crate::{
    config::{PackageConfig, PartyCredentials, Peer},
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

    /// Update only the package fields for a party
    async fn update_party_packages(
        &mut self,
        dec_party_id: &str,
        packages: &PackageConfig,
    ) -> Result;
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
        let row = PartyCredentialsRow::from_domain(creds);

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
                governance_core,
                governance_token_custody,
                utility_credential,
                utility_registry,
                vault,
                vault_governance
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(&row.governance_core)
        .bind(&row.governance_token_custody)
        .bind(&row.utility_credential)
        .bind(&row.utility_registry)
        .bind(&row.vault)
        .bind(&row.vault_governance)
        .execute(&mut **self)
        .await?;

        Ok(())
    }

    async fn update_party_packages(
        &mut self,
        dec_party_id: &str,
        packages: &PackageConfig,
    ) -> Result {
        sqlx::query(
            r"
            UPDATE party_credentials
            SET governance_core = ?,
                governance_token_custody = ?,
                utility_credential = ?,
                utility_registry = ?,
                vault = ?,
                vault_governance = ?
            WHERE dec_party_id = ?
            ",
        )
        .bind(&packages.governance_core)
        .bind(&packages.governance_token_custody)
        .bind(&packages.utility_credential)
        .bind(&packages.utility_registry)
        .bind(&packages.vault)
        .bind(&packages.vault_governance)
        .bind(dec_party_id)
        .execute(&mut **self)
        .await?;

        Ok(())
    }
}
