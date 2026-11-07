//! Copies this node's Daml key for each party into DecMan's key store.
//!
//! A participant on Canton 3.6 or older exports its keys freely. A Canton 3.7
//! participant exports only while `canton.parameters.enable-key-download` is
//! on. The task runs at boot and then on a fixed interval, so a node that runs
//! this version before its participant upgrades has its keys copied in time.
//! It copies only the parties that this node hosts.
//! An operator who upgraded first turns the setting on until the log reports
//! the copy, then turns it off. The vault keeps its copy either way.

use std::{collections::BTreeSet, time::Duration};

use sqlx::SqlitePool;
use tonic::transport::Channel;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    signing::{
        key_store::{KeyStore, STORE_OFF_HINT},
        vault_export::{export_private_key, export_protocol_version, key_download_disabled},
    },
    workflow::contracts::steps::sign::own_daml_key,
};

/// How often the task looks for keys it has not copied yet.
pub const MIGRATION_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How long the task waits after a run that could not finish. Each further
/// failure doubles the wait, up to [`MIGRATION_INTERVAL`]. The participant
/// may still be starting when DecMan boots.
const FIRST_RETRY: Duration = Duration::from_secs(30);

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
    /// Canton refused the export, because the participant does not allow key
    /// download.
    ExportRefused,
}

/// Run the migration at boot and then every [`MIGRATION_INTERVAL`]. A run
/// that cannot finish is retried sooner.
pub async fn run_key_migration(config: NodeConfig, db: SqlitePool) {
    let mut retry = FIRST_RETRY;
    loop {
        let wait = match migrate_all(&config, &db).await {
            Ok(()) => {
                retry = FIRST_RETRY;
                MIGRATION_INTERVAL
            }
            Err(e) => {
                tracing::warn!(
                    "The party key migration could not run: {e:#}. Retrying in {}s",
                    retry.as_secs()
                );
                let wait = retry;
                retry = next_retry(retry);
                wait
            }
        };
        tokio::time::sleep(wait).await;
    }
}

fn next_retry(retry: Duration) -> Duration {
    retry.saturating_mul(2).min(MIGRATION_INTERVAL)
}

/// Copy the key of every party this node hosts that DecMan does not hold yet.
///
/// # Errors
///
/// Errors when the store cannot be opened, the parties cannot be listed, or
/// the participant cannot be reached. A party whose key cannot be copied is
/// logged and skipped.
pub async fn migrate_all(config: &NodeConfig, db: &SqlitePool) -> Result {
    let Some(store) = KeyStore::open(config, db.clone()).await? else {
        tracing::error!(
            "{STORE_OFF_HINT}. DecMan copies no keys, and it cannot sign for a party once \
             the participant stops exporting keys"
        );
        return Ok(());
    };
    let parties = hosted_parties(db, &config.participant_id().to_string()).await?;
    if parties.is_empty() {
        return Ok(());
    }
    let channel = config.admin_channel().await?;
    let protocol_version = export_protocol_version(config).await?;
    for party in parties {
        match migrate_party(config, db, &store, (&channel, protocol_version), &party).await {
            Ok(Outcome::Stored) => {
                tracing::info!("Copied this node's Daml key for {party} into DecMan's key store")
            }
            Ok(Outcome::ExportRefused) => tracing::warn!(
                "Canton refused to export this node's Daml key for {party}, because key \
                 download is off. Set enableKeyDownload: true in the splice-participant Helm \
                 values, or canton.parameters.enable-key-download = yes, until this message \
                 stops. Then turn it off again"
            ),
            Ok(outcome) => tracing::debug!("Key migration of {party}: {outcome:?}"),
            Err(e) => tracing::warn!(
                "Could not copy this node's Daml key for {party} into DecMan's key store: {e:#}"
            ),
        }
    }
    Ok(())
}

/// The decentralized parties that DecMan's party cache lists
/// `participant_uid` as a host of. The cache also holds parties that other
/// members host, and this node has no key for those.
async fn hosted_parties(db: &SqlitePool, participant_uid: &str) -> Result<Vec<CantonId>> {
    let party_ids: BTreeSet<String> = db
        .get_all_dec_party_participants("")
        .await?
        .into_iter()
        .filter(|host| host.participant_uid == participant_uid)
        .map(|host| host.dec_party_id)
        .collect();
    Ok(party_ids
        .into_iter()
        .filter_map(|party_id| match CantonId::parse(&party_id) {
            Ok(party) => Some(party),
            Err(e) => {
                tracing::warn!("Skipping the key migration of {party_id}: {e:#}");
                None
            }
        })
        .collect())
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
    let seed = match export_private_key(channel, &key, protocol_version).await {
        Ok(seed) => seed,
        Err(e) if key_download_disabled(&e) => return Ok(Outcome::ExportRefused),
        Err(e) => return Err(e.into()),
    };
    store
        .put(party, &key.fingerprint, &key.public_key, &seed)
        .await?;
    Ok(Outcome::Stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        MIGRATOR,
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaWrite},
    };

    const NS: &str = "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    async fn cache_party(db: &SqlitePool, prefix: &str, hosts: &[&str]) -> Result {
        let party_id = format!("{prefix}::{NS}");
        let mut tx = db.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party_id.clone(),
            prefix: prefix.to_string(),
            threshold: 1,
            updated_at: 0,
            my_owner_key: None,
            confirmation_threshold: None,
        })
        .await?;
        let rows: Vec<_> = hosts
            .iter()
            .map(|host| DecPartyParticipantRow {
                dec_party_id: party_id.clone(),
                participant_uid: host.to_string(),
                permission: "confirmation".to_string(),
                owner_key: None,
                signing_key: None,
            })
            .collect();
        tx.replace_dec_party_participants(&CantonId::parse(&party_id)?, &rows)
            .await?;
        Commitable::commit(tx).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn only_the_parties_this_node_hosts_are_copied(db: SqlitePool) -> Result {
        cache_party(&db, "ours", &["me::1220bb", "peer::1220cc"]).await?;
        cache_party(&db, "theirs", &["peer::1220cc"]).await?;
        cache_party(&db, "also-ours", &["me::1220bb"]).await?;

        let hosted: Vec<String> = hosted_parties(&db, "me::1220bb")
            .await?
            .iter()
            .map(ToString::to_string)
            .collect();

        assert_eq!(
            hosted,
            vec![format!("also-ours::{NS}"), format!("ours::{NS}")]
        );
        Ok(())
    }

    #[test]
    fn a_failed_run_is_retried_sooner_and_backs_off_to_the_interval() {
        let mut retry = FIRST_RETRY;
        let mut waits = vec![];
        for _ in 0..7 {
            waits.push(retry.as_secs());
            retry = next_retry(retry);
        }
        assert_eq!(waits, vec![30, 60, 120, 240, 480, 900, 900]);
    }
}
