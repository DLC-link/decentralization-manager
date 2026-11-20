pub mod config;
pub mod coordinator;
pub mod peer;
pub mod steps;

pub use config::{
    CommittedDeployment, ContractDefinition, ContractsConfig, DarFile, FieldDefinition,
};
pub use steps::{
    execute_submissions, prepare_submissions, sign_submissions, upload_dars, upload_dars_from_bytes,
};

use crate::{
    noise::MessageType,
    server::WorkflowKind,
    workflow::state::{WorkflowStep, workflow_steps},
};

workflow_steps! {
    /// Contracts workflow steps (contract deployment only)
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
}

impl WorkflowStep for ContractsStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::SignSubmissions => Some(MessageType::SignSubmissions),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers | Self::PrepareSubmissions | Self::ExecuteSubmissions => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::SignSubmissions
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Contracts
    }
}
