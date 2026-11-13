//! The party's Daml signing keys, and which member contributed each one.
//!
//! `PartyToParticipant.party_signing_keys` is a flat list: the protobuf
//! records no owner per key, and the Daml key — unlike the namespace key —
//! gets no `NamespaceDelegation`, so nothing on-chain says whose key is
//! whose. Only the node that generated a key can say it is theirs.
//!
//! Three sources are therefore combined to attribute the keys of a party:
//!
//! 1. `dec_party_identity` — every peer's key bundle on the node that
//!    coordinated onboarding, the new member's on the node that ran
//!    add-party, and its own on every node.
//! 2. `dec_party_participant.signing_key` — each peer's own fingerprint as it
//!    reported it over the `OwnerKeys` exchange, cached by the
//!    `/decentralized-parties` refresh.
//! 3. Elimination — a key claimed by no surviving member belongs to the one
//!    being removed.
//! 4. The vault — for a key list already in hand, whether this node holds a
//!    private half decides which of those keys is its own, whatever the key
//!    is named locally.
//!
//! A party onboarded before Canton 3.4 keeps its keys in a deprecated
//! `PartyToKeyMapping` rather than inline on the `PartyToParticipant`;
//! [`adopt_legacy_signing_keys`] attributes that list the same way, so the
//! keys can move inline without carrying a departed member's along.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            ListKeysFilters, ListMyKeysRequest, private_key_metadata,
            vault_service_client::VaultServiceClient,
        },
        v30::{SigningKeyUsage, SigningPublicKey, public_key},
    },
    protocol::v30::PartyToParticipant,
};
use sqlx::SqlitePool;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    utils,
    workflow::{
        onboarding::steps::proposals::create::decode_keys_payload,
        storage::{WorkflowStorage, identity_kinds},
        topology,
    },
};

/// Vault name of the Daml signing key a node holds for the party with this
/// id prefix. Onboarding and add-party both mint the key under this name, and
/// `get_or_create_signing_key` finds it again by it, so it is the local
/// record of which party a protocol key belongs to.
pub fn party_daml_key_name(party_id_prefix: &str) -> String {
    format!("{party_id_prefix}-daml-transactions")
}

/// Whether this node's vault holds the private half of a key.
///
/// # Errors
///
/// Errors when the vault cannot be listed.
pub async fn vault_holds(config: &NodeConfig, fingerprint: &Fingerprint) -> Result<bool> {
    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    let response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: fingerprint.to_string(),
                name: String::new(),
                purpose: vec![],
                usage_v30: vec![],
            }),
            base_request: None,
        }))
        .await?
        .into_inner();

    Ok(!response.private_keys_metadata.is_empty())
}

/// This node's namespace key for a party: the vault key with namespace usage
/// whose fingerprint is in the party's decentralized-namespace owner set.
///
/// Returns `None` when the party's namespace definition cannot be read or no
/// vault key matches — this node is then not an owner, or the key is gone.
///
/// # Errors
///
/// Errors when the vault cannot be listed.
pub async fn own_namespace_key(
    config: &NodeConfig,
    dec_party_id: &CantonId,
    synchronizer_id: &str,
) -> Result<Option<SigningPublicKey>> {
    let owners = match topology::fetch_namespace_definition(
        config,
        synchronizer_id,
        &dec_party_id.namespace.to_hex(),
    )
    .await
    {
        Ok(definition) => definition.owners,
        Err(e) => {
            tracing::warn!("Cannot read the namespace definition for {dec_party_id}: {e:#}");
            return Ok(None);
        }
    };

    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    let response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: String::new(),
                name: String::new(),
                purpose: vec![],
                usage_v30: vec![SigningKeyUsage::Namespace as i32],
            }),
            base_request: None,
        }))
        .await?
        .into_inner();

    for meta in response.private_keys_metadata {
        if let Some(private_key_metadata::PublicKeyWithName::V30(named)) = meta.public_key_with_name
            && let Some(pk) = named.public_key
            && let Some(public_key::Key::SigningPublicKey(signing_key)) = pk.key
            && owners.contains(&utils::compute_fingerprint(&signing_key).into())
        {
            return Ok(Some(signing_key));
        }
    }

    Ok(None)
}

/// This node's own Daml signing-key fingerprint for a party: from its
/// long-lived identity row, then the vault key named after the party, then —
/// for any `candidates` the caller already holds — the one whose private half
/// is in this node's vault. Returns `None` when none of the three answer.
///
/// The last step is what saves a party whose key was minted under some other
/// name: the name is a local convention, the vault is the fact.
async fn own_signing_key_fingerprint(
    config: &NodeConfig,
    db: &SqlitePool,
    dec_party_id: &CantonId,
    candidates: &[SigningPublicKey],
) -> Result<Option<Fingerprint>> {
    let self_id = config.participant_id().to_string();
    if let Some(payload) = db
        .read_identity(dec_party_id, identity_kinds::PEER_PUBLIC_KEYS, &self_id)
        .await?
    {
        let keys = decode_keys_payload(&payload)?;
        if keys.len() == 2 {
            return Ok(Some(utils::compute_fingerprint(&keys[1])));
        }
        tracing::warn!(
            "PEER_PUBLIC_KEYS for {self_id} on {dec_party_id} holds {count} keys, not 2 — \
             falling back to the vault",
            count = keys.len()
        );
    }

    let name = party_daml_key_name(&dec_party_id.prefix);
    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    let response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: String::new(),
                name: name.clone(),
                purpose: vec![],
                usage_v30: vec![SigningKeyUsage::Protocol as i32],
            }),
            base_request: None,
        }))
        .await?
        .into_inner();

    for meta in response.private_keys_metadata {
        if let Some(private_key_metadata::PublicKeyWithName::V30(pkn)) = meta.public_key_with_name
            && let Some(pk) = pkn.public_key
            && let Some(public_key::Key::SigningPublicKey(spk)) = pk.key
        {
            return Ok(Some(utils::compute_fingerprint(&spk)));
        }
    }

    tracing::warn!("No Daml signing key named '{name}' in this node's vault");

    // `candidates` is the whole legacy mapping, which also holds the keys of
    // members that left, and a node that rotated its Daml key still holds the
    // old one. So collect every match instead of taking the first: the mapping
    // order must not decide which key this node claims.
    let mut held: Vec<&SigningPublicKey> = Vec::new();
    for candidate in candidates {
        if vault_holds(config, &utils::compute_fingerprint(candidate)).await? {
            held.push(candidate);
        }
    }

    // A party signing key is a protocol key, so those win outright. Anything
    // else is a guess, and a guess between several is worth saying out loud.
    let protocol: Vec<&SigningPublicKey> = held
        .iter()
        .copied()
        .filter(|key| key.usage.contains(&(SigningKeyUsage::Protocol as i32)))
        .collect();
    let considered = if protocol.is_empty() {
        &held
    } else {
        &protocol
    };

    let Some(chosen) = considered.first() else {
        return Ok(None);
    };
    let fingerprint = utils::compute_fingerprint(chosen);

    if considered.len() > 1 {
        tracing::warn!(
            "This node holds {count} of {dec_party_id}'s signing keys under other names \
             ({all}); taking {fingerprint}. A rotated key or a former member's key can look \
             the same from here, so check that this is the key this node signs with",
            count = considered.len(),
            all = considered
                .iter()
                .map(|key| utils::compute_fingerprint(key).into())
                .collect::<Vec<String>>()
                .join(", ")
        );
    } else {
        tracing::info!(
            "This node holds {fingerprint}, one of the party's signing keys, under another \
             name; taking it as its own Daml key for {dec_party_id}"
        );
    }

    Ok(Some(fingerprint))
}

/// Which Daml signing-key fingerprint each member of a party contributed, as
/// far as this node can tell. Members whose key is unknown are absent from
/// the map.
///
/// Reads the identity rows this node happens to hold, then the fingerprints
/// participants reported for themselves, then — for this node — its own key.
pub async fn known_signing_keys_by_member(
    config: &NodeConfig,
    db: &SqlitePool,
    dec_party_id: &CantonId,
    candidates: &[SigningPublicKey],
) -> Result<BTreeMap<String, Fingerprint>> {
    let mut claims = recorded_signing_keys_by_member(db, dec_party_id).await?;

    let self_id = config.participant_id().to_string();
    if !claims.contains_key(&self_id)
        && let Some(own) = own_signing_key_fingerprint(config, db, dec_party_id, candidates).await?
    {
        claims.insert(self_id, own);
    }

    Ok(claims)
}

/// Member key fingerprints recorded in the cache and onboarding identities.
/// This performs no ledger or vault call.
pub(crate) async fn recorded_signing_keys_by_member(
    db: &SqlitePool,
    dec_party_id: &CantonId,
) -> Result<BTreeMap<String, Fingerprint>> {
    let mut claims = BTreeMap::new();

    for participant in db.get_dec_party_participants(dec_party_id).await? {
        let Some(stored) = participant.signing_key else {
            continue;
        };
        match stored.parse::<Fingerprint>() {
            Ok(fingerprint) => {
                claims.insert(participant.participant_uid, fingerprint);
            }
            Err(e) => tracing::warn!(
                "Ignoring the stored Daml signing key of {uid} on {dec_party_id}: {e:#}",
                uid = participant.participant_uid
            ),
        }
    }

    // Identity rows outrank the cache: they are the key bundle this node was
    // handed during the run that added the member, not a fingerprint relayed
    // over the network afterwards.
    for (peer_id, payload) in db
        .list_identity(dec_party_id, identity_kinds::PEER_PUBLIC_KEYS)
        .await?
    {
        match decode_keys_payload(&payload) {
            Ok(keys) if keys.len() == 2 => {
                let recorded = utils::compute_fingerprint(&keys[1]);
                // An identity row is a bundle this node was handed; the cache
                // is the fingerprint the peer read out of its own vault. Only
                // the second is evidence the peer can still sign, so a
                // disagreement is worth saying out loud before the row wins.
                if let Some(attested) = claims.get(&peer_id)
                    && *attested != recorded
                {
                    tracing::warn!(
                        "{peer_id} reports Daml signing key {attested} for {dec_party_id} but \
                         this node recorded {recorded}; taking the recorded one"
                    );
                }
                claims.insert(peer_id, recorded);
            }
            Ok(keys) => tracing::warn!(
                "PEER_PUBLIC_KEYS for {peer_id} on {dec_party_id} holds {count} keys, not 2",
                count = keys.len()
            ),
            Err(e) => {
                tracing::warn!(
                    "PEER_PUBLIC_KEYS for {peer_id} on {dec_party_id} will not decode: {e:#}"
                )
            }
        }
    }

    Ok(claims)
}

/// A refusal about who owns a party signing key. The message says what the
/// operator can do, so it is safe to show as it is.
#[derive(Debug, thiserror::Error)]
pub enum KeyClaimError {
    /// Two members that stay claim one key. A retry cannot tell which claim
    /// is wrong.
    #[error("{0}")]
    Conflict(String),
    /// A member's key is not on record, or not a key the party carries.
    #[error("{0}")]
    Unresolved(String),
}

/// The key claims a kick may rely on.
///
/// Each member reports its own key, and nothing checks the report. The member
/// being removed must not block its own kick by claiming the key of a member
/// that stays. Its claim is then dropped, and its key is found by
/// elimination. `required` lists the members that stay whose key must be on
/// record.
///
/// # Errors
///
/// Errors when two members that stay claim one key, or a required member's
/// key is not on record.
pub(crate) fn kick_key_claims(
    removed: Option<&str>,
    survivors: &[String],
    claims: &BTreeMap<String, Fingerprint>,
    required: &[String],
) -> Result<BTreeMap<String, Fingerprint>> {
    let mut owners: BTreeMap<&Fingerprint, &str> = BTreeMap::new();
    for member in survivors {
        let Some(fingerprint) = claims.get(member) else {
            continue;
        };
        if let Some(other) = owners.insert(fingerprint, member)
            && other != member
        {
            return Err(KeyClaimError::Conflict(format!(
                "Cannot kick: members {other} and {member} both claim signing key \
                 {fingerprint}. The kick cannot tell which claim is wrong. Check which of \
                 the two nodes holds that key, then refresh /decentralized-parties."
            ))
            .into());
        }
    }

    let mut usable = claims.clone();
    if let Some(removed) = removed
        && let Some(fingerprint) = claims.get(removed)
        && let Some(survivor) = owners.get(fingerprint)
    {
        tracing::warn!(
            "{removed} claims signing key {fingerprint}, which {survivor} also claims. \
             The claim of the member being removed is ignored"
        );
        usable.remove(removed);
    }

    let unknown: Vec<&str> = required
        .iter()
        .filter(|member| !claims.contains_key(*member))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(KeyClaimError::Unresolved(format!(
            "Cannot kick: this node has no record of the signing key of {members}. \
             A peer reports its key when /decentralized-parties is refreshed. \
             A member that is not a peer must first be added as one.",
            members = unknown.join(", ")
        ))
        .into());
    }
    Ok(usable)
}

/// A quorum may omit any survivor, so a known survivor's authority must remain
/// in the proposed mapping independently of which members later sign it.
///
/// # Errors
///
/// Errors when the proposed keys omit any known surviving member's key.
pub(crate) fn ensure_survivor_keys_preserved(
    proposed: &[SigningPublicKey],
    survivors: &[String],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result {
    let keys: BTreeSet<Fingerprint> = proposed.iter().map(utils::compute_fingerprint).collect();
    for survivor in survivors {
        if let Some(fingerprint) = claims.get(survivor)
            && !keys.contains(fingerprint)
        {
            return Err(KeyClaimError::Unresolved(format!(
                "Kick proposal does not preserve surviving member {survivor}'s known signing \
                 key {fingerprint}. Refresh /decentralized-parties to resolve the key records, \
                 then retry"
            ))
            .into());
        }
    }
    Ok(())
}

/// Whether rebuilding this mapping moves legacy keys inline. A partial
/// migration needs adoption just as a mapping with no inline keys does.
pub(crate) fn requires_signing_key_adoption(mapping: &PartyToParticipant) -> bool {
    mapping
        .party_signing_keys
        .as_ref()
        .map_or(0, |keys| keys.keys.len())
        < mapping.participants.len()
}

/// Which of the hosts that are not peers of this node must sign because
/// their keys move inline. Such a host cannot be invited to sign.
///
/// # Errors
///
/// Errors when a partial migration's key attribution cannot establish whether
/// a named host already has its key inline. Unknown attribution is not consent.
pub(crate) fn members_requiring_key_adoption(
    mapping: &PartyToParticipant,
    not_peers: &[String],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result<Vec<String>> {
    if !requires_signing_key_adoption(mapping) {
        return Ok(Vec::new());
    }
    let inline: BTreeSet<Fingerprint> = mapping
        .party_signing_keys
        .as_ref()
        .into_iter()
        .flat_map(|keys| &keys.keys)
        .map(utils::compute_fingerprint)
        .collect();
    let hosts: BTreeSet<&str> = mapping
        .participants
        .iter()
        .map(|host| host.participant_uid.as_str())
        .collect();
    let mut required = Vec::new();
    for member in not_peers
        .iter()
        .filter(|member| hosts.contains(member.as_str()))
    {
        if inline.is_empty() {
            required.push(member.clone());
            continue;
        }
        let fingerprint = claims.get(member).ok_or_else(|| {
            KeyClaimError::Unresolved(format!(
                "Cannot kick: this node has no record of the signing key of {member}. \
                 Without it, the kick cannot tell whether that key must move into the \
                 hosting list. {member} is not a peer of this node, so a refresh cannot read \
                 its key. Add it as a peer, refresh /decentralized-parties, then retry."
            ))
        })?;
        if !inline.contains(fingerprint) {
            required.push(member.clone());
        }
    }
    Ok(required)
}

/// The key set a party should carry once `removed` is no longer a member.
///
/// `current` is the party's on-chain key list, `survivors` the members that
/// stay, and `claims` the attribution [`known_signing_keys_by_member`]
/// assembled. The removed member's key is identified directly when its
/// fingerprint is known and no survivor claims it, and otherwise by
/// elimination — a key no survivor claims can only be the departing member's.
///
/// # Errors
///
/// Errors when survivor attribution is missing or conflicting, or the key
/// cannot be identified. A removed member's key must leave without taking
/// away a surviving member's authority.
pub fn signing_keys_without_member(
    current: &[SigningPublicKey],
    removed: &str,
    survivors: &[String],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result<Vec<SigningPublicKey>> {
    let claims = &kick_key_claims(Some(removed), survivors, claims, survivors)?;
    ensure_survivor_keys_preserved(current, survivors, claims)?;
    let fingerprints: Vec<Fingerprint> = current.iter().map(utils::compute_fingerprint).collect();

    let claimed_by_removed = claims.get(removed).filter(|f| fingerprints.contains(f));
    let target = match claimed_by_removed {
        Some(fingerprint) => {
            tracing::info!("Removing {removed}'s Daml signing key {fingerprint} from the party");
            fingerprint.clone()
        }
        None => {
            let survivor_claims: BTreeSet<&Fingerprint> =
                survivors.iter().filter_map(|uid| claims.get(uid)).collect();
            let unclaimed: Vec<&Fingerprint> = fingerprints
                .iter()
                .filter(|f| !survivor_claims.contains(f))
                .collect();
            match unclaimed.as_slice() {
                [fingerprint] => {
                    tracing::info!(
                        "{removed}'s Daml signing key is not recorded locally; {fingerprint} is \
                         the only party signing key no remaining member claims, so it is theirs"
                    );
                    (*fingerprint).clone()
                }
                [] => {
                    tracing::info!(
                        "Every party signing key is claimed by a remaining member, so \
                         {removed} has none left to remove"
                    );
                    return Ok(current.to_vec());
                }
                _ => anyhow::bail!(
                    "Cannot tell which of the party's {total} signing keys belongs to \
                     {removed}: {count} of them are claimed by no remaining member. Refresh \
                     /decentralized-parties so every member reports its signing key, then \
                     retry — leaving the key in would keep {removed} counting towards the \
                     party's signing threshold",
                    total = fingerprints.len(),
                    count = unclaimed.len()
                ),
            }
        }
    };

    let proposed: Vec<SigningPublicKey> = current
        .iter()
        .zip(&fingerprints)
        .filter(|(_, fingerprint)| *fingerprint != &target)
        .map(|(key, _)| key.clone())
        .collect();
    ensure_survivor_keys_preserved(&proposed, survivors, claims)?;
    Ok(proposed)
}

/// The key set a party should carry once a kick removes `removed`.
///
/// A member that removes its own hosting entry leaves its signing key behind.
/// When only one key is surplus, [`signing_keys_without_member`] finds it.
/// When more are, the kick also takes out keys other members left behind. No
/// survivor claims any of those, so the survivors' own keys stay and the rest
/// go.
///
/// # Errors
///
/// Errors when the surplus keys cannot be told apart from the survivors'.
/// With several to remove, that needs every survivor's claim, and each claim
/// must be a distinct key the party carries.
pub fn signing_keys_after_kick(
    current: &[SigningPublicKey],
    removed: &str,
    survivors: &[String],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result<Vec<SigningPublicKey>> {
    let claims = &kick_key_claims(Some(removed), survivors, claims, survivors)?;
    if current.len() <= survivors.len() + 1 {
        return signing_keys_without_member(current, removed, survivors, claims);
    }
    let fingerprints: Vec<Fingerprint> = current.iter().map(utils::compute_fingerprint).collect();
    let mut kept = BTreeSet::new();
    for survivor in survivors {
        let Some(fingerprint) = claims.get(survivor) else {
            anyhow::bail!(
                "The kick of {removed} must remove {count} of the party's {total} signing keys, \
                 but {survivor} has not reported which key is its own. Refresh \
                 /decentralized-parties so every member reports its signing key, then retry",
                count = current.len() - survivors.len(),
                total = current.len()
            );
        };
        if !fingerprints.contains(fingerprint) {
            anyhow::bail!(
                "{survivor} claims signing key {fingerprint}, which the party does not carry. \
                 Refresh /decentralized-parties, then retry"
            );
        }
        if !kept.insert(fingerprint) {
            anyhow::bail!(
                "{survivor} claims signing key {fingerprint}, which another member also claims"
            );
        }
    }
    tracing::info!(
        "Removing {count} signing keys that no remaining member claims",
        count = current.len() - kept.len()
    );
    let proposed: Vec<SigningPublicKey> = current
        .iter()
        .zip(&fingerprints)
        .filter(|(_, fingerprint)| kept.contains(fingerprint))
        .map(|(key, _)| key.clone())
        .collect();
    ensure_survivor_keys_preserved(&proposed, survivors, claims)?;
    Ok(proposed)
}

/// Load the party's signing keys from its deprecated `PartyToKeyMapping` and
/// attribute them to `members`.
///
/// For a party whose inline `party_signing_keys` do not cover every member.
/// That is a party which never had them, and also one a pre-1.10.0 build left
/// half-migrated: adding a member then wrote only the joiner's key inline
/// instead of adopting the legacy set, and every later membership change was
/// refused because the counts no longer matched.
///
/// `inline` is whatever the `PartyToParticipant` already carries. Those keys
/// join the legacy mapping's as candidates, because a half-migrated party
/// holds some members' keys in one place and the rest in the other. Attribution
/// is per member either way, so a key no current member claims is left behind.
///
/// See [`legacy_signing_keys_for_members`] for what the attribution rejects.
///
/// # Errors
///
/// Errors when the party has no `PartyToKeyMapping` and the inline keys do not
/// cover every member, or when the keys available cannot be attributed to every
/// current member.
pub async fn adopt_legacy_signing_keys(
    config: &NodeConfig,
    db: &SqlitePool,
    synchronizer_id: &str,
    dec_party_id: &CantonId,
    members: &[String],
    inline: &[SigningPublicKey],
) -> Result<Vec<SigningPublicKey>> {
    let mapping =
        topology::fetch_party_to_key_mapping(config, synchronizer_id, dec_party_id).await?;

    if mapping.is_none() && inline.is_empty() {
        anyhow::bail!(
            "Party {dec_party_id} carries neither inline party signing keys nor a legacy \
             PartyToKeyMapping, so its signing key set cannot be rebuilt"
        );
    }

    let legacy_keys = mapping
        .as_ref()
        .map(|m| m.signing_keys.clone())
        .unwrap_or_default();

    match &mapping {
        Some(m) => tracing::info!(
            "Party {dec_party_id} keeps signing keys in a legacy PartyToKeyMapping: {keys} key(s) \
             at threshold {threshold}, plus {inline} inline, against {members} current member(s)",
            keys = m.signing_keys.len(),
            threshold = m.threshold,
            inline = inline.len(),
            members = members.len()
        ),
        None => tracing::info!(
            "Party {dec_party_id} has no legacy PartyToKeyMapping; rebuilding from the {inline} \
             inline key(s) against {members} current member(s)",
            inline = inline.len(),
            members = members.len()
        ),
    }

    // Deduped by fingerprint so a key held in both places is one candidate.
    let mut candidates = legacy_keys;
    let mut seen: BTreeSet<Fingerprint> =
        candidates.iter().map(utils::compute_fingerprint).collect();
    for key in inline {
        if seen.insert(utils::compute_fingerprint(key)) {
            candidates.push(key.clone());
        }
    }

    let claims = known_signing_keys_by_member(config, db, dec_party_id, &candidates).await?;
    legacy_signing_keys_for_members(&candidates, members, &claims)
}

/// The keys of `legacy_keys` that `members` claim, in member order — the set
/// a party's `PartyToParticipant` should carry inline.
///
/// A party onboarded before Canton 3.4 has no inline `party_signing_keys`, so
/// a workflow that merges a new member's key into that empty set would
/// propose a party whose only signing key is the new member's. The legacy
/// `PartyToKeyMapping` holds the real keys, but nothing rewrites it when a
/// member leaves, so it also holds the keys of everyone who was ever a
/// member. Only a key a current member claims may move inline.
///
/// Inline keys take precedence over the legacy mapping, so a key left behind
/// stops authorizing while the inline set is in force. It is shadowed rather
/// than revoked: Canton reads the inline keys first and falls back to the
/// mapping, so a later `PartyToParticipant` without inline keys would hand
/// authority back to it. Every mapping this tool writes carries them, and
/// removing the `PartyToKeyMapping` outright is tracked separately.
///
/// # Errors
///
/// Errors when a member's key is unknown to this node, when a member claims a
/// key the mapping does not carry, or when two members claim the same key.
/// Each of those would put a key set into topology that does not match the
/// membership, which every peer refuses to sign.
pub fn legacy_signing_keys_for_members(
    legacy_keys: &[SigningPublicKey],
    members: &[String],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result<Vec<SigningPublicKey>> {
    let by_fingerprint: HashMap<Fingerprint, &SigningPublicKey> = legacy_keys
        .iter()
        .map(|key| (utils::compute_fingerprint(key), key))
        .collect();

    let mut keys = Vec::with_capacity(members.len());
    let mut taken = BTreeSet::new();
    let mut unclaimed = Vec::new();
    let mut unmatched = Vec::new();
    let mut duplicated = Vec::new();
    for member in members {
        let Some(fingerprint) = claims.get(member) else {
            unclaimed.push(member.clone());
            continue;
        };
        match by_fingerprint.get(fingerprint) {
            Some(key) if taken.insert(fingerprint.clone()) => keys.push((*key).clone()),
            Some(_) => duplicated.push(format!("{member} ({fingerprint})")),
            None => unmatched.push(format!("{member} ({fingerprint})")),
        }
    }

    if !unclaimed.is_empty() {
        anyhow::bail!(
            "Cannot tell which Daml signing key these members contributed: {members}. \
             Refresh /decentralized-parties so every member reports its signing key, then \
             retry",
            members = unclaimed.join(", ")
        );
    }
    if !unmatched.is_empty() {
        anyhow::bail!(
            "These members claim a Daml signing key the party's PartyToKeyMapping does not \
             carry, so it cannot be moved onto the PartyToParticipant: {members}",
            members = unmatched.join(", ")
        );
    }
    if !duplicated.is_empty() {
        anyhow::bail!(
            "These members claim a Daml signing key another member already claimed: \
             {members}. One of the two attributions is wrong, and the party would end up \
             with fewer keys than members",
            members = duplicated.join(", ")
        );
    }

    // Count distinct keys, not entries. A legacy mapping is allowed to repeat a
    // key, and `by_fingerprint` collapses those while `legacy_keys` does not,
    // so the raw length would report keys left behind that do not exist. An
    // operator reading this during a one-shot migration deserves a true count.
    let distinct = by_fingerprint.len();
    if legacy_keys.len() != distinct {
        tracing::info!(
            "The party's PartyToKeyMapping lists {entries} entries for {distinct} distinct \
             key(s); the repeats are counted once",
            entries = legacy_keys.len()
        );
    }

    let left_behind = distinct.saturating_sub(keys.len());
    if left_behind > 0 {
        tracing::warn!(
            "Moving {adopted} of the party's {distinct} legacy signing keys onto the \
             PartyToParticipant; the remaining {left_behind} are claimed by no current \
             member. They stop authorizing while the inline keys are in force, which \
             shadows the old mapping rather than emptying it",
            adopted = keys.len()
        );
    }

    Ok(keys)
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeysWithThreshold,
        protocol::v30::party_to_participant::HostingParticipant,
    };

    use super::*;
    use crate::db::{
        MIGRATOR,
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaWrite},
    };

    /// A `SigningPublicKey` whose fingerprint is stable and distinct per
    /// `seed` — the tests only ever compare fingerprints.
    fn key(seed: u8) -> SigningPublicKey {
        SigningPublicKey {
            public_key: vec![seed; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        }
    }

    fn fingerprint(seed: u8) -> Fingerprint {
        utils::compute_fingerprint(&key(seed))
    }

    fn claims(entries: &[(&str, u8)]) -> BTreeMap<String, Fingerprint> {
        entries
            .iter()
            .map(|(uid, seed)| ((*uid).to_string(), fingerprint(*seed)))
            .collect()
    }

    fn uids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    fn seeds(keys: &[SigningPublicKey]) -> Vec<Fingerprint> {
        keys.iter().map(utils::compute_fingerprint).collect()
    }

    /// p1 and p2 host. p3 and p4 removed their own hosting entries and left
    /// keys 3 and 4 behind. Kicking p3 leaves two hosts, so both keys go.
    /// The same holds when p3 hosts and only p4 left its key behind.
    #[test]
    fn a_kick_drops_every_key_no_survivor_claims() -> Result {
        let current = vec![key(1), key(2), key(3), key(4)];
        let survivors = uids(&["p1", "p2"]);
        for claims in [
            claims(&[("p1", 1), ("p2", 2)]),
            claims(&[("p1", 1), ("p2", 2), ("p3", 3)]),
        ] {
            let kept = signing_keys_after_kick(&current, "p3", &survivors, &claims)?;
            assert_eq!(seeds(&kept), vec![fingerprint(1), fingerprint(2)]);
        }
        Ok(())
    }

    #[test]
    fn a_kick_that_drops_several_keys_needs_every_survivor_claim() {
        let current = vec![key(1), key(2), key(3), key(4)];
        let claims = claims(&[("p1", 1), ("p3", 3)]);
        match signing_keys_after_kick(&current, "p3", &uids(&["p1", "p2"]), &claims) {
            Ok(kept) => panic!("expected a refusal, kept {:?}", seeds(&kept)),
            Err(error) => assert!(
                error
                    .to_string()
                    .contains("no record of the signing key of p2"),
                "unexpected error: {error}"
            ),
        }
    }

    /// The target's claim cannot establish which authority survivors retain.
    /// They may be absent from the final quorum, so missing records must stop.
    #[test]
    fn a_kick_needs_survivor_claims_even_when_the_target_reports_a_key() {
        let current = vec![key(1), key(2), key(3)];
        let claims = claims(&[("p3", 3)]);
        let error = signing_keys_after_kick(&current, "p3", &uids(&["p1", "p2"]), &claims)
            .expect_err("a target's claim cannot stand in for the surviving members")
            .to_string();
        assert!(
            error.contains("no record of the signing key of p1"),
            "{error}"
        );
        assert!(error.contains("refreshed"), "{error}");
    }

    #[test]
    fn drops_the_key_the_removed_member_claims() -> Result {
        let current = vec![key(1), key(2), key(3)];
        let claims = claims(&[("p1", 1), ("p2", 2), ("p3", 3)]);

        let remaining = signing_keys_without_member(&current, "p3", &uids(&["p1", "p2"]), &claims)?;

        assert_eq!(
            remaining
                .iter()
                .map(utils::compute_fingerprint)
                .collect::<Vec<_>>(),
            vec![fingerprint(1), fingerprint(2)]
        );
        Ok(())
    }

    /// The devnet case: a fourth member joined via add-party, and the node
    /// coordinating the kick never recorded its key. The three survivors do
    /// claim theirs, so the fourth key is the one to drop.
    #[test]
    fn drops_the_unclaimed_key_when_the_removed_member_is_unknown() -> Result {
        let current = vec![key(1), key(2), key(3), key(4)];
        let claims = claims(&[("p1", 1), ("p2", 2), ("p3", 3)]);

        let remaining =
            signing_keys_without_member(&current, "p4", &uids(&["p1", "p2", "p3"]), &claims)?;

        assert_eq!(
            remaining
                .iter()
                .map(utils::compute_fingerprint)
                .collect::<Vec<_>>(),
            vec![fingerprint(1), fingerprint(2), fingerprint(3)]
        );
        Ok(())
    }

    /// A stale cached fingerprint that is not in the party's key set must not
    /// be taken at its word — elimination still finds the right key.
    #[test]
    fn ignores_a_claim_that_is_not_in_the_key_set() -> Result {
        let current = vec![key(1), key(2), key(3)];
        let claims = claims(&[("p1", 1), ("p2", 2), ("p3", 9)]);

        let remaining = signing_keys_without_member(&current, "p3", &uids(&["p1", "p2"]), &claims)?;

        assert_eq!(
            remaining
                .iter()
                .map(utils::compute_fingerprint)
                .collect::<Vec<_>>(),
            vec![fingerprint(1), fingerprint(2)]
        );
        Ok(())
    }

    /// Retrying a kick that already landed: the key is gone and there is
    /// nothing left to take out.
    #[test]
    fn leaves_the_set_alone_when_the_removed_key_is_already_gone() -> Result {
        let current = vec![key(1), key(2)];
        let claims = claims(&[("p1", 1), ("p2", 2)]);

        let remaining = signing_keys_without_member(&current, "p3", &uids(&["p1", "p2"]), &claims)?;

        assert_eq!(remaining.len(), 2);
        Ok(())
    }

    /// Two unattributed keys and no record of the removed member's: guessing
    /// would either strand a survivor's key or leave the departing member's
    /// in, so the kick has to stop.
    #[test]
    fn refuses_when_two_keys_are_unclaimed() {
        let current = vec![key(1), key(2), key(3)];
        let claims = claims(&[("p1", 1)]);

        match signing_keys_without_member(&current, "p3", &uids(&["p1", "p2"]), &claims) {
            Ok(_) => panic!("ambiguous attribution must not produce a key set"),
            Err(error) => assert!(
                error.to_string().contains("p2"),
                "unexpected error: {error}"
            ),
        }
    }

    /// The mainnet case: the party predates inline signing keys and its
    /// `PartyToKeyMapping` still holds the keys of members that have left.
    /// Only the two keys the current members claim move inline.
    #[test]
    fn takes_only_the_keys_current_members_claim() -> Result {
        let legacy = vec![key(1), key(2), key(3), key(4)];
        let claims = claims(&[("p1", 1), ("p2", 2)]);

        let adopted = legacy_signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)?;

        assert_eq!(
            adopted
                .iter()
                .map(utils::compute_fingerprint)
                .collect::<Vec<_>>(),
            vec![fingerprint(1), fingerprint(2)]
        );
        Ok(())
    }

    /// Guessing here would hand the party a key set that does not match its
    /// membership, so an unattributable member stops the run instead.
    #[test]
    fn refuses_a_member_whose_key_is_unknown() -> Result {
        let legacy = vec![key(1), key(2), key(3)];
        let claims = claims(&[("p1", 1)]);

        let error = legacy_signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(error.contains("p2"), "unexpected error: {error}");
        assert!(
            error.contains("Refresh /decentralized-parties"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A stale cached fingerprint must not silently drop a member's key.
    #[test]
    fn refuses_a_claim_the_legacy_mapping_does_not_carry() -> Result {
        let legacy = vec![key(1), key(2)];
        let claims = claims(&[("p1", 1), ("p2", 9)]);

        let error = legacy_signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(
            error.contains("PartyToKeyMapping does not"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn refuses_two_members_that_claim_the_same_key() -> Result {
        let legacy = vec![key(1), key(2)];
        let claims = claims(&[("p1", 1), ("p2", 1)]);

        let error = legacy_signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(
            error.contains("another member already claimed"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// Nothing pairs key *i* with member *i*: `SigningKeysWithThreshold` holds
    /// a set, and Canton sorts it by fingerprint when it serializes. The order
    /// is pinned here only so the proposal this tool builds is reproducible,
    /// which makes a diff between two runs mean something.
    #[test]
    fn follows_the_member_order_not_the_claim_order() -> Result {
        let legacy = vec![key(1), key(2), key(3)];
        let claims = claims(&[("p1", 1), ("p2", 2), ("p3", 3)]);

        let adopted =
            legacy_signing_keys_for_members(&legacy, &uids(&["p3", "p1", "p2"]), &claims)?;

        assert_eq!(
            adopted
                .iter()
                .map(utils::compute_fingerprint)
                .collect::<Vec<_>>(),
            vec![fingerprint(3), fingerprint(1), fingerprint(2)]
        );
        Ok(())
    }

    /// A legacy party nobody ever left: every key is claimed, so none is left
    /// behind and none of the three refusals applies.
    #[test]
    fn takes_every_key_when_no_member_has_left() -> Result {
        let legacy = vec![key(1), key(2)];
        let claims = claims(&[("p1", 1), ("p2", 2)]);

        let adopted = legacy_signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)?;

        assert_eq!(adopted.len(), legacy.len());
        Ok(())
    }

    /// Canton lets a legacy mapping repeat a key. The adopted set must still
    /// be one key per member, and the count of what is left behind has to go
    /// by distinct keys or it reports keys that do not exist.
    #[test]
    fn handles_a_mapping_that_repeats_a_key() -> Result {
        let legacy = vec![key(1), key(2), key(1), key(2)];
        let claims = claims(&[("p1", 1), ("p2", 2)]);

        let adopted = legacy_signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)?;

        assert_eq!(
            adopted
                .iter()
                .map(utils::compute_fingerprint)
                .collect::<Vec<_>>(),
            vec![fingerprint(1), fingerprint(2)]
        );
        Ok(())
    }

    /// The half-migrated shape a pre-1.10.0 add leaves behind: the joiner's key
    /// inline, every other member's still in the legacy mapping, and a key in
    /// that mapping no current member claims. Resolving over both sources has
    /// to produce one key per member, in member order, and leave the stray one.
    #[test]
    fn resolves_members_split_across_legacy_and_inline() -> Result {
        let legacy = vec![key(1), key(2), key(9)];
        let inline = vec![key(3)];
        let claims = claims(&[("p1", 1), ("p2", 2), ("p3", 3)]);

        let mut candidates = legacy;
        let mut seen: BTreeSet<Fingerprint> =
            candidates.iter().map(utils::compute_fingerprint).collect();
        for k in &inline {
            if seen.insert(utils::compute_fingerprint(k)) {
                candidates.push(k.clone());
            }
        }

        let adopted =
            legacy_signing_keys_for_members(&candidates, &uids(&["p1", "p2", "p3"]), &claims)?;

        assert_eq!(
            adopted
                .iter()
                .map(utils::compute_fingerprint)
                .collect::<Vec<_>>(),
            vec![fingerprint(1), fingerprint(2), fingerprint(3)],
            "one key per member, in member order, with the unclaimed key left behind"
        );
        Ok(())
    }

    /// A member whose key is in neither place must stop the run rather than
    /// produce a short key set that the membership check rejects later with a
    /// count that says nothing about which member is missing.
    #[test]
    fn refuses_a_member_whose_key_is_in_neither_place() {
        let candidates = vec![key(1), key(2)];
        let claims = claims(&[("p1", 1), ("p2", 2), ("p3", 3)]);

        let err = legacy_signing_keys_for_members(&candidates, &uids(&["p1", "p2", "p3"]), &claims)
            .expect_err("a member with no key anywhere must be refused");

        assert!(
            err.to_string().contains("p3"),
            "the error names the member that cannot be resolved: {err}"
        );
    }

    fn mapping_with_inline_keys(inline: &[u8]) -> PartyToParticipant {
        PartyToParticipant {
            party: "party".to_string(),
            threshold: 2,
            participants: uids(&["p1", "p2", "p3", "p4"])
                .into_iter()
                .map(|participant_uid| HostingParticipant {
                    participant_uid,
                    ..Default::default()
                })
                .collect(),
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: inline.iter().copied().map(key).collect(),
                threshold: 2,
            }),
        }
    }

    #[test]
    fn a_legacy_non_peer_must_sign_before_its_key_moves_inline() -> Result {
        let mapping = mapping_with_inline_keys(&[]);
        assert!(requires_signing_key_adoption(&mapping));
        assert_eq!(
            members_requiring_key_adoption(&mapping, &uids(&["p3"]), &BTreeMap::new())?,
            uids(&["p3"]),
        );
        Ok(())
    }

    #[test]
    fn a_partial_migration_names_only_the_non_peer_whose_key_is_not_inline() -> Result {
        let mapping = mapping_with_inline_keys(&[1, 3]);
        assert!(requires_signing_key_adoption(&mapping));
        let attributed = claims(&[("p1", 1), ("p2", 2), ("p3", 3), ("p4", 4)]);
        assert!(members_requiring_key_adoption(&mapping, &uids(&["p3"]), &attributed)?.is_empty());
        assert_eq!(
            members_requiring_key_adoption(&mapping, &uids(&["p2", "p3"]), &attributed)?,
            uids(&["p2"]),
        );
        let unknown = members_requiring_key_adoption(&mapping, &uids(&["p3"]), &BTreeMap::new())
            .expect_err("unknown attribution cannot establish that a missing member need not sign")
            .to_string();
        assert!(unknown.contains("p3"), "{unknown}");
        assert!(unknown.contains("Add it as a peer"), "{unknown}");
        let inline = mapping_with_inline_keys(&[1, 2, 3, 4]);
        assert!(!requires_signing_key_adoption(&inline));
        assert!(
            members_requiring_key_adoption(&inline, &uids(&["p3"]), &BTreeMap::new())?.is_empty()
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn key_adoption_uses_the_recorded_key_of_each_missing_member(pool: SqlitePool) -> Result {
        let party = CantonId::parse(&format!("party::1220{}", "09".repeat(32)))?;
        let members: Vec<String> = (1..=4)
            .map(|tag| format!("p{tag}::1220{}", format!("{tag:02x}").repeat(32)))
            .collect();
        let rows: Vec<DecPartyParticipantRow> = members
            .iter()
            .enumerate()
            .map(|(index, uid)| DecPartyParticipantRow {
                dec_party_id: party.to_string(),
                participant_uid: uid.clone(),
                permission: "confirmation".to_string(),
                owner_key: None,
                signing_key: Some(fingerprint((index + 1) as u8).to_string()),
            })
            .collect();
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party.to_string(),
            prefix: party.prefix.clone(),
            threshold: 2,
            updated_at: 0,
            my_owner_key: None,
            confirmation_threshold: None,
        })
        .await?;
        tx.replace_dec_party_participants(&party, &rows).await?;
        Commitable::commit(tx).await?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(CantonId::parse(&members[0])?);
        let mut mapping = mapping_with_inline_keys(&[1, 2]);
        for (host, uid) in mapping.participants.iter_mut().zip(&members) {
            host.participant_uid = uid.clone();
        }
        let attributed =
            known_signing_keys_by_member(&config, &pool, &party, &[key(1), key(2)]).await?;
        assert_eq!(
            members_requiring_key_adoption(&mapping, &members[2..3], &attributed)?,
            members[2..3],
        );
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![key(1), key(3)],
            threshold: 2,
        });
        // p2 still needs adoption, but p3 is a missing peer whose key is
        // already inline. Its absence cannot justify refusing this kick.
        assert!(members_requiring_key_adoption(&mapping, &members[2..3], &attributed)?.is_empty());
        Ok(())
    }

    /// The kicked member reports a remaining member's key as its own. It is
    /// still kicked: its claim is ignored, its own key is found by
    /// elimination, and the remaining member keeps its key.
    #[test]
    fn a_target_that_claims_a_survivors_key_is_still_kicked() -> Result {
        let current = vec![key(1), key(2), key(3), key(4)];
        let survivors = uids(&["p1", "p2", "p3"]);
        let attributed = claims(&[("p1", 1), ("p2", 2), ("p3", 3), ("p4", 3)]);
        for kept in [
            signing_keys_without_member(&current, "p4", &survivors, &attributed)?,
            signing_keys_after_kick(&current, "p4", &survivors, &attributed)?,
        ] {
            assert_eq!(
                seeds(&kept),
                vec![fingerprint(1), fingerprint(2), fingerprint(3)]
            );
        }
        let wrong = vec![key(1), key(2), key(4)];
        let error = ensure_survivor_keys_preserved(&wrong, &survivors, &attributed)
            .expect_err("the right number of keys does not preserve p3's authority")
            .to_string();
        assert!(error.contains("p3"), "{error}");
        assert!(error.contains(&fingerprint(3).to_string()), "{error}");
        Ok(())
    }

    /// A remaining member's claim is never dropped for the kicked member's,
    /// so the key a remaining member claims is never the one removed.
    #[test]
    fn a_survivors_claim_is_never_dropped_for_the_targets() -> Result {
        let current = vec![key(1), key(2), key(3), key(4)];
        let survivors = uids(&["p1", "p2", "p3"]);
        let attributed = claims(&[("p1", 1), ("p2", 2), ("p3", 4), ("p4", 4)]);
        for kept in [
            signing_keys_without_member(&current, "p4", &survivors, &attributed)?,
            signing_keys_after_kick(&current, "p4", &survivors, &attributed)?,
        ] {
            assert!(
                seeds(&kept).contains(&fingerprint(4)),
                "p3's claimed key stays"
            );
        }
        Ok(())
    }

    #[test]
    fn two_survivors_that_claim_one_key_are_a_conflict() {
        let current = vec![key(1), key(2), key(3)];
        let attributed = claims(&[("p1", 1), ("p2", 1), ("p3", 3)]);
        let error = signing_keys_after_kick(&current, "p3", &uids(&["p1", "p2"]), &attributed)
            .expect_err("two remaining members claim one key");
        assert!(
            matches!(
                error.downcast_ref::<KeyClaimError>(),
                Some(KeyClaimError::Conflict(message))
                    if message.contains("p1") && message.contains("p2")
            ),
            "{error:#}"
        );
    }
}
