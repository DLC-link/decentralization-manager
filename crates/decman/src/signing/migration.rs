//! Copies this node's Daml key for each party into DecMan's key store.
//!
//! A participant on Canton 3.6 or older exports its keys freely. A Canton 3.7
//! participant exports only while `canton.parameters.enable-key-download` is
//! on. The task runs at boot and then on a fixed interval, so a node that runs
//! this version before its participant upgrades has its keys copied in time.
//! An operator who upgraded first turns the setting on until the log reports
//! the copy, then turns it off. The vault keeps its copy either way.

use std::time::Duration;

use sqlx::SqlitePool;
use tonic::transport::Channel;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    signing::{
        key_store::{KeyStore, STORE_OFF_HINT},
        vault_export::{export_private_key, export_protocol_version},
    },
    workflow::contracts::steps::sign::own_daml_key,
};

/// How often the task looks for keys it has not copied yet.
pub const MIGRATION_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// What one party's migration came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The key was exported and stored now.
    Stored,
    /// DecMan already held the key.
    AlreadyStored,
    /// A KMS holds the private half, so it cannot be exported. Deploys sign
    /// through the KMS.
    KmsHeld,
}

/// Run the migration at boot and then every [`MIGRATION_INTERVAL`].
pub async fn run_key_migration(config: NodeConfig, db: SqlitePool) {
    let mut tick = tokio::time::interval(MIGRATION_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = migrate_all(&config, &db).await {
            tracing::warn!("The party key migration could not run: {e:#}");
        }
    }
}

/// Copy the key of every party this node knows that DecMan does not hold yet.
///
/// # Errors
///
/// Errors when the store cannot be opened or the parties cannot be listed. A
/// party whose key cannot be copied is logged and skipped.
pub async fn migrate_all(config: &NodeConfig, db: &SqlitePool) -> Result {
    let Some(store) = KeyStore::open(config, db.clone()).await? else {
        tracing::error!(
            "{STORE_OFF_HINT}. DecMan copies no keys, and it cannot sign for a party once \
             the participant stops exporting keys"
        );
        return Ok(());
    };
    let channel = config.admin_channel().await?;
    let protocol_version = export_protocol_version(config).await?;
    for row in db.get_dec_parties_by_prefix("").await? {
        let party = match CantonId::parse(&row.party_id) {
            Ok(party) => party,
            Err(e) => {
                tracing::warn!("Skipping the key migration of {}: {e:#}", row.party_id);
                continue;
            }
        };
        match migrate_party(config, db, &store, (&channel, protocol_version), &party).await {
            Ok(Outcome::Stored) => {
                tracing::info!("Copied this node's Daml key for {party} into DecMan's key store")
            }
            Ok(outcome) => tracing::debug!("Key migration of {party}: {outcome:?}"),
            Err(e) => tracing::warn!(
                "Could not copy this node's Daml key for {party} into DecMan's key store: \
                 {e:#}. If Canton refuses the export, set \
                 canton.parameters.enable-key-download = yes on the participant until this \
                 message stops, then turn it off again"
            ),
        }
    }
    Ok(())
}

async fn migrate_party(
    config: &NodeConfig,
    db: &SqlitePool,
    store: &KeyStore,
    (channel, protocol_version): (&Channel, i32),
    party: &CantonId,
) -> Result<Outcome> {
    let key = own_daml_key(config, db, party).await?;
    if key.kms_key_id.is_some() {
        return Ok(Outcome::KmsHeld);
    }
    match store.get(party, &key.fingerprint).await {
        Ok(Some(_)) => return Ok(Outcome::AlreadyStored),
        Ok(None) => {}
        Err(e) => tracing::warn!("{e:#}. Copying the key again"),
    }
    let seed = export_private_key(channel, &key, protocol_version).await?;
    store
        .put(party, &key.fingerprint, &key.public_key, &seed)
        .await?;
    Ok(Outcome::Stored)
}
