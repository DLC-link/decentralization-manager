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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use canton_proto_rs::com::digitalasset::canton::crypto::{
    admin::v30::{
        ListKeysFilters, ListMyKeysRequest, private_key_metadata,
        vault_service_client::VaultServiceClient,
    },
    v30::{SigningKeyUsage, SigningPublicKey, public_key},
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
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

/// This node's own Daml signing-key fingerprint for a party: from its
/// long-lived identity row, falling back to the vault key named after the
/// party. Returns `None` when the node holds neither.
async fn own_signing_key_fingerprint(
    config: &NodeConfig,
    db: &SqlitePool,
    dec_party_id: &CantonId,
) -> Result<Option<String>> {
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
    Ok(None)
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
) -> Result<BTreeMap<String, String>> {
    let mut claims = BTreeMap::new();

    for participant in db.get_dec_party_participants(dec_party_id).await? {
        if let Some(fingerprint) = participant.signing_key {
            claims.insert(participant.participant_uid, fingerprint);
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
                claims.insert(peer_id, utils::compute_fingerprint(&keys[1]));
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

    let self_id = config.participant_id().to_string();
    if !claims.contains_key(&self_id)
        && let Some(own) = own_signing_key_fingerprint(config, db, dec_party_id).await?
    {
        claims.insert(self_id, own);
    }

    Ok(claims)
}

/// The key set a party should carry once `removed` is no longer a member.
///
/// `current` is the party's on-chain key list, `survivors` the members that
/// stay, and `claims` the attribution [`known_signing_keys_by_member`]
/// assembled. The removed member's key is identified directly when its
/// fingerprint is known, and otherwise by elimination — a key no survivor
/// claims can only be the departing member's.
///
/// # Errors
///
/// Errors when the key cannot be attributed either way. Signing a mapping
/// that still carries the key would leave a removed participant counting
/// towards the party's signing threshold, which is the whole point of taking
/// it out.
pub fn signing_keys_without_member(
    current: &[SigningPublicKey],
    removed: &str,
    survivors: &[String],
    claims: &BTreeMap<String, String>,
) -> Result<Vec<SigningPublicKey>> {
    let fingerprints: Vec<String> = current.iter().map(utils::compute_fingerprint).collect();

    let claimed_by_removed = claims.get(removed).filter(|f| fingerprints.contains(f));
    let target = match claimed_by_removed {
        Some(fingerprint) => {
            tracing::info!("Removing {removed}'s Daml signing key {fingerprint} from the party");
            fingerprint.clone()
        }
        None => {
            let survivor_claims: BTreeSet<&String> =
                survivors.iter().filter_map(|uid| claims.get(uid)).collect();
            let unclaimed: Vec<&String> = fingerprints
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

    Ok(current
        .iter()
        .zip(&fingerprints)
        .filter(|(_, fingerprint)| *fingerprint != &target)
        .map(|(key, _)| key.clone())
        .collect())
}

/// Load the party's signing keys from its deprecated `PartyToKeyMapping` and
/// attribute them to `members`.
///
/// Only for a party whose `PartyToParticipant` carries no inline
/// `party_signing_keys` — see [`signing_keys_for_members`] for what the
/// attribution rejects.
///
/// # Errors
///
/// Errors when the party has no `PartyToKeyMapping` either, or when the keys
/// it holds cannot be attributed to every current member.
pub async fn adopt_legacy_signing_keys(
    config: &NodeConfig,
    db: &SqlitePool,
    synchronizer_id: &str,
    dec_party_id: &CantonId,
    members: &[String],
) -> Result<Vec<SigningPublicKey>> {
    let mapping = topology::fetch_party_to_key_mapping(config, synchronizer_id, dec_party_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Party {dec_party_id} carries neither inline party signing keys nor a legacy \
                 PartyToKeyMapping, so its signing key set cannot be rebuilt"
            )
        })?;

    tracing::info!(
        "Party {dec_party_id} keeps its signing keys in a legacy PartyToKeyMapping: \
         {keys} key(s) at threshold {threshold}, against {members} current member(s)",
        keys = mapping.signing_keys.len(),
        threshold = mapping.threshold,
        members = members.len()
    );

    let claims = known_signing_keys_by_member(config, db, dec_party_id).await?;
    signing_keys_for_members(&mapping.signing_keys, members, &claims)
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
/// Inline keys take precedence over the legacy mapping, so the keys left
/// behind stop authorizing for the party once the proposal is in force.
///
/// # Errors
///
/// Errors when a member's key is unknown to this node, when a member claims a
/// key the mapping does not carry, or when two members claim the same key.
/// Each of those would put a key set into topology that does not match the
/// membership, which every peer refuses to sign.
pub fn signing_keys_for_members(
    legacy_keys: &[SigningPublicKey],
    members: &[String],
    claims: &BTreeMap<String, String>,
) -> Result<Vec<SigningPublicKey>> {
    let by_fingerprint: HashMap<String, &SigningPublicKey> = legacy_keys
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

    let left_behind = legacy_keys.len() - keys.len();
    if left_behind > 0 {
        tracing::warn!(
            "Moving {adopted} of the party's {total} legacy signing keys onto the \
             PartyToParticipant; the remaining {left_behind} are claimed by no current \
             member and stop authorizing for the party",
            adopted = keys.len(),
            total = legacy_keys.len()
        );
    }

    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `SigningPublicKey` whose fingerprint is stable and distinct per
    /// `seed` — the tests only ever compare fingerprints.
    fn key(seed: u8) -> SigningPublicKey {
        SigningPublicKey {
            public_key: vec![seed; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        }
    }

    fn fingerprint(seed: u8) -> String {
        utils::compute_fingerprint(&key(seed))
    }

    fn claims(entries: &[(&str, u8)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(uid, seed)| ((*uid).to_string(), fingerprint(*seed)))
            .collect()
    }

    fn uids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
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
                error.to_string().contains("Cannot tell which"),
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

        let adopted = signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)?;

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

        let error = signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)
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

        let error = signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)
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

        let error = signing_keys_for_members(&legacy, &uids(&["p1", "p2"]), &claims)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(
            error.contains("another member already claimed"),
            "unexpected error: {error}"
        );
        Ok(())
    }
}
