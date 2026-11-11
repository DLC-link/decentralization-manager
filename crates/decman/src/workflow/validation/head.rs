use std::collections::{BTreeMap, BTreeSet};

use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    DecentralizedNamespaceDefinition, PartyToKeyMapping, PartyToParticipant,
};

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    server::WorkflowKind,
    utils,
    workflow::topology,
};

use super::delta::{AllowedDelta, already_applied, check_dns_against_head, check_set_delta};

/// The party's head topology state as this node reads it.
pub(super) trait TopologyHead {
    async fn namespace_definition(
        &self,
        namespace: &str,
    ) -> Result<(u32, DecentralizedNamespaceDefinition)>;
    async fn p2p_mapping(&self, party: &CantonId) -> Result<(u32, PartyToParticipant)>;
    async fn party_to_key_mapping(&self, party: &CantonId) -> Result<Option<PartyToKeyMapping>>;
}

pub(super) struct SynchronizerHead<'a> {
    pub(super) config: &'a NodeConfig,
    pub(super) synchronizer_id: String,
}

impl TopologyHead for SynchronizerHead<'_> {
    async fn namespace_definition(
        &self,
        namespace: &str,
    ) -> Result<(u32, DecentralizedNamespaceDefinition)> {
        topology::fetch_namespace_definition_at_head(self.config, &self.synchronizer_id, namespace)
            .await
    }

    async fn p2p_mapping(&self, party: &CantonId) -> Result<(u32, PartyToParticipant)> {
        topology::fetch_p2p_mapping_at_head(self.config, &self.synchronizer_id, party).await
    }

    async fn party_to_key_mapping(&self, party: &CantonId) -> Result<Option<PartyToKeyMapping>> {
        topology::fetch_party_to_key_mapping(self.config, &self.synchronizer_id, party).await
    }
}

/// The member an accepted kick invitation names. Other runs name none.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Departing<'a> {
    /// The kicked participant, when the invitation names one.
    pub(super) participant: Option<&'a str>,
    /// The kicked owner key. Absent from coordinators that predate the field.
    pub(super) owner_key: Option<&'a str>,
    /// What this node has on record about the owner keys, to check
    /// `owner_key` against. A kick without records refuses a named key.
    pub(super) records: Option<&'a OwnerRecords>,
}

/// What this node has on record about who holds a party's owner keys.
#[derive(Clone, Debug, Default)]
pub(super) struct OwnerRecords {
    /// Each host's owner key, by participant uid.
    pub(super) host_keys: BTreeMap<String, String>,
    /// The participant that claimed each owner key, by owner key.
    pub(super) claims: BTreeMap<String, String>,
}

/// `new_member` is this node's `(namespace, daml)` fingerprints when it is the
/// new member. `departing` is the member a kick invitation names.
///
/// The host set must match the accepted invitation, but the coordinator writes
/// the invitation, so the host changes are pinned here too. Only add-party
/// hosts a participant the on-chain mapping does not, and only one. Only a
/// kick stops hosting one, and only the participant the invitation names.
///
/// A kick removes one owner key. It removes every signing key above one per
/// host left: the host's own key, or the key a member left behind when it
/// removed its own hosting entry. A kick that removes no host must name the
/// owner key, because the unchanged hosts give a peer nothing else to tie the
/// removal to.
pub(super) async fn check_against_head(
    head: &impl TopologyHead,
    kind: WorkflowKind,
    dec_party_id: &CantonId,
    (dns_serial, namespace_def): (u32, &DecentralizedNamespaceDefinition),
    (p2p_serial, mapping): (u32, &PartyToParticipant),
    new_member: Option<(&str, &str)>,
    departing: Departing<'_>,
) -> Result {
    let (head_dns_serial, current_def) = head
        .namespace_definition(&dec_party_id.namespace.to_hex())
        .await
        .map_err(|e| {
            anyhow::anyhow!("cannot read the current owner set of {dec_party_id}: {e:#}")
        })?;
    let (head_p2p_serial, current_p2p) = head.p2p_mapping(dec_party_id).await.map_err(|e| {
        anyhow::anyhow!("cannot read the current signing keys of {dec_party_id}: {e:#}")
    })?;

    let hosts_added = hosts_not_in(mapping, &current_p2p);
    let max_hosts_added = usize::from(kind == WorkflowKind::AddParty);
    if hosts_added.len() > max_hosts_added {
        anyhow::bail!(
            "{kind:?} proposal hosts {hosts_added:?}, which the on-chain mapping does not \
             host; it may add at most {max_hosts_added}"
        );
    }
    let departing = if kind == WorkflowKind::Kick {
        departing
    } else {
        Departing::default()
    };
    let hosts_removed = hosts_not_in(&current_p2p, mapping);
    if let Some(evicted) = hosts_removed
        .iter()
        .find(|uid| Some(**uid) != departing.participant)
    {
        anyhow::bail!(
            "{kind:?} proposal stops hosting {evicted}, which the accepted invitation does not \
             name as departing"
        );
    }
    let kicked_owner = departing.owner_key;
    if kind == WorkflowKind::Kick {
        // A DNS proposal at the head serial is the namespace already in force
        // (or refused below), so it removes nothing that needs pinning.
        if hosts_removed.is_empty() && kicked_owner.is_none() && dns_serial != head_dns_serial {
            anyhow::bail!(
                "Kick proposal removes no host, so it removes an owner that does not host the \
                 party, but the accepted invitation names no owner key to check it against"
            );
        }
        if let Some(owner) = kicked_owner {
            // The namespace in force is the one the kick leaves, so it must
            // no longer list the owner the kick removes.
            if dns_serial == head_dns_serial && current_def.owners.iter().any(|o| o == owner) {
                anyhow::bail!(
                    "Kick proposal keeps the namespace in force, which still lists the owner key \
                     {owner} that the accepted invitation removes"
                );
            }
            check_kicked_owner_key(
                owner,
                departing.participant,
                &current_p2p,
                &current_def.owners,
                departing.records.unwrap_or(&OwnerRecords::default()),
            )?;
        }
    }

    check_dns_against_head(
        kind,
        (dns_serial, namespace_def),
        (head_dns_serial, &current_def),
        new_member.map(|(namespace, _)| namespace),
        kicked_owner,
    )?;
    if already_applied(
        "P2P",
        (p2p_serial, mapping),
        (head_p2p_serial, &current_p2p),
    )? {
        return Ok(());
    }

    // A party onboarded before Canton 3.4 keeps its keys in a legacy
    // PartyToKeyMapping that still holds departed members' keys, so the
    // proposal adopts a subset of it rather than the whole set.
    //
    // The baseline has to match what the coordinator built its proposal from,
    // or the keys it restored read as additions and the peer refuses to sign.
    // A pre-1.10.0 add left parties half-migrated, holding one member's key
    // inline and the rest in the mapping, so a short inline set means the
    // baseline is both sources together.
    let members = current_p2p.participants.len();
    let inline = current_p2p
        .party_signing_keys
        .map(|keys| keys.keys)
        .unwrap_or_default();
    let (current_keys, legacy) = if inline.len() >= members && !inline.is_empty() {
        (inline, false)
    } else {
        let legacy_keys = head.party_to_key_mapping(dec_party_id).await?;
        if legacy_keys.is_none() && inline.is_empty() {
            anyhow::bail!(
                "{dec_party_id} carries neither inline signing keys nor a PartyToKeyMapping, \
                 so the proposed keys cannot be checked"
            );
        }
        let mut union = legacy_keys.map(|m| m.signing_keys).unwrap_or_default();
        let mut seen: BTreeSet<Fingerprint> =
            union.iter().map(utils::compute_fingerprint).collect();
        for key in inline {
            if seen.insert(utils::compute_fingerprint(&key)) {
                union.push(key);
            }
        }
        (union, true)
    };

    let proposed_keys = mapping
        .party_signing_keys
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("P2P proposal carries no party signing keys"))?;

    let mut allowed = AllowedDelta::of("party signing key", kind)?;
    allowed.added = new_member.map(|(_, daml)| daml);
    if kind == WorkflowKind::Kick {
        allowed.removals = current_keys
            .len()
            .saturating_sub(mapping.participants.len());
    }
    check_set_delta(
        "party signing key",
        kind,
        allowed,
        &current_keys
            .iter()
            .map(|key| utils::compute_fingerprint(key).into())
            .collect(),
        &proposed_keys
            .keys
            .iter()
            .map(|key| utils::compute_fingerprint(key).into())
            .collect::<Vec<String>>(),
        legacy,
    )
}

/// The hosts of `from` that `other` does not host.
fn hosts_not_in<'a>(from: &'a PartyToParticipant, other: &PartyToParticipant) -> Vec<&'a str> {
    let other: BTreeSet<&str> = other
        .participants
        .iter()
        .map(|p| p.participant_uid.as_str())
        .collect();
    from.participants
        .iter()
        .map(|p| p.participant_uid.as_str())
        .filter(|uid| !other.contains(uid))
        .collect()
}

/// The owner key a kick removes must belong to the member the invitation
/// names, and to no host that stays.
///
/// The coordinator picks the key from its own cache, so a peer checks it
/// against its own records. Canton counts only the namespace threshold, so a
/// host whose key goes cannot block the change alone. A host whose key this
/// node does not know could hold the named key, so the peer refuses until it
/// knows every host's key. A recorded key that is no longer an owner is stale
/// and counts as unknown, except for the departing host: a resumed kick may
/// have removed its key already.
fn check_kicked_owner_key(
    owner: &str,
    departing: Option<&str>,
    head: &PartyToParticipant,
    head_owners: &[String],
    records: &OwnerRecords,
) -> Result {
    let is_owner = |key: &str| head_owners.iter().any(|o| o == key);
    let mut departing_hosts = false;
    for host in &head.participants {
        let uid = host.participant_uid.as_str();
        let recorded = records.host_keys.get(uid).map(String::as_str);
        if Some(uid) == departing {
            departing_hosts = true;
            if recorded != Some(owner) {
                anyhow::bail!(
                    "Kick invitation removes owner key {owner} with host {uid}, but this node has \
                     {found} on record for {uid}",
                    found = recorded.unwrap_or("no owner key")
                );
            }
            continue;
        }
        match recorded.filter(|key| is_owner(key)) {
            Some(key) if key == owner => anyhow::bail!(
                "Kick invitation removes owner key {owner}, which belongs to host {uid}, and {uid} \
                 stays in the party"
            ),
            Some(_) => {}
            None => anyhow::bail!(
                "Cannot tell whether owner key {owner} belongs to host {uid}: this node has no \
                 current owner key on record for {uid}. Refresh /decentralized-parties on this \
                 node and retry"
            ),
        }
    }
    if let Some(participant) = departing
        && !departing_hosts
        && let Some(claimant) = records.claims.get(owner)
        && claimant != participant
    {
        anyhow::bail!(
            "Kick invitation removes owner key {owner} with {participant}, but this node has the \
             key on record as claimed by {claimant}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        SigningKeysWithThreshold, SigningPublicKey,
    };

    use super::*;
    use crate::workflow::validation::test_support::*;

    struct StubHead {
        dns: Option<(u32, DecentralizedNamespaceDefinition)>,
        p2p: Option<(u32, PartyToParticipant)>,
        legacy_keys: Option<Vec<SigningPublicKey>>,
    }

    impl TopologyHead for StubHead {
        async fn namespace_definition(
            &self,
            _namespace: &str,
        ) -> Result<(u32, DecentralizedNamespaceDefinition)> {
            self.dns
                .clone()
                .ok_or_else(|| anyhow::anyhow!("topology read unavailable"))
        }

        async fn p2p_mapping(&self, _party: &CantonId) -> Result<(u32, PartyToParticipant)> {
            self.p2p
                .clone()
                .ok_or_else(|| anyhow::anyhow!("topology read unavailable"))
        }

        async fn party_to_key_mapping(
            &self,
            _party: &CantonId,
        ) -> Result<Option<PartyToKeyMapping>> {
            Ok(self
                .legacy_keys
                .clone()
                .map(|signing_keys| PartyToKeyMapping {
                    signing_keys,
                    ..Default::default()
                }))
        }
    }

    fn keyed_p2p(keys: Option<SigningKeysWithThreshold>) -> Result<PartyToParticipant> {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        Ok(PartyToParticipant {
            party_signing_keys: keys,
            ..p2p("party", &hosts, 2)
        })
    }

    /// A change-threshold run on a three-member party whose on-chain keys are
    /// `head_keys` (inline) or, when those are absent, `legacy_keys`.
    async fn change_threshold_against(
        head_keys: Option<SigningKeysWithThreshold>,
        legacy_keys: Option<Vec<SigningPublicKey>>,
    ) -> Result {
        let def = namespace(&["a", "b", "c"]);
        let head = StubHead {
            dns: Some((2, def.clone())),
            p2p: Some((5, keyed_p2p(head_keys)?)),
            legacy_keys,
        };
        check_against_head(
            &head,
            WorkflowKind::ChangeThreshold,
            &canton_id("party", 9)?,
            (3, &def),
            (6, &keyed_p2p(Some(signing_keys(3, 2)))?),
            None,
            Departing::default(),
        )
        .await
    }

    #[tokio::test]
    async fn a_legacy_party_checks_the_keys_against_its_party_to_key_mapping() -> Result {
        let departed = signing_keys(4, 2).keys;
        change_threshold_against(None, Some(departed.clone())).await?;

        let error = match change_threshold_against(
            Some(SigningKeysWithThreshold {
                keys: departed,
                threshold: 2,
            }),
            None,
        )
        .await
        {
            Ok(()) => anyhow::bail!("an inline key set must not take the legacy rule"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 0"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_party_with_no_key_source_is_refused() -> Result {
        let error = match change_threshold_against(None, None).await {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("neither inline signing keys nor a PartyToKeyMapping"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A pre-1.10.0 add left parties holding one member's key inline and the
    /// rest in the legacy mapping. The peer baseline has to be both sources,
    /// or the keys the coordinator restores read as additions and every peer
    /// refuses to sign the repair.
    #[tokio::test]
    async fn a_half_migrated_party_checks_against_inline_and_legacy_together() -> Result {
        let all = signing_keys(3, 2).keys;
        let inline = SigningKeysWithThreshold {
            keys: all[2..].to_vec(),
            threshold: 1,
        };
        let legacy = all[..2].to_vec();

        change_threshold_against(Some(inline), Some(legacy)).await
    }

    #[tokio::test]
    async fn a_failed_topology_read_refuses_the_proposal() -> Result {
        let def = namespace(&["a", "b", "c"]);
        let proposal = keyed_p2p(Some(signing_keys(3, 2)))?;
        let party = canton_id("party", 9)?;
        let heads = [
            (
                StubHead {
                    dns: None,
                    p2p: Some((5, proposal.clone())),
                    legacy_keys: None,
                },
                "cannot read the current owner set",
            ),
            (
                StubHead {
                    dns: Some((2, def.clone())),
                    p2p: None,
                    legacy_keys: None,
                },
                "cannot read the current signing keys",
            ),
        ];
        for (head, expected) in heads {
            let error = match check_against_head(
                &head,
                WorkflowKind::ChangeThreshold,
                &party,
                (3, &def),
                (6, &proposal),
                None,
                Departing::default(),
            )
            .await
            {
                Ok(()) => anyhow::bail!("expected a refusal"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains(expected), "unexpected error: {error}");
        }
        Ok(())
    }

    /// A party whose namespace has an owner that hosts nothing, as an add-party
    /// that stopped after its DNS change leaves it. Hosts p1 and p2 hold keys
    /// 1 and 2; owner "c" hosts nothing and holds no key.
    fn owner_only_head() -> Result<StubHead> {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        Ok(StubHead {
            dns: Some((2, namespace(&["a", "b", "c"]))),
            p2p: Some((
                5,
                PartyToParticipant {
                    party_signing_keys: Some(signing_keys(2, 2)),
                    ..p2p("party", &hosts, 2)
                },
            )),
            legacy_keys: None,
        })
    }

    /// The owner keys a peer of [`owner_only_head`] has on record: p1 and p2
    /// reported "a" and "b", and p3 claimed "c".
    fn owner_records() -> Result<OwnerRecords> {
        Ok(OwnerRecords {
            host_keys: [
                (canton_id("p1", 1)?.to_string(), "a".to_string()),
                (canton_id("p2", 2)?.to_string(), "b".to_string()),
            ]
            .into(),
            claims: [("c".to_string(), canton_id("p3", 3)?.to_string())].into(),
        })
    }

    async fn owner_only_kick(
        proposed_owners: &[&str],
        proposed_keys: u8,
        kicked_owner: Option<&str>,
    ) -> Result {
        owner_only_kick_as(
            proposed_owners,
            proposed_keys,
            kicked_owner,
            None,
            &owner_records()?,
        )
        .await
    }

    /// A kick from [`owner_only_head`] that keeps both hosts, with the
    /// invitation naming `participant` and a peer holding `records`.
    async fn owner_only_kick_as(
        proposed_owners: &[&str],
        proposed_keys: u8,
        kicked_owner: Option<&str>,
        participant: Option<&str>,
        records: &OwnerRecords,
    ) -> Result {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(proposed_keys, 1)),
            ..p2p("party", &hosts, 1)
        };
        check_against_head(
            &owner_only_head()?,
            WorkflowKind::Kick,
            &canton_id("party", 9)?,
            (3, &namespace(proposed_owners)),
            (6, &proposal),
            None,
            Departing {
                participant,
                owner_key: kicked_owner,
                records: Some(records),
            },
        )
        .await
    }

    fn expect_refusal(outcome: Result, expected: &str) -> Result {
        let error = match outcome {
            Ok(()) => anyhow::bail!("expected a refusal that says {expected:?}"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(expected), "unexpected error: {error}");
        Ok(())
    }

    async fn owner_only_kick_error(
        proposed_owners: &[&str],
        proposed_keys: u8,
        kicked_owner: Option<&str>,
    ) -> Result<String> {
        match owner_only_kick(proposed_owners, proposed_keys, kicked_owner).await {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => Ok(error.to_string()),
        }
    }

    /// The owner-only kick removes the named owner and leaves the hosts and
    /// their keys as they are.
    #[tokio::test]
    async fn an_owner_only_kick_removes_the_named_owner_and_no_key() -> Result {
        owner_only_kick(&["a", "b"], 2, Some("c")).await
    }

    #[tokio::test]
    async fn an_owner_only_kick_refuses_to_remove_another_owner() -> Result {
        let error = owner_only_kick_error(&["a", "c"], 2, Some("c")).await?;
        assert!(
            error.contains("not the c the accepted invitation names"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// With no host leaving, nothing but the named key ties the removal to
    /// what the operator accepted.
    #[tokio::test]
    async fn an_owner_only_kick_needs_a_named_owner_key() -> Result {
        let error = owner_only_kick_error(&["a", "b"], 2, None).await?;
        assert!(
            error.contains("names no owner key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A member that removed its own hosting entry left its signing key in
    /// the party. Its kick takes that key out with its owner key.
    #[tokio::test]
    async fn an_owner_only_kick_drops_the_key_it_left_behind() -> Result {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let party = canton_id("party", 9)?;
        let head = StubHead {
            p2p: Some((
                5,
                PartyToParticipant {
                    party_signing_keys: Some(signing_keys(3, 2)),
                    ..p2p("party", &hosts, 2)
                },
            )),
            ..owner_only_head()?
        };
        let records = owner_records()?;
        let kick = async |keys: u8| {
            let proposal = PartyToParticipant {
                party_signing_keys: Some(signing_keys(keys, 1)),
                ..p2p("party", &hosts, 1)
            };
            check_against_head(
                &head,
                WorkflowKind::Kick,
                &party,
                (3, &namespace(&["a", "b"])),
                (6, &proposal),
                None,
                Departing {
                    owner_key: Some("c"),
                    records: Some(&records),
                    ..Departing::default()
                },
            )
            .await
        };

        kick(2).await?;
        let error = match kick(3).await {
            Ok(()) => anyhow::bail!("the leftover key must go"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// An owner that hosts nothing and left no key behind must not take a
    /// host's key with it.
    #[tokio::test]
    async fn an_owner_only_kick_refuses_to_drop_a_signing_key() -> Result {
        let error = owner_only_kick_error(&["a", "b"], 1, Some("c")).await?;
        assert!(
            error.contains("must remove exactly 0"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A coordinator built before #448 sends a kick already in force at the
    /// head serials, and names no owner key. Nothing is left to pin, so the
    /// peer signs it as before.
    #[tokio::test]
    async fn a_kick_already_in_force_needs_no_named_owner() -> Result {
        let head = owner_only_head()?;
        let (dns, p2p) = (
            head.dns
                .clone()
                .ok_or_else(|| anyhow::anyhow!("stub DNS"))?,
            head.p2p
                .clone()
                .ok_or_else(|| anyhow::anyhow!("stub P2P"))?,
        );
        check_against_head(
            &head,
            WorkflowKind::Kick,
            &canton_id("party", 9)?,
            (dns.0, &dns.1),
            (p2p.0, &p2p.1),
            None,
            Departing::default(),
        )
        .await
    }

    /// A host kick on a party that also has an owner hosting nothing still
    /// removes exactly the host's owner key and signing key.
    #[tokio::test]
    async fn a_host_kick_beside_an_owner_only_member_drops_one_key() -> Result {
        let (p1, p2) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let proposal = |keys: u8| PartyToParticipant {
            party_signing_keys: Some(signing_keys(keys, 1)),
            ..p2p("party", std::slice::from_ref(&p1), 1)
        };
        let party = canton_id("party", 9)?;
        let kick = |keys: u8| {
            let head = owner_only_head();
            let records = owner_records();
            let proposal = proposal(keys);
            let party = party.clone();
            let kicked = p2.to_string();
            async move {
                let records = records?;
                check_against_head(
                    &head?,
                    WorkflowKind::Kick,
                    &party,
                    (3, &namespace(&["a", "c"])),
                    (6, &proposal),
                    None,
                    Departing {
                        participant: Some(&kicked),
                        owner_key: Some("b"),
                        records: Some(&records),
                    },
                )
                .await
            }
        };
        kick(1).await?;
        let error = match kick(2).await {
            Ok(()) => anyhow::bail!("expected a refusal, {p2} kept its key"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A kick of host p2 from [`owner_only_head`], whose namespace in force is
    /// `head_owners` at serial 2. The invitation names `owner` as p2's key.
    async fn host_kick(
        head_owners: &[&str],
        (dns_serial, owners): (u32, &[&str]),
        owner: &str,
        records: &OwnerRecords,
    ) -> Result {
        let p1 = canton_id("p1", 1)?;
        let kicked = canton_id("p2", 2)?.to_string();
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(1, 1)),
            ..p2p("party", std::slice::from_ref(&p1), 1)
        };
        let head = StubHead {
            dns: Some((2, namespace(head_owners))),
            ..owner_only_head()?
        };
        check_against_head(
            &head,
            WorkflowKind::Kick,
            &canton_id("party", 9)?,
            (dns_serial, &namespace(owners)),
            (6, &proposal),
            None,
            Departing {
                participant: Some(&kicked),
                owner_key: Some(owner),
                records: Some(records),
            },
        )
        .await
    }

    /// A DNS proposal equal to the namespace in force would leave p2's key
    /// "b" in it while p2 stops hosting the party.
    #[tokio::test]
    async fn a_host_kick_refuses_to_keep_the_kicked_owner_key() -> Result {
        let head = ["a", "b", "c"];
        expect_refusal(
            host_kick(&head, (2, &head), "b", &owner_records()?).await,
            "still lists the owner key b",
        )
    }

    /// A kick resumed after its DNS change sends the namespace in force,
    /// which no longer lists p2's key.
    #[tokio::test]
    async fn a_resumed_host_kick_accepts_the_namespace_in_force() -> Result {
        let head = ["a", "c"];
        host_kick(&head, (2, &head), "b", &owner_records()?).await
    }

    /// Canton counts only the namespace threshold, so p2's own refusal does
    /// not stop a kick that names p2's key "b" as an owner-only member's.
    #[tokio::test]
    async fn an_owner_only_kick_refuses_the_key_of_a_staying_host() -> Result {
        expect_refusal(
            owner_only_kick(&["a", "c"], 2, Some("b")).await,
            "which belongs to host p2::",
        )
    }

    /// A host whose current key this node does not know may hold the named
    /// key. A recorded key that is no longer an owner is stale.
    #[tokio::test]
    async fn an_owner_only_kick_needs_every_hosts_current_key() -> Result {
        let p2 = canton_id("p2", 2)?.to_string();
        for p2_key in [None, Some("rotated")] {
            let mut records = owner_records()?;
            records.host_keys.remove(&p2);
            if let Some(key) = p2_key {
                records.host_keys.insert(p2.clone(), key.to_string());
            }
            expect_refusal(
                owner_only_kick_as(&["a", "b"], 2, Some("c"), None, &records).await,
                "no current owner key on record for p2::",
            )?;
        }
        Ok(())
    }

    /// The participant and the owner key the invitation names must belong
    /// together in this node's records.
    #[tokio::test]
    async fn a_host_kick_needs_the_hosts_recorded_key() -> Result {
        let head = ["a", "b", "c"];
        expect_refusal(
            host_kick(&head, (3, &["a", "b"]), "c", &owner_records()?).await,
            "this node has b on record for p2::",
        )?;
        let mut unrecorded = owner_records()?;
        unrecorded
            .host_keys
            .remove(&canton_id("p2", 2)?.to_string());
        expect_refusal(
            host_kick(&head, (3, &["a", "c"]), "b", &unrecorded).await,
            "this node has no owner key on record for p2::",
        )
    }

    #[tokio::test]
    async fn an_owner_only_kick_matches_the_recorded_claimant() -> Result {
        let (p3, p4) = (
            canton_id("p3", 3)?.to_string(),
            canton_id("p4", 4)?.to_string(),
        );
        let records = owner_records()?;
        owner_only_kick_as(&["a", "b"], 2, Some("c"), Some(&p3), &records).await?;
        expect_refusal(
            owner_only_kick_as(&["a", "b"], 2, Some("c"), Some(&p4), &records).await,
            "on record as claimed by p3::",
        )
    }

    /// A change of threshold on a party with an owner that hosts nothing keeps
    /// every owner and every key.
    #[tokio::test]
    async fn a_change_threshold_beside_an_owner_only_member_is_accepted() -> Result {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(2, 1)),
            ..p2p("party", &hosts, 1)
        };
        check_against_head(
            &owner_only_head()?,
            WorkflowKind::ChangeThreshold,
            &canton_id("party", 9)?,
            (3, &namespace(&["a", "b", "c"])),
            (6, &proposal),
            None,
            Departing::default(),
        )
        .await
    }

    /// A proposal from `kind` that hosts `hosts` with one key each. The party
    /// is [`owner_only_head`] with owner "c" still holding key 3, so a new
    /// host can take that key over and no key check catches it.
    async fn host_set_change(kind: WorkflowKind, hosts: &[CantonId], owners: &[&str]) -> Result {
        let head = StubHead {
            p2p: Some((
                5,
                PartyToParticipant {
                    party_signing_keys: Some(signing_keys(3, 2)),
                    ..p2p("party", &[canton_id("p1", 1)?, canton_id("p2", 2)?], 2)
                },
            )),
            ..owner_only_head()?
        };
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(u8::try_from(hosts.len())?, 1)),
            ..p2p("party", hosts, 1)
        };
        check_against_head(
            &head,
            kind,
            &canton_id("party", 9)?,
            (3, &namespace(owners)),
            (6, &proposal),
            None,
            Departing::default(),
        )
        .await
    }

    fn expect_host_refusal(outcome: Result, what: &str) -> Result {
        let error = match outcome {
            Ok(()) => anyhow::bail!("{what} must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("which the on-chain mapping does not host"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A kick that drops p2 and hosts p3 instead would hand p3 the party,
    /// even when the invitation lists p3.
    #[tokio::test]
    async fn a_kick_refuses_to_host_a_new_participant() -> Result {
        let swapped = [canton_id("p1", 1)?, canton_id("p3", 3)?];
        expect_host_refusal(
            host_set_change(WorkflowKind::Kick, &swapped, &["a", "c"]).await,
            "a kick that hosts p3",
        )
    }

    #[tokio::test]
    async fn a_change_threshold_refuses_to_host_a_new_participant() -> Result {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        expect_host_refusal(
            host_set_change(WorkflowKind::ChangeThreshold, &hosts, &["a", "b", "c"]).await,
            "a change of threshold that hosts p3",
        )
    }

    fn expect_eviction_refusal(outcome: Result, what: &str) -> Result {
        let error = match outcome {
            Ok(()) => anyhow::bail!("{what} must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("stops hosting p2::"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A kick of owner "c", which hosts nothing, that also stops hosting p2.
    /// The invitation lists only p1, so p2's absence matches it, but the
    /// invitation names c's member as departing, not p2.
    #[tokio::test]
    async fn a_kick_stops_hosting_only_the_participant_it_names() -> Result {
        let p1 = canton_id("p1", 1)?;
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(1, 1)),
            ..p2p("party", std::slice::from_ref(&p1), 1)
        };
        let claimant = canton_id("p3", 3)?.to_string();
        let records = owner_records()?;
        for participant in [None, Some(claimant.as_str())] {
            expect_eviction_refusal(
                check_against_head(
                    &owner_only_head()?,
                    WorkflowKind::Kick,
                    &canton_id("party", 9)?,
                    (3, &namespace(&["a", "b"])),
                    (6, &proposal),
                    None,
                    Departing {
                        participant,
                        owner_key: Some("c"),
                        records: Some(&records),
                    },
                )
                .await,
                "a kick of c that drops p2",
            )?;
        }
        Ok(())
    }

    /// A legacy party's keys allow any removal, so only the host check stops
    /// a change of threshold from dropping p3.
    #[tokio::test]
    async fn a_change_threshold_refuses_to_stop_hosting_a_participant() -> Result {
        let def = namespace(&["a", "b", "c"]);
        let head = StubHead {
            dns: Some((2, def.clone())),
            p2p: Some((5, keyed_p2p(None)?)),
            legacy_keys: Some(signing_keys(3, 2).keys),
        };
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(2, 2)),
            ..p2p("party", &hosts, 2)
        };
        let error = match check_against_head(
            &head,
            WorkflowKind::ChangeThreshold,
            &canton_id("party", 9)?,
            (3, &def),
            (6, &proposal),
            None,
            Departing::default(),
        )
        .await
        {
            Ok(()) => anyhow::bail!("a change of threshold that drops p3 must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("stops hosting p3::"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_add_party_hosts_at_most_one_new_participant() -> Result {
        let one = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        let two = [one.as_slice(), &[canton_id("p4", 4)?]].concat();
        let owners = ["a", "b", "c", "d"];
        host_set_change(WorkflowKind::AddParty, &one, &owners).await?;
        expect_host_refusal(
            host_set_change(WorkflowKind::AddParty, &two, &owners).await,
            "an add-party that hosts p3 and p4",
        )
    }
}
