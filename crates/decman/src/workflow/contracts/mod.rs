pub mod config;
pub mod coordinator;
pub mod steps;

pub use config::{
    CommittedDeployment, ContractDefinition, ContractsConfig, DarFile, FieldDefinition,
};
pub use steps::{execute_submissions, prepare_submissions, upload_dars, upload_dars_from_bytes};

use crate::{noise::MessageType, server::WorkflowKind, workflow::state::WorkflowStep};

/// Contracts workflow steps (contract deployment only)
///
/// The party's Daml key cannot leave a Canton 3.7 vault, so each run signs
/// with one-shot keys. The chosen signers announce a key each and sign the
/// rotation that makes those keys the party's signing keys. Once the rotation
/// is in force, the coordinator prepares the creates, the same signers sign
/// them, and the coordinator executes. Canton prepares a transaction for the
/// party only while it has signing keys, so the rotation comes first.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContractsStep {
    /// Waiting for all peers to connect
    WaitingForPeers,
    /// Peers make a one-shot key each and send its public half
    AnnounceKeys,
    /// Coordinator picks the signers and builds and signs the key rotation
    BuildRotation,
    /// The chosen peers sign the rotation and keep their key
    SignRotation,
    /// Coordinator checks the signatures and publishes the rotation
    PublishRotation,
    /// Coordinator prepares submissions and signs them
    PrepareSubmissions,
    /// The chosen peers sign the prepared submissions and drop their key
    SignSubmissions,
    /// Coordinator executes submissions
    ExecuteSubmissions,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for ContractsStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::AnnounceKeys => Some(MessageType::AnnounceKey),
            Self::SignRotation => Some(MessageType::SignRotation),
            Self::SignSubmissions => Some(MessageType::SignSubmissions),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers
            | Self::BuildRotation
            | Self::PublishRotation
            | Self::PrepareSubmissions
            | Self::ExecuteSubmissions => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::AnnounceKeys),
            Self::AnnounceKeys => Some(Self::BuildRotation),
            Self::BuildRotation => Some(Self::SignRotation),
            Self::SignRotation => Some(Self::PublishRotation),
            Self::PublishRotation => Some(Self::PrepareSubmissions),
            Self::PrepareSubmissions => Some(Self::SignSubmissions),
            Self::SignSubmissions => Some(Self::ExecuteSubmissions),
            Self::ExecuteSubmissions => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        matches!(
            self,
            Self::AnnounceKeys | Self::SignRotation | Self::SignSubmissions
        )
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::AnnounceKeys => 1,
            Self::BuildRotation => 2,
            Self::SignRotation => 3,
            Self::PublishRotation => 4,
            Self::PrepareSubmissions => 5,
            Self::SignSubmissions => 6,
            Self::ExecuteSubmissions => 7,
            Self::Complete => 8,
        }
    }

    fn step_total() -> i64 {
        9
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::AnnounceKeys => "AnnounceKeys",
            Self::BuildRotation => "BuildRotation",
            Self::SignRotation => "SignRotation",
            Self::PublishRotation => "PublishRotation",
            Self::PrepareSubmissions => "PrepareSubmissions",
            Self::SignSubmissions => "SignSubmissions",
            Self::ExecuteSubmissions => "ExecuteSubmissions",
            Self::Complete => "Complete",
        }
    }

    /// The one-shot keys live only in memory until the creates are signed, so
    /// a run that restarts anywhere before execution announces fresh keys.
    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "AnnounceKeys" | "BuildRotation" | "SignRotation" | "PublishRotation"
            | "PrepareSubmissions" | "SignSubmissions" => Self::AnnounceKeys,
            "ExecuteSubmissions" => Self::ExecuteSubmissions,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::Contracts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restart_before_execution_announces_again() {
        for name in [
            "AnnounceKeys",
            "BuildRotation",
            "SignRotation",
            "PublishRotation",
            "PrepareSubmissions",
            "SignSubmissions",
        ] {
            assert_eq!(
                ContractsStep::try_from_step_name(name),
                Some(ContractsStep::AnnounceKeys),
                "{name}"
            );
        }
        assert_eq!(
            ContractsStep::try_from_step_name("ExecuteSubmissions"),
            Some(ContractsStep::ExecuteSubmissions)
        );
    }

    #[test]
    fn the_steps_run_in_index_order() {
        let mut step = ContractsStep::WaitingForPeers;
        let mut index = 0;
        while let Some(next) = step.next() {
            assert_eq!(step.step_index(), index);
            index += 1;
            step = next;
        }
        assert_eq!(step.step_index() + 1, ContractsStep::step_total());
    }
}
