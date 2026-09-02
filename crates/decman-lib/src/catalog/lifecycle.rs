//! Classify governance choice names into lifecycle events.
//!
//! An indexer that reads exercised events needs the lifecycle meaning of a
//! choice, not its name. [`classify_choice`] maps the exact choice names of
//! the governance protocol onto [`GovernanceLifecycleEvent`]. The names are
//! the ones the builders in [`super::commands`] exercise, plus the two
//! `GovernableAction` interface choices the ledger exercises downstream.

/// The lifecycle meaning of one exercised governance choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GovernanceLifecycleEvent {
    /// A member confirmed a proposal or a self action. On the self path the
    /// first confirmation also acts as the proposal.
    Confirmed,
    /// A confirmed action executed.
    Executed,
    /// A member expired a stale confirmation.
    Expired,
    /// A member withdrew their own confirmation.
    ConfirmationCancelled,
    /// The proposer or the governance party cancelled the proposal contract.
    ProposalCancelled,
}

/// Map an exercised choice name onto its lifecycle event. Returns `None`
/// for a choice outside the governance protocol.
pub fn classify_choice(choice: &str) -> Option<GovernanceLifecycleEvent> {
    use GovernanceLifecycleEvent::*;
    Some(match choice {
        "GovernanceRules_ConfirmAction" | "GovernanceRules_ConfirmGovernanceAction" => Confirmed,
        "GovernanceRules_ExecuteConfirmedAction"
        | "GovernanceRules_ExecuteGovernanceAction"
        | "GovernableAction_Execute" => Executed,
        "GovernanceRules_ExpireConfirmation"
        | "GovernanceRules_ExpireGovernanceSelfConfirmation" => Expired,
        "GovernanceConfirmation_Cancel" | "GovernanceSelfConfirmation_Cancel" => {
            ConfirmationCancelled
        }
        "GovernableAction_ProposerCancel" | "GovernableAction_Cancel" => ProposalCancelled,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::GovernanceLifecycleEvent::*;
    use super::*;

    /// Every choice name the builders in `catalog::commands` exercise, with
    /// its expected event. The builder table test in `commands.rs` pins the
    /// same names to the builders, so the two tables tie the classifier to
    /// the builders.
    #[test]
    fn every_builder_choice_classifies() {
        let cases = [
            ("GovernanceRules_ConfirmGovernanceAction", Confirmed),
            ("GovernanceRules_ConfirmAction", Confirmed),
            ("GovernanceRules_ExecuteGovernanceAction", Executed),
            ("GovernanceRules_ExecuteConfirmedAction", Executed),
            ("GovernanceRules_ExpireGovernanceSelfConfirmation", Expired),
            ("GovernanceRules_ExpireConfirmation", Expired),
            ("GovernanceSelfConfirmation_Cancel", ConfirmationCancelled),
            ("GovernanceConfirmation_Cancel", ConfirmationCancelled),
            ("GovernableAction_ProposerCancel", ProposalCancelled),
        ];
        for (choice, expected) in cases {
            assert_eq!(classify_choice(choice), Some(expected), "{choice}");
        }
    }

    /// The `GovernableAction` interface choices have no builder: the ledger
    /// exercises `GovernableAction_Execute` downstream of an execute, and
    /// `GovernableAction_Cancel` belongs to the governance party.
    #[test]
    fn interface_choices_classify() {
        assert_eq!(classify_choice("GovernableAction_Execute"), Some(Executed));
        assert_eq!(
            classify_choice("GovernableAction_Cancel"),
            Some(ProposalCancelled)
        );
    }

    #[test]
    fn foreign_choices_return_none() {
        for choice in [
            "Archive",
            "MintOffer_Accept",
            "TransferInstruction_Accept",
            "GovernanceRules_SomethingNew",
            "ProposerCancel",
            "",
        ] {
            assert_eq!(classify_choice(choice), None, "{choice}");
        }
    }
}
