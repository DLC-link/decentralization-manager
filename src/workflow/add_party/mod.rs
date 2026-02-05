pub mod attestor;
pub mod config;
pub mod coordinator;
pub mod dirs;
pub mod steps;

pub use config::AddPartyConfig;
pub use dirs::AddPartyDirs;
pub use steps::{create_proposals, export_state, generate_keys, sign_proposals, submit_add_party};

use crate::{noise::MessageType, workflow::state::WorkflowStep};

/// Add party workflow steps (adding a new member to decentralized party)
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AddPartyStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
    /// New member generates keys
    GenerateNewMemberKeys,
    /// Coordinator exports current state
    ExportState,
    /// Coordinator creates add proposals
    CreateProposals,
    /// Existing members sign proposals
    SignProposals,
    /// Coordinator submits add party
    SubmitAddParty,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for AddPartyStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::GenerateNewMemberKeys => Some(MessageType::GenerateAddPartyKeys),
            Self::SignProposals => Some(MessageType::SignAddParty),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForAttestors
            | Self::ExportState
            | Self::CreateProposals
            | Self::SubmitAddParty => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::GenerateNewMemberKeys),
            Self::GenerateNewMemberKeys => Some(Self::ExportState),
            Self::ExportState => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignProposals),
            Self::SignProposals => Some(Self::SubmitAddParty),
            Self::SubmitAddParty => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        *self == Self::GenerateNewMemberKeys || *self == Self::SignProposals
    }

    fn is_waiting_for_attestors(&self) -> bool {
        *self == Self::WaitingForAttestors
    }
}
