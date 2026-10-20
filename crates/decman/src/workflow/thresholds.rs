//! The two thresholds a decentralized party's topology carries.
//!
//! `DecentralizedNamespaceDefinition.threshold` is how many owners must sign a
//! topology change. `PartyToParticipant.threshold` is how many hosting
//! participants must confirm a transaction. The party's signing keys follow
//! the namespace threshold, because both count member keys while the
//! confirmation threshold counts nodes.

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{PartyToParticipant, party_to_participant::HostingParticipant},
};

use crate::error::Result;

/// The thresholds one proposal writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartyThresholds {
    /// Owners that must sign a topology change, and the threshold of the
    /// party's signing keys.
    pub namespace: i32,
    /// Hosting participants that must confirm a transaction.
    pub confirmation: i32,
}

impl PartyThresholds {
    /// An absent confirmation threshold takes the namespace one, which is
    /// the shape every party had before the two could differ.
    pub fn new(namespace: i32, confirmation: Option<i32>) -> Self {
        Self {
            namespace,
            confirmation: confirmation.unwrap_or(namespace),
        }
    }

    /// The party's `PartyToParticipant` with each threshold in its place.
    ///
    /// # Errors
    ///
    /// Errors if either threshold is negative.
    pub fn party_to_participant(
        self,
        party: String,
        participants: Vec<HostingParticipant>,
        signing_keys: Vec<SigningPublicKey>,
    ) -> Result<PartyToParticipant> {
        Ok(PartyToParticipant {
            party,
            threshold: self.confirmation.try_into()?,
            participants,
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: signing_keys,
                threshold: self.namespace.try_into()?,
            }),
        })
    }
}

/// The confirmation threshold a kick or add-party sets, or `None` when it
/// takes the new namespace threshold.
///
/// A party whose two thresholds match keeps them matched, as kick and
/// add-party always did. A party whose thresholds differ keeps its
/// confirmation threshold, capped at the hosts left after the change.
pub fn carried_confirmation_threshold(current: PartyThresholds, host_count: usize) -> Option<i32> {
    if current.confirmation == current.namespace {
        return None;
    }
    let hosts = i32::try_from(host_count).unwrap_or(i32::MAX);
    Some(current.confirmation.min(hosts))
}

/// Refuse a kick or add-party whose confirmation threshold came from a stale
/// cache.
///
/// The HTTP handler works the threshold out from the cached party, and the
/// peers hold the proposal to what it sent them. If the chain has moved since,
/// building on the cached value would reset a threshold nobody asked to
/// change.
///
/// # Errors
///
/// Errors when `planned` differs from what `on_chain` calls for.
pub fn ensure_carried_from(
    on_chain: PartyThresholds,
    host_count: usize,
    planned: PartyThresholds,
) -> Result {
    let expected =
        carried_confirmation_threshold(on_chain, host_count).unwrap_or(planned.namespace);
    if expected != planned.confirmation {
        anyhow::bail!(
            "The party's confirmation threshold is {current} on-chain, so this run should set \
             {expected}, not {planned}. The cached party is out of date. Refresh it and start \
             the run again",
            current = on_chain.confirmation,
            planned = planned.confirmation,
        );
    }
    Ok(())
}

/// A confirmation threshold must name at least one host and no more hosts
/// than the party has.
///
/// # Errors
///
/// Errors when `confirmation` is outside `1..=host_count`.
pub fn check_confirmation_threshold(confirmation: i32, host_count: usize) -> Result {
    let hosts = i32::try_from(host_count)?;
    if !(1..=hosts).contains(&confirmation) {
        anyhow::bail!(
            "confirmation_threshold must be between 1 and {hosts} (the hosting participants); \
             got {confirmation}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(uid: &str) -> HostingParticipant {
        HostingParticipant {
            participant_uid: uid.to_string(),
            permission: 2,
            onboarding: None,
        }
    }

    fn key(byte: u8) -> SigningPublicKey {
        SigningPublicKey {
            public_key: vec![byte; 32],
            ..Default::default()
        }
    }

    /// The confirmation threshold goes on the mapping, the namespace one on
    /// its signing keys. Swapping them would let one member key authorize a
    /// transaction the operator meant to need three.
    #[test]
    fn each_threshold_lands_in_its_own_field() -> Result {
        let mapping = PartyThresholds::new(1, Some(3)).party_to_participant(
            "party".to_string(),
            vec![host("a"), host("b"), host("c")],
            vec![key(1), key(2), key(3)],
        )?;

        assert_eq!(mapping.threshold, 3);
        let keys = mapping
            .party_signing_keys
            .ok_or_else(|| anyhow::anyhow!("no signing keys"))?;
        assert_eq!(keys.threshold, 1);
        assert_eq!(keys.keys.len(), 3);
        Ok(())
    }

    /// Callers that send no confirmation threshold get today's mapping:
    /// both thresholds equal.
    #[test]
    fn an_absent_confirmation_threshold_takes_the_namespace_one() -> Result {
        let thresholds = PartyThresholds::new(2, None);
        assert_eq!(thresholds.confirmation, 2);

        let mapping =
            thresholds.party_to_participant("party".to_string(), vec![host("a")], vec![key(1)])?;
        assert_eq!(mapping.threshold, 2);
        assert_eq!(mapping.party_signing_keys.map(|k| k.threshold), Some(2));
        Ok(())
    }

    #[test]
    fn a_negative_threshold_is_refused() {
        let built = PartyThresholds::new(2, Some(-1)).party_to_participant(
            "party".to_string(),
            Vec::new(),
            Vec::new(),
        );
        assert!(built.is_err());
    }

    /// Matching thresholds stay matched, so a coupled party's kick and
    /// add-party proposals are what an older peer expects.
    #[test]
    fn matching_thresholds_follow_the_new_namespace_threshold() {
        assert_eq!(
            carried_confirmation_threshold(PartyThresholds::new(2, Some(2)), 4),
            None
        );
    }

    #[test]
    fn a_distinct_confirmation_threshold_is_kept() {
        assert_eq!(
            carried_confirmation_threshold(PartyThresholds::new(1, Some(3)), 4),
            Some(3)
        );
    }

    /// A kick can leave fewer hosts than the threshold counts. Canton refuses
    /// a threshold above the host count, so it drops to what is left.
    #[test]
    fn a_kept_confirmation_threshold_is_capped_at_the_remaining_hosts() {
        assert_eq!(
            carried_confirmation_threshold(PartyThresholds::new(1, Some(3)), 2),
            Some(2)
        );
    }

    #[test]
    fn a_plan_that_matches_the_chain_is_accepted() -> Result {
        // Matching on-chain thresholds: the confirmation one follows.
        ensure_carried_from(
            PartyThresholds::new(2, Some(2)),
            3,
            PartyThresholds::new(3, None),
        )?;
        // Distinct ones: the confirmation threshold stays.
        ensure_carried_from(
            PartyThresholds::new(1, Some(3)),
            4,
            PartyThresholds::new(2, Some(3)),
        )
    }

    /// The cache said the thresholds matched, but a change-threshold run has
    /// since set them apart. Following the cache would undo that run.
    #[test]
    fn a_plan_from_a_stale_cache_is_refused() {
        assert!(
            ensure_carried_from(
                PartyThresholds::new(1, Some(3)),
                4,
                PartyThresholds::new(2, None),
            )
            .is_err()
        );
        assert!(
            ensure_carried_from(
                PartyThresholds::new(2, Some(2)),
                4,
                PartyThresholds::new(3, Some(2)),
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_a_confirmation_threshold_within_the_hosts() -> Result {
        check_confirmation_threshold(1, 3)?;
        check_confirmation_threshold(3, 3)
    }

    #[test]
    fn refuses_zero_and_more_than_the_hosts() {
        assert!(check_confirmation_threshold(0, 3).is_err());
        assert!(check_confirmation_threshold(-1, 3).is_err());
        assert!(check_confirmation_threshold(4, 3).is_err());
    }
}
