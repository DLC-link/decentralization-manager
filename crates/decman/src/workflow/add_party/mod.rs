pub mod config;
pub mod coordinator;
pub mod peer;
pub mod steps;

pub use config::AddPartyConfig;
pub use steps::{
    ClearOutcome, author_clear_proposal, clear_onboarding_flag, generate_keys, import_party_acs,
    sign_clear_proposal, sign_proposals,
};

use crate::{
    auth::WorkflowAuth, canton_id::CantonId, noise::MessageType, server::WorkflowKind,
    workflow::state::WorkflowStep,
};

/// Resolve a ledger-API token for `party` from the node's auth registry.
/// Best-effort: the token only feeds the begin-offset capture's primary
/// tier; without one the capture degrades to the admin-API tiers.
pub(crate) async fn resolve_ledger_token(
    auth: &Option<WorkflowAuth>,
    party: &CantonId,
) -> Option<String> {
    let auth = auth.as_ref()?;
    match auth.get_credentials(party).await {
        Ok(creds) => Some(creds.token),
        Err(e) => {
            tracing::warn!(
                "No ledger credentials for {party} ({e}); offset capture will use \
                 the tokenless tiers"
            );
            None
        }
    }
}

/// Add-party workflow steps (adding a new member to an existing
/// decentralized party).
///
/// Peer-gated steps come in two shapes:
/// - **all-peer** (`SignProposals`, `SignClearOnboarding`): every invited
///   peer (existing members + the new member) does the work;
/// - **new-member-only** (`GenerateNewMemberKeys`, `SyncAcs`,
///   `ProposeClearOnboarding`): only the new member acts; the other peers
///   recognise from the config payload that the command isn't addressed to
///   them and reply with a skip status so the all-peers-complete gate still
///   fires.
///
/// `PrepareClearOnboarding` / `PrepareClearSign` exist because the generic
/// state machine auto-advances out of a peer-gated step the moment the last
/// peer completes — two consecutive peer-gated steps would leave the previous
/// step's `command_payload` being served for the next command. Each Prepare
/// step is a coordinator-only beat that swaps the payload before the next
/// peer-gated step begins.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AddPartyStep {
    /// Waiting for all peers (existing members + new member) to connect
    WaitingForPeers,
    /// New member generates its namespace + DAML keys and uploads them
    GenerateNewMemberKeys,
    /// Coordinator exports current DNS + P2P state and validates the add
    ExportState,
    /// Coordinator creates the updated DNS + P2P proposals
    CreateProposals,
    /// All peers sign both proposals
    SignProposals,
    /// Coordinator submits DNS then P2P and exports the party's ACS
    SubmitProposals,
    /// New member imports the ACS snapshot (skipped when empty)
    SyncAcs,
    /// Coordinator swaps the command payload for the clear-flag phase
    PrepareClearOnboarding,
    /// New member drives `ClearPartyOnboardingFlag` past Canton's safe time
    ProposeClearOnboarding,
    /// Coordinator creates the clearing proposal (or a skip marker)
    PrepareClearSign,
    /// All peers sign the clearing proposal (no-op on skip marker)
    SignClearOnboarding,
    /// Coordinator submits the clearing proposal and waits for the flag to drop
    SubmitClearOnboarding,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for AddPartyStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::GenerateNewMemberKeys => Some(MessageType::GenerateAddPartyKeys),
            Self::SignProposals => Some(MessageType::SignAddParty),
            Self::SyncAcs => Some(MessageType::ImportAcs),
            Self::ProposeClearOnboarding => Some(MessageType::ClearOnboardingFlag),
            Self::SignClearOnboarding => Some(MessageType::SignClearOnboarding),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForPeers
            | Self::ExportState
            | Self::CreateProposals
            | Self::SubmitProposals
            | Self::PrepareClearOnboarding
            | Self::PrepareClearSign
            | Self::SubmitClearOnboarding => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForPeers => Some(Self::GenerateNewMemberKeys),
            Self::GenerateNewMemberKeys => Some(Self::ExportState),
            Self::ExportState => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignProposals),
            Self::SignProposals => Some(Self::SubmitProposals),
            Self::SubmitProposals => Some(Self::SyncAcs),
            Self::SyncAcs => Some(Self::PrepareClearOnboarding),
            Self::PrepareClearOnboarding => Some(Self::ProposeClearOnboarding),
            Self::ProposeClearOnboarding => Some(Self::PrepareClearSign),
            Self::PrepareClearSign => Some(Self::SignClearOnboarding),
            Self::SignClearOnboarding => Some(Self::SubmitClearOnboarding),
            Self::SubmitClearOnboarding => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_peers(&self) -> bool {
        matches!(
            self,
            Self::GenerateNewMemberKeys
                | Self::SignProposals
                | Self::SyncAcs
                | Self::ProposeClearOnboarding
                | Self::SignClearOnboarding
        )
    }

    fn is_waiting_for_peers(&self) -> bool {
        *self == Self::WaitingForPeers
    }

    fn step_index(&self) -> i64 {
        match self {
            Self::WaitingForPeers => 0,
            Self::GenerateNewMemberKeys => 1,
            Self::ExportState => 2,
            Self::CreateProposals => 3,
            Self::SignProposals => 4,
            Self::SubmitProposals => 5,
            Self::SyncAcs => 6,
            Self::PrepareClearOnboarding => 7,
            Self::ProposeClearOnboarding => 8,
            Self::PrepareClearSign => 9,
            Self::SignClearOnboarding => 10,
            Self::SubmitClearOnboarding => 11,
            Self::Complete => 12,
        }
    }

    fn step_total() -> i64 {
        13
    }

    fn step_name(&self) -> &'static str {
        match self {
            Self::WaitingForPeers => "WaitingForPeers",
            Self::GenerateNewMemberKeys => "GenerateNewMemberKeys",
            Self::ExportState => "ExportState",
            Self::CreateProposals => "CreateProposals",
            Self::SignProposals => "SignProposals",
            Self::SubmitProposals => "SubmitProposals",
            Self::SyncAcs => "SyncAcs",
            Self::PrepareClearOnboarding => "PrepareClearOnboarding",
            Self::ProposeClearOnboarding => "ProposeClearOnboarding",
            Self::PrepareClearSign => "PrepareClearSign",
            Self::SignClearOnboarding => "SignClearOnboarding",
            Self::SubmitClearOnboarding => "SubmitClearOnboarding",
            Self::Complete => "Complete",
        }
    }

    fn try_from_step_name(name: &str) -> Option<Self> {
        Some(match name {
            "WaitingForPeers" => Self::WaitingForPeers,
            "GenerateNewMemberKeys" => Self::GenerateNewMemberKeys,
            "ExportState" => Self::ExportState,
            "CreateProposals" => Self::CreateProposals,
            "SignProposals" => Self::SignProposals,
            "SubmitProposals" => Self::SubmitProposals,
            "SyncAcs" => Self::SyncAcs,
            "PrepareClearOnboarding" => Self::PrepareClearOnboarding,
            "ProposeClearOnboarding" => Self::ProposeClearOnboarding,
            "PrepareClearSign" => Self::PrepareClearSign,
            "SignClearOnboarding" => Self::SignClearOnboarding,
            "SubmitClearOnboarding" => Self::SubmitClearOnboarding,
            "Complete" => Self::Complete,
            _ => return None,
        })
    }

    fn kind() -> WorkflowKind {
        WorkflowKind::AddParty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must round-trip through its persisted step name — a
    /// mismatch would break resume-after-restart for runs stopped on that
    /// step.
    #[test]
    fn step_names_round_trip() {
        let mut step = AddPartyStep::WaitingForPeers;
        let mut seen = 0;
        loop {
            assert_eq!(
                AddPartyStep::try_from_step_name(step.step_name()),
                Some(step)
            );
            seen += 1;
            match step.next() {
                Some(next) => step = next,
                None => break,
            }
        }
        assert_eq!(seen, AddPartyStep::step_total());
        assert!(AddPartyStep::try_from_step_name("NotAStep").is_none());
    }

    /// Step indices must be unique and dense (0..step_total) — the frontend
    /// renders progress as `step_index + 1 / step_total`.
    #[test]
    fn step_indices_are_dense_and_follow_next_order() {
        let mut step = AddPartyStep::WaitingForPeers;
        let mut expected_index = 0;
        loop {
            assert_eq!(step.step_index(), expected_index);
            expected_index += 1;
            match step.next() {
                Some(next) => step = next,
                None => break,
            }
        }
        assert_eq!(expected_index, AddPartyStep::step_total());
    }

    /// Peer-gated steps must all map to a command, and no two consecutive
    /// peer-gated steps may share the chain without a coordinator step in
    /// between *unless* the coordinator prepared the payload beforehand. The
    /// structural invariant we can check here: every `requires_peers` step
    /// has a command, and Prepare steps have none.
    #[test]
    fn peer_gated_steps_have_commands() {
        let mut step = AddPartyStep::WaitingForPeers;
        loop {
            if step.requires_peers() {
                assert!(
                    step.to_command().is_some(),
                    "{step:?} is peer-gated but has no command"
                );
            }
            match step.next() {
                Some(next) => step = next,
                None => break,
            }
        }
        assert!(
            AddPartyStep::PrepareClearOnboarding.to_command().is_none(),
            "Prepare steps are coordinator-only beats"
        );
        assert!(AddPartyStep::PrepareClearSign.to_command().is_none());
    }
}
