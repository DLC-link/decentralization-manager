pub mod attestor;
pub mod config;
pub mod coordinator;
pub mod dirs;
pub mod steps;

pub use config::AddPartyConfig;
pub use dirs::AddPartyDirs;
pub use steps::{
    create_proposals, export_state, generate_keys, import_party_acs, sign_proposals,
    submit_add_party,
};

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
    /// Coordinator exports ACS (if party has contracts) and new member imports
    SyncAcs,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for AddPartyStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::GenerateNewMemberKeys => Some(MessageType::GenerateAddPartyKeys),
            Self::SignProposals => Some(MessageType::SignAddParty),
            Self::SyncAcs => Some(MessageType::ImportAcs),
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
            // Note: SyncAcs step may be skipped if party has no contracts
            Self::SubmitAddParty => Some(Self::SyncAcs),
            Self::SyncAcs => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        *self == Self::GenerateNewMemberKeys
            || *self == Self::SignProposals
            || *self == Self::SyncAcs
    }

    fn is_waiting_for_attestors(&self) -> bool {
        *self == Self::WaitingForAttestors
    }
}
