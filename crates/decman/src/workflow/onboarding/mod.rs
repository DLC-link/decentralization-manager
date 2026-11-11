pub mod config;
pub mod coordinator;
pub mod peer;
pub mod steps;

pub use config::OnboardingConfig;
pub use steps::{
    create_proposals, generate_keys, sign_dns_proposals, sign_p2p_proposals, submit_dns_proposals,
    submit_final_proposals,
};

use crate::{
    noise::MessageType,
    server::WorkflowKind,
    workflow::state::{WorkflowStep, workflow_steps},
};

workflow_steps! {
    /// Onboarding workflow steps (decentralized party creation)
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub enum OnboardingStep {
        /// Waiting for all peers to connect
        WaitingForPeers,
        /// Generate keys
        GenerateKeys,
        /// Coordinator creates proposals
        CreateProposals,
        /// Sign DNS proposals
        SignDns,
        /// Coordinator submits DNS proposals
        SubmitDns,
        /// Sign P2P proposals
        SignP2p,
        /// Coordinator submits final proposals
        SubmitFinal,
        /// Workflow complete
        Complete,
    }
}

impl WorkflowStep for OnboardingStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::GenerateKeys => Some(MessageType::GenerateKeys),
            Self::SignDns => Some(MessageType::SignDns),
            Self::SignP2p => Some(MessageType::SignP2p),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers | Self::CreateProposals | Self::SubmitDns | Self::SubmitFinal => {
                None
            }
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::GenerateKeys || *self == Self::SignDns || *self == Self::SignP2p
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Onboarding
    }
}
