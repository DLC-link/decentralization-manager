pub mod attestor;
pub mod config;
pub mod coordinator;
pub mod dirs;
pub mod steps;

pub use config::KickConfig;
pub use dirs::KickDirs;
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
}
