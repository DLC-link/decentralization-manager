pub mod attestor;
pub mod config;
pub mod coordinator;
pub mod steps;

pub use config::OnboardingConfig;
pub use steps::{
    create_proposals, generate_keys, sign_dns_proposals, sign_p2p_proposals, submit_dns_proposals,
    submit_final_proposals,
};

use crate::{noise::MessageType, workflow::state::WorkflowStep};

/// Onboarding workflow steps (decentralized party creation)
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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
        *self == Self::GenerateKeys || *self == Self::SignDns || *self == Self::SignP2p
    }

    fn is_waiting_for_attestors(&self) -> bool {
        *self == Self::WaitingForAttestors
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForAttestors => 0,
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
            Self::WaitingForAttestors => "WaitingForAttestors",
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
            "WaitingForAttestors" => Self::WaitingForAttestors,
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
}
