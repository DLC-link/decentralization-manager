use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use super::{
    rows::{
        ChainAuditCacheRow, DecPartyContractRow, DecPartyParticipantRow, DecPartyRow,
        GovernanceAuditRow, PartyCredentialsRow, PeerRow,
    },
    schema::{Commitable, SchemaRead, SchemaWrite},
};
use crate::{
    config::{PartyCredentials, Peer},
    error::Result,
    participant_id::CantonId,
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
        party_id: &str,
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
        .bind(party_id)
        .bind(limit)
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
                updated_at,
                my_owner_key
            ) VALUES (?, ?, ?, ?, ?)
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
                    permission,
                    owner_key
                ) VALUES (?, ?, ?, ?)
                ",
            )
            .bind(party_id)
            .bind(&p.participant_uid)
            .bind(&p.permission)
            .bind(&p.owner_key)
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

    async fn update_participant_owner_key(
        &mut self,
        party_id: &str,
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
        .bind(party_id)
        .bind(participant_uid)
        .execute(&mut **self)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use crate::{
        config::{KeycloakConfig, PackageConfig, PartyCredentials, Peer},
        db::{
            rows::{DecPartyContractRow, DecPartyParticipantRow, DecPartyRow},
            schema::{Commitable, SchemaRead, SchemaWrite},
        },
        error::Result,
        participant_id::CantonId,
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
            packages: PackageConfig {
                governance_core: Some("#gov-core".to_string()),
                governance_token_custody: None,
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

        let dec_id = format!("party-a::{TEST_NS}");
        assert!(pool.get_party_credentials(&dec_id).await?.is_some());
        assert!(pool.get_party_credentials("nonexistent").await?.is_none());

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
        let party_id = format!("net-a::{TEST_NS}");
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
        let party_id = format!("net-a::{TEST_NS}");
        let participants = vec![
            DecPartyParticipantRow {
                dec_party_id: party_id.clone(),
                participant_uid: "node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fingerprint-1".to_string()),
            },
            DecPartyParticipantRow {
                dec_party_id: party_id.clone(),
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
        assert_eq!(result[0].permission, "submission");
        assert_eq!(result[0].owner_key, Some("fingerprint-1".to_string()));
        assert_eq!(result[1].owner_key, None);

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_dec_party_contracts(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        let party_id = format!("net-a::{TEST_NS}");
        let contracts = vec![
            DecPartyContractRow {
                dec_party_id: party_id.clone(),
                contract_id: "contract-1".to_string(),
                template_id: "Governance:GovernanceRules".to_string(),
                package_id: "#gov-core".to_string(),
            },
            DecPartyContractRow {
                dec_party_id: party_id.clone(),
                contract_id: "contract-2".to_string(),
                template_id: "Vault:Vault".to_string(),
                package_id: "#vault".to_string(),
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
        let party_id = format!("net-a::{TEST_NS}");

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&test_dec_party("net-a")).await?;
        tx.replace_dec_party_owners(&party_id, &["owner-1".to_string()])
            .await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id.clone(),
                participant_uid: "node1".to_string(),
                permission: "submission".to_string(),
                owner_key: None,
            }],
        )
        .await?;
        tx.replace_dec_party_contracts(
            &party_id,
            &[DecPartyContractRow {
                dec_party_id: party_id.clone(),
                contract_id: "c1".to_string(),
                template_id: "t1".to_string(),
                package_id: "p1".to_string(),
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
}
