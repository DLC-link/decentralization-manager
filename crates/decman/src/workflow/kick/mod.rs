pub mod config;
pub mod coordinator;
pub mod peer;
pub mod steps;

pub use config::KickConfig;
pub use steps::{
    create_proposals, export_state, prune_cached_membership, sign_proposals, submit_kick,
};

use crate::{
    noise::MessageType,
    server::WorkflowKind,
    workflow::state::{WorkflowStep, workflow_steps},
};

workflow_steps! {
    /// Kick workflow steps (removing a member from decentralized party)
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
        /// Members retire the shadowed PartyToKeyMapping after the new topology is effective.
        RetireLegacyKeys,
        /// Workflow complete
        Complete,
    }
}

impl WorkflowStep for KickStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::SignProposals => Some(MessageType::SignKick),
            Self::RetireLegacyKeys => Some(MessageType::RetireLegacyKeys),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers
            | Self::ExportState
            | Self::CreateProposals
            | Self::SubmitKick => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::SignProposals
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Kick
    }
}
