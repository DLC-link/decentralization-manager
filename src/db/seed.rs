use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
};

/// Seed the database from config files if tables are empty.
///
/// Loads peers from peers.csv and party credentials from node.toml's
/// `[[parties]]` sections, inserting them into the database only when
/// the corresponding table is empty and the source files exist.
/// This is a transitional bridge for migrating from file-based config
/// to database storage.
pub async fn seed_from_config(pool: &SqlitePool, config: &NodeConfig) -> Result {
    // Seed peers from CSV (only if file exists and DB is empty)
    let peer_count = pool.get_peer_count().await?;
    if peer_count == 0 {
        let csv_path = config.peers_csv_path();
        if csv_path.exists() {
            match config.load_network_config().await {
                Ok(network_config) if !network_config.peers.is_empty() => {
                    tracing::info!(
                        "Seeding {} peers from CSV into database",
                        network_config.peers.len()
                    );
                    let mut tx = pool.begin_transaction().await?;
                    for peer in &network_config.peers {
                        tx.insert_peer(peer).await?;
                    }
                    Commitable::commit(tx).await?;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to read peers.csv for seeding: {e}");
                }
            }
        }
    }

    // Seed party credentials from TOML (only if present and DB is empty)
    let creds_count = pool.get_all_party_credentials().await?.len();
    if creds_count == 0 && !config.parties.is_empty() {
        tracing::info!(
            "Seeding {} party credentials from TOML into database",
            config.parties.len()
        );
        let mut tx = pool.begin_transaction().await?;
        for creds in &config.parties {
            tx.upsert_party_credentials(creds).await?;
        }
        Commitable::commit(tx).await?;
    }

    Ok(())
}
