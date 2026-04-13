use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions},
};

use crate::error::Result;

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

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_update_party_packages(pool: SqlitePool) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_party_credentials(&test_creds("party-a")).await?;
        Commitable::commit(tx).await?;

        let dec_id = format!("party-a::{TEST_NS}");
        let new_packages = PackageConfig {
            governance_core: Some("#new-gov".to_string()),
            governance_token_custody: Some("#new-custody".to_string()),
            ..PackageConfig::default()
        };

        let mut tx = pool.begin_transaction().await?;
        tx.update_party_packages(&dec_id, &new_packages).await?;
        Commitable::commit(tx).await?;

        let creds = pool.get_party_credentials(&dec_id).await?.unwrap();
        assert_eq!(creds.packages.governance_core, Some("#new-gov".to_string()));
        assert_eq!(
            creds.packages.governance_token_custody,
            Some("#new-custody".to_string())
        );
        assert_eq!(creds.user_id, "test-user");

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
}
