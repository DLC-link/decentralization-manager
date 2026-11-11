use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    PartyToParticipant,
    enums::ParticipantPermission,
    party_to_participant::{HostingParticipant, hosting_participant},
};
use common::api::HostPermission;

use crate::{canton_id::CantonId, error::Result};

/// Canton's enum value for a requested host permission.
fn canton_permission(permission: HostPermission) -> i32 {
    match permission {
        HostPermission::Confirmation => ParticipantPermission::Confirmation as i32,
        HostPermission::Submission => ParticipantPermission::Submission as i32,
    }
}

/// Build the serial-N+1 `PartyToParticipant` that adds `new_hosts` to `current`.
///
/// Current hosts are carried over untouched, new hosts land at Confirmation with
/// the `Onboarding` marker, and the threshold is left alone. The participant list
/// is sorted so every host produces byte-identical output for the same input.
///
/// # Errors
/// Returns an error if `new_hosts` is empty, contains a duplicate, or names a
/// participant that already hosts the party.
pub(super) fn add_hosts_mapping(
    current: &PartyToParticipant,
    new_hosts: &[CantonId],
    permission: HostPermission,
) -> Result<PartyToParticipant> {
    if new_hosts.is_empty() {
        anyhow::bail!("add-hosts named no new hosting participant");
    }
    // Canton's rule, quoted from topology.proto: "if threshold > 1, must be
    // Confirmation or Observation". A Submission host is the failover-only
    // offer, and it only exists while the party needs exactly one confirmation.
    if permission == HostPermission::Submission && current.threshold != 1 {
        anyhow::bail!(
            "a Submission host needs the party's threshold to be 1, and {party} is at {t}; \
             Canton forbids Submission once more than one host must confirm",
            party = current.party,
            t = current.threshold
        );
    }

    let mut participants = current.participants.clone();
    for host in new_hosts {
        let uid = host.to_string();
        if participants.iter().any(|p| p.participant_uid == uid) {
            anyhow::bail!(
                "{uid} already hosts {party}; add-hosts only adds hosts",
                party = current.party
            );
        }
        participants.push(HostingParticipant {
            participant_uid: uid,
            permission: canton_permission(permission),
            // The marker is what keeps the party suspended on the new host until
            // its ACS import lands. Without it the host would be expected to
            // confirm transactions it has no contracts for.
            onboarding: Some(hosting_participant::Onboarding {}),
        });
    }

    // Same canonical order onboarding uses, and for the same reason: the wallet
    // compares the hosts' bytes against each other, so list order must not depend
    // on which host built the transaction.
    participants.sort_by(|a, b| a.participant_uid.cmp(&b.participant_uid));

    Ok(PartyToParticipant {
        party: current.party.clone(),
        // Unchanged. The new hosts do not count toward it until their markers
        // clear, so raising it here could leave the party below its own threshold.
        threshold: current.threshold,
        participants,
        // Unchanged. This step adds hosts, never keys.
        party_signing_keys: current.party_signing_keys.clone(),
    })
}

/// Does this mapping already host every one of `new_hosts` at `permission`?
///
/// The "already authorized" half of the retry check. The onboarding marker is
/// deliberately not compared: it is cleared once the joiner's ACS import lands,
/// so a retry arriving after that would otherwise read as a stale pin.
pub(super) fn hosts_present(
    mapping: &PartyToParticipant,
    new_hosts: &[CantonId],
    permission: HostPermission,
) -> bool {
    let wanted = canton_permission(permission);
    new_hosts.iter().all(|host| {
        let uid = host.to_string();
        mapping
            .participants
            .iter()
            .any(|p| p.participant_uid == uid && p.permission == wanted)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::external_party::add_hosts::test_support::*;

    // ------------------------------------------------------------------
    // The builder
    // ------------------------------------------------------------------

    /// The marker is the whole point: without it the new host goes live holding
    /// none of the party's contracts.
    #[test]
    fn new_hosts_land_at_confirmation_carrying_the_marker() {
        let mapping = built();
        let Some(added) = mapping
            .participants
            .iter()
            .find(|p| p.participant_uid == participant(3).to_string())
        else {
            panic!("the new host must appear in the mapping");
        };
        assert_eq!(added.permission, ParticipantPermission::Confirmation as i32);
        assert!(added.onboarding.is_some(), "the new host must be marked");
    }

    /// The wallet compares every host's bytes, so list order must not depend on
    /// which host built the transaction.
    #[test]
    fn participants_are_sorted_so_every_host_builds_the_same_bytes() {
        let mapping = match add_hosts_mapping(
            &current().mapping,
            &[participant(9), participant(3)],
            HostPermission::Confirmation,
        ) {
            Ok(m) => m,
            Err(e) => panic!("building must succeed: {e}"),
        };
        let uids: Vec<&str> = mapping
            .participants
            .iter()
            .map(|p| p.participant_uid.as_str())
            .collect();
        let mut sorted = uids.clone();
        sorted.sort_unstable();
        assert_eq!(uids, sorted, "participants must be in canonical order");
    }

    /// Current hosts survive untouched, and the threshold does not move — raising
    /// it is a separate serial bump.
    #[test]
    fn current_hosts_and_threshold_are_untouched() {
        let mapping = built();
        assert_eq!(mapping.threshold, current().mapping.threshold);
        assert_eq!(
            mapping.party_signing_keys,
            current().mapping.party_signing_keys
        );
        for existing in &current().mapping.participants {
            assert!(
                mapping.participants.contains(existing),
                "current host {uid} must survive byte-identical",
                uid = existing.participant_uid
            );
        }
    }

    #[test]
    fn builder_refuses_a_host_that_already_hosts_the_party() {
        let Err(e) = add_hosts_mapping(
            &current().mapping,
            &[participant(2)],
            HostPermission::Confirmation,
        ) else {
            panic!("adding an existing host must be refused");
        };
        assert!(e.to_string().contains("already hosts"), "{e}");
    }

    #[test]
    fn builder_refuses_an_empty_host_list() {
        let Err(e) = add_hosts_mapping(&current().mapping, &[], HostPermission::Confirmation)
        else {
            panic!("adding no host must be refused");
        };
        assert!(e.to_string().contains("no new hosting participant"), "{e}");
    }

    /// Canton's rule, not ours: `topology.proto` says a mapping with threshold
    /// above 1 may not have a Submission host. Refused at the builder so the
    /// caller hears why rather than getting a Canton rejection.
    #[test]
    fn refuses_a_submission_host_above_threshold_one() {
        let mut current = current();
        current.mapping.threshold = 2;
        let Err(e) = add_hosts_mapping(
            &current.mapping,
            &[participant(3)],
            HostPermission::Submission,
        ) else {
            panic!("a Submission host above threshold 1 must be refused");
        };
        assert!(e.to_string().contains("threshold to be 1"), "{e}");
    }

    /// A driver's retry after the second node completed the add must read as
    /// success, not as a conflict, or a lost response leaves it unable to learn
    /// the change is live.
    #[test]
    fn a_landed_add_is_recognized_on_retry() {
        let mut mapping = current().mapping;
        mapping.participants.push(HostingParticipant {
            participant_uid: participant(3).to_string(),
            permission: ParticipantPermission::Confirmation as i32,
            onboarding: Some(hosting_participant::Onboarding {}),
        });
        assert!(hosts_present(
            &mapping,
            &[participant(3)],
            HostPermission::Confirmation
        ));
    }

    /// The marker is gone once the joiner's ACS import lands, and a retry after
    /// that is still the same landed add.
    #[test]
    fn a_cleared_marker_still_counts_as_present() {
        let mut mapping = current().mapping;
        mapping.participants.push(HostingParticipant {
            participant_uid: participant(3).to_string(),
            permission: ParticipantPermission::Confirmation as i32,
            onboarding: None,
        });
        assert!(hosts_present(
            &mapping,
            &[participant(3)],
            HostPermission::Confirmation
        ));
    }

    /// A host sitting at another permission is not the change this call asked
    /// for, so the pin is genuinely stale.
    #[test]
    fn a_host_at_another_permission_is_not_present() {
        let mut mapping = current().mapping;
        mapping.participants.push(HostingParticipant {
            participant_uid: participant(3).to_string(),
            permission: ParticipantPermission::Submission as i32,
            onboarding: None,
        });
        assert!(!hosts_present(
            &mapping,
            &[participant(3)],
            HostPermission::Confirmation
        ));
    }

    /// Every named host has to be there. A partially applied set means some
    /// other write moved the party.
    #[test]
    fn a_missing_host_is_not_present() {
        let mut mapping = current().mapping;
        mapping.participants.push(HostingParticipant {
            participant_uid: participant(3).to_string(),
            permission: ParticipantPermission::Confirmation as i32,
            onboarding: None,
        });
        assert!(!hosts_present(
            &mapping,
            &[participant(3), participant(4)],
            HostPermission::Confirmation
        ));
    }
}
