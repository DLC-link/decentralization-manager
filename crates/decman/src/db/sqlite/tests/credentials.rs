//! The party credentials table.

use sqlx::SqlitePool;

use crate::db::sqlite::connect::MIGRATOR;
use crate::{
    canton_id::CantonId,
    config::{Auth0M2MConfig, PartyCredentials},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
};

use super::fixtures::{TEST_NS, test_creds};

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

    let dec_id = CantonId::parse(&format!("party-a::{TEST_NS}")).unwrap();
    assert!(pool.get_party_credentials(&dec_id).await?.is_some());
    let nonexistent = CantonId::parse(&format!("nonexistent::{TEST_NS}")).unwrap();
    assert!(pool.get_party_credentials(&nonexistent).await?.is_none());

    Ok(())
}

// ====================================================================
// Party credentials — Auth0 round-trip
// ====================================================================

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_party_credentials_auth0_roundtrip(pool: SqlitePool) -> Result {
    let mut creds = test_creds("party-a");
    creds.auth0 = Some(Auth0M2MConfig {
        domain: "tenant.us.auth0.com".to_string(),
        audience: "https://api.example.com".to_string(),
        client_id: "auth0-client-id".to_string(),
        client_secret: "auth0-client-secret".to_string(),
    });

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_party_credentials(&creds).await?;
    Commitable::commit(tx).await?;

    let assert_auth0 = |c: &PartyCredentials| {
        let Some(a) = c.auth0.as_ref() else {
            panic!("auth0 config did not round-trip");
        };
        assert_eq!(a.domain, "tenant.us.auth0.com");
        assert_eq!(a.audience, "https://api.example.com");
        assert_eq!(a.client_id, "auth0-client-id");
        assert_eq!(a.client_secret, "auth0-client-secret");
    };

    let all = pool.get_all_party_credentials().await?;
    assert_eq!(all.len(), 1);
    assert_auth0(&all[0]);

    let Some(by_id) = pool.get_party_credentials(&creds.dec_party_id).await? else {
        panic!("credentials not found by dec_party_id");
    };
    assert_auth0(&by_id);

    Ok(())
}
