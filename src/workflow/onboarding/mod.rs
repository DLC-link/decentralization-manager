pub mod config;
pub mod coordinator;
pub mod peer;
pub mod steps;

pub use config::OnboardingConfig;
pub use steps::{
    create_proposals, generate_keys, sign_dns_proposals, sign_p2p_proposals, submit_dns_proposals,
    submit_final_proposals,
};

use crate::{noise::MessageType, server::WorkflowKind, workflow::state::WorkflowStep};

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

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::GenerateKeys),
            Self::GenerateKeys => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignDns),
            Self::SignDns => Some(Self::SubmitDns),
            Self::SubmitDns => Some(Self::SignP2p),
            Self::SignP2p => Some(Self::SubmitFinal),
            Self::SubmitFinal => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::GenerateKeys || *self == Self::SignDns || *self == Self::SignP2p
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::GenerateKeys => 1,
            Self::CreateProposals => 2,
            Self::SignDns => 3,
            Self::SubmitDns => 4,
            Self::SignP2p => 5,
            Self::SubmitFinal => 6,
            Self::Complete => 7,
        }
    }

    fn step_total() -> i64 {
        8
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::GenerateKeys => "GenerateKeys",
            Self::CreateProposals => "CreateProposals",
            Self::SignDns => "SignDns",
            Self::SubmitDns => "SubmitDns",
            Self::SignP2p => "SignP2p",
            Self::SubmitFinal => "SubmitFinal",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "GenerateKeys" => Self::GenerateKeys,
            "CreateProposals" => Self::CreateProposals,
            "SignDns" => Self::SignDns,
            "SubmitDns" => Self::SubmitDns,
            "SignP2p" => Self::SignP2p,
            "SubmitFinal" => Self::SubmitFinal,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Onboarding
    }
}
