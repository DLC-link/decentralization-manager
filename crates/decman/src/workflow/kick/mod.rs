pub mod config;
pub mod steps;

pub use config::KickConfig;
pub use steps::{export_state, prune_cached_membership};

use crate::{server::WorkflowKind, workflow::state::WorkflowStep};

/// Kick workflow steps of the 1.x transport. Kept for the run cards of rows that
/// predate the 2.0 upgrade; the on-ledger engine has its own step lists
/// (`crate::onledger::engine::kick`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KickStep {
    /// Waiting for all peers to connect
    WaitingForPeers,
    /// Coordinator exports current state
    ExportState,
    /// Coordinator creates kick proposals
    CreateProposals,
    /// Remaining members sign proposals
    SignProposals,
    /// Coordinator submits kick
    SubmitKick,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for KickStep {
    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::ExportState),
            Self::ExportState => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignProposals),
            Self::SignProposals => Some(Self::SubmitKick),
            Self::SubmitKick => Some(Self::Complete),
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
            Self::SubmitKick => 4,
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
            Self::SubmitKick => "SubmitKick",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "ExportState" => Self::ExportState,
            "CreateProposals" => Self::CreateProposals,
            "SignProposals" => Self::SignProposals,
            "SubmitKick" => Self::SubmitKick,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Kick
    }
}
