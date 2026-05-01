pub mod config;
pub mod coordinator;

pub use config::DarsConfig;

use crate::{noise::MessageType, workflow::state::WorkflowStep};

/// DARs upload workflow steps
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DarsStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
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
            Self::WaitingForAttestors => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::UploadDars),
            Self::UploadDars => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        *self == Self::UploadDars
    }

    fn is_waiting_for_attestors(&self) -> bool {
        *self == Self::WaitingForAttestors
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForAttestors => 0,
            Self::UploadDars => 1,
            Self::Complete => 2,
        }
    }

    fn step_total() -> i64 {
        3
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForAttestors => "WaitingForAttestors",
            Self::UploadDars => "UploadDars",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForAttestors" => Self::WaitingForAttestors,
            "UploadDars" => Self::UploadDars,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }
}
