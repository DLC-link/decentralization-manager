use std::collections::BTreeSet;

use canton_proto_rs::com::digitalasset::canton::protocol::v30::{PartyToParticipant, enums};

use super::expectations::PeerExpectations;
use crate::{canton_id::CantonId, error::Result, server::WorkflowKind};

impl PeerExpectations {
    /// The hosting-participant set must be exactly the accepted members, and
    /// this node must be one of them.
    pub(super) fn check_p2p_membership(&self, mapping: &PartyToParticipant) -> Result {
        let mut hosts = BTreeSet::new();
        for participant in &mapping.participants {
            let id = CantonId::parse(&participant.participant_uid)?;
            if !hosts.insert(id.clone()) {
                anyhow::bail!("P2P proposal lists participant {id} twice");
            }
            // Every proposal this tool builds hosts at Confirmation. Submission
            // would let that participant submit for the party directly, so a
            // silent upgrade is a change to who can act, not a detail.
            if participant.permission != enums::ParticipantPermission::Confirmation as i32 {
                anyhow::bail!(
                    "P2P proposal hosts {id} with permission {permission}, not Confirmation",
                    permission = enums::ParticipantPermission::try_from(participant.permission)
                        .map(|p| p.as_str_name().to_string())
                        .unwrap_or_else(|_| format!("unknown ({})", participant.permission))
                );
            }
        }

        if hosts != self.members {
            let extra: Vec<String> = hosts
                .difference(&self.members)
                .map(CantonId::to_string)
                .collect();
            let missing: Vec<String> = self
                .members
                .difference(&hosts)
                .map(CantonId::to_string)
                .collect();
            anyhow::bail!(
                "P2P proposal host set does not match the accepted invitation \
                 (unexpected: {extra:?}, missing: {missing:?})"
            );
        }
        if !hosts.contains(&self.self_id) {
            anyhow::bail!(
                "P2P proposal does not host this node ({self_id})",
                self_id = self.self_id
            );
        }
        if let Some(kicked) = &self.kicked_participant
            && hosts.contains(kicked)
        {
            anyhow::bail!("P2P proposal still hosts {kicked}, the participant being kicked");
        }
        if let Some(added) = &self.new_participant
            && !hosts.contains(added)
        {
            anyhow::bail!("P2P proposal does not host {added}, the participant being added");
        }
        Ok(())
    }

    /// The onboarding marker decides who is still importing an ACS. Dropping it
    /// activates the new member before its import finishes; moving it to an
    /// existing host suspends that host instead. So exactly the accepted new
    /// participant carries it, and on a run that adds nobody, no one does.
    ///
    /// Not applied to the clearing proposal, whose purpose is to remove the
    /// marker — [`check_clear_onboarding`](Self::check_clear_onboarding)
    /// requires its absence instead.
    pub(super) fn check_onboarding_markers(&self, mapping: &PartyToParticipant) -> Result {
        for participant in &mapping.participants {
            let id = CantonId::parse(&participant.participant_uid)?;
            let expected = self.new_participant.as_ref() == Some(&id);
            if participant.onboarding.is_some() != expected {
                anyhow::bail!(
                    "P2P proposal marks {id} as onboarding: {found}, but the accepted \
                     invitation calls for {expected}",
                    found = participant.onboarding.is_some()
                );
            }
        }
        Ok(())
    }

    /// A `PartyToParticipant` mapping carries two thresholds: how many hosting
    /// participants must confirm, and how many of the party's signing keys
    /// authorize a transaction for it. Both are pinned — leaving the signing
    /// threshold unchecked would let a coordinator raise a confirmation
    /// threshold the operator agreed to while quietly dropping the signing one
    /// to 1.
    ///
    /// The confirmation threshold is held to the accepted confirmation
    /// threshold, and the signing one to the accepted namespace threshold.
    ///
    /// The size of the key set is pinned with them, because a threshold only
    /// means what the operator agreed to if the set it counts over does. Two
    /// of three member keys is the accepted 2; two of a set that still holds
    /// a kicked member's key is not (#428).
    pub(super) fn check_p2p_thresholds(&self, mapping: &PartyToParticipant) -> Result {
        self.check_threshold_against(
            "confirmation",
            i32::try_from(mapping.threshold)?,
            self.members.len(),
            self.confirmation_threshold.or(self.threshold),
        )?;

        // Signing a mapping with no party signing keys does not merely leave a
        // threshold unchecked — it submits a party that can no longer
        // authorize anything. Every mapping this tool produces carries them
        // (onboarding sets them, and the kick / add-party / change-threshold /
        // clearing proposals all derive from the current on-chain mapping), so
        // an absent set is a stripped proposal.
        let Some(signing_keys) = &mapping.party_signing_keys else {
            anyhow::bail!(
                "P2P proposal carries no party signing keys, which would leave the party \
                 unable to authorize anything"
            );
        };

        self.check_threshold(
            "signing",
            i32::try_from(signing_keys.threshold)?,
            self.members.len(),
        )?;

        // Every member contributes exactly one Daml key, so the counts match.
        // A peer cannot tell whose key is whose — the mapping records no owner
        // per key — but it can tell that one too many is present, which is
        // what a kick that forgot to drop the departing member's key looks
        // like from here.
        if signing_keys.keys.len() != self.members.len() {
            anyhow::bail!(
                "P2P proposal carries {keys} party signing key(s) for {members} member(s); \
                 each member contributes exactly one, so a surplus key would let a \
                 non-member keep authorizing for the party",
                keys = signing_keys.keys.len(),
                members = self.members.len()
            );
        }
        Ok(())
    }

    /// The threshold must be the one the invitation advertised, and in any
    /// case a sane value for the owner count — a threshold of 0 or one above
    /// the owner count would either need no signatures or deadlock the party.
    ///
    /// `kind` names which threshold is being checked. A mapping carries a
    /// confirmation and a signing threshold and both come through here, so a
    /// message that named neither read as though the coordinator had ignored
    /// the operator's input when in fact the other one was wrong.
    pub(super) fn check_threshold(&self, kind: &str, threshold: i32, owner_count: usize) -> Result {
        self.check_threshold_against(kind, threshold, owner_count, self.threshold)
    }

    /// [`check_threshold`](Self::check_threshold) against an `expected`
    /// value other than the namespace threshold.
    fn check_threshold_against(
        &self,
        kind: &str,
        threshold: i32,
        owner_count: usize,
        expected: Option<i32>,
    ) -> Result {
        let owner_count = i32::try_from(owner_count)?;
        if !(1..=owner_count).contains(&threshold) {
            anyhow::bail!("proposed {kind} threshold {threshold} is outside 1..={owner_count}");
        }
        match expected {
            Some(expected) if expected != threshold => {
                anyhow::bail!(
                    "proposed {kind} threshold {threshold} differs from the accepted {expected}"
                )
            }
            Some(_) => Ok(()),
            // Only the onboarding invite's threshold is optional on the wire
            // (`Option<i32>`, absent from coordinators that predate it). Kick,
            // add-party and change-threshold all require it, so an absent one
            // there would let the coordinator pick any in-range value.
            None if self.kind == WorkflowKind::Onboarding => {
                tracing::warn!(
                    "accepted onboarding invitation carried no threshold; only the range \
                     check was applied to the proposed {kind} threshold {threshold}"
                );
                Ok(())
            }
            None => anyhow::bail!(
                "accepted {workflow:?} invitation carries no threshold, so the proposed \
                 {kind} threshold {threshold} cannot be checked against it",
                workflow = self.kind
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
        enums, party_to_participant::hosting_participant,
    };

    use super::*;
    use crate::workflow::validation::test_support::*;

    #[test]
    fn accepts_a_matching_host_set() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.check_p2p_membership(&p2p("dec::x", &[a, b], 2))
    }

    /// The attack this whole module exists for: a coordinator proposing a
    /// party hosted somewhere the operator never agreed to.
    #[test]
    fn rejects_an_unexpected_host() -> Result {
        let (a, b, mallory) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("evil", 9)?,
        );
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a, b, mallory], 2))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_dropping_a_member() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b], a.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a], 1))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_proposal_that_does_not_host_this_node() -> Result {
        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let expectations = expectations(vec![b.clone(), c.clone()], a);
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[b, c], 2))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_kicked_participant_that_is_still_hosted() -> Result {
        let (a, b, kicked) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let mut expectations = expectations(vec![a.clone(), b.clone(), kicked.clone()], a.clone());
        expectations.kicked_participant = Some(kicked.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a, b, kicked], 2))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_duplicated_host() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a.clone(), a], 1))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_threshold_the_invitation_did_not_advertise() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.check_threshold("namespace", 1, 3).is_err());
        Ok(())
    }

    /// A threshold of zero would let any single signer act for the party.
    #[test]
    fn rejects_an_out_of_range_threshold() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.threshold = Some(0);
        assert!(expectations.check_threshold("namespace", 0, 3).is_err());
        expectations.threshold = Some(4);
        assert!(expectations.check_threshold("namespace", 4, 3).is_err());
        Ok(())
    }

    #[test]
    fn accepts_the_advertised_threshold() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        expectations.check_threshold("namespace", 2, 3)
    }

    /// The signing threshold decides how many member keys authorize a
    /// transaction for the party. Dropping it to 1 while the hosting
    /// threshold looks right would hand any single member the party.
    #[test]
    fn rejects_a_weakened_party_signing_threshold() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        // A full key set, so the failure is the threshold and not the count.
        mapping.party_signing_keys = Some(signing_keys(2, 1));
        assert!(expectations.check_p2p_thresholds(&mapping).is_err());
        Ok(())
    }

    /// A stripped `party_signing_keys` is not a missing threshold — it is a
    /// mapping that would leave the party unable to authorize anything.
    /// Submission permission lets a participant submit for the party directly.
    /// Every proposal this tool builds hosts at Confirmation, so an upgrade is
    /// a change to who can act, not a formatting detail.
    #[test]
    fn rejects_a_host_upgraded_to_submission() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        mapping.participants[1].permission = enums::ParticipantPermission::Submission as i32;
        assert!(expectations.check_p2p_membership(&mapping).is_err());
        Ok(())
    }

    /// Dropping the marker activates the new member before its ACS import
    /// finishes; moving it to an existing host suspends that host instead.
    #[test]
    fn rejects_a_misplaced_onboarding_marker() -> Result {
        let (a, b, joining) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let mut expectations = expectations(vec![a.clone(), b.clone(), joining.clone()], a.clone());
        expectations.new_participant = Some(joining.clone());

        // Marker on the wrong host.
        let mut moved = p2p("dec::x", &[a.clone(), b.clone(), joining.clone()], 2);
        moved.participants[1].onboarding = Some(hosting_participant::Onboarding::default());
        assert!(expectations.check_onboarding_markers(&moved).is_err());

        // Marker missing entirely.
        let dropped = p2p("dec::x", &[a.clone(), b.clone(), joining.clone()], 2);
        assert!(expectations.check_onboarding_markers(&dropped).is_err());

        // Marker exactly where the invitation puts it.
        let mut correct = p2p("dec::x", &[a, b, joining], 2);
        correct.participants[2].onboarding = Some(hosting_participant::Onboarding::default());
        expectations.check_onboarding_markers(&correct)
    }

    /// A run that adds nobody must not introduce an onboarding marker either.
    #[test]
    fn rejects_an_onboarding_marker_on_a_run_that_adds_nobody() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        assert!(expectations.new_participant.is_none());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        mapping.participants[0].onboarding = Some(hosting_participant::Onboarding::default());
        assert!(expectations.check_onboarding_markers(&mapping).is_err());
        Ok(())
    }

    /// Only the onboarding invite's threshold is optional on the wire. For the
    /// other kinds an absent one would let the coordinator pick any in-range
    /// value.
    #[test]
    fn rejects_an_absent_threshold_except_for_onboarding() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.threshold = None;

        expectations.kind = WorkflowKind::Kick;
        assert!(expectations.check_threshold("namespace", 2, 3).is_err());
        expectations.kind = WorkflowKind::AddParty;
        assert!(expectations.check_threshold("namespace", 2, 3).is_err());

        expectations.kind = WorkflowKind::Onboarding;
        expectations.check_threshold("namespace", 2, 3)
    }

    #[test]
    fn rejects_a_proposal_with_no_party_signing_keys() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mapping = p2p("dec::x", &[a, b], 2);
        assert!(mapping.party_signing_keys.is_none());
        assert!(expectations.check_p2p_thresholds(&mapping).is_err());
        Ok(())
    }

    #[test]
    fn accepts_matching_hosting_and_signing_thresholds() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        mapping.party_signing_keys = Some(signing_keys(2, 2));
        expectations.check_p2p_thresholds(&mapping)
    }

    /// The bug in #428: kick dropped the departing member from
    /// `participants` but carried `party_signing_keys` over verbatim, leaving
    /// its Daml key still counting towards the party's signing threshold. A
    /// peer cannot tell whose key is whose — the mapping records no owner per
    /// key — but one key too many for the member set is exactly what that
    /// mistake looks like from here.
    #[test]
    fn rejects_a_surplus_party_signing_key() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        // Two remaining members, three keys — the kicked member's is still in.
        mapping.party_signing_keys = Some(signing_keys(3, 2));

        let error = expectations
            .check_p2p_thresholds(&mapping)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("3 party signing key(s) for 2 member(s)"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// The other half of #428: the operator asked for 2, the coordinator moved
    /// the hosting threshold and left the signing one at 3, and the refusal
    /// said only "proposed threshold 3 differs from the accepted 2" — which
    /// reads as though the coordinator had ignored the operator's input. The
    /// message has to say which of the two thresholds is wrong.
    #[test]
    fn names_the_signing_threshold_when_only_it_is_stale() -> Result {
        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let expectations = expectations(vec![a.clone(), b.clone(), c.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b, c], 2);
        mapping.party_signing_keys = Some(signing_keys(3, 3));

        let error = expectations
            .check_p2p_thresholds(&mapping)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("signing threshold 3 differs from the accepted 2"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    fn three_members_at(threshold: i32, confirmation: Option<i32>) -> Result<PeerExpectations> {
        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let mut expectations = expectations(vec![a.clone(), b, c], a);
        expectations.threshold = Some(threshold);
        expectations.confirmation_threshold = confirmation;
        Ok(expectations)
    }

    fn three_host_mapping(confirmation: u32, signing: u32) -> Result<PartyToParticipant> {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        let mut mapping = p2p("dec::x", &hosts, confirmation);
        mapping.party_signing_keys = Some(signing_keys(3, signing));
        Ok(mapping)
    }

    /// An invitation that named two different thresholds is signed when the
    /// proposal carries exactly those two, each in its own place.
    #[test]
    fn accepts_the_two_thresholds_the_invitation_named() -> Result {
        three_members_at(1, Some(3))?.check_p2p_thresholds(&three_host_mapping(3, 1)?)
    }

    /// Each threshold is held to its own accepted value, so neither can be
    /// moved, nor the two swapped.
    #[test]
    fn refuses_thresholds_the_invitation_did_not_name() -> Result {
        let expectations = three_members_at(1, Some(3))?;
        for (confirmation, signing, expected) in [
            (2, 1, "confirmation threshold 2 differs from the accepted 3"),
            (3, 3, "signing threshold 3 differs from the accepted 1"),
            (1, 3, "confirmation threshold 1 differs from the accepted 3"),
        ] {
            let error = expectations
                .check_p2p_thresholds(&three_host_mapping(confirmation, signing)?)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
        Ok(())
    }

    /// An invitation without a confirmation threshold comes from a
    /// coordinator that always proposes one value, so a differing pair is
    /// refused. A peer built before the field applies the same rule to every
    /// invitation, so it refuses a differing pair too.
    #[test]
    fn an_invitation_without_a_confirmation_threshold_refuses_a_differing_pair() -> Result {
        let expectations = three_members_at(1, None)?;
        expectations.check_p2p_thresholds(&three_host_mapping(1, 1)?)?;

        let error = expectations
            .check_p2p_thresholds(&three_host_mapping(3, 1)?)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("confirmation threshold 3 differs from the accepted 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }
}
