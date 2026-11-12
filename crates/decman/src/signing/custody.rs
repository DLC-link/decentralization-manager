//! Where a new party Daml key may live.
//!
//! Canton 3.7 refuses to export a key from its software vault unless the node
//! sets `enable-key-download`, and Canton's configuration reference says that
//! setting "should not be enabled in production". A key in AWS KMS needs no
//! export, because DecMan signs with the KMS `Sign` API. DecMan therefore
//! puts a new party's keys into a vault that keeps its keys in KMS.
//!
//! Two settings allow a participant without KMS:
//!
//! - `DECPM_ALLOW_SOFTWARE_PARTY_KEYS` is for a participant that has not moved
//!   to KMS yet. DecMan then generates each new party Daml key itself and
//!   keeps it in its key store. Canton never holds the key, so it never has
//!   to export it.
//! - Insecure mode lets Canton create a software key, with a warning only. It
//!   runs on devnet only.
//!
//! A vault that only wraps its software keys with a KMS key (`wrapper_key_id`)
//! does not count. Those keys still need an export to sign.

use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            ListKeysFilters, ListMyKeysRequest, PrivateKeyMetadata,
            vault_service_client::VaultServiceClient,
        },
        v30::{SigningKeyUsage, SigningPublicKey},
    },
    protocol::v30::SignedTopologyTransaction,
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
        key_store::{KeyStore, STORE_OFF_HINT, sign_topology_transaction},
    },
    utils::compute_fingerprint,
    workflow::{
        onboarding::steps::{
            generate_keys::get_or_create_signing_key, proposals::create::decode_keys_payload,
        },
        storage::{WorkflowStorage, identity_kinds},
        topology::dedupe_signatures,
    },
};

/// What this node does with a new party key outside KMS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoftwareKeys {
    /// Insecure mode lets Canton create a software key, with a warning.
    Allowed,
    /// The operator allowed participants without KMS. DecMan generates each
    /// new Daml key itself and keeps it in its key store.
    Stored,
    /// DecMan refuses the key.
    Refused,
}

/// This node's rule for new party keys outside KMS. The opt-out wins over
/// insecure mode, because a key that DecMan generates never needs an export.
pub fn software_keys(config: &NodeConfig) -> SoftwareKeys {
    if config.allow_software_party_keys {
        SoftwareKeys::Stored
    } else if config.insecure {
        SoftwareKeys::Allowed
    } else {
        SoftwareKeys::Refused
    }
}

/// Refuse to create party keys on a participant that keeps no key in KMS.
///
/// Call this before any key is generated, so a refused run leaves nothing
/// behind in the vault or the topology. A node that allows software keys
/// must have its key store on, because DecMan generates the Daml key there.
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

/// A new party Daml key, and where it lives.
#[derive(Debug)]
pub struct NewDamlKey {
    pub public_key: SigningPublicKey,
    /// The KMS key id, when the key lives in KMS.
    pub kms_key_id: Option<String>,
    /// Whether an earlier run created the key under the same name.
    pub existed: bool,
}

/// Create this node's Daml key for a party, or find the key an earlier run
/// created under `name`.
///
/// On a participant with KMS, Canton creates the key in KMS. On a participant
/// without KMS, the opt-out has DecMan generate the key and keep it in its key
/// store, so Canton never holds it. Insecure mode lets Canton create a
/// software key instead. When `party` is known, DecMan files a key it
/// generated under the party at once. Onboarding learns the party later and
/// files the key with [`file_new_party_key`].
///
/// # Errors
///
/// Errors when the vault cannot be listed or refuses, when the key store is
/// off or fails, or when the key is outside KMS and this node's rule refuses
/// software keys.
pub async fn new_daml_key(
    config: &NodeConfig,
    db: &SqlitePool,
    vault_client: &mut VaultServiceClient<Channel>,
    name: &str,
    party: Option<&CantonId>,
) -> Result<NewDamlKey> {
    let has_kms = list_keys(vault_client, None).await?.iter().any(in_kms);
    if !has_kms && software_keys(config) == SoftwareKeys::Stored {
        let store = open_store(config, db).await?;
        let (public_key, existed) = store.generate(name).await?;
        let fingerprint = compute_fingerprint(&public_key);
        if let Some(party) = party {
            file_under_party(&store, party, &fingerprint).await?;
        }
        tracing::info!(
            "{verb} Daml key {fingerprint} ({name}) in DecMan's key store, because this \
             participant has no KMS",
            verb = if existed { "Reusing" } else { "Generated" },
        );
        return Ok(NewDamlKey {
            public_key,
            kms_key_id: None,
            existed,
        });
    }
    let (public_key, existed) =
        get_or_create_signing_key(vault_client, name, SigningKeyUsage::Protocol).await?;
    let fingerprint = compute_fingerprint(&public_key);
    let kms_key_id = require_kms_key(config, vault_client, &fingerprint).await?;
    if kms_key_id.is_none() && software_keys(config) == SoftwareKeys::Stored {
        // Only a key made before the participant moved to KMS gets here.
        anyhow::bail!(
            "The vault holds the software key {fingerprint} under the name {name}, from before \
             this participant moved to KMS. DecMan cannot sign with it without an export. \
             Delete that key from the participant's vault, then retry"
        );
    }
    Ok(NewDamlKey {
        public_key,
        kms_key_id,
        existed,
    })
}

/// File this node's Daml key for a party that onboarding is creating, when
/// DecMan generated the key.
///
/// Onboarding generates the key before the party id is known, and DecMan
/// signs each party's deploys with the key filed under that party. Each
/// member calls this once it knows the id, and before it signs the party's
/// `PartyToParticipant` mapping. A failure stops the member from signing, so
/// the party never comes into force with a key DecMan cannot sign with.
///
/// # Errors
///
/// Errors when this node has no record of its key for the party, or when the
/// key store fails.
pub async fn file_new_party_key(config: &NodeConfig, db: &SqlitePool, party: &CantonId) -> Result {
    if software_keys(config) != SoftwareKeys::Stored {
        return Ok(());
    }
    let Some(store) = KeyStore::open(config, db.clone()).await? else {
        return Ok(());
    };
    let self_id = config.participant_id().to_string();
    let payload = db
        .read_identity(party, identity_kinds::PEER_PUBLIC_KEYS, &self_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("This node has no record of its Daml key for {party}"))?;
    let keys = decode_keys_payload(&payload)?;
    let [_, daml_key] = keys.as_slice() else {
        anyhow::bail!(
            "This node's key record for {party} holds {count} keys, not 2",
            count = keys.len()
        );
    };
    let fingerprint = compute_fingerprint(daml_key);
    if store.generated_seed(&fingerprint).await?.is_some() {
        file_under_party(&store, party, &fingerprint).await?;
    }
    Ok(())
}

/// Add `key`'s signature to a topology transaction, when DecMan generated
/// the key. Returns whether it signed.
///
/// Canton requires each key that a `PartyToParticipant` mapping adds to sign
/// that mapping. Canton's vault adds the signature for a key it holds. It
/// cannot for a key DecMan generated, so DecMan adds the signature here. A
/// key DecMan did not generate is left to the vault.
///
/// # Errors
///
/// Errors when the key store fails, or when the signature does not verify.
pub async fn sign_with_generated_key(
    config: &NodeConfig,
    db: &SqlitePool,
    transaction: &mut SignedTopologyTransaction,
    key: &SigningPublicKey,
) -> Result<bool> {
    if software_keys(config) != SoftwareKeys::Stored {
        return Ok(false);
    }
    let Some(store) = KeyStore::open(config, db.clone()).await? else {
        return Ok(false);
    };
    let fingerprint = compute_fingerprint(key);
    let Some((public_key, seed)) = store.generated_seed(&fingerprint).await? else {
        return Ok(false);
    };
    let signature = sign_topology_transaction(&seed, &public_key, &transaction.transaction)?;
    transaction.signatures.push(signature);
    dedupe_signatures(transaction);
    tracing::info!("Signed the topology transaction with DecMan's key {fingerprint}");
    Ok(true)
}

/// Store a generated key under `party`, so the deploy signer finds it.
async fn file_under_party(store: &KeyStore, party: &CantonId, fingerprint: &Fingerprint) -> Result {
    let (public_key, seed) = store
        .generated_seed(fingerprint)
        .await?
        .ok_or_else(|| anyhow::anyhow!("DecMan did not generate the key {fingerprint}"))?;
    if store.put(party, fingerprint, &public_key, &seed).await? {
        tracing::info!("Filed DecMan's Daml key {fingerprint} under {party}");
    }
    Ok(())
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
            // The context shows the AWS cause, such as AccessDeniedException,
            // instead of only "service error".
            let cause = aws_sdk_kms::error::DisplayErrorContext(&e);
            anyhow::anyhow!(
                "DecMan cannot sign with the new KMS key {kms_key_id} ({fingerprint}): {cause}. \
                 Give DecMan's AWS role kms:Sign on that key, as docs/KMS_SIGNING.md explains. \
                 Then start the rotation again, which reuses the same key"
            )
        })
}

/// Log once at boot whether this node can create party keys, so an operator
/// learns it before a run fails. A participant that cannot be reached yet is
/// skipped, because every run checks again.
pub async fn log_rule_at_boot(config: &NodeConfig, db: &SqlitePool) {
    let mut wait = std::time::Duration::from_secs(10);
    let has_kms = loop {
        let metadata = match config.admin_channel().await {
            Ok(channel) => list_keys(&mut VaultServiceClient::new(channel), None).await,
            Err(e) => Err(e),
        };
        match metadata {
            Ok(metadata) => break metadata.iter().any(in_kms),
            Err(e) => {
                tracing::debug!(
                    "The participant is not ready for the party key rule check: {e:#}. \
                     Retrying in {}s",
                    wait.as_secs()
                );
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(std::time::Duration::from_secs(300));
            }
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
                 DECPM_ALLOW_SOFTWARE_PARTY_KEYS lets DecMan generate each new party Daml \
                 key itself and keep it in its key store"
            ),
            Err(e) => tracing::error!("DecMan cannot create party keys on this node: {e:#}"),
        },
        (false, SoftwareKeys::Refused) => tracing::warn!(
            "This participant keeps its keys in Canton's software vault, not in a KMS. DecMan \
             refuses to create or join a party here, and to rotate a key. Move the participant \
             to KMS, as docs/KMS_SIGNING.md explains, or set DECPM_ALLOW_SOFTWARE_PARTY_KEYS=true"
        ),
    }
}

async fn open_store(config: &NodeConfig, db: &SqlitePool) -> Result<KeyStore> {
    KeyStore::open(config, db.clone()).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "{STORE_OFF_HINT}. DECPM_ALLOW_SOFTWARE_PARTY_KEYS needs the key store, because \
             DecMan generates each new Daml key there"
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
            tracing::info!("{problem}. DECPM_ALLOW_SOFTWARE_PARTY_KEYS allows it");
            Ok(())
        }
        SoftwareKeys::Refused => anyhow::bail!(
            "{problem}. DecMan creates party keys only on a KMS-backed participant, because \
             Canton 3.7 refuses to export a software key. Moving a participant to KMS is a \
             Canton migration, not a config change; docs/KMS_SIGNING.md explains it. Until \
             then, set DECPM_ALLOW_SOFTWARE_PARTY_KEYS=true so DecMan generates the party's \
             Daml keys itself"
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
    fn the_opt_out_wins_over_insecure_mode() {
        assert_eq!(software_keys(&config(false, false)), SoftwareKeys::Refused);
        assert_eq!(software_keys(&config(false, true)), SoftwareKeys::Stored);
        assert_eq!(software_keys(&config(true, false)), SoftwareKeys::Allowed);
        assert_eq!(software_keys(&config(true, true)), SoftwareKeys::Stored);
    }

    #[test]
    fn a_software_key_is_refused_without_a_setting_that_allows_it() {
        let error = check(false, SoftwareKeys::Refused, "Software vault")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("Software vault"), "{error}");
        assert!(error.contains("Canton migration"), "{error}");
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
}

/// The custody rules against a fake Canton admin API, so the vault calls,
/// the export and the key store all run.
#[cfg(test)]
mod against_a_vault {
    use super::*;
    use crate::{
        db::MIGRATOR,
        signing::{
            fake_admin_api::{FakeAdminApi, VaultKey},
            key_store::{ed25519_signing_public_key, topology_transaction_hash},
            vault_export::ed25519_public_key,
        },
        workflow::onboarding::steps::generate_keys::encode_keys_payload,
    };

    const PARTY: &str =
        "party::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn party() -> Result<CantonId> {
        CantonId::parse(PARTY)
    }

    /// The error text of a call that should have been refused.
    fn refusal<T: std::fmt::Debug>(result: Result<T>) -> Result<String> {
        match result {
            Ok(value) => anyhow::bail!("expected a refusal, got {value:?}"),
            Err(e) => Ok(format!("{e:#}")),
        }
    }

    async fn vault(config: &NodeConfig) -> Result<VaultServiceClient<Channel>> {
        Ok(VaultServiceClient::new(config.admin_channel().await?))
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_vault_without_a_kms_key_is_refused_under_the_strict_rule(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![
            VaultKey::software("p-namespace", 1),
            VaultKey::wrapped("p-daml-transactions", 2),
            VaultKey {
                kms_key_id: Some(String::new()),
                ..VaultKey::software("q-daml-transactions", 3)
            },
        ])
        .await?;
        let node = fake.node(SoftwareKeys::Refused, true).await?;

        let error = refusal(
            require_kms_participant(&node.config, &db, &mut vault(&node.config).await?).await,
        )?;

        assert!(error.contains("software vault, not in a KMS"), "{error}");
        assert!(
            error.contains("DECPM_ALLOW_SOFTWARE_PARTY_KEYS=true"),
            "{error}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn one_kms_key_satisfies_the_strict_rule(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![
            VaultKey::software("old-daml-transactions", 1),
            VaultKey::kms("participant-signing", 2)?,
        ])
        .await?;
        let node = fake.node(SoftwareKeys::Refused, false).await?;

        require_kms_participant(&node.config, &db, &mut vault(&node.config).await?).await
    }

    /// The opt-out keeps a copy of each new Daml key, so a node without a key
    /// store is refused before any key exists. Insecure mode keeps no copy.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_opt_out_needs_the_key_store_before_any_key_is_made(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("p-namespace", 1)]).await?;

        let without_store = fake.node(SoftwareKeys::Stored, false).await?;
        let error = refusal(
            require_kms_participant(
                &without_store.config,
                &db,
                &mut vault(&without_store.config).await?,
            )
            .await,
        )?;
        assert!(error.contains(STORE_OFF_HINT), "{error}");

        let with_store = fake.node(SoftwareKeys::Stored, true).await?;
        require_kms_participant(
            &with_store.config,
            &db,
            &mut vault(&with_store.config).await?,
        )
        .await?;
        let insecure = fake.node(SoftwareKeys::Allowed, false).await?;
        require_kms_participant(&insecure.config, &db, &mut vault(&insecure.config).await?).await
    }

    /// A key found by name can predate the move to KMS, so each returned key
    /// is checked on its own.
    #[tokio::test]
    async fn each_party_key_outside_kms_is_refused_under_the_strict_rule() -> Result {
        let software = VaultKey::software("p-daml-transactions", 1);
        let wrapped = VaultKey::wrapped("p-namespace", 2);
        let kms = VaultKey::kms("q-daml-transactions", 3)?;
        let fake =
            FakeAdminApi::start(vec![software.clone(), wrapped.clone(), kms.clone()]).await?;
        let strict = fake.node(SoftwareKeys::Refused, false).await?;
        let mut client = vault(&strict.config).await?;

        for key in [&software, &wrapped] {
            let fingerprint = key.fingerprint();
            let error = refusal(require_kms_key(&strict.config, &mut client, &fingerprint).await)?;
            assert!(
                error.contains(&format!("{fingerprint} is in Canton's software vault")),
                "{error}"
            );
        }
        assert_eq!(
            require_kms_key(&strict.config, &mut client, &kms.fingerprint()).await?,
            kms.kms_key_id
        );

        let opt_out = fake.node(SoftwareKeys::Stored, true).await?;
        let mut client = vault(&opt_out.config).await?;
        assert_eq!(
            require_kms_key(&opt_out.config, &mut client, &software.fingerprint()).await?,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_key_the_vault_does_not_hold_is_refused() -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::kms("p-daml-transactions", 1)?]).await?;
        let node = fake.node(SoftwareKeys::Allowed, false).await?;
        let missing = VaultKey::software("elsewhere", 9).fingerprint();

        let error = refusal(
            require_kms_key(&node.config, &mut vault(&node.config).await?, &missing).await,
        )?;

        assert!(error.contains("does not hold the key"), "{error}");
        Ok(())
    }

    async fn store(config: &NodeConfig, db: &SqlitePool) -> Result<KeyStore> {
        open_store(config, db).await
    }

    async fn party_rows(db: &SqlitePool) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM party_key_store")
            .fetch_one(db)
            .await?)
    }

    async fn generated_rows(db: &SqlitePool) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM generated_party_keys")
                .fetch_one(db)
                .await?,
        )
    }

    /// Record `key` as this node's Daml key for the party, as onboarding does
    /// once the party id is known.
    async fn record_own_key(
        config: &NodeConfig,
        db: &SqlitePool,
        key: &SigningPublicKey,
    ) -> Result {
        let namespace = VaultKey::software("namespace", 1);
        db.write_identity(
            &party()?,
            identity_kinds::PEER_PUBLIC_KEYS,
            &config.participant_id().to_string(),
            &encode_keys_payload(&namespace.public_key, key),
        )
        .await
    }

    /// Under the opt-out, DecMan generates the key. The vault makes nothing,
    /// exports nothing, and a second run finds the same key by name.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_opt_out_generates_the_daml_key_in_decman(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("p-namespace", 1)]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        let mut client = vault(&node.config).await?;

        let first = new_daml_key(
            &node.config,
            &db,
            &mut client,
            "p-daml-transactions",
            Some(&party()?),
        )
        .await?;
        let second = new_daml_key(
            &node.config,
            &db,
            &mut client,
            "p-daml-transactions",
            Some(&party()?),
        )
        .await?;

        assert!(!first.existed && second.existed);
        assert_eq!(first.public_key, second.public_key);
        assert_eq!(first.kms_key_id, None);
        assert!(fake.generated().is_empty());
        assert!(fake.exported().is_empty());
        let fingerprint = compute_fingerprint(&first.public_key);
        let store = store(&node.config, &db).await?;
        let (public_key, seed) = store
            .generated_seed(&fingerprint)
            .await?
            .ok_or_else(|| anyhow::anyhow!("DecMan holds no seed for {fingerprint}"))?;
        assert_eq!(public_key, first.public_key);
        assert_eq!(ed25519_signing_public_key(&seed), first.public_key);
        assert_eq!(
            store.get(&party()?, &fingerprint).await?.as_deref(),
            Some(&*seed),
            "the key is filed under the party, so deploys can sign with it"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kms_participant_keeps_the_daml_key_in_kms_under_the_opt_out(
        db: SqlitePool,
    ) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::kms("participant-signing", 1)?]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;

        let key = new_daml_key(
            &node.config,
            &db,
            &mut vault(&node.config).await?,
            "p-daml-transactions",
            Some(&party()?),
        )
        .await?;

        assert_eq!(fake.generated(), vec!["p-daml-transactions".to_string()]);
        assert!(key.kms_key_id.is_some());
        assert_eq!(generated_rows(&db).await?, 0);
        assert_eq!(party_rows(&db).await?, 0);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn insecure_mode_lets_the_vault_make_a_software_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("p-namespace", 1)]).await?;
        let node = fake.node(SoftwareKeys::Allowed, true).await?;

        let key = new_daml_key(
            &node.config,
            &db,
            &mut vault(&node.config).await?,
            "p-daml-transactions",
            None,
        )
        .await?;

        assert_eq!(fake.generated(), vec!["p-daml-transactions".to_string()]);
        assert_eq!(key.kms_key_id, None);
        assert_eq!(generated_rows(&db).await?, 0);
        Ok(())
    }

    /// A software key made before the participant moved to KMS needs an
    /// export to sign, so the opt-out refuses it instead of using it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_opt_out_refuses_a_pre_kms_software_key(db: SqlitePool) -> Result {
        let old = VaultKey::software("p-daml-transactions", 2);
        let fake = FakeAdminApi::start(vec![VaultKey::kms("participant-signing", 1)?, old.clone()])
            .await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;

        let error = refusal(
            new_daml_key(
                &node.config,
                &db,
                &mut vault(&node.config).await?,
                "p-daml-transactions",
                None,
            )
            .await,
        )?;

        assert!(
            error.contains(&format!("software key {}", old.fingerprint())),
            "{error}"
        );
        assert!(error.contains("moved to KMS"), "{error}");
        assert!(fake.exported().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_opt_out_without_a_key_store_generates_nothing(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("p-namespace", 1)]).await?;
        let node = fake.node(SoftwareKeys::Stored, false).await?;

        let error = refusal(
            new_daml_key(
                &node.config,
                &db,
                &mut vault(&node.config).await?,
                "p-daml-transactions",
                None,
            )
            .await,
        )?;

        assert!(error.contains(STORE_OFF_HINT), "{error}");
        assert!(fake.generated().is_empty());
        assert_eq!(generated_rows(&db).await?, 0);
        Ok(())
    }

    /// Onboarding generates the key before the party exists, and files it
    /// once the member knows the party id.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn onboarding_files_the_generated_key_once_the_party_id_is_known(
        db: SqlitePool,
    ) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("p-namespace", 1)]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        let key = new_daml_key(
            &node.config,
            &db,
            &mut vault(&node.config).await?,
            "p-daml-transactions",
            None,
        )
        .await?
        .public_key;
        assert_eq!(party_rows(&db).await?, 0);
        record_own_key(&node.config, &db, &key).await?;

        file_new_party_key(&node.config, &db, &party()?).await?;

        let fingerprint = compute_fingerprint(&key);
        assert!(
            store(&node.config, &db)
                .await?
                .get(&party()?, &fingerprint)
                .await?
                .is_some()
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn filing_leaves_a_vault_key_in_the_vault(db: SqlitePool) -> Result {
        let key = VaultKey::kms("p-daml-transactions", 7)?;
        let fake = FakeAdminApi::start(vec![key.clone()]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        node.record_daml_key(&db, &party()?, &key).await?;

        file_new_party_key(&node.config, &db, &party()?).await?;

        assert_eq!(party_rows(&db).await?, 0);
        assert!(fake.exported().is_empty());
        Ok(())
    }

    /// A member without a record of its key cannot file it, so it must not
    /// sign the party mapping.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn filing_without_a_key_record_stops_the_member(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;

        let error = refusal(file_new_party_key(&node.config, &db, &party()?).await)?;

        assert!(error.contains("no record of its Daml key"), "{error}");
        Ok(())
    }

    /// A key DecMan generated signs the mapping that adds it, because the
    /// vault cannot. Canton verifies the signature against the transaction's
    /// hash.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_generated_key_signs_the_topology_transaction(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        let key = new_daml_key(
            &node.config,
            &db,
            &mut vault(&node.config).await?,
            "p-daml-transactions",
            None,
        )
        .await?
        .public_key;
        let mut transaction = SignedTopologyTransaction {
            transaction: b"a serialized topology transaction".to_vec(),
            ..Default::default()
        };

        assert!(sign_with_generated_key(&node.config, &db, &mut transaction, &key).await?);
        assert!(sign_with_generated_key(&node.config, &db, &mut transaction, &key).await?);

        let [signature] = transaction.signatures.as_slice() else {
            anyhow::bail!("expected one signature, got {:?}", transaction.signatures);
        };
        assert_eq!(signature.signed_by, compute_fingerprint(&key).to_string());
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&ed25519_public_key(&key)?)?;
        let bytes: [u8; 64] = signature.signature.as_slice().try_into()?;
        verifying.verify_strict(
            &topology_transaction_hash(&transaction.transaction),
            &ed25519_dalek::Signature::from_bytes(&bytes),
        )?;
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_vault_key_is_left_to_the_vault_to_sign(db: SqlitePool) -> Result {
        let vault_key = VaultKey::software("p-daml-transactions", 7);
        let fake = FakeAdminApi::start(vec![vault_key.clone()]).await?;
        let mut transaction = SignedTopologyTransaction {
            transaction: b"a serialized topology transaction".to_vec(),
            ..Default::default()
        };

        for rule in [
            SoftwareKeys::Stored,
            SoftwareKeys::Allowed,
            SoftwareKeys::Refused,
        ] {
            let node = fake.node(rule, true).await?;
            assert!(
                !sign_with_generated_key(
                    &node.config,
                    &db,
                    &mut transaction,
                    &vault_key.public_key
                )
                .await?
            );
        }
        assert!(transaction.signatures.is_empty());
        Ok(())
    }
}
