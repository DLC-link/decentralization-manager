pub mod attestor;
pub mod config;
pub mod coordinator;
pub mod steps;

pub use config::KickConfig;
pub use steps::{create_proposals, export_state, sign_proposals, submit_kick};

use crate::{noise::MessageType, workflow::state::WorkflowStep};

/// Kick workflow steps (removing a member from decentralized party)
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KickStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
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
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::SignProposals => Some(MessageType::SignKick),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForAttestors
            | Self::ExportState
            | Self::CreateProposals
            | Self::SubmitKick => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::ExportState),
            Self::ExportState => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignProposals),
            Self::SignProposals => Some(Self::SubmitKick),
            Self::SubmitKick => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        *self == Self::SignProposals
    }

    fn is_waiting_for_attestors(&self) -> bool {
        *self == Self::WaitingForAttestors
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForAttestors => 0,
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
            Self::WaitingForAttestors => "WaitingForAttestors",
            Self::ExportState => "ExportState",
            Self::CreateProposals => "CreateProposals",
            Self::SignProposals => "SignProposals",
            Self::SubmitKick => "SubmitKick",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForAttestors" => Self::WaitingForAttestors,
            "ExportState" => Self::ExportState,
            "CreateProposals" => Self::CreateProposals,
            "SignProposals" => Self::SignProposals,
            "SubmitKick" => Self::SubmitKick,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }
}
