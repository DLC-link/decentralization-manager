use std::collections::BTreeSet;

use canton_proto_rs::com::digitalasset::canton::protocol::v30::DecentralizedNamespaceDefinition;

use crate::{error::Result, server::WorkflowKind};

/// Whether a proposal is the head transaction itself. The same serial with a
/// different mapping is refused: Canton would reject it, and the peer cannot
/// tell what the coordinator meant by it.
///
/// Any other proposal must sit at the next serial. Canton checks the serial
/// only on submission, and the coordinator holds the signatures until then,
/// so a signature on a later serial could be submitted against a state this
/// peer never checked.
pub(super) fn already_applied<T: PartialEq>(
    what: &str,
    (proposal_serial, proposal): (u32, &T),
    (head_serial, head): (u32, &T),
) -> Result<bool> {
    if proposal_serial == head_serial {
        if proposal != head {
            anyhow::bail!(
                "{what} proposal reuses the on-chain serial {head_serial} but differs from the \
                 on-chain mapping"
            );
        }
        return Ok(true);
    }
    if head_serial.checked_add(1) != Some(proposal_serial) {
        anyhow::bail!(
            "{what} proposal is at serial {proposal_serial} but the on-chain serial is \
             {head_serial}; only the next serial can be signed"
        );
    }
    Ok(false)
}

pub(super) fn check_dns_against_head(
    kind: WorkflowKind,
    proposal: (u32, &DecentralizedNamespaceDefinition),
    head: (u32, &DecentralizedNamespaceDefinition),
    new_member: Option<&str>,
    removed_owner: Option<&str>,
) -> Result {
    if already_applied("DNS", proposal, head)? {
        return Ok(());
    }
    let mut allowed = AllowedDelta::of("DNS owner", kind)?;
    allowed.added = new_member;
    allowed.removed = removed_owner;
    check_set_delta(
        "DNS owner",
        kind,
        allowed,
        &head.1.owners.iter().cloned().collect(),
        &proposal.1.owners,
        false,
    )
}

/// How far a run may move one on-chain set.
#[derive(Clone, Copy, Debug)]
pub(super) struct AllowedDelta<'a> {
    pub(super) max_added: usize,
    pub(super) removals: usize,
    /// The entry the run adds, when this node knows it.
    pub(super) added: Option<&'a str>,
    /// The entry the run removes, when the invitation names it.
    pub(super) removed: Option<&'a str>,
}

impl AllowedDelta<'_> {
    /// The delta a run of `kind` makes by default: nothing for
    /// change-threshold, one entry removed for kick, and at most one added
    /// for add-party (none when a former host is hosted again).
    pub(super) fn of(what: &str, kind: WorkflowKind) -> Result<Self> {
        let (max_added, removals) = match kind {
            WorkflowKind::ChangeThreshold => (0, 0),
            WorkflowKind::Kick => (0, 1),
            WorkflowKind::AddParty => (1, 0),
            other => anyhow::bail!("no on-chain {what} delta is defined for a {other:?} run"),
        };
        Ok(Self {
            max_added,
            removals,
            added: None,
            removed: None,
        })
    }
}

/// The proposed set must be the current on-chain set changed only as
/// `allowed` permits.
///
/// `allowed.added` and `allowed.removed` pin the changed entry when this node
/// knows it. `legacy_source` marks a current set that may hold departed
/// members' keys, so a proposal may drop any of them but still add none beyond
/// the new member's.
pub(super) fn check_set_delta(
    what: &str,
    kind: WorkflowKind,
    allowed: AllowedDelta<'_>,
    current: &BTreeSet<String>,
    proposed: &[String],
    legacy_source: bool,
) -> Result {
    let proposed_set: BTreeSet<String> = proposed.iter().cloned().collect();
    if proposed_set.len() != proposed.len() {
        anyhow::bail!("proposal repeats a {what}");
    }
    let added: Vec<&String> = proposed_set.difference(current).collect();
    let removed: Vec<&String> = current.difference(&proposed_set).collect();

    let AllowedDelta {
        max_added,
        removals,
        added: new_member,
        removed: departing,
    } = allowed;
    if added.len() > max_added {
        anyhow::bail!(
            "{kind:?} proposal adds {what}(s) {added:?} that are not on-chain; it may add at \
             most {max_added}"
        );
    }
    if !legacy_source && removed.len() != removals {
        anyhow::bail!(
            "{kind:?} proposal removes {what}(s) {removed:?} from the on-chain set; it must \
             remove exactly {removals}"
        );
    }
    if let Some(new_member) = new_member {
        if let Some(stranger) = added.iter().find(|added| added.as_str() != new_member) {
            anyhow::bail!("proposal adds {what} {stranger}, not the new member's {new_member}");
        }
        if !proposed_set.contains(new_member) {
            anyhow::bail!("proposal does not carry the new member's {what} {new_member}");
        }
    }
    if let Some(departing) = departing
        && let Some(other) = removed.iter().find(|removed| removed.as_str() != departing)
    {
        anyhow::bail!(
            "{kind:?} proposal removes {what} {other}, not the {departing} the accepted \
             invitation names"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::validation::test_support::*;

    fn set_delta(
        what: &str,
        kind: WorkflowKind,
        current: &BTreeSet<String>,
        proposed: &[String],
        new_member: Option<&str>,
        legacy_source: bool,
    ) -> Result {
        let mut allowed = AllowedDelta::of(what, kind)?;
        allowed.added = new_member;
        check_set_delta(what, kind, allowed, current, proposed, legacy_source)
    }

    fn delta_error(
        kind: WorkflowKind,
        current: &[&str],
        proposed: &[&str],
        new_member: Option<&str>,
        legacy_source: bool,
    ) -> String {
        set_delta(
            "DNS owner",
            kind,
            &owners(current).into_iter().collect(),
            &owners(proposed),
            new_member,
            legacy_source,
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default()
    }

    #[test]
    fn change_threshold_keeps_the_on_chain_set() -> Result {
        set_delta(
            "DNS owner",
            WorkflowKind::ChangeThreshold,
            &owners(&["a", "b", "c"]).into_iter().collect(),
            &owners(&["c", "a", "b"]),
            None,
            false,
        )
    }

    /// The #420 attack: the count and the signer's own entry stay, but a
    /// non-signing member's entry is swapped for one the coordinator holds.
    #[test]
    fn change_threshold_rejects_a_swapped_entry() {
        let error = delta_error(
            WorkflowKind::ChangeThreshold,
            &["a", "b", "c"],
            &["a", "b", "x"],
            None,
            false,
        );
        assert!(
            error.contains("adds DNS owner"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn kick_removes_exactly_one_entry() -> Result {
        set_delta(
            "DNS owner",
            WorkflowKind::Kick,
            &owners(&["a", "b", "c"]).into_iter().collect(),
            &owners(&["a", "b"]),
            None,
            false,
        )
    }

    #[test]
    fn kick_rejects_a_swap_or_a_second_removal() {
        let swap = delta_error(
            WorkflowKind::Kick,
            &["a", "b", "c"],
            &["a", "x"],
            None,
            false,
        );
        assert!(swap.contains("adds DNS owner"), "unexpected error: {swap}");
        let two = delta_error(WorkflowKind::Kick, &["a", "b", "c"], &["a"], None, false);
        assert!(
            two.contains("must remove exactly 1"),
            "unexpected error: {two}"
        );
        let none = delta_error(WorkflowKind::Kick, &["a", "b"], &["a", "b"], None, false);
        assert!(
            none.contains("must remove exactly 1"),
            "unexpected error: {none}"
        );
    }

    #[test]
    fn add_party_adds_at_most_one_entry() -> Result {
        let current: BTreeSet<String> = owners(&["a", "b"]).into_iter().collect();
        set_delta(
            "DNS owner",
            WorkflowKind::AddParty,
            &current,
            &owners(&["a", "b", "n"]),
            None,
            false,
        )?;
        // A former host hosted again leaves the namespace as it is.
        set_delta(
            "DNS owner",
            WorkflowKind::AddParty,
            &current,
            &owners(&["a", "b"]),
            None,
            false,
        )
    }

    #[test]
    fn add_party_rejects_a_second_addition_or_a_removal() {
        let two = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b", "n", "x"],
            None,
            false,
        );
        assert!(two.contains("at most 1"), "unexpected error: {two}");
        let swap = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "n", "x"],
            None,
            false,
        );
        assert!(swap.contains("at most 1"), "unexpected error: {swap}");
        let dropped = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "n"],
            None,
            false,
        );
        assert!(
            dropped.contains("must remove exactly 0"),
            "unexpected error: {dropped}"
        );
    }

    #[test]
    fn add_party_pins_the_addition_to_the_new_member_when_known() -> Result {
        let error = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b", "x"],
            Some("n"),
            false,
        );
        assert!(
            error.contains("not the new member's n"),
            "unexpected error: {error}"
        );
        let missing = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b"],
            Some("n"),
            false,
        );
        assert!(
            missing.contains("does not carry the new member's"),
            "unexpected error: {missing}"
        );
        set_delta(
            "DNS owner",
            WorkflowKind::AddParty,
            &owners(&["a", "b"]).into_iter().collect(),
            &owners(&["a", "b", "n"]),
            Some("n"),
            false,
        )
    }

    /// A legacy PartyToKeyMapping still holds departed members' keys, so the
    /// proposal may drop several of them. Only add-party may bring in a new
    /// one, the new member's.
    #[test]
    fn a_legacy_source_allows_dropping_but_not_adding() -> Result {
        set_delta(
            "party signing key",
            WorkflowKind::ChangeThreshold,
            &owners(&["a", "b", "c", "old1", "old2"])
                .into_iter()
                .collect(),
            &owners(&["a", "b", "c"]),
            None,
            true,
        )?;
        let error = match set_delta(
            "party signing key",
            WorkflowKind::Kick,
            &owners(&["a", "b", "c", "old1"]).into_iter().collect(),
            &owners(&["a", "x"]),
            None,
            true,
        ) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("adds party signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn accepts_a_kick_already_applied_at_the_proposal_serial() -> Result {
        let kicked = namespace(&["a", "b"]);
        check_dns_against_head(WorkflowKind::Kick, (2, &kicked), (2, &kicked), None, None)?;

        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mapping = p2p("party", &[a, b], 1);
        assert!(already_applied("P2P", (2, &mapping), (2, &mapping))?);
        Ok(())
    }

    #[test]
    fn refuses_a_different_mapping_at_the_head_serial() -> Result {
        let error = match check_dns_against_head(
            WorkflowKind::Kick,
            (2, &namespace(&["a", "x"])),
            (2, &namespace(&["a", "b"])),
            None,
            None,
        ) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("reuses the on-chain serial 2"),
            "unexpected error: {error}"
        );

        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        assert!(
            already_applied(
                "P2P",
                (2, &p2p("party", &[a.clone(), c], 1)),
                (2, &p2p("party", &[a, b], 1)),
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn refuses_a_kick_whose_head_matches_at_another_serial() -> Result {
        let kicked = namespace(&["a", "b"]);
        let error = match check_dns_against_head(
            WorkflowKind::Kick,
            (3, &kicked),
            (2, &kicked),
            None,
            None,
        ) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    fn dns_error(
        kind: WorkflowKind,
        proposal: (u32, &DecentralizedNamespaceDefinition),
        head: (u32, &DecentralizedNamespaceDefinition),
    ) -> Result<String> {
        match check_dns_against_head(kind, proposal, head, None, None) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => Ok(error.to_string()),
        }
    }

    #[test]
    fn refuses_a_change_threshold_beyond_the_next_serial() -> Result {
        let def = namespace(&["a", "b", "c"]);
        let error = dns_error(WorkflowKind::ChangeThreshold, (4, &def), (2, &def))?;
        assert!(
            error.contains("only the next serial can be signed"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn accepts_a_change_threshold_at_the_next_serial() -> Result {
        let def = namespace(&["a", "b", "c"]);
        check_dns_against_head(
            WorkflowKind::ChangeThreshold,
            (3, &def),
            (2, &def),
            None,
            None,
        )
    }

    /// The same valid kick delta at head + 1 passes, so the refusal at head + 2
    /// comes from the serial check and not from the kick's removal rule.
    #[test]
    fn the_serial_check_refuses_a_valid_kick_delta_at_a_later_serial() -> Result {
        let (head, kicked) = (namespace(&["a", "b", "c"]), namespace(&["a", "b"]));
        check_dns_against_head(WorkflowKind::Kick, (3, &kicked), (2, &head), None, None)?;
        let error = dns_error(WorkflowKind::Kick, (4, &kicked), (2, &head))?;
        assert!(
            error.contains("only the next serial can be signed"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn refuses_a_p2p_proposal_beyond_the_next_serial() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mapping = p2p("party", &[a, b], 1);
        assert!(!already_applied("P2P", (3, &mapping), (2, &mapping))?);
        for serial in [1, 4] {
            let error = match already_applied("P2P", (serial, &mapping), (2, &mapping)) {
                Ok(applied) => anyhow::bail!("expected a refusal, got {applied}"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("P2P proposal is at serial"),
                "unexpected error: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_a_repeated_entry() {
        let error = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b", "b"],
            None,
            false,
        );
        assert!(
            error.contains("repeats a DNS owner"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_a_delta_for_a_kind_without_one() {
        let error = delta_error(WorkflowKind::Onboarding, &[], &["a"], None, false);
        assert!(error.contains("no on-chain"), "unexpected error: {error}");
    }
}
