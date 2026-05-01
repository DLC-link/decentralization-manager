pub mod attestor;
pub mod config;
pub mod coordinator;
pub mod steps;

pub use config::{ContractDefinition, ContractsConfig, DarFile, FieldDefinition};
pub use steps::{
    execute_submissions, prepare_submissions, sign_submissions, upload_dars, upload_dars_from_bytes,
};

use crate::{noise::MessageType, workflow::state::WorkflowStep};

/// Contracts workflow steps (contract deployment only)
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContractsStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
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
            Self::SignSubmissions => Some(MessageType::SignSubmissions),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForAttestors | Self::PrepareSubmissions | Self::ExecuteSubmissions => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::PrepareSubmissions),
            Self::PrepareSubmissions => Some(Self::SignSubmissions),
            Self::SignSubmissions => Some(Self::ExecuteSubmissions),
            Self::ExecuteSubmissions => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        *self == Self::SignSubmissions
    }

    fn is_waiting_for_attestors(&self) -> bool {
        *self == Self::WaitingForAttestors
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForAttestors => 0,
            Self::PrepareSubmissions => 1,
            Self::SignSubmissions => 2,
            Self::ExecuteSubmissions => 3,
            Self::Complete => 4,
        }
    }

    fn step_total() -> i64 {
        5
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForAttestors => "WaitingForAttestors",
            Self::PrepareSubmissions => "PrepareSubmissions",
            Self::SignSubmissions => "SignSubmissions",
            Self::ExecuteSubmissions => "ExecuteSubmissions",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForAttestors" => Self::WaitingForAttestors,
            "PrepareSubmissions" => Self::PrepareSubmissions,
            "SignSubmissions" => Self::SignSubmissions,
            "ExecuteSubmissions" => Self::ExecuteSubmissions,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }
}
