pub mod config;
pub mod coordinator;
pub mod peer;
pub mod steps;

pub use config::RotateKeyConfig;

use crate::{
    noise::MessageType,
    server::WorkflowKind,
    workflow::{state::WorkflowStep, topology::RETIRED_LEGACY_KEYS_STATUS},
};

/// The status a member sends once it has recorded the coordinator's new key.
pub const RECORDED_KEY_STATUS: &[u8] = b"RecordRotatedKey completed";

/// Rotate-key workflow steps.
///
/// A member replaces its own Daml signing key for a party. The coordinator
/// creates the new key in its vault and proposes a P2P mapping that differs
/// from the current one only in that key. Every member signs it, the
/// coordinator submits it, and every member then records the new key, so a
/// later kick or rotation attributes it to the coordinator.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RotateKeyStep {
    /// Waiting for all peers to connect
    WaitingForPeers,
    /// Coordinator creates the new key in its vault
    GenerateKey,
    /// Coordinator builds and signs the P2P proposal
    CreateProposal,
    /// Members sign the proposal
    SignProposal,
    /// Coordinator submits the proposal
    Submit,
    /// Members retire the shadowed PartyToKeyMapping after the new topology is effective.
    RetireLegacyKeys,
    /// Members record the coordinator's new key
    RecordKey,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for RotateKeyStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::SignProposal => Some(MessageType::SignRotateKey),
            Self::RetireLegacyKeys => Some(MessageType::RetireLegacyKeys),
            Self::RecordKey => Some(MessageType::RecordRotatedKey),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers | Self::GenerateKey | Self::CreateProposal | Self::Submit => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::GenerateKey),
            Self::GenerateKey => Some(Self::CreateProposal),
            Self::CreateProposal => Some(Self::SignProposal),
            Self::SignProposal => Some(Self::Submit),
            Self::Submit => Some(Self::RetireLegacyKeys),
            Self::RetireLegacyKeys => Some(Self::RecordKey),
            Self::RecordKey => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    /// Recording waits for every member too. A member that misses it keeps
    /// the old key on record for the coordinator, and then refuses the
    /// coordinator's next rotation.
    fn requires_peers(&self) -> bool {
        matches!(self, Self::SignProposal | Self::RecordKey)
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::GenerateKey => 1,
            Self::CreateProposal => 2,
            Self::SignProposal => 3,
            Self::Submit => 4,
            Self::RetireLegacyKeys => 5,
            Self::RecordKey => 6,
            Self::Complete => 7,
        }
    }

    fn step_total() -> i64 {
        8
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::GenerateKey => "GenerateKey",
            Self::CreateProposal => "CreateProposal",
            Self::SignProposal => "SignProposal",
            Self::Submit => "Submit",
            Self::RetireLegacyKeys => "RetireLegacyKeys",
            Self::RecordKey => "RecordKey",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "GenerateKey" => Self::GenerateKey,
            "CreateProposal" => Self::CreateProposal,
            "SignProposal" => Self::SignProposal,
            "Submit" => Self::Submit,
            "RetireLegacyKeys" => Self::RetireLegacyKeys,
            "RecordKey" => Self::RecordKey,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::RotateKey
    }

    /// A status counts only for the step whose command asked for it. A
    /// member's late reply to RetireLegacyKeys would otherwise complete
    /// RecordKey, and that member would never record the new key. A member
    /// completes SignProposal with its upload, never with a status.
    fn accepts_status(&self, status: &[u8]) -> bool {
        match self {
            Self::RetireLegacyKeys => status == RETIRED_LEGACY_KEYS_STATUS,
            Self::RecordKey => status == RECORDED_KEY_STATUS,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [RotateKeyStep; 8] = [
        RotateKeyStep::WaitingForPeers,
        RotateKeyStep::GenerateKey,
        RotateKeyStep::CreateProposal,
        RotateKeyStep::SignProposal,
        RotateKeyStep::Submit,
        RotateKeyStep::RetireLegacyKeys,
        RotateKeyStep::RecordKey,
        RotateKeyStep::Complete,
    ];

    #[test]
    fn each_step_counts_only_its_own_status() {
        assert!(RotateKeyStep::RetireLegacyKeys.accepts_status(RETIRED_LEGACY_KEYS_STATUS));
        assert!(RotateKeyStep::RecordKey.accepts_status(RECORDED_KEY_STATUS));

        assert!(!RotateKeyStep::RecordKey.accepts_status(RETIRED_LEGACY_KEYS_STATUS));
        assert!(!RotateKeyStep::RetireLegacyKeys.accepts_status(RECORDED_KEY_STATUS));
        for step in ALL {
            if !matches!(
                step,
                RotateKeyStep::RetireLegacyKeys | RotateKeyStep::RecordKey
            ) {
                assert!(!step.accepts_status(RETIRED_LEGACY_KEYS_STATUS), "{step:?}");
                assert!(!step.accepts_status(RECORDED_KEY_STATUS), "{step:?}");
            }
        }
    }

    #[test]
    fn steps_advance_in_order_to_completion() {
        let mut step = RotateKeyStep::WaitingForPeers;
        let mut seen = vec![step];
        while let Some(next) = step.next() {
            seen.push(next);
            step = next;
        }
        assert_eq!(seen, ALL);
        assert_eq!(seen.len() as i64, RotateKeyStep::step_total());
        for (index, step) in ALL.iter().enumerate() {
            assert_eq!(step.step_index(), index as i64);
        }
    }

    #[test]
    fn signing_and_recording_wait_for_the_peers() {
        for step in ALL {
            assert_eq!(
                step.requires_peers(),
                matches!(step, RotateKeyStep::SignProposal | RotateKeyStep::RecordKey),
                "{step:?} peer requirement"
            );
        }
        assert_eq!(
            RotateKeyStep::SignProposal.to_command(),
            Some(MessageType::SignRotateKey)
        );
        assert_eq!(
            RotateKeyStep::RecordKey.to_command(),
            Some(MessageType::RecordRotatedKey)
        );
        assert_eq!(RotateKeyStep::GenerateKey.to_command(), None);
    }

    #[test]
    fn step_names_round_trip() {
        for step in ALL {
            assert_eq!(
                RotateKeyStep::try_from_step_name(step.step_name()),
                Some(step)
            );
        }
        assert_eq!(RotateKeyStep::try_from_step_name("Nope"), None);
    }
}
