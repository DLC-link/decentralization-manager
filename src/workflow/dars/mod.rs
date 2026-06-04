pub mod config;
pub mod coordinator;

pub use config::DarsConfig;

use crate::{noise::MessageType, server::WorkflowKind, workflow::state::WorkflowStep};

/// DARs upload workflow steps
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
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::UploadDars => Some(MessageType::UploadDars),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers => None,
        }
    }

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
