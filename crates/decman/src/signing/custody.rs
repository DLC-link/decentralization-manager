//! Where a new party Daml key may live.
//!
//! Canton 3.7 refuses to export a key from its software vault unless the node
//! sets `enable-key-download`, and Canton's configuration reference says that
//! setting "should not be enabled in production". A key in AWS KMS needs no
//! export, because DecMan signs with the KMS `Sign` API. DecMan therefore
//! puts a new party's keys into a vault that keeps its keys in KMS.
//!
//! Two settings allow the software vault:
//!
//! - `DECPM_ALLOW_SOFTWARE_PARTY_KEYS` is for a participant that has not moved
//!   to KMS yet. DecMan then needs a copy of each new Daml key in its key
//!   store, so it exports the key at once and refuses it when Canton refuses.
//! - Insecure mode allows the software vault with a warning only. It runs on
//!   devnet only.
//!
//! A vault that only wraps its software keys with a KMS key (`wrapper_key_id`)
//! does not count. Those keys still need an export to sign.

use canton_proto_rs::com::digitalasset::canton::crypto::{
    admin::v30::{
        ListKeysFilters, ListMyKeysRequest, PrivateKeyMetadata,
        vault_service_client::VaultServiceClient,
    },
    v30::SigningPublicKey,
};
use sqlx::SqlitePool;
use tonic::transport::Channel;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    signing::{
        SigningKeyContext,
        aws_kms::AwsKmsSigner,
        key_store::{KeyStore, STORE_OFF_HINT},
        migration::{self, Outcome},
        vault_export::{export_private_key, export_protocol_version, key_download_disabled},
    },
    utils::compute_fingerprint,
};

/// What this node does with a new party key outside KMS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoftwareKeys {
    /// Insecure mode accepts the key with a warning.
    Allowed,
    /// The operator allowed software keys. DecMan accepts a Daml key only
    /// when it can copy the key into its key store.
    Stored,
    /// DecMan refuses the key.
    Refused,
}

/// This node's rule for new party keys outside KMS.
pub fn software_keys(config: &NodeConfig) -> SoftwareKeys {
    if config.insecure {
        SoftwareKeys::Allowed
    } else if config.allow_software_party_keys {
        SoftwareKeys::Stored
    } else {
        SoftwareKeys::Refused
    }
}

/// Refuse to create party keys on a participant that keeps no key in KMS.
///
/// Call this before any key is generated, so a refused run leaves nothing
/// behind in the vault or the topology. A node that allows software keys
/// must have its key store on, because it needs a copy of each new key.
///
/// # Errors
///
/// Errors when the vault cannot be listed, or when it holds no KMS key and
/// this node's rule refuses software keys.
pub async fn require_kms_participant(
    config: &NodeConfig,
    db: &SqlitePool,
    vault_client: &mut VaultServiceClient<Channel>,
) -> Result {
    let metadata = list_keys(vault_client, None).await?;
    let has_kms = metadata.iter().any(in_kms);
    let rule = software_keys(config);
    check(
        has_kms,
        rule,
        "This participant keeps its keys in Canton's software vault, not in a KMS",
    )?;
    if !has_kms && rule == SoftwareKeys::Stored {
        open_store(config, db).await?;
    }
    Ok(())
}

/// Refuse a party key that does not live in KMS, and return its KMS key id.
///
/// A key found by name can predate the participant's move to KMS, so the key
/// itself is checked as well as the participant. Only the Daml key needs an
/// export to sign, but the namespace key gets the same rule, so a party's
/// keys live in one place. `None` means a software key that this node's rule
/// accepts.
///
/// # Errors
///
/// Errors when the vault cannot be listed, does not hold the key, or holds it
/// outside KMS while this node's rule refuses software keys.
pub async fn require_kms_key(
    config: &NodeConfig,
    vault_client: &mut VaultServiceClient<Channel>,
    fingerprint: &Fingerprint,
) -> Result<Option<String>> {
    let metadata = list_keys(vault_client, Some(fingerprint)).await?;
    anyhow::ensure!(
        !metadata.is_empty(),
        "The vault does not hold the key {fingerprint} it just returned"
    );
    check(
        metadata.iter().all(in_kms),
        software_keys(config),
        &format!("The party key {fingerprint} is in Canton's software vault, not in a KMS"),
    )?;
    Ok(metadata
        .iter()
        .find(|m| in_kms(m))
        .and_then(|m| m.kms_key_id.clone()))
}

/// Make sure DecMan can sign with a new Daml key outside KMS.
///
/// On a node that allows software keys, DecMan exports the key at once. A
/// refused export means DecMan could never sign with the key, so the run
/// stops before the key joins a party. When the party is known, DecMan
/// stores the copy now. Onboarding learns the party later and copies the key
/// with [`copy_new_party_key`]. Other nodes need nothing here.
///
/// # Errors
///
/// Errors when the key store is off, when Canton refuses the export, or when
/// the copy cannot be stored.
pub async fn secure_software_daml_key(
    config: &NodeConfig,
    db: &SqlitePool,
    key: &SigningPublicKey,
    kms_key_id: Option<&str>,
    party: Option<&CantonId>,
) -> Result {
    if kms_key_id.is_some() || software_keys(config) != SoftwareKeys::Stored {
        return Ok(());
    }
    let store = open_store(config, db).await?;
    let fingerprint = compute_fingerprint(key);
    let context = SigningKeyContext {
        fingerprint: fingerprint.clone(),
        public_key: key.clone(),
        kms_key_id: None,
    };
    let channel = config.admin_channel().await?;
    let protocol_version = export_protocol_version(config).await?;
    let seed = match export_private_key(&channel, &context, protocol_version).await {
        Ok(seed) => seed,
        Err(e) if key_download_disabled(&e) => anyhow::bail!(
            "Canton refused to export the new Daml key {fingerprint}, because key download is \
             off. DecMan cannot sign with a key it cannot copy. Move the participant to KMS, or \
             turn on key download while the party's keys are created"
        ),
        Err(e) => return Err(e.into()),
    };
    if let Some(party) = party {
        store.put(party, &fingerprint, key, &seed).await?;
        tracing::info!("Copied the new Daml key {fingerprint} for {party} into DecMan's key store");
    }
    Ok(())
}

/// Copy this node's Daml key for a party that onboarding just created.
///
/// Onboarding creates the key before the party exists, and DecMan stores
/// each key under its party. The background copy task would store it later,
/// but key download may be off by then. A failure is only logged, because
/// the party exists already and the copy task tries again.
pub async fn copy_new_party_key(config: &NodeConfig, db: &SqlitePool, party: &CantonId) {
    if software_keys(config) != SoftwareKeys::Stored {
        return;
    }
    match migration::copy_party_key(config, db, party).await {
        Ok(Outcome::ExportRefused | Outcome::CopyUnreadable) => tracing::error!(
            "Canton refused to export this node's Daml key for {party}, so DecMan holds no copy \
             of it. Turn on key download until the copy task stores the key"
        ),
        Ok(outcome) => {
            tracing::info!("Key store copy of this node's Daml key for {party}: {outcome:?}")
        }
        Err(e) => tracing::error!(
            "Could not copy this node's Daml key for {party} into DecMan's key store: {e:#}. \
             The copy task tries again every 15 minutes"
        ),
    }
}

/// Check that DecMan can sign with a new KMS key before the key joins a party.
///
/// The participant creates the key under its own AWS role, so DecMan's role
/// has no access to it until an operator grants `kms:Sign`. A rotation that
/// submits such a key would leave the party with a key DecMan cannot use.
///
/// # Errors
///
/// Errors when KMS refuses the call or returns a signature that does not
/// match the key.
pub async fn require_kms_sign_access(
    fingerprint: &Fingerprint,
    key: &SigningPublicKey,
    kms_key_id: &str,
) -> Result {
    let context = SigningKeyContext {
        fingerprint: fingerprint.clone(),
        public_key: key.clone(),
        kms_key_id: Some(kms_key_id.to_string()),
    };
    AwsKmsSigner::new()
        .await
        .check_access(&context)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "DecMan cannot sign with the new KMS key {kms_key_id} ({fingerprint}): {e}. \
                 Give DecMan's AWS role kms:Sign on that key, as docs/KMS_SIGNING.md explains. \
                 Then start the rotation again, which reuses the same key"
            )
        })
}

/// Log once at boot whether this node can create party keys, so an operator
/// learns it before a run fails. A participant that cannot be reached yet is
/// skipped, because every run checks again.
pub async fn log_rule_at_boot(config: &NodeConfig, db: &SqlitePool) {
    let metadata = match config.admin_channel().await {
        Ok(channel) => list_keys(&mut VaultServiceClient::new(channel), None).await,
        Err(e) => Err(e),
    };
    let has_kms = match metadata {
        Ok(metadata) => metadata.iter().any(in_kms),
        Err(e) => {
            tracing::debug!("Skipped the party key rule check at boot: {e:#}");
            return;
        }
    };
    match (has_kms, software_keys(config)) {
        (true, _) => {
            tracing::info!("This participant keeps its keys in KMS. New party keys go there")
        }
        (false, SoftwareKeys::Allowed) => tracing::warn!(
            "This participant keeps its keys in Canton's software vault. Insecure mode lets \
             DecMan create party keys there"
        ),
        (false, SoftwareKeys::Stored) => match open_store(config, db).await {
            Ok(_) => tracing::info!(
                "This participant keeps its keys in Canton's software vault. \
                 DECPM_ALLOW_SOFTWARE_PARTY_KEYS lets DecMan create party keys there, and \
                 DecMan keeps a copy of each new Daml key"
            ),
            Err(e) => tracing::error!("DecMan cannot create party keys on this node: {e:#}"),
        },
        (false, SoftwareKeys::Refused) => tracing::warn!(
            "This participant keeps its keys in Canton's software vault, not in a KMS. DecMan \
             refuses to create or join a party here, and to rotate a key. Set the participant's \
             crypto provider to kms, or set DECPM_ALLOW_SOFTWARE_PARTY_KEYS=true"
        ),
    }
}

async fn open_store(config: &NodeConfig, db: &SqlitePool) -> Result<KeyStore> {
    KeyStore::open(config, db.clone()).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "{STORE_OFF_HINT}. DECPM_ALLOW_SOFTWARE_PARTY_KEYS needs the key store, because \
             DecMan must hold a copy of each software key it signs with"
        )
    })
}

fn in_kms(metadata: &PrivateKeyMetadata) -> bool {
    metadata
        .kms_key_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
}

fn check(in_kms: bool, rule: SoftwareKeys, problem: &str) -> Result {
    if in_kms {
        return Ok(());
    }
    match rule {
        SoftwareKeys::Allowed => {
            tracing::warn!(
                "{problem}. Insecure mode allows it. Its first deploy on Splice 0.10 needs \
                 canton.parameters.enable-key-download"
            );
            Ok(())
        }
        SoftwareKeys::Stored => {
            tracing::info!(
                "{problem}. DECPM_ALLOW_SOFTWARE_PARTY_KEYS allows it, so DecMan keeps a copy \
                 of the Daml key in its key store"
            );
            Ok(())
        }
        SoftwareKeys::Refused => anyhow::bail!(
            "{problem}. DecMan creates party keys only on a KMS-backed participant, because \
             Canton 3.7 refuses to export a software key. Set the participant's crypto provider \
             to kms (canton.participants.<name>.crypto.provider = kms). Until then, set \
             DECPM_ALLOW_SOFTWARE_PARTY_KEYS=true to keep copies in DecMan's key store"
        ),
    }
}

async fn list_keys(
    vault_client: &mut VaultServiceClient<Channel>,
    fingerprint: Option<&Fingerprint>,
) -> Result<Vec<PrivateKeyMetadata>> {
    Ok(vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: fingerprint.map(ToString::to_string).unwrap_or_default(),
                name: String::new(),
                purpose: vec![],
                usage_v30: vec![],
            }),
            base_request: None,
        }))
        .await?
        .into_inner()
        .private_keys_metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(kms_key_id: Option<&str>, wrapper_key_id: Option<&str>) -> PrivateKeyMetadata {
        PrivateKeyMetadata {
            public_key_with_name: None,
            wrapper_key_id: wrapper_key_id.map(str::to_string),
            kms_key_id: kms_key_id.map(str::to_string),
        }
    }

    fn config(insecure: bool, allow_software_party_keys: bool) -> NodeConfig {
        let mut config = NodeConfig::default();
        config.insecure = insecure;
        config.allow_software_party_keys = allow_software_party_keys;
        config
    }

    #[test]
    fn a_kms_key_counts_and_a_wrapped_software_key_does_not() {
        assert!(in_kms(&metadata(Some("mrk-0123"), None)));
        assert!(!in_kms(&metadata(None, Some("wrapper"))));
        assert!(!in_kms(&metadata(Some(""), None)));
        assert!(!in_kms(&metadata(None, None)));
    }

    #[test]
    fn the_rule_comes_from_insecure_mode_then_the_opt_out() {
        assert_eq!(software_keys(&config(false, false)), SoftwareKeys::Refused);
        assert_eq!(software_keys(&config(false, true)), SoftwareKeys::Stored);
        assert_eq!(software_keys(&config(true, false)), SoftwareKeys::Allowed);
        assert_eq!(software_keys(&config(true, true)), SoftwareKeys::Allowed);
    }

    #[test]
    fn a_software_key_is_refused_without_a_setting_that_allows_it() {
        let error = check(false, SoftwareKeys::Refused, "Software vault")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("Software vault"), "{error}");
        assert!(error.contains("crypto.provider = kms"), "{error}");
        assert!(error.contains("DECPM_ALLOW_SOFTWARE_PARTY_KEYS"), "{error}");
    }

    #[test]
    fn insecure_mode_and_the_opt_out_accept_a_software_key() -> Result {
        check(false, SoftwareKeys::Allowed, "Software vault")?;
        check(false, SoftwareKeys::Stored, "Software vault")
    }

    #[test]
    fn a_kms_key_is_accepted_under_every_rule() -> Result {
        for rule in [
            SoftwareKeys::Allowed,
            SoftwareKeys::Stored,
            SoftwareKeys::Refused,
        ] {
            check(true, rule, "unused")?;
        }
        Ok(())
    }

    /// Only the opt-out exports a key. These calls return before the export,
    /// so they need no Canton.
    #[tokio::test]
    async fn only_the_opt_out_copies_a_software_key() -> Result {
        let db = SqlitePool::connect("sqlite::memory:").await?;
        let key = SigningPublicKey::default();
        secure_software_daml_key(&config(false, true), &db, &key, Some("kms-id"), None).await?;
        secure_software_daml_key(&config(true, false), &db, &key, None, None).await?;
        secure_software_daml_key(&config(false, false), &db, &key, None, None).await
    }
}
