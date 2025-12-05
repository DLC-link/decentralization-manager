pub mod attestor;
pub mod coordinator;
pub mod steps;

pub use steps::{
    create_proposals, generate_keys, sign_dns_proposals, sign_p2p_proposals, submit_dns_proposals,
    submit_final_proposals,
};

use crate::{noise::MessageType, workflow::state::WorkflowStep};

/// Onboarding workflow steps (decentralized party creation)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnboardingStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
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

impl WorkflowStep for OnboardingStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::GenerateKeys => Some(MessageType::GenerateKeys),
            Self::SignDns => Some(MessageType::SignDns),
            Self::SignP2p => Some(MessageType::SignP2p),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForAttestors
            | Self::CreateProposals
            | Self::SubmitDns
            | Self::SubmitFinal => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::GenerateKeys),
            Self::GenerateKeys => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignDns),
            Self::SignDns => Some(Self::SubmitDns),
            Self::SubmitDns => Some(Self::SignP2p),
            Self::SignP2p => Some(Self::SubmitFinal),
            Self::SubmitFinal => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        matches!(self, Self::GenerateKeys | Self::SignDns | Self::SignP2p)
    }

    fn is_waiting_for_attestors(&self) -> bool {
        matches!(self, Self::WaitingForAttestors)
    }
}
