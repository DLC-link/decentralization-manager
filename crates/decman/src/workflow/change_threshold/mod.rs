pub mod config;
pub mod coordinator;
pub mod peer;
pub mod steps;

use crate::{
    noise::MessageType,
    server::WorkflowKind,
    workflow::{
        state::{WorkflowStep, workflow_steps},
        storage::artifact_kinds,
        topology::DnsP2pArtifactKinds,
    },
};

pub use config::ChangeThresholdConfig;
pub use steps::{create_proposals, export_state, sign_proposals, submit_change};

/// The artefact kinds of a change-threshold run's DNS + P2P proposals and of each
/// member's signed pair.
pub const ARTIFACTS: DnsP2pArtifactKinds<'static> = DnsP2pArtifactKinds {
    dns_proposal: artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
    p2p_proposal: artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
    signed_dns: artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
    signed_p2p: artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
};

workflow_steps! {
    /// Change-threshold workflow steps (re-issuing an existing dec party's
    /// namespace + P2P threshold without changing its membership).
    ///
    /// Structurally identical to the kick workflow minus the removal: the
    /// coordinator exports the current namespace, builds new DNS + P2P proposals
    /// carrying the new threshold (same owners/participants), the members sign a
    /// quorum, and the coordinator submits.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub enum ChangeThresholdStep {
        /// Waiting for all peers to connect
        WaitingForPeers,
        /// Coordinator exports current namespace state
        ExportState,
        /// Coordinator creates the new-threshold proposals
        CreateProposals,
        /// Members sign the proposals
        SignProposals,
        /// Coordinator submits the change
        Submit,
        /// Members retire the shadowed PartyToKeyMapping after the new topology is effective.
        RetireLegacyKeys,
        /// Workflow complete
        Complete,
    }
}

impl WorkflowStep for ChangeThresholdStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::SignProposals => Some(MessageType::SignChangeThreshold),
            Self::RetireLegacyKeys => Some(MessageType::RetireLegacyKeys),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers | Self::ExportState | Self::CreateProposals | Self::Submit => {
                None
            }
        }
    }

    fn requires_peers(&self) -> bool {
        *self == Self::SignProposals
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::ChangeThreshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_advance_in_order_to_completion() {
        // Walking `next()` from the start lands on every step exactly once and
        // terminates at `Complete`.
        let mut step = ChangeThresholdStep::WaitingForPeers;
        let mut seen = vec![step];
        while let Some(next) = step.next() {
            seen.push(next);
            step = next;
        }
        assert_eq!(
            seen,
            vec![
                ChangeThresholdStep::WaitingForPeers,
                ChangeThresholdStep::ExportState,
                ChangeThresholdStep::CreateProposals,
                ChangeThresholdStep::SignProposals,
                ChangeThresholdStep::Submit,
                ChangeThresholdStep::RetireLegacyKeys,
                ChangeThresholdStep::Complete,
            ]
        );
        assert_eq!(seen.len() as i64, ChangeThresholdStep::step_total());
    }

    #[test]
    fn only_sign_step_requires_peer_status_quorum() {
        for step in [
            ChangeThresholdStep::WaitingForPeers,
            ChangeThresholdStep::ExportState,
            ChangeThresholdStep::CreateProposals,
            ChangeThresholdStep::SignProposals,
            ChangeThresholdStep::Submit,
            ChangeThresholdStep::RetireLegacyKeys,
            ChangeThresholdStep::Complete,
        ] {
            assert_eq!(
                step.requires_peers(),
                step == ChangeThresholdStep::SignProposals,
                "{step:?} peer requirement"
            );
        }
        assert_eq!(
            ChangeThresholdStep::SignProposals.to_command(),
            Some(MessageType::SignChangeThreshold)
        );
        assert_eq!(
            ChangeThresholdStep::Complete.to_command(),
            Some(MessageType::Disconnect)
        );
        assert_eq!(ChangeThresholdStep::CreateProposals.to_command(), None);
    }

    #[test]
    fn step_names_round_trip() {
        for step in [
            ChangeThresholdStep::WaitingForPeers,
            ChangeThresholdStep::ExportState,
            ChangeThresholdStep::CreateProposals,
            ChangeThresholdStep::SignProposals,
            ChangeThresholdStep::Submit,
            ChangeThresholdStep::RetireLegacyKeys,
            ChangeThresholdStep::Complete,
        ] {
            assert_eq!(
                ChangeThresholdStep::try_from_step_name(step.step_name()),
                Some(step)
            );
        }
        assert_eq!(ChangeThresholdStep::try_from_step_name("Nope"), None);
    }
}
