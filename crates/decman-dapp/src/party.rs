use std::collections::{BTreeSet, HashSet};

use common::fingerprint::{compute_decentralized_namespace, fingerprint_from_public_key};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use crate::{Error, signature::parse_public_key};

/// Canton keeps the last 185 characters of a party id for the name.
const MAX_HINT_LENGTH: usize = 185;
/// The dApp creates parties that more than one owner controls.
const MIN_OWNERS: usize = 2;
/// The most namespace owners that the Ledger API's `AllocateExternalParty`
/// accepts for a decentralized party.
pub const MAX_OWNERS: usize = 10;
/// Canton puts this between the party name and the namespace.
const PARTY_ID_DELIMITER: &str = "::";

/// A new party as its creator proposes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartyProposal {
    pub party_hint: String,
    pub owners: Vec<ProposedOwner>,
    /// Owner keys needed to transact as the party.
    pub signing_threshold: u32,
    /// Owners needed to change the topology. Every party with the same owners
    /// shares the namespace, and so this threshold.
    pub namespace_threshold: u32,
    /// Hosts needed to confirm each transaction.
    pub confirmation_threshold: u32,
    #[serde(default)]
    pub covalidators: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposedOwner {
    /// The owner's raw 32-byte Ed25519 public key, hex-encoded.
    pub public_key: String,
    /// The participant id of the node that hosts the party for this owner.
    pub participant: String,
}

/// A checked proposal and the party id that its owners create.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyPlan {
    pub party_id: String,
    pub namespace: String,
    /// Sorted by namespace.
    pub owners: Vec<PlannedOwner>,
    /// The owners' participants and the covalidators, sorted and deduplicated.
    pub hosts: Vec<String>,
    pub signing_threshold: u32,
    pub namespace_threshold: u32,
    pub confirmation_threshold: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedOwner {
    pub namespace: String,
    pub public_key: String,
    pub participant: String,
    #[serde(skip)]
    pub key: VerifyingKey,
}

/// Checks `proposal` and derives the party id that its owners create.
///
/// The party namespace is the decentralized namespace of the owners' key
/// namespaces, as the server computes it.
pub fn plan_party(proposal: &PartyProposal) -> Result<PartyPlan, Error> {
    check_hint(&proposal.party_hint)?;
    if proposal.owners.len() < MIN_OWNERS {
        return Err(Error::TooFewOwners {
            min: MIN_OWNERS,
            got: proposal.owners.len(),
        });
    }
    if proposal.owners.len() > MAX_OWNERS {
        return Err(Error::TooManyOwners {
            max: MAX_OWNERS,
            got: proposal.owners.len(),
        });
    }

    let mut owners = proposal
        .owners
        .iter()
        .enumerate()
        .map(|(index, owner)| plan_owner(index + 1, owner))
        .collect::<Result<Vec<_>, _>>()?;
    owners.sort_by(|a, b| a.namespace.cmp(&b.namespace));
    if owners
        .windows(2)
        .any(|pair| pair[0].namespace == pair[1].namespace)
    {
        return Err(Error::DuplicateOwner);
    }
    check_threshold("signing", proposal.signing_threshold, owners.len())?;
    check_threshold("namespace", proposal.namespace_threshold, owners.len())?;

    let hosts: BTreeSet<String> = owners
        .iter()
        .map(|owner| owner.participant.clone())
        .chain(
            proposal
                .covalidators
                .iter()
                .map(|covalidator| covalidator.trim())
                .filter(|covalidator| !covalidator.is_empty())
                .map(str::to_owned),
        )
        .collect();
    check_threshold("confirmation", proposal.confirmation_threshold, hosts.len())?;

    let owner_namespaces: HashSet<String> =
        owners.iter().map(|owner| owner.namespace.clone()).collect();
    let namespace = compute_decentralized_namespace(&owner_namespaces);
    Ok(PartyPlan {
        party_id: format!("{}{PARTY_ID_DELIMITER}{namespace}", proposal.party_hint),
        namespace,
        owners,
        hosts: hosts.into_iter().collect(),
        signing_threshold: proposal.signing_threshold,
        namespace_threshold: proposal.namespace_threshold,
        confirmation_threshold: proposal.confirmation_threshold,
    })
}

fn plan_owner(number: usize, owner: &ProposedOwner) -> Result<PlannedOwner, Error> {
    let key =
        parse_public_key(owner.public_key.trim()).map_err(|_| Error::InvalidOwnerKey { number })?;
    let participant = owner.participant.trim();
    if participant.is_empty() {
        return Err(Error::MissingParticipant { number });
    }
    Ok(PlannedOwner {
        namespace: fingerprint_from_public_key(key.as_bytes()).to_string(),
        public_key: hex::encode(key.as_bytes()),
        participant: participant.to_owned(),
        key,
    })
}

/// Applies Canton's rules for the name part of a party id.
fn check_hint(hint: &str) -> Result<(), Error> {
    if hint.is_empty() {
        return Err(Error::EmptyHint);
    }
    if hint.chars().count() > MAX_HINT_LENGTH {
        return Err(Error::HintTooLong {
            max: MAX_HINT_LENGTH,
        });
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | ':');
    if !hint.chars().all(allowed) {
        return Err(Error::InvalidHintCharacter);
    }
    if hint.contains(PARTY_ID_DELIMITER) {
        return Err(Error::HintContainsDelimiter);
    }
    // Canton splits a party id at its first "::", so "a:" + "::ns" would
    // read as the name "a" and the namespace ":ns".
    if hint.ends_with(':') {
        return Err(Error::HintEndsWithColon);
    }
    Ok(())
}

fn check_threshold(kind: &'static str, threshold: u32, max: usize) -> Result<(), Error> {
    let in_range = usize::try_from(threshold).is_ok_and(|t| (1..=max).contains(&t));
    if in_range {
        Ok(())
    } else {
        Err(Error::ThresholdOutOfRange {
            kind,
            max,
            got: threshold,
        })
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;

    fn key(seed: u8) -> String {
        hex::encode(
            SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .as_bytes(),
        )
    }

    fn proposal() -> PartyProposal {
        PartyProposal {
            party_hint: "treasury".to_owned(),
            owners: vec![
                ProposedOwner {
                    public_key: key(1),
                    participant: "participant-a".to_owned(),
                },
                ProposedOwner {
                    public_key: key(2),
                    participant: "participant-b".to_owned(),
                },
            ],
            signing_threshold: 2,
            namespace_threshold: 2,
            confirmation_threshold: 2,
            covalidators: vec!["covalidator-c".to_owned()],
        }
    }

    #[test]
    fn party_id_uses_the_owners_decentralized_namespace() -> Result<(), Error> {
        let plan = plan_party(&proposal())?;
        let owner_namespaces: HashSet<String> = [1, 2]
            .into_iter()
            .map(|seed| {
                let key = SigningKey::from_bytes(&[seed; 32]).verifying_key();
                fingerprint_from_public_key(key.as_bytes()).to_string()
            })
            .collect();
        let namespace = compute_decentralized_namespace(&owner_namespaces);
        assert_eq!(plan.namespace, namespace);
        assert_eq!(plan.party_id, format!("treasury::{namespace}"));
        Ok(())
    }

    #[test]
    fn hosts_are_the_owner_participants_and_covalidators() -> Result<(), Error> {
        let mut proposal = proposal();
        proposal.covalidators = vec![
            " covalidator-c ".to_owned(),
            "participant-a".to_owned(),
            "  ".to_owned(),
        ];
        let plan = plan_party(&proposal)?;
        assert_eq!(
            plan.hosts,
            ["covalidator-c", "participant-a", "participant-b"]
        );
        Ok(())
    }

    #[test]
    fn owner_order_does_not_change_the_plan() -> Result<(), Error> {
        let mut reversed = proposal();
        reversed.owners.reverse();
        let plan = plan_party(&proposal())?;
        let reversed_plan = plan_party(&reversed)?;
        assert_eq!(plan.party_id, reversed_plan.party_id);
        assert_eq!(plan.owners[0].namespace, reversed_plan.owners[0].namespace);
        Ok(())
    }

    #[test]
    fn rejects_a_single_owner() {
        let mut proposal = proposal();
        proposal.owners.truncate(1);
        assert!(matches!(
            plan_party(&proposal),
            Err(Error::TooFewOwners { min: 2, got: 1 })
        ));
    }

    #[test]
    fn accepts_ten_owners_and_rejects_eleven() {
        let mut proposal = proposal();
        proposal.owners = (1..=10)
            .map(|seed| ProposedOwner {
                public_key: key(seed),
                participant: format!("participant-{seed}"),
            })
            .collect();
        proposal.confirmation_threshold = 1;
        assert!(plan_party(&proposal).is_ok());

        proposal.owners.push(ProposedOwner {
            public_key: key(11),
            participant: "participant-11".to_owned(),
        });
        assert!(matches!(
            plan_party(&proposal),
            Err(Error::TooManyOwners { max: 10, got: 11 })
        ));
    }

    #[test]
    fn rejects_the_same_key_twice() {
        let mut proposal = proposal();
        proposal.owners[1].public_key = key(1);
        assert!(matches!(plan_party(&proposal), Err(Error::DuplicateOwner)));
    }

    #[test]
    fn rejects_a_bad_owner_key() {
        let mut proposal = proposal();
        proposal.owners[1].public_key = "abcd".to_owned();
        assert!(matches!(
            plan_party(&proposal),
            Err(Error::InvalidOwnerKey { number: 2 })
        ));
    }

    #[test]
    fn rejects_a_missing_participant() {
        let mut proposal = proposal();
        proposal.owners[0].participant = "  ".to_owned();
        assert!(matches!(
            plan_party(&proposal),
            Err(Error::MissingParticipant { number: 1 })
        ));
    }

    /// Two owners and one covalidator: the signing and namespace thresholds
    /// are capped at 2 owners, and the confirmation threshold at 3 hosts.
    #[test]
    fn rejects_thresholds_out_of_range() {
        let cases = [
            (0, 1, 1, "signing", 2, 0),
            (3, 1, 1, "signing", 2, 3),
            (1, 0, 1, "namespace", 2, 0),
            (1, 3, 1, "namespace", 2, 3),
            (1, 1, 0, "confirmation", 3, 0),
            (1, 1, 4, "confirmation", 3, 4),
        ];
        for (signing, namespace, confirmation, kind, max, got) in cases {
            let mut proposal = proposal();
            proposal.signing_threshold = signing;
            proposal.namespace_threshold = namespace;
            proposal.confirmation_threshold = confirmation;
            let result = plan_party(&proposal);
            assert!(
                matches!(
                    &result,
                    Err(Error::ThresholdOutOfRange { kind: k, max: m, got: g })
                        if *k == kind && *m == max && *g == got
                ),
                "signing {signing}, namespace {namespace}, confirmation {confirmation} gave \
                 {result:?}"
            );
        }
    }

    #[test]
    fn each_threshold_may_reach_its_own_count() -> Result<(), Error> {
        let mut proposal = proposal();
        proposal.signing_threshold = 2;
        proposal.namespace_threshold = 2;
        // Above the owner count, so only the host count may cap it.
        proposal.confirmation_threshold = 3;
        let plan = plan_party(&proposal)?;
        assert_eq!(plan.signing_threshold, 2);
        assert_eq!(plan.namespace_threshold, 2);
        assert_eq!(plan.confirmation_threshold, 3);
        Ok(())
    }

    #[test]
    fn a_covalidator_on_an_owners_participant_adds_no_confirmer() {
        let mut proposal = proposal();
        proposal.covalidators = vec!["participant-a".to_owned()];
        proposal.confirmation_threshold = 3;
        assert!(matches!(
            plan_party(&proposal),
            Err(Error::ThresholdOutOfRange {
                kind: "confirmation",
                max: 2,
                got: 3
            })
        ));
    }

    fn plan_with_hint(hint: &str) -> Result<PartyPlan, Error> {
        let mut proposal = proposal();
        proposal.party_hint = hint.to_owned();
        plan_party(&proposal)
    }

    #[test]
    fn applies_cantons_party_name_rules() {
        assert!(matches!(plan_with_hint(""), Err(Error::EmptyHint)));
        assert!(matches!(
            plan_with_hint("a::b"),
            Err(Error::HintContainsDelimiter)
        ));
        for hint in ["treasury:", ":", "a:b:"] {
            assert!(
                matches!(plan_with_hint(hint), Err(Error::HintEndsWithColon)),
                "{hint:?}"
            );
        }
        assert!(plan_with_hint(":treasury").is_ok());
        for hint in ["café", "a/b", "a.b", "a\tb"] {
            assert!(
                matches!(plan_with_hint(hint), Err(Error::InvalidHintCharacter)),
                "{hint:?}"
            );
        }
        assert!(matches!(
            plan_with_hint(&"a".repeat(MAX_HINT_LENGTH + 1)),
            Err(Error::HintTooLong {
                max: MAX_HINT_LENGTH
            })
        ));
        assert!(plan_with_hint(&"a".repeat(MAX_HINT_LENGTH)).is_ok());
        assert!(plan_with_hint("Team treasury-1_a:b").is_ok());
    }
}
