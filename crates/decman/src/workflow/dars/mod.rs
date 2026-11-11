pub mod config;
pub mod coordinator;

use crate::{
    noise::MessageType,
    server::WorkflowKind,
    workflow::state::{WorkflowStep, workflow_steps},
};

pub use config::DarsConfig;

workflow_steps! {
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
}

impl WorkflowStep for DarsStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::UploadDars => Some(MessageType::UploadDars),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers => None,
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::UploadDars
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Dars
    }
}
