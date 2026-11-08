//! These tests turn on DecMan's database encryption. The key is set once for
//! the whole process, so they live in their own test binary.

use std::sync::Once;

use dec_party_manager::{
    canton_id::CantonId,
    db::{
        MIGRATOR, crypto,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    workflow::{contracts::steps::sign::backfill_peer_keys, storage::artifact_kinds},
};
use sqlx::SqlitePool;

type Result<T = ()> = anyhow::Result<T>;

const PARTY: &str =
    "dec-party::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MEMBER: &str = "member::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NODE: &str =
    "participant1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn encryption_on() {
    static ON: Once = Once::new();
    ON.call_once(|| crypto::init_key("database encryption test"));
}

async fn completed_onboarding(db: &SqlitePool, instance: &str) -> Result {
    sqlx::query(
        "INSERT INTO workflow_runs (instance_name, kind, role, status, current_step, \
         step_index, step_total, config_json, expected_peers_json, completed_peers_json, \
         dec_party_id, created_at, updated_at) \
         VALUES (?, 'Onboarding', 'Coordinator', 'completed', '', 0, 0, '{}', '[]', '[]', ?, 1, 1)",
    )
    .bind(instance)
    .bind(PARTY)
    .execute(db)
    .await?;
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn the_onboarding_backfill_decrypts_the_stored_keys(db: SqlitePool) -> Result {
    encryption_on();
    let keys = b"this node's namespace and Daml public keys";
    completed_onboarding(&db, "onboarding-1").await?;
    let mut tx = db.begin_transaction().await?;
    tx.write_workflow_artifact(
        "onboarding-1",
        artifact_kinds::PEER_PUBLIC_KEYS,
        Some(NODE),
        keys,
    )
    .await?;
    Commitable::commit(tx).await?;

    let (stored,): (Vec<u8>,) = sqlx::query_as("SELECT payload FROM workflow_artifacts")
        .fetch_one(&db)
        .await?;
    assert_ne!(stored, keys, "the artifact is stored encrypted");

    let backfilled = backfill_peer_keys(&db, &CantonId::parse(PARTY)?, NODE).await?;
    assert_eq!(backfilled.as_deref(), Some(&keys[..]));
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn rows_written_before_the_key_existed_stay_readable(db: SqlitePool) -> Result {
    encryption_on();
    // Each value is longer than a nonce, and the secret also decodes as
    // base64, so DecMan tries to decrypt both before it returns them unchanged.
    let keys = b"keys that onboarding stored before encryption";
    let secret = "c2VjcmV0IHN0b3JlZCBpbiBwbGFpbnRleHQ=";
    completed_onboarding(&db, "onboarding-1").await?;
    sqlx::query(
        "INSERT INTO workflow_artifacts (instance_name, artifact_kind, peer_id, payload, \
         created_at) VALUES ('onboarding-1', ?, ?, ?, 1)",
    )
    .bind(artifact_kinds::PEER_PUBLIC_KEYS)
    .bind(NODE)
    .bind(&keys[..])
    .execute(&db)
    .await?;
    sqlx::query(
        "INSERT INTO party_credentials (dec_party_id, member_party_id, user_id, \
         keycloak_client_id, keycloak_client_secret) VALUES (?, ?, 'decman', 'decman', ?)",
    )
    .bind(PARTY)
    .bind(MEMBER)
    .bind(secret)
    .execute(&db)
    .await?;

    let artifact = db
        .read_workflow_artifact("onboarding-1", artifact_kinds::PEER_PUBLIC_KEYS, Some(NODE))
        .await?;
    assert_eq!(artifact.as_deref(), Some(&keys[..]));
    let party = CantonId::parse(PARTY)?;
    let backfilled = backfill_peer_keys(&db, &party, NODE).await?;
    assert_eq!(backfilled.as_deref(), Some(&keys[..]));
    let credentials = db
        .get_party_credentials(&party)
        .await?
        .expect("the credentials row");
    assert_eq!(credentials.keycloak.client_id, "decman");
    assert_eq!(credentials.keycloak.client_secret.as_deref(), Some(secret));
    Ok(())
}
