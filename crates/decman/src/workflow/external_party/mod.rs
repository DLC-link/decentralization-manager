//! Decentrally-hosted external-party onboarding workflow.
//!
//! See `docs/decentralice-external-party-v0.md`. This workflow onboards a
//! sovereign external party whose Ed25519 namespace key is generated and held
//! client-side (by DPM, standing in for a wallet) and hosted with Confirmation
//! permission across N participants at an M-of-N confirmation threshold.
//!
//! The coordinator generates the key, builds the multi-host onboarding topology
//! naming every host, signs the multi-hash once, allocates on its own
//! participant, then fans the party-signed bundle out to each hosting peer over
//! Noise; each peer authorizes hosting on its own participant. The topology
//! stays a proposal until the last host signs.

pub mod config;
pub mod coordinator;
pub mod keys;
pub mod peer;
pub mod steps;

pub use config::ExternalPartyConfig;

use crate::{noise::MessageType, server::WorkflowKind, workflow::state::WorkflowStep};

/// External-party workflow steps. The coordinator holds the single party key
/// and drives the coordinator-local steps; `AllocatePeers` is the one peer-gated
/// step — each hosting peer authorizes hosting on its own participant.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExternalPartyStep {
    /// Wait for every hosting peer to connect before onboarding.
    WaitingForPeers,
    /// Generate (or reload) the party's client-side Ed25519 key.
    GenerateKeys,
    /// Build the multi-host onboarding topology + multi-hash, sign it, and
    /// allocate on the coordinator's own participant.
    PrepareTopology,
    /// Peer-gated: each hosting peer runs `AllocateExternalParty` on its own
    /// participant with the party-signed bundle.
    AllocatePeers,
    /// Workflow complete.
    Complete,
}

impl WorkflowStep for ExternalPartyStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::AllocatePeers => Some(MessageType::AllocateExternalParty),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers | Self::GenerateKeys | Self::PrepareTopology => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::GenerateKeys),
            Self::GenerateKeys => Some(Self::PrepareTopology),
            Self::PrepareTopology => Some(Self::AllocatePeers),
            Self::AllocatePeers => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::AllocatePeers
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::GenerateKeys => 1,
            Self::PrepareTopology => 2,
            Self::AllocatePeers => 3,
            Self::Complete => 4,
        }
    }

    fn step_total() -> i64 {
        5
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::GenerateKeys => "GenerateKeys",
            Self::PrepareTopology => "PrepareTopology",
            Self::AllocatePeers => "AllocatePeers",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        match name {
            "WaitingForPeers" => Some(Self::WaitingForPeers),
            "GenerateKeys" => Some(Self::GenerateKeys),
            "PrepareTopology" => Some(Self::PrepareTopology),
            "AllocatePeers" => Some(Self::AllocatePeers),
            "Complete" => Some(Self::Complete),
            _ => None,
        }
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::ExternalParty
    }
}
