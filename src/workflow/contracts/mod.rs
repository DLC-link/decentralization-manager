pub mod attestor;
pub mod config;
pub mod coordinator;
pub mod dirs;
pub mod steps;

pub use config::ContractsConfig;
pub use dirs::ContractsDirs;
pub use steps::{execute_submissions, prepare_submissions, sign_submissions, upload_dars};

use crate::{noise::MessageType, workflow::state::WorkflowStep};

/// Contracts workflow steps (DAR upload and contract creation)
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContractsStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
    /// Upload DARs
    UploadDars,
    /// Coordinator prepares submissions
    PrepareSubmissions,
    /// Sign submissions
    SignSubmissions,
    /// Coordinator executes submissions
    ExecuteSubmissions,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for ContractsStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::UploadDars => Some(MessageType::UploadDars),
            Self::SignSubmissions => Some(MessageType::SignSubmissions),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForAttestors | Self::PrepareSubmissions | Self::ExecuteSubmissions => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::UploadDars),
            Self::UploadDars => Some(Self::PrepareSubmissions),
            Self::PrepareSubmissions => Some(Self::SignSubmissions),
            Self::SignSubmissions => Some(Self::ExecuteSubmissions),
            Self::ExecuteSubmissions => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        *self == Self::UploadDars || *self == Self::SignSubmissions
    }

    fn is_waiting_for_attestors(&self) -> bool {
        *self == Self::WaitingForAttestors
    }
}
