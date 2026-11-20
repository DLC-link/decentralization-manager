use std::collections::{BTreeMap, BTreeSet};

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningPublicKey, protocol::v30::PartyToParticipant,
};
use sqlx::SqlitePool;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    server::WorkflowKind,
    utils,
    workflow::{
        rotate_key::config::{require_complete_inline_keys, signing_threshold_in_force},
        signing_keys::known_signing_keys_by_member,
    },
};

use super::{
    decode::{decode_party_to_participant, decode_proposal_serial},
    delta::{AllowedDelta, already_applied, check_set_delta},
    expectations::PeerExpectations,
    head::{
        SynchronizerHead, TopologyHead, check_signing_key_attributes, hosts_not_in,
        signing_key_baseline,
    },
    own_keys::WhenUnrecorded,
};

impl PeerExpectations {
    /// Validate the P2P proposal of a rotate-key run, and return the new key
    /// it adds for the coordinator.
    ///
    /// The proposal must keep the members and both on-chain thresholds, and
    /// swap exactly the key the invitation names for one new key. This
    /// node's own records must attribute the replaced key to the coordinator
    /// and every other key to the member that holds it.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation, the chain, or this
    /// node's records.
    pub async fn check_rotation_proposal(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        p2p_payload: &[u8],
    ) -> Result<Fingerprint> {
        let dec_party_id = self.dec_party_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted rotate-key invitation carries no dec party id, so the proposal \
                 cannot be checked against it"
            )
        })?;
        let rotated_key = self.rotated_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted rotate-key invitation names no key to replace, so the proposal \
                 cannot be checked against it"
            )
        })?;

        let mapping = decode_party_to_participant(p2p_payload)?;
        if mapping.party != dec_party_id.to_string() {
            anyhow::bail!(
                "P2P proposal is for party {found} but the accepted invitation is for \
                 {dec_party_id}",
                found = mapping.party
            );
        }
        self.check_p2p_membership(&mapping)?;
        self.check_onboarding_markers(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;
        self.check_own_daml_key(
            config,
            storage,
            instance_name,
            &mapping,
            WhenUnrecorded::Skip,
        )
        .await?;

        let head = SynchronizerHead {
            config,
            synchronizer_id: utils::get_synchronizer_id(config).await?,
        };
        let proposed_keys = mapping
            .party_signing_keys
            .as_ref()
            .map(|keys| keys.keys.clone())
            .unwrap_or_default();
        // A proposal already in force has no head left to compare with, but
        // this node's records still decide which key it adds.
        let candidates = check_rotation_against_head(
            &head,
            dec_party_id,
            (decode_proposal_serial(p2p_payload)?, &mapping),
            rotated_key,
        )
        .await?
        .unwrap_or_else(|| proposed_keys.clone());
        let claims =
            known_signing_keys_by_member(config, storage, dec_party_id, &candidates).await?;
        let members: Vec<String> = mapping
            .participants
            .iter()
            .map(|p| p.participant_uid.clone())
            .collect();
        let proposed: Vec<Fingerprint> = proposed_keys
            .iter()
            .map(utils::compute_fingerprint)
            .collect();
        let new_key = check_rotation_claims(
            &self.coordinator_id.to_string(),
            rotated_key,
            &members,
            &proposed,
            &claims,
        )?;

        tracing::info!(
            "Rotate-key proposal for {dec_party_id} swaps only {coordinator}'s key {rotated_key} \
             for {new_key}",
            coordinator = self.coordinator_id
        );
        Ok(new_key)
    }
}

/// Hold a rotate-key proposal to the party's on-chain state.
///
/// The hosts stay as they are, and the key set changes by exactly one swap:
/// `rotated_key` leaves and one new key joins. Returns the on-chain key set
/// the proposal was built from, or `None` when the proposal is already in
/// force and there is nothing left to check.
async fn check_rotation_against_head(
    head: &impl TopologyHead,
    dec_party_id: &CantonId,
    (p2p_serial, mapping): (u32, &PartyToParticipant),
    rotated_key: &Fingerprint,
) -> Result<Option<Vec<SigningPublicKey>>> {
    let kind = WorkflowKind::RotateKey;
    let (head_p2p_serial, current_p2p) = head.p2p_mapping(dec_party_id).await.map_err(|e| {
        anyhow::anyhow!("cannot read the current signing keys of {dec_party_id}: {e:#}")
    })?;
    if already_applied(
        "P2P",
        (p2p_serial, mapping),
        (head_p2p_serial, &current_p2p),
    )? {
        return Ok(None);
    }

    let added = hosts_not_in(mapping, &current_p2p);
    let removed = hosts_not_in(&current_p2p, mapping);
    if !added.is_empty() || !removed.is_empty() {
        anyhow::bail!(
            "{kind:?} proposal changes the hosts (added {added:?}, removed {removed:?}); a key \
             rotation keeps them"
        );
    }
    // Same hosts is not enough: a changed permission or onboarding flag
    // would change what a host may do.
    let entries = |m: &PartyToParticipant| {
        let mut hosts = m.participants.clone();
        hosts.sort_by(|a, b| a.participant_uid.cmp(&b.participant_uid));
        hosts
    };
    if entries(mapping) != entries(&current_p2p) {
        anyhow::bail!(
            "{kind:?} proposal changes a host's permission or onboarding state; a key rotation \
             keeps them"
        );
    }
    // The invitation's thresholds come from the coordinator, so the chain
    // pins them here. A rotation keeps the signing threshold in force, even
    // where it differs from the namespace threshold.
    if mapping.threshold != current_p2p.threshold {
        anyhow::bail!(
            "{kind:?} proposal changes the confirmation threshold from {from} to {to}; a key \
             rotation keeps it",
            from = current_p2p.threshold,
            to = mapping.threshold
        );
    }
    require_complete_inline_keys(&current_p2p, dec_party_id)?;
    let legacy_mapping = if current_p2p.party_signing_keys.is_none() {
        head.party_to_key_mapping(dec_party_id).await?
    } else {
        None
    };
    let in_force = signing_threshold_in_force(&current_p2p, legacy_mapping.as_ref());
    let signing = mapping
        .party_signing_keys
        .as_ref()
        .map(|keys| keys.threshold);
    if signing != in_force {
        let shown =
            |threshold: Option<u32>| threshold.map_or("none".to_string(), |t| t.to_string());
        anyhow::bail!(
            "{kind:?} proposal changes the signing threshold from {from} to {to}; a key rotation \
             keeps it",
            from = shown(in_force),
            to = shown(signing)
        );
    }

    let (current_keys, legacy) = signing_key_baseline(head, dec_party_id, &current_p2p).await?;
    let current: BTreeSet<String> = current_keys
        .iter()
        .map(|key| utils::compute_fingerprint(key).into())
        .collect();
    if !current.contains(rotated_key.as_str()) {
        anyhow::bail!(
            "{dec_party_id} does not carry the key {rotated_key} that the accepted invitation \
             replaces"
        );
    }
    let proposed_keys = &mapping
        .party_signing_keys
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("P2P proposal carries no party signing keys"))?
        .keys;
    let proposed: Vec<String> = proposed_keys
        .iter()
        .map(|key| utils::compute_fingerprint(key).into())
        .collect();
    if proposed.iter().any(|key| key == rotated_key.as_str()) {
        anyhow::bail!("{kind:?} proposal keeps the key {rotated_key} it is meant to replace");
    }
    let additions = proposed
        .iter()
        .filter(|key| !current.contains(*key))
        .count();
    if additions != 1 {
        anyhow::bail!("{kind:?} proposal adds {additions} signing key(s); a rotation adds one");
    }

    let mut allowed = AllowedDelta::of("party signing key", kind)?;
    // A legacy baseline may also drop departed members' keys, so the removed
    // key is pinned above instead of here.
    allowed.removed = (!legacy).then_some(rotated_key.as_str());
    check_set_delta(
        "party signing key",
        kind,
        allowed,
        &current,
        &proposed,
        legacy,
    )?;
    check_signing_key_attributes(kind, &current_keys, proposed_keys)?;
    Ok(Some(current_keys))
}

/// Hold a rotation to this node's records of each member's Daml key, and
/// return the key the proposal adds for the coordinator.
///
/// Every other host must have a key on record here. That key must stay in
/// the proposal, and it must not be the key the rotation replaces. Exactly
/// one proposed key is then left, and it is the coordinator's new key. The
/// replaced key must be the coordinator's on record. A node that already
/// recorded the new key accepts it in place of the old one.
///
/// Without these checks a coordinator could swap or drop another member's
/// key, or keep a departed member's key, and so hold two of the party's keys.
fn check_rotation_claims(
    coordinator: &str,
    rotated_key: &Fingerprint,
    members: &[String],
    proposed: &[Fingerprint],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result<Fingerprint> {
    let mut others: BTreeMap<&Fingerprint, &str> = BTreeMap::new();
    for member in members.iter().filter(|m| m.as_str() != coordinator) {
        let Some(claimed) = claims.get(member) else {
            anyhow::bail!(
                "This node has no Daml key on record for {member}, so it cannot tell whether \
                 the rotation keeps theirs. Refresh /decentralized-parties on this node and retry"
            );
        };
        if claimed == rotated_key {
            anyhow::bail!(
                "The rotation replaces {rotated_key}, which this node has on record as \
                 {member}'s Daml key"
            );
        }
        if !proposed.contains(claimed) {
            anyhow::bail!(
                "The rotation drops {claimed}, which this node has on record as {member}'s \
                 Daml key"
            );
        }
        if let Some(previous) = others.insert(claimed, member) {
            anyhow::bail!(
                "This node has {claimed} on record for both {previous} and {member}. Refresh \
                 /decentralized-parties on this node and retry"
            );
        }
    }

    let left: Vec<&Fingerprint> = proposed
        .iter()
        .filter(|key| !others.contains_key(key))
        .collect();
    let [new_key] = left.as_slice() else {
        anyhow::bail!(
            "The rotation leaves {count} keys that no other member holds by this node's records \
             ({left:?}); it may leave only the coordinator's new key",
            count = left.len()
        );
    };
    if *new_key == rotated_key {
        anyhow::bail!("The rotation keeps the key {rotated_key} it is meant to replace");
    }
    if let Some((holder, _)) = claims
        .iter()
        .find(|(member, key)| member.as_str() != coordinator && key == new_key)
    {
        anyhow::bail!(
            "The rotation adds {new_key}, which this node has on record as {holder}'s Daml key"
        );
    }
    match claims.get(coordinator) {
        Some(claimed) if claimed == rotated_key || claimed == *new_key => {}
        Some(claimed) => anyhow::bail!(
            "The rotation replaces {rotated_key}, but this node has {claimed} on record as \
             {coordinator}'s Daml key. If {coordinator}'s last rotation failed after it was \
             submitted, retry that run instead of starting a new one"
        ),
        None => anyhow::bail!(
            "This node has no Daml key on record for {coordinator}, so it cannot tell whether \
             {rotated_key} is theirs. Refresh /decentralized-parties on this node and retry"
        ),
    }
    Ok((*new_key).clone())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold},
        protocol::v30::{DecentralizedNamespaceDefinition, enums},
    };

    use super::*;
    use crate::workflow::validation::test_support::*;

    fn keyed(seeds: &[u8]) -> SigningKeysWithThreshold {
        SigningKeysWithThreshold {
            keys: seeds.iter().copied().map(key_of).collect(),
            threshold: 2,
        }
    }

    /// A rotation on a three-member party whose on-chain keys are `head`
    /// (inline, at serial 5). The proposal carries `proposed` at `serial`
    /// and replaces the key of seed `rotated`.
    async fn rotation_against(
        head: Option<&[u8]>,
        legacy: Option<&[u8]>,
        (serial, proposed): (u32, &PartyToParticipant),
        rotated: u8,
    ) -> Result<Option<Vec<SigningPublicKey>>> {
        let stub = StubHead {
            dns: Some((
                2,
                DecentralizedNamespaceDefinition {
                    threshold: 2,
                    ..namespace(&["a", "b", "c"])
                },
            )),
            p2p: Some((5, keyed_p2p(head.map(keyed))?)),
            legacy_keys: legacy.map(|seeds| seeds.iter().copied().map(key_of).collect()),
        };
        check_rotation_against_head(
            &stub,
            &canton_id("party", 9)?,
            (serial, proposed),
            &utils::compute_fingerprint(&key_of(rotated)),
        )
        .await
    }

    async fn rotation_error(proposed: &PartyToParticipant, rotated: u8) -> Result<String> {
        match rotation_against(Some(&[1, 2, 3]), None, (6, proposed), rotated).await {
            Ok(_) => anyhow::bail!("expected the rotation to be refused"),
            Err(error) => Ok(error.to_string()),
        }
    }

    #[tokio::test]
    async fn a_rotation_may_swap_the_named_key_for_one_new_key() -> Result {
        let proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        let baseline = rotation_against(Some(&[1, 2, 3]), None, (6, &proposed), 2).await?;
        assert_eq!(baseline.map(|keys| keys.len()), Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_must_drop_the_key_the_invitation_names() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[9, 2, 3])))?, 2).await?;
        assert!(error.contains("keeps the key"), "unexpected error: {error}");
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_adds_exactly_one_key() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[8, 9, 3])))?, 2).await?;
        assert!(
            error.contains("adds 2 signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// Dropping another member's key with the named one would leave that
    /// member unable to sign for the party.
    #[tokio::test]
    async fn a_rotation_removes_no_key_but_the_named_one() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[9, 3])))?, 2).await?;
        assert!(
            error.contains("it must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_that_adds_no_key_is_refused() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[1, 3])))?, 2).await?;
        assert!(
            error.contains("adds 0 signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A coordinator could keep each member's key bytes and drop their
    /// protocol usage. Its own new key would then be the only key that
    /// counts toward the signing threshold.
    #[tokio::test]
    async fn a_rotation_keeps_each_other_key_as_it_is_on_chain() -> Result {
        let mut rewritten = keyed(&[1, 9, 3]);
        rewritten.keys[0].usage = vec![SigningKeyUsage::Namespace as i32];
        let error = rotation_error(&keyed_p2p(Some(rewritten))?, 2).await?;
        assert!(
            error.contains("changes its usage, spec or format"),
            "unexpected error: {error}"
        );

        let mut new_key = keyed(&[1, 9, 3]);
        new_key.keys[1].usage = vec![SigningKeyUsage::Namespace as i32];
        let error = rotation_error(&keyed_p2p(Some(new_key))?, 2).await?;
        assert!(
            error.contains("adds the signing key") && error.contains("without protocol usage"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A half-migrated party counts only its inline keys. A rotation would
    /// put every member's key inline at the inline threshold, so more
    /// members could sign alone.
    #[tokio::test]
    async fn a_rotation_of_a_half_migrated_party_is_refused() -> Result {
        let stub = StubHead {
            dns: Some((
                2,
                DecentralizedNamespaceDefinition {
                    threshold: 2,
                    ..namespace(&["a", "b", "c"])
                },
            )),
            p2p: Some((
                5,
                keyed_p2p(Some(SigningKeysWithThreshold {
                    threshold: 1,
                    ..keyed(&[2])
                }))?,
            )),
            legacy_keys: Some(vec![key_of(1), key_of(3)]),
        };
        let proposed = keyed_p2p(Some(SigningKeysWithThreshold {
            threshold: 1,
            ..keyed(&[1, 9, 3])
        }))?;
        let error = match check_rotation_against_head(
            &stub,
            &canton_id("party", 9)?,
            (6, &proposed),
            &utils::compute_fingerprint(&key_of(2)),
        )
        .await
        {
            Ok(_) => anyhow::bail!("a half-migrated party must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("1 inline signing key(s) for 3 hosts")
                && error.contains("Run change-threshold first"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// The invitation pins both thresholds, and a rotation changes neither.
    #[test]
    fn a_rotation_keeps_both_thresholds() -> Result {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        let rotation = PeerExpectations {
            kind: WorkflowKind::RotateKey,
            confirmation_threshold: Some(2),
            ..expectations(hosts.to_vec(), hosts[0].clone())
        };
        let swapped = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        rotation.check_p2p_thresholds(&swapped)?;

        let confirmation = PartyToParticipant {
            threshold: 3,
            ..swapped.clone()
        };
        let signing = PartyToParticipant {
            party_signing_keys: Some(SigningKeysWithThreshold {
                threshold: 1,
                ..keyed(&[1, 9, 3])
            }),
            ..swapped
        };
        for (proposal, expected) in [
            (
                confirmation,
                "confirmation threshold 3 differs from the accepted 2",
            ),
            (signing, "signing threshold 1 differs from the accepted 2"),
        ] {
            let error = match rotation.check_p2p_thresholds(&proposal) {
                Ok(()) => anyhow::bail!("expected a refusal: {expected}"),
                Err(e) => e.to_string(),
            };
            assert!(error.contains(expected), "unexpected error: {error}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_keeps_the_hosts() -> Result {
        let mut proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        proposed.participants[2].participant_uid = canton_id("p4", 4)?.to_string();
        let error = rotation_error(&proposed, 2).await?;
        assert!(
            error.contains("changes the hosts"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_of_a_key_the_party_does_not_carry_is_refused() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[1, 9, 3])))?, 7).await?;
        assert!(
            error.contains("does not carry the key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_only_signs_the_next_serial() -> Result {
        let proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        let error = match rotation_against(Some(&[1, 2, 3]), None, (7, &proposed), 2).await {
            Ok(_) => anyhow::bail!("a serial gap must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("only the next serial"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A retried peer may be handed the proposal after it went into force.
    #[tokio::test]
    async fn a_rotation_already_in_force_has_nothing_left_to_check() -> Result {
        let in_force = keyed_p2p(Some(keyed(&[1, 2, 3])))?;
        assert!(
            rotation_against(Some(&[1, 2, 3]), None, (5, &in_force), 2)
                .await?
                .is_none()
        );
        Ok(())
    }

    /// A legacy party moves its keys inline in the same proposal, which also
    /// drops a departed member's key. Only the new key may be added.
    #[tokio::test]
    async fn a_legacy_rotation_may_drop_departed_keys_but_adds_only_the_new_one() -> Result {
        let proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        rotation_against(None, Some(&[1, 2, 3, 4]), (6, &proposed), 2).await?;

        let stranger = keyed_p2p(Some(keyed(&[8, 9, 3])))?;
        let error = match rotation_against(None, Some(&[1, 2, 3, 4]), (6, &stranger), 2).await {
            Ok(_) => anyhow::bail!("two new keys must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("adds 2 signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A coordinator could otherwise set either threshold through its own
    /// invitation, because the invitation only repeats what it chose.
    #[tokio::test]
    async fn a_rotation_keeps_both_thresholds_on_chain() -> Result {
        let confirmation = PartyToParticipant {
            threshold: 1,
            ..keyed_p2p(Some(keyed(&[1, 9, 3])))?
        };
        let error = rotation_error(&confirmation, 2).await?;
        assert!(
            error.contains("changes the confirmation threshold from 2 to 1"),
            "unexpected error: {error}"
        );

        let signing = keyed_p2p(Some(SigningKeysWithThreshold {
            threshold: 1,
            ..keyed(&[1, 9, 3])
        }))?;
        let error = rotation_error(&signing, 2).await?;
        assert!(
            error.contains("changes the signing threshold from 2 to 1"),
            "unexpected error: {error}"
        );

        // A legacy party keeps its mapping's threshold too.
        let error = match rotation_against(None, Some(&[1, 2, 3, 4]), (6, &signing), 2).await {
            Ok(_) => anyhow::bail!("a legacy rotation must keep the legacy threshold"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("changes the signing threshold from 2 to 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A permission or onboarding flag is part of what a host may do, so a
    /// rotation that keeps the hosts must keep those too.
    #[tokio::test]
    async fn a_rotation_keeps_each_hosts_permission_and_onboarding_state() -> Result {
        let mut observer = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        observer.participants[0].permission = enums::ParticipantPermission::Observation as i32;
        let error = rotation_error(&observer, 2).await?;
        assert!(
            error.contains("changes a host's permission or onboarding state"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A party whose signing threshold differs from its namespace threshold
    /// keeps its signing threshold. A rotation must not move it to the
    /// namespace value.
    #[tokio::test]
    async fn a_rotation_keeps_the_signing_threshold_in_force() -> Result {
        let stub = StubHead {
            dns: Some((
                2,
                DecentralizedNamespaceDefinition {
                    threshold: 2,
                    ..namespace(&["a", "b", "c"])
                },
            )),
            p2p: Some((
                5,
                keyed_p2p(Some(SigningKeysWithThreshold {
                    threshold: 3,
                    ..keyed(&[1, 2, 3])
                }))?,
            )),
            legacy_keys: None,
        };
        let error = match check_rotation_against_head(
            &stub,
            &canton_id("party", 9)?,
            (6, &keyed_p2p(Some(keyed(&[1, 9, 3])))?),
            &utils::compute_fingerprint(&key_of(2)),
        )
        .await
        {
            Ok(_) => anyhow::bail!("lowering the signing threshold must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("changes the signing threshold from 3"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// p2 rotates its key 2 for key 9 on a party that p1, p2 and p3 host.
    fn claims_check(proposed: &[u8], claims: &[(&str, u8)]) -> Result<Fingerprint> {
        let members = ["p1", "p2", "p3"].map(str::to_string);
        check_rotation_claims(
            "p2",
            &utils::compute_fingerprint(&key_of(2)),
            &members,
            &fingerprints(proposed),
            &member_claims(claims),
        )
    }

    #[test]
    fn a_rotation_returns_the_one_key_no_other_member_holds() -> Result {
        let new_key = claims_check(&[1, 9, 3], &[("p1", 1), ("p2", 2), ("p3", 3)])?;
        assert_eq!(new_key, utils::compute_fingerprint(&key_of(9)));
        // A node that already recorded the new key accepts the same proposal.
        let again = claims_check(&[1, 9, 3], &[("p1", 1), ("p2", 9), ("p3", 3)])?;
        assert_eq!(again, new_key);
        Ok(())
    }

    /// Each refusal closes a way for a coordinator to take another member's
    /// place, or to keep a key that no current member holds.
    #[test]
    fn a_rotation_is_refused_unless_every_other_key_is_accounted_for() {
        for (proposed, claims, expected) in [
            (
                &[1u8, 9, 3][..],
                &[("p1", 1u8), ("p2", 2)][..],
                "no Daml key on record for p3",
            ),
            (
                &[1, 9, 3],
                &[("p1", 2), ("p2", 2), ("p3", 3)],
                "on record as p1's",
            ),
            (&[9, 3, 4], &[("p1", 1), ("p2", 2), ("p3", 3)], "drops"),
            (
                &[1, 9, 3, 4],
                &[("p1", 1), ("p2", 2), ("p3", 3)],
                "leaves 2 keys",
            ),
            (
                &[1, 9, 3],
                &[("p1", 3), ("p2", 2), ("p3", 3)],
                "on record for both",
            ),
            (
                &[1, 9, 3],
                &[("p1", 1), ("p2", 2), ("p3", 3), ("p4", 9)],
                "on record as p4's",
            ),
            (
                &[1, 9, 3],
                &[("p1", 1), ("p2", 5), ("p3", 3)],
                "retry that run",
            ),
            (
                &[1, 9, 3],
                &[("p1", 1), ("p3", 3)],
                "no Daml key on record for p2",
            ),
        ] {
            let error = match claims_check(proposed, claims) {
                Ok(key) => panic!("expected a refusal for {claims:?}, got {key}"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains(expected),
                "{claims:?}: expected {expected:?} in: {error}"
            );
        }
    }
}
