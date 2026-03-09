pub mod config;
pub mod coordinator;
pub mod dirs;

pub use config::DarsConfig;
pub use dirs::DarsDirs;

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
}
