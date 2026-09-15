pub mod config;

pub use config::DarsConfig;

use crate::{server::WorkflowKind, workflow::state::WorkflowStep};

/// DARs workflow steps of the 1.x transport. Kept for the run cards of rows that
/// predate the 2.0 upgrade; the on-ledger engine has its own step lists
/// (`crate::onledger::engine::dars`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DarsStep {
    /// Waiting for all peers to connect
    WaitingForPeers,
    /// Upload DARs
    UploadDars,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for DarsStep {
    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::UploadDars),
            Self::UploadDars => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::UploadDars
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::UploadDars => 1,
            Self::Complete => 2,
        }
    }

    fn step_total() -> i64 {
        3
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::UploadDars => "UploadDars",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "UploadDars" => Self::UploadDars,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Dars
    }
}
