pub mod config;
pub mod steps;

pub use config::{ContractDefinition, ContractsConfig, DarFile, FieldDefinition};
pub use steps::{
    execute_submissions, prepare_submissions, sign_submissions, upload_dars, upload_dars_from_bytes,
};

use crate::{server::WorkflowKind, workflow::state::WorkflowStep};

/// Contracts workflow steps of the 1.x transport. Kept for the run cards of rows
/// that predate the 2.0 upgrade; the on-ledger engine has its own step
/// lists (`crate::onledger::engine::contracts`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContractsStep {
    /// Waiting for all peers to connect
    WaitingForPeers,
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
    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::PrepareSubmissions),
            Self::PrepareSubmissions => Some(Self::SignSubmissions),
            Self::SignSubmissions => Some(Self::ExecuteSubmissions),
            Self::ExecuteSubmissions => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::SignSubmissions
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
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
            Self::WaitingForPeers => "WaitingForPeers",
            Self::PrepareSubmissions => "PrepareSubmissions",
            Self::SignSubmissions => "SignSubmissions",
            Self::ExecuteSubmissions => "ExecuteSubmissions",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "PrepareSubmissions" => Self::PrepareSubmissions,
            "SignSubmissions" => Self::SignSubmissions,
            "ExecuteSubmissions" => Self::ExecuteSubmissions,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Contracts
    }
}
