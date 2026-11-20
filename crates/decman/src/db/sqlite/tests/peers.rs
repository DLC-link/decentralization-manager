//! The peers table.

use sqlx::SqlitePool;

use crate::db::sqlite::connect::MIGRATOR;
use crate::{
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
};

use super::fixtures::test_peer;

// ====================================================================
// Peers
// ====================================================================

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_insert_and_get_peers(pool: SqlitePool) -> Result {
    assert_eq!(pool.get_all_peers().await?.len(), 0);

    let mut tx = pool.begin_transaction().await?;
    tx.insert_peer(&test_peer(1)).await?;
    tx.insert_peer(&test_peer(2)).await?;
    Commitable::commit(tx).await?;

    assert_eq!(pool.get_all_peers().await?.len(), 2);

    let peers = pool.get_all_peers().await?;
    assert_eq!(peers.len(), 2);
    assert_eq!(peers[0].name, "Node 1");
    assert_eq!(peers[1].port, 9002);

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

    assert_eq!(pool.get_all_peers().await?.len(), 2);

    let mut tx = pool.begin_transaction().await?;
    tx.delete_all_peers().await?;
    Commitable::commit(tx).await?;

    assert_eq!(pool.get_all_peers().await?.len(), 0);

    Ok(())
}
