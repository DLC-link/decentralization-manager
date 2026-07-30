//! Decentrally-hosted external-party onboarding workflow.
//!
//! This workflow onboards a sovereign external party whose Ed25519 namespace key
//! is generated and held client-side by the wallet — DPM never sees the private
//! key — and hosted with Confirmation permission across N participants at an
//! M-of-N confirmation threshold.
//!
//! The wallet generates the key, asks DPM (`/v0/tenant/prepare`) to build the
//! multi-host onboarding topology naming every host, signs the multi-hash
//! locally, and submits the signed bundle (`/v0/tenant/onboard`). The
//! coordinator allocates that bundle on its own participant, then fans the same
//! bundle out to each hosting peer over Noise; each peer authorizes hosting on
//! its own participant. The topology stays a proposal until the last host signs.

pub mod config;
pub mod coordinator;
pub mod keys;
pub mod peer;
pub mod steps;

pub use config::ExternalPartyConfig;

use crate::{noise::MessageType, server::WorkflowKind, workflow::state::WorkflowStep};

/// External-party workflow steps. The coordinator allocates the wallet-signed
/// bundle on its own participant, then `AllocatePeers` is the one peer-gated
/// step — each hosting peer authorizes hosting on its own participant.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExternalPartyStep {
    /// Wait for every hosting peer to connect before onboarding.
    WaitingForPeers,
    /// Allocate the wallet-signed party on the coordinator's own participant
    /// (submitting the prepared topology transactions) and stage the bundle for
    /// fan-out.
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
            Self::WaitingForPeers | Self::PrepareTopology => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::PrepareTopology),
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
            Self::PrepareTopology => 1,
            Self::AllocatePeers => 2,
            Self::Complete => 3,
        }
    }

    fn step_total() -> i64 {
        4
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::PrepareTopology => "PrepareTopology",
            Self::AllocatePeers => "AllocatePeers",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        match name {
            "WaitingForPeers" => Some(Self::WaitingForPeers),
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
