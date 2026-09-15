pub mod config;
pub mod steps;

pub use config::ChangeThresholdConfig;
pub use steps::export_state;

use crate::{server::WorkflowKind, workflow::state::WorkflowStep};

/// Change-threshold workflow steps of the 1.x transport. Kept for the run cards
/// of rows that predate the 2.0 upgrade; the on-ledger engine has its own
/// step lists (`crate::onledger::engine::change_threshold`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChangeThresholdStep {
    /// Waiting for all peers to connect
    WaitingForPeers,
    /// Coordinator exports current namespace state
    ExportState,
    /// Coordinator creates the new-threshold proposals
    CreateProposals,
    /// Members sign the proposals
    SignProposals,
    /// Coordinator submits the change
    Submit,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for ChangeThresholdStep {
    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::ExportState),
            Self::ExportState => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignProposals),
            Self::SignProposals => Some(Self::Submit),
            Self::Submit => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::SignProposals
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::ExportState => 1,
            Self::CreateProposals => 2,
            Self::SignProposals => 3,
            Self::Submit => 4,
            Self::Complete => 5,
        }
    }

    fn step_total() -> i64 {
        6
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::ExportState => "ExportState",
            Self::CreateProposals => "CreateProposals",
            Self::SignProposals => "SignProposals",
            Self::Submit => "Submit",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "ExportState" => Self::ExportState,
            "CreateProposals" => Self::CreateProposals,
            "SignProposals" => Self::SignProposals,
            "Submit" => Self::Submit,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::ChangeThreshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_advance_in_order_to_completion() {
        // Walking `next()` from the start lands on every step exactly once and
        // terminates at `Complete`.
        let mut step = ChangeThresholdStep::WaitingForPeers;
        let mut seen = vec![step];
        while let Some(next) = step.next() {
            seen.push(next);
            step = next;
        }
        assert_eq!(
            seen,
            vec![
                ChangeThresholdStep::WaitingForPeers,
                ChangeThresholdStep::ExportState,
                ChangeThresholdStep::CreateProposals,
                ChangeThresholdStep::SignProposals,
                ChangeThresholdStep::Submit,
                ChangeThresholdStep::Complete,
            ]
        );
        assert_eq!(seen.len() as i64, ChangeThresholdStep::step_total());
    }

    #[test]
    fn only_sign_step_requires_peers() {
        for step in [
            ChangeThresholdStep::WaitingForPeers,
            ChangeThresholdStep::ExportState,
            ChangeThresholdStep::CreateProposals,
            ChangeThresholdStep::SignProposals,
            ChangeThresholdStep::Submit,
            ChangeThresholdStep::Complete,
        ] {
            assert_eq!(
                step.requires_peers(),
                step == ChangeThresholdStep::SignProposals,
                "{step:?} peer requirement"
            );
        }
    }

    #[test]
    fn step_names_round_trip() {
        for step in [
            ChangeThresholdStep::WaitingForPeers,
            ChangeThresholdStep::ExportState,
            ChangeThresholdStep::CreateProposals,
            ChangeThresholdStep::SignProposals,
            ChangeThresholdStep::Submit,
            ChangeThresholdStep::Complete,
        ] {
            assert_eq!(
                ChangeThresholdStep::try_from_step_name(step.step_name()),
                Some(step)
            );
        }
        assert_eq!(ChangeThresholdStep::try_from_step_name("Nope"), None);
    }
}
