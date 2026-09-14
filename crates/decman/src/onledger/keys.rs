//! Key material shared by every kind driver (design D4).
//!
//! A new decentralized party gets one vault key per member, named
//! `{prefix}-key`, with usages `[Namespace, Protocol]`. Its self-signed root
//! `NamespaceDelegation` carries the public key into the synchronizer store,
//! where the proposer reads it back as the single source of key bytes. The
//! same key is the member's Daml signing key, so the DND owner fingerprint
//! and the party signing-key fingerprint are equal and a kick can attribute
//! keys on-chain.
//!
//! Legacy parties hold two keys (`{prefix}-namespace` and
//! `{prefix}-daml-transactions`). Every lookup here checks the chain first,
//! then the local caches, then the legacy vault names, so both models work.
//!
//! The vault helpers were copied from
//! `workflow/onboarding/steps/generate_keys.rs`, which the Noise removal
//! deletes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            GenerateSigningKeyRequest, ListKeysFilters, ListMyKeysRequest,
            generate_signing_key_response, private_key_metadata,
            vault_service_client::VaultServiceClient,
        },
        v30::{SigningKeySpec, SigningKeyUsage, SigningPublicKey, public_key},
    },
    protocol::v30::{
        NamespaceDelegation, TopologyMapping, enums::TopologyChangeOp, namespace_delegation,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, BaseQuery, ListNamespaceDelegationRequest, StoreId, authorize_request,
        base_query, store_id,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use common::canton_id::CantonId;
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    db::{
        rows::DecPartyParticipantRow,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    utils,
    workflow::{signing_keys, topology::authorize_with_topology_retry},
};

use super::{
    engine::ProposerKeyMaterial,
    topology::{self, WaitBudget},
    validation::{KickedMember, LocalIdentity},
};

/// The legacy Daml signing-key name, `{prefix}-daml-transactions`.
pub use crate::workflow::signing_keys::party_daml_key_name as legacy_daml_key_name;

type Channel = tonic::transport::Channel;

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// The vault name of the dual-usage key a member holds for a new party
/// (design D4). Generation is idempotent by this name.
pub fn party_key_name(prefix: &str) -> String {
    format!("{prefix}-key")
}

/// The legacy namespace-key name, `{prefix}-namespace`. Same derivation as
/// `OnboardingConfig::namespace_key_name` and `AddPartyConfig::namespace_key_name`.
pub fn legacy_namespace_key_name(prefix: &str) -> String {
    format!("{prefix}-namespace")
}

// ---------------------------------------------------------------------------
// The dual key
// ---------------------------------------------------------------------------

/// One member's dual-usage key for one party.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartyKey {
    pub key: SigningPublicKey,
    /// `compute_fingerprint(key)`: the DND owner and the Daml key fingerprint.
    pub fingerprint: String,
    /// Lowercase hex of the prost-encoded `SigningPublicKey`, as the
    /// `signingPublicKeyHex` fields of the proposal and acceptance carry it.
    pub key_hex: String,
}

impl PartyKey {
    /// Derive the fingerprint and the hex form from the key bytes.
    pub fn from_key(key: SigningPublicKey) -> Self {
        let fingerprint = utils::compute_fingerprint(&key);
        let key_hex = hex::encode(key.encode_to_vec());
        Self {
            key,
            fingerprint,
            key_hex,
        }
    }
}

/// Decode a `signingPublicKeyHex` value back into the key it encodes.
///
/// # Errors
/// Returns an error when the text is not hex or not a `SigningPublicKey`.
pub fn decode_key_hex(key_hex: &str) -> Result<SigningPublicKey> {
    let bytes = hex::decode(key_hex).context("signingPublicKeyHex is not hex")?;
    SigningPublicKey::decode(bytes.as_slice())
        .context("signingPublicKeyHex is not a SigningPublicKey")
}

/// The proposal fields a coordinator fills from its own key (design D6).
/// The Daml key fingerprint equals the namespace fingerprint because the
/// dual key is also the Daml key.
pub fn proposer_key_material(key: &PartyKey) -> ProposerKeyMaterial {
    ProposerKeyMaterial {
        namespace_fingerprint: Some(key.fingerprint.clone()),
        signing_public_key_hex: Some(key.key_hex.clone()),
        daml_key_fingerprint: Some(key.fingerprint.clone()),
    }
}

/// Make sure this node holds the `{prefix}-key` and that its root
/// `NamespaceDelegation` is on its way to the synchronizer store.
///
/// The key is reused by name when it exists, because a fresh key would mint
/// a new fingerprint and strand the delegation of the old one. The
/// delegation is published to the Authorized store with
/// `must_fully_authorize = true`, exactly as legacy onboarding did; a
/// delegation already in the synchronizer store, or already authorized and
/// waiting for dispatch, is not proposed again (Canton rejects a duplicate
/// mapping). A run interrupted between key generation and the publish is
/// therefore healed on retry (#261).
///
/// # Errors
/// Returns an error when the vault or the topology manager fails, or when a
/// key of that name exists without both usages.
pub async fn ensure_party_key(config: &NodeConfig, prefix: &str) -> Result<PartyKey> {
    let name = party_key_name(prefix);
    let mut vault = VaultServiceClient::new(config.admin_channel().await?);
    let (key, was_existing) = get_or_create_dual_key(&mut vault, &name).await?;
    let party_key = PartyKey::from_key(key);
    let fingerprint = party_key.fingerprint.as_str();

    // A freshly minted key cannot have a delegation yet, so the check is
    // skipped for it.
    let published = was_existing && root_delegation_published(config, fingerprint).await?;
    if published {
        tracing::info!(
            key = %name,
            fingerprint,
            "reusing the party key; its root NamespaceDelegation is already published"
        );
    } else {
        propose_root_delegation(config, &party_key).await?;
    }
    Ok(party_key)
}

/// Wait until this node's root delegation is effective in the synchronizer
/// store, so a proposer can read the key bytes from it (design D4).
///
/// # Errors
/// Returns an error when the budget runs out.
pub async fn wait_own_root_delegation(
    config: &NodeConfig,
    sync_id: &str,
    fingerprint: &str,
    budget: WaitBudget,
) -> Result<()> {
    topology::wait_owner_root_delegations(config, sync_id, &[fingerprint.to_string()], budget)
        .await
        .map(|_| ())
}

/// Look up a signing key by exact vault name, or generate it with usages
/// `[Namespace, Protocol]`. Returns the key and whether it existed before.
///
/// Copied from `get_or_create_signing_key` in
/// `workflow/onboarding/steps/generate_keys.rs`, with the dual usage set.
async fn get_or_create_dual_key(
    vault: &mut VaultServiceClient<Channel>,
    name: &str,
) -> Result<(SigningPublicKey, bool)> {
    let usages = [
        SigningKeyUsage::Namespace as i32,
        SigningKeyUsage::Protocol as i32,
    ];

    let existing = vault
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: String::new(),
                name: name.to_string(),
                purpose: vec![],
                usage_v30: vec![],
            }),
            base_request: None,
        }))
        .await
        .context("ListMyKeys")?
        .into_inner();

    // The name filter is a match on the text, not an exact match, so the
    // name is compared again here.
    let existing = existing
        .private_keys_metadata
        .into_iter()
        .filter_map(VaultKey::from_metadata)
        .find(|k| k.name == name);
    if let Some(vault_key) = existing {
        if !usages.iter().all(|u| vault_key.key.usage.contains(u)) {
            bail!(
                "vault key '{name}' exists with usages {:?}, not [Namespace, Protocol]; \
                 rename or remove it before this party is created",
                vault_key.key.usage
            );
        }
        tracing::info!(key = %name, "reusing the existing party key");
        return Ok((vault_key.key, true));
    }

    tracing::debug!(key = %name, "generating the party key");
    // The key spec stays UNSPECIFIED so the node picks its own default:
    // Ed25519 on the JCE provider, P-256 on a KMS node. A hardcoded spec
    // broke party creation on KMS nodes (#264).
    let response = vault
        .generate_signing_key(tonic::Request::new(GenerateSigningKeyRequest {
            key_spec: SigningKeySpec::Unspecified as i32,
            name: name.to_string(),
            usage_v30: usages.to_vec(),
            base_request: None,
        }))
        .await
        .context("GenerateSigningKey")?
        .into_inner();
    let Some(generate_signing_key_response::PublicKey::V30(key)) = response.public_key else {
        bail!("GenerateSigningKey returned no public key for '{name}'");
    };
    Ok((key, false))
}

/// Is the root delegation of `fingerprint` in the synchronizer store, or at
/// least authorized locally and waiting for dispatch?
///
/// The synchronizer store is what a proposer reads, so it is checked first.
/// The Authorized store is checked second because a delegation that is
/// authorized but not yet dispatched must not be proposed again.
async fn root_delegation_published(config: &NodeConfig, fingerprint: &str) -> Result<bool> {
    let sync_id = utils::get_synchronizer_id(config).await?;
    if topology::read_root_delegation(config, &sync_id, fingerprint)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    root_delegation_authorized(config, fingerprint).await
}

/// Is the root delegation of `fingerprint` in this participant's Authorized
/// store? Copied from `namespace_delegation_exists` in
/// `workflow/onboarding/steps/generate_keys.rs`.
async fn root_delegation_authorized(config: &NodeConfig, fingerprint: &str) -> Result<bool> {
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
    let response = client
        .list_namespace_delegation(tonic::Request::new(ListNamespaceDelegationRequest {
            base_query: Some(BaseQuery {
                store: Some(authorized_store()),
                proposals: false,
                operation: 0,
                time_query: Some(base_query::TimeQuery::HeadState(())),
                filter_signed_key: String::new(),
                protocol_version: None,
                client_version: None,
            }),
            filter_namespace: fingerprint.to_string(),
            filter_target_key_fingerprint: fingerprint.to_string(),
        }))
        .await
        .context("ListNamespaceDelegation (Authorized store)")?
        .into_inner();
    Ok(!response.results.is_empty())
}

/// Publish the self-signed root delegation of `key` to the Authorized store.
/// Copied from `propose_namespace_delegation` in
/// `workflow/onboarding/steps/generate_keys.rs`; the request is unchanged.
async fn propose_root_delegation(config: &NodeConfig, key: &PartyKey) -> Result<()> {
    tracing::debug!(fingerprint = %key.fingerprint, "proposing the root NamespaceDelegation");
    let delegation = NamespaceDelegation {
        namespace: key.fingerprint.clone(),
        // target == namespace makes this the root of the namespace.
        target_key: Some(key.key.clone()),
        #[allow(deprecated)]
        is_root_delegation: false,
        // With `is_root_delegation = false`, only this restriction lets the
        // key sign every mapping type, delegations included.
        restriction: Some(namespace_delegation::Restriction::CanSignAllMappings(
            namespace_delegation::CanSignAllMappings {},
        )),
    };
    let request = AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::NamespaceDelegation(delegation)),
                })),
            },
        )),
        must_fully_authorize: true,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(authorized_store()),
        wait_to_become_effective: None,
    };
    authorize_with_topology_retry(config, request, "root NamespaceDelegation").await?;
    tracing::info!(fingerprint = %key.fingerprint, "root NamespaceDelegation proposed");
    Ok(())
}

fn authorized_store() -> StoreId {
    StoreId {
        store: Some(store_id::Store::Authorized(store_id::Authorized {})),
    }
}

// ---------------------------------------------------------------------------
// The local vault
// ---------------------------------------------------------------------------

/// One signing key of this node's vault, with the name the vault knows it by.
#[derive(Clone, Debug, PartialEq)]
pub struct VaultKey {
    pub name: String,
    pub key: SigningPublicKey,
    pub fingerprint: String,
}

impl VaultKey {
    fn from_metadata(
        meta: canton_proto_rs::com::digitalasset::canton::crypto::admin::v30::PrivateKeyMetadata,
    ) -> Option<Self> {
        let private_key_metadata::PublicKeyWithName::V30(pkn) = meta.public_key_with_name?;
        let public_key::Key::SigningPublicKey(key) = pkn.public_key?.key? else {
            return None;
        };
        let fingerprint = utils::compute_fingerprint(&key);
        Some(Self {
            name: pkn.name,
            key,
            fingerprint,
        })
    }

    pub fn has_usage(&self, usage: SigningKeyUsage) -> bool {
        self.key.usage.contains(&(usage as i32))
    }
}

/// Every signing key in this node's vault (`ListMyKeys`, no filter).
///
/// # Errors
/// Returns an error when the vault call fails.
pub async fn list_vault_keys(config: &NodeConfig) -> Result<Vec<VaultKey>> {
    let mut vault = VaultServiceClient::new(config.admin_channel().await?);
    let response = vault
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: None,
            base_request: None,
        }))
        .await
        .context("ListMyKeys")?
        .into_inner();
    Ok(response
        .private_keys_metadata
        .into_iter()
        .filter_map(VaultKey::from_metadata)
        .collect())
}

// ---------------------------------------------------------------------------
// Local identity (design section 5 inputs)
// ---------------------------------------------------------------------------

/// This node as the validation rules must see it, for one party or for a
/// party that does not exist yet.
///
/// `owner_fingerprints`: with a party id, the Namespace-usage vault keys
/// that own the head DND, plus the `{prefix}-key` when the vault holds it
/// (an add-party joiner owns nothing yet). Without a party id, every
/// Namespace-usage vault key, since onboarding has no DND to compare with.
///
/// `daml_key_fingerprint`: chain first (`party_signing_keys` of the head
/// P2P intersected with the vault), then the `dec_party_identity` bundle,
/// then the legacy vault name; without a party, the `{prefix}-key` and then
/// the legacy `{prefix}-daml-transactions` key.
///
/// # Errors
/// Returns an error when the vault or a topology read fails.
pub async fn local_identity_for_party(
    config: &NodeConfig,
    db: &SqlitePool,
    dec_party_id: Option<&CantonId>,
    prefix: Option<&str>,
) -> Result<LocalIdentity> {
    let participant_id = config.participant_id().clone();
    let vault = list_vault_keys(config).await?;
    let prefix = prefix.or(dec_party_id.map(|id| id.prefix.as_str()));

    let Some(party) = dec_party_id else {
        return Ok(LocalIdentity {
            participant_id,
            owner_fingerprints: owner_fingerprints_from(&vault, None, prefix),
            daml_key_fingerprint: prefix.and_then(|p| vault_daml_key_for_prefix(&vault, p)),
        });
    };

    let sync_id = utils::get_synchronizer_id(config).await?;
    let head_owners: BTreeSet<String> =
        topology::read_accepted_dnd(config, &sync_id, &party.namespace.to_string())
            .await?
            .map(|dnd| dnd.mapping.owners.into_iter().collect())
            .unwrap_or_default();
    let owner_fingerprints = owner_fingerprints_from(&vault, Some(&head_owners), prefix);

    let head_keys: BTreeSet<String> = topology::read_accepted_p2p(config, &sync_id, party)
        .await?
        .map(|p2p| super::validation::key_fingerprints(&p2p.mapping))
        .unwrap_or_default();
    let daml_key_fingerprint = match on_chain_daml_key(&vault, &head_keys) {
        Some(fp) => Some(fp),
        None => signing_keys::own_signing_key_fingerprint(config, db, party).await?,
    };

    Ok(LocalIdentity {
        participant_id,
        owner_fingerprints,
        daml_key_fingerprint,
    })
}

/// The Namespace-usage vault fingerprints that count as "mine" for a party.
/// See [`local_identity_for_party`] for the two branches.
fn owner_fingerprints_from(
    vault: &[VaultKey],
    head_owners: Option<&BTreeSet<String>>,
    prefix: Option<&str>,
) -> BTreeSet<String> {
    let namespace_keys = vault
        .iter()
        .filter(|k| k.has_usage(SigningKeyUsage::Namespace));
    match head_owners {
        None => namespace_keys.map(|k| k.fingerprint.clone()).collect(),
        Some(owners) => {
            let dual_name = prefix.map(party_key_name);
            namespace_keys
                .filter(|k| {
                    owners.contains(&k.fingerprint) || dual_name.as_deref() == Some(&k.name)
                })
                .map(|k| k.fingerprint.clone())
                .collect()
        }
    }
}

/// The one Protocol-usage vault key that is also a party signing key on the
/// head P2P. `None` when there is no match, or when two keys match: two
/// matches cannot be attributed and the caches decide.
fn on_chain_daml_key(vault: &[VaultKey], head_keys: &BTreeSet<String>) -> Option<String> {
    let mut matches = vault
        .iter()
        .filter(|k| k.has_usage(SigningKeyUsage::Protocol) && head_keys.contains(&k.fingerprint))
        .map(|k| k.fingerprint.clone());
    let first = matches.next()?;
    if let Some(second) = matches.next() {
        tracing::warn!(
            first,
            second,
            "two vault keys are party signing keys of the same party; falling back to the caches"
        );
        return None;
    }
    Some(first)
}

/// The Daml key this node would contribute to a party with `prefix` that
/// has no P2P yet: the dual key, or else the legacy Daml key.
fn vault_daml_key_for_prefix(vault: &[VaultKey], prefix: &str) -> Option<String> {
    let dual = party_key_name(prefix);
    let legacy = legacy_daml_key_name(prefix);
    [dual, legacy].iter().find_map(|name| {
        vault
            .iter()
            .find(|k| k.name == *name && k.has_usage(SigningKeyUsage::Protocol))
            .map(|k| k.fingerprint.clone())
    })
}

// ---------------------------------------------------------------------------
// Member key caches (`dec_party_participant`)
// ---------------------------------------------------------------------------

/// The member a kick removes, from this node's own cache and never from the
/// proposer (design section 5).
///
/// `Ok(None)`: no cached owner key for that participant, so the kick DND
/// cannot be validated. The cached fingerprint is cross-checked against the
/// head DND, because a fingerprint that is not an owner would fail every
/// later check with a less useful message.
///
/// # Errors
/// Returns an error when the cache and the head DND disagree, or when a read
/// fails.
pub async fn kicked_member(
    config: &NodeConfig,
    db: &SqlitePool,
    dec_party_id: &CantonId,
    kicked_participant: &CantonId,
) -> Result<Option<KickedMember>> {
    let rows = db.get_dec_party_participants(dec_party_id).await?;
    let Some(member) = kicked_member_from_rows(&rows, &kicked_participant.to_string()) else {
        return Ok(None);
    };

    let sync_id = utils::get_synchronizer_id(config).await?;
    let head = topology::read_accepted_dnd(config, &sync_id, &dec_party_id.namespace.to_string())
        .await?
        .with_context(|| {
            format!("{dec_party_id} has no accepted DecentralizedNamespaceDefinition")
        })?;
    if !head.mapping.owners.contains(&member.owner_fingerprint) {
        bail!(
            "cached owner key {} of {kicked_participant} is not an owner of {dec_party_id} at \
             serial {}: the cache is stale, or the kick already landed",
            member.owner_fingerprint,
            head.serial
        );
    }
    Ok(Some(member))
}

/// The pure half of [`kicked_member`]: the row of `kicked_uid` with a cached
/// owner key, as a [`KickedMember`].
pub fn kicked_member_from_rows(
    rows: &[DecPartyParticipantRow],
    kicked_uid: &str,
) -> Option<KickedMember> {
    rows.iter()
        .find(|r| r.participant_uid == kicked_uid)
        .and_then(|r| {
            r.owner_key.as_ref().map(|owner| KickedMember {
                participant_id: r.participant_uid.clone(),
                owner_fingerprint: owner.clone(),
                signing_key_fingerprint: r.signing_key.clone(),
            })
        })
}

/// Which Daml signing key each cached member claims: participant uid to
/// fingerprint, from `dec_party_participant.signing_key`. The kicked member's
/// own claim is included when cached; the kick check skips it by uid.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn survivor_key_claims(
    db: &SqlitePool,
    dec_party_id: &CantonId,
) -> Result<BTreeMap<String, String>> {
    Ok(key_claims_from_rows(
        &db.get_dec_party_participants(dec_party_id).await?,
    ))
}

/// The pure half of [`survivor_key_claims`].
pub fn key_claims_from_rows(rows: &[DecPartyParticipantRow]) -> BTreeMap<String, String> {
    rows.iter()
        .filter_map(|r| {
            r.signing_key
                .as_ref()
                .map(|fp| (r.participant_uid.clone(), fp.clone()))
        })
        .collect()
}

/// Record what a member said about its own keys in an acceptance, so the
/// caches a later kick reads are filled by the member itself (design M6).
/// `None` leaves the column alone. A participant without a cached row is
/// skipped with a warning: the row appears with the next parties refresh,
/// and the caller may record again then.
///
/// # Errors
/// Returns an error when the write fails.
pub async fn record_member_keys(
    db: &SqlitePool,
    dec_party_id: &CantonId,
    participant: &CantonId,
    owner_fp: Option<&str>,
    signing_fp: Option<&str>,
) -> Result<()> {
    if owner_fp.is_none() && signing_fp.is_none() {
        return Ok(());
    }
    let uid = participant.to_string();
    let known = db
        .get_dec_party_participants(dec_party_id)
        .await?
        .iter()
        .any(|r| r.participant_uid == uid);
    if !known {
        tracing::warn!(
            party = %dec_party_id,
            participant = %uid,
            "no cached participant row; member keys not recorded yet"
        );
        return Ok(());
    }

    let mut tx = db.begin_transaction().await?;
    if let Some(owner) = owner_fp {
        tx.update_participant_owner_key(dec_party_id, &uid, owner)
            .await?;
    }
    if let Some(signing) = signing_fp {
        tx.update_participant_signing_key(dec_party_id, &uid, signing)
            .await?;
    }
    Commitable::commit(tx).await?;
    tracing::debug!(
        party = %dec_party_id,
        participant = %uid,
        owner = owner_fp.unwrap_or("-"),
        signing = signing_fp.unwrap_or("-"),
        "member keys recorded"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{MIGRATOR, rows::DecPartyRow},
        onledger::topology::tests::key,
    };

    const NS: &str = "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn vault_key(name: &str, seed: u8, usages: &[SigningKeyUsage]) -> VaultKey {
        let mut key = key(seed);
        key.usage = usages.iter().map(|u| *u as i32).collect();
        let fingerprint = utils::compute_fingerprint(&key);
        VaultKey {
            name: name.to_string(),
            key,
            fingerprint,
        }
    }

    fn fp(seed: u8) -> String {
        utils::compute_fingerprint(&key(seed))
    }

    fn row(uid: &str, owner: Option<&str>, signing: Option<&str>) -> DecPartyParticipantRow {
        DecPartyParticipantRow {
            dec_party_id: format!("net::{NS}"),
            participant_uid: uid.to_string(),
            permission: "confirmation".to_string(),
            owner_key: owner.map(str::to_string),
            signing_key: signing.map(str::to_string),
        }
    }

    #[test]
    fn names_follow_the_prefix() {
        assert_eq!(party_key_name("acme"), "acme-key");
        assert_eq!(legacy_namespace_key_name("acme"), "acme-namespace");
        assert_eq!(legacy_daml_key_name("acme"), "acme-daml-transactions");
    }

    #[test]
    fn key_hex_round_trips_through_prost() -> Result<()> {
        let original = key(7);
        let party_key = PartyKey::from_key(original.clone());

        assert_eq!(party_key.fingerprint, utils::compute_fingerprint(&original));
        assert_eq!(party_key.key_hex, party_key.key_hex.to_lowercase());
        assert_eq!(decode_key_hex(&party_key.key_hex)?, original);
        Ok(())
    }

    #[test]
    fn key_hex_rejects_garbage() {
        assert!(decode_key_hex("zz").is_err());
    }

    #[test]
    fn proposer_material_uses_one_fingerprint_twice() {
        let party_key = PartyKey::from_key(key(3));
        let material = proposer_key_material(&party_key);

        assert_eq!(
            material.namespace_fingerprint.as_deref(),
            Some(party_key.fingerprint.as_str())
        );
        assert_eq!(
            material.daml_key_fingerprint.as_deref(),
            Some(party_key.fingerprint.as_str())
        );
        assert_eq!(
            material.signing_public_key_hex.as_deref(),
            Some(party_key.key_hex.as_str())
        );
    }

    #[test]
    fn owner_fingerprints_without_a_party_are_every_namespace_key() {
        let vault = vec![
            vault_key("root", 1, &[SigningKeyUsage::Namespace]),
            vault_key(
                "acme-key",
                2,
                &[SigningKeyUsage::Namespace, SigningKeyUsage::Protocol],
            ),
            vault_key("acme-daml-transactions", 3, &[SigningKeyUsage::Protocol]),
        ];

        let owners = owner_fingerprints_from(&vault, None, Some("acme"));

        assert_eq!(owners, BTreeSet::from([fp(1), fp(2)]));
    }

    /// A member of a legacy party holds several namespace keys; only the one
    /// that owns this party's DND counts. A joiner's fresh `{prefix}-key`
    /// counts too, because the proposed DND adds it.
    #[test]
    fn owner_fingerprints_with_a_party_are_head_owners_plus_the_party_key() {
        let vault = vec![
            vault_key("root", 1, &[SigningKeyUsage::Namespace]),
            vault_key("other-namespace", 2, &[SigningKeyUsage::Namespace]),
            vault_key(
                "net-key",
                3,
                &[SigningKeyUsage::Namespace, SigningKeyUsage::Protocol],
            ),
        ];
        let head = BTreeSet::from([fp(2), fp(9)]);

        assert_eq!(
            owner_fingerprints_from(&vault, Some(&head), Some("net")),
            BTreeSet::from([fp(2), fp(3)])
        );
        assert_eq!(
            owner_fingerprints_from(&vault, Some(&head), None),
            BTreeSet::from([fp(2)])
        );
    }

    #[test]
    fn on_chain_daml_key_needs_exactly_one_match() {
        let vault = vec![
            vault_key("a", 1, &[SigningKeyUsage::Protocol]),
            vault_key("b", 2, &[SigningKeyUsage::Protocol]),
            vault_key("ns", 3, &[SigningKeyUsage::Namespace]),
        ];

        assert_eq!(
            on_chain_daml_key(&vault, &BTreeSet::from([fp(1), fp(8)])),
            Some(fp(1))
        );
        // A Namespace-only key is not a party signing key even when listed.
        assert_eq!(on_chain_daml_key(&vault, &BTreeSet::from([fp(3)])), None);
        assert_eq!(
            on_chain_daml_key(&vault, &BTreeSet::from([fp(1), fp(2)])),
            None
        );
        assert_eq!(on_chain_daml_key(&vault, &BTreeSet::new()), None);
    }

    #[test]
    fn vault_daml_key_prefers_the_dual_key() {
        let vault = vec![
            vault_key("net-daml-transactions", 1, &[SigningKeyUsage::Protocol]),
            vault_key(
                "net-key",
                2,
                &[SigningKeyUsage::Namespace, SigningKeyUsage::Protocol],
            ),
        ];
        assert_eq!(vault_daml_key_for_prefix(&vault, "net"), Some(fp(2)));
        assert_eq!(vault_daml_key_for_prefix(&vault[..1], "net"), Some(fp(1)));
        assert_eq!(vault_daml_key_for_prefix(&vault, "other"), None);
    }

    #[test]
    fn kicked_member_needs_a_cached_owner_key() {
        let rows = vec![
            row("p1::1220aa", Some("owner-1"), Some("daml-1")),
            row("p2::1220bb", None, Some("daml-2")),
        ];

        assert_eq!(
            kicked_member_from_rows(&rows, "p1::1220aa"),
            Some(KickedMember {
                participant_id: "p1::1220aa".to_string(),
                owner_fingerprint: "owner-1".to_string(),
                signing_key_fingerprint: Some("daml-1".to_string()),
            })
        );
        assert_eq!(kicked_member_from_rows(&rows, "p2::1220bb"), None);
        assert_eq!(kicked_member_from_rows(&rows, "p3::1220cc"), None);
    }

    #[test]
    fn key_claims_skip_members_without_a_signing_key() {
        let rows = vec![
            row("p1::1220aa", Some("owner-1"), Some("daml-1")),
            row("p2::1220bb", Some("owner-2"), None),
        ];

        assert_eq!(
            key_claims_from_rows(&rows),
            BTreeMap::from([("p1::1220aa".to_string(), "daml-1".to_string())])
        );
    }

    async fn seed_party(pool: &SqlitePool, party: &CantonId, uids: &[&str]) -> Result<()> {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party.to_string(),
            prefix: party.prefix.clone(),
            threshold: 1,
            updated_at: 0,
            my_owner_key: None,
        })
        .await?;
        let rows: Vec<DecPartyParticipantRow> =
            uids.iter().map(|uid| row(uid, None, None)).collect();
        tx.replace_dec_party_participants(party, &rows).await?;
        Commitable::commit(tx).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn record_member_keys_fills_the_cache_a_kick_reads(pool: SqlitePool) -> Result<()> {
        let party = CantonId::parse(&format!("net::{NS}"))?;
        let p1 = CantonId::parse(
            "p1::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )?;
        let p2 = CantonId::parse(
            "p2::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )?;
        seed_party(&pool, &party, &[&p1.to_string(), &p2.to_string()]).await?;

        record_member_keys(&pool, &party, &p1, Some("owner-1"), Some("daml-1")).await?;
        // A partial record leaves the other column alone.
        record_member_keys(&pool, &party, &p2, None, Some("daml-2")).await?;

        let rows = pool.get_dec_party_participants(&party).await?;
        assert_eq!(
            kicked_member_from_rows(&rows, &p1.to_string()),
            Some(KickedMember {
                participant_id: p1.to_string(),
                owner_fingerprint: "owner-1".to_string(),
                signing_key_fingerprint: Some("daml-1".to_string()),
            })
        );
        assert_eq!(kicked_member_from_rows(&rows, &p2.to_string()), None);
        assert_eq!(
            survivor_key_claims(&pool, &party).await?,
            BTreeMap::from([
                (p1.to_string(), "daml-1".to_string()),
                (p2.to_string(), "daml-2".to_string()),
            ])
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn record_member_keys_skips_an_unknown_participant(pool: SqlitePool) -> Result<()> {
        let party = CantonId::parse(&format!("net::{NS}"))?;
        let p1 = CantonId::parse(
            "p1::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )?;
        seed_party(&pool, &party, &[]).await?;

        record_member_keys(&pool, &party, &p1, Some("owner-1"), None).await?;

        assert!(pool.get_dec_party_participants(&party).await?.is_empty());
        assert!(survivor_key_claims(&pool, &party).await?.is_empty());
        Ok(())
    }
}
