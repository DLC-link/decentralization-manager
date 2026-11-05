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
/// with one-shot keys. The chosen signers announce a key each, sign the
/// rotation that makes those keys the party's signing keys, and sign the
/// prepared creates. The coordinator then publishes the rotation and executes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContractsStep {
    /// Waiting for all peers to connect
    WaitingForPeers,
    /// Coordinator prepares submissions
    PrepareSubmissions,
    /// Peers make a one-shot key each and send its public half
    AnnounceKeys,
    /// Coordinator picks the signers and builds and signs the key rotation
    BuildRotation,
    /// The chosen peers sign the rotation and the prepared submissions
    SignSubmissions,
    /// Coordinator checks the signatures and publishes the rotation
    PublishRotation,
    /// Coordinator executes submissions
    ExecuteSubmissions,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for ContractsStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::AnnounceKeys => Some(MessageType::AnnounceKey),
            Self::SignSubmissions => Some(MessageType::SignSubmissions),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers
            | Self::PrepareSubmissions
            | Self::BuildRotation
            | Self::PublishRotation
            | Self::ExecuteSubmissions => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::PrepareSubmissions),
            Self::PrepareSubmissions => Some(Self::AnnounceKeys),
            Self::AnnounceKeys => Some(Self::BuildRotation),
            Self::BuildRotation => Some(Self::SignSubmissions),
            Self::SignSubmissions => Some(Self::PublishRotation),
            Self::PublishRotation => Some(Self::ExecuteSubmissions),
            Self::ExecuteSubmissions => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        matches!(self, Self::AnnounceKeys | Self::SignSubmissions)
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::PrepareSubmissions => 1,
            Self::AnnounceKeys => 2,
            Self::BuildRotation => 3,
            Self::SignSubmissions => 4,
            Self::PublishRotation => 5,
            Self::ExecuteSubmissions => 6,
            Self::Complete => 7,
        }
    }

    fn step_total() -> i64 {
        8
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::PrepareSubmissions => "PrepareSubmissions",
            Self::AnnounceKeys => "AnnounceKeys",
            Self::BuildRotation => "BuildRotation",
            Self::SignSubmissions => "SignSubmissions",
            Self::PublishRotation => "PublishRotation",
            Self::ExecuteSubmissions => "ExecuteSubmissions",
            Self::Complete => "Complete",
        }
    }

    /// The one-shot keys and the signatures made with them live only in
    /// memory, so a run that restarts anywhere between announcing keys and
    /// publishing the rotation announces fresh keys.
    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "PrepareSubmissions" => Self::PrepareSubmissions,
            "AnnounceKeys" | "BuildRotation" | "SignSubmissions" | "PublishRotation" => {
                Self::AnnounceKeys
            }
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
    fn a_restart_between_announcing_and_publishing_announces_again() {
        for name in [
            "AnnounceKeys",
            "BuildRotation",
            "SignSubmissions",
            "PublishRotation",
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
