use std::{collections::HashSet, sync::Arc};

use common::types::AcsTransferProgress;

use super::NoiseServer;
use crate::{
    canton_id::CantonId,
    noise::{Message, NoiseError},
    workflow::{
        add_party::AddPartyStep, change_threshold::ChangeThresholdStep, contracts::ContractsStep,
        dars::DarsStep, kick::KickStep, onboarding::OnboardingStep,
    },
};

/// A coordinator's in-flight workflow server, type-erased over the step type so
/// the single always-on Noise listener can route workflow-command messages to
/// it via `AppState.active_workflow`. An enum rather than a `dyn` trait object
/// because the codebase uses native `async fn in trait`, which is not
/// object-safe, and we avoid pulling in `async-trait`.
#[derive(Clone)]
pub enum ActiveWorkflow {
    Onboarding(Arc<NoiseServer<OnboardingStep>>),
    Kick(Arc<NoiseServer<KickStep>>),
    Contracts(Arc<NoiseServer<ContractsStep>>),
    Dars(Arc<NoiseServer<DarsStep>>),
    AddParty(Arc<NoiseServer<AddPartyStep>>),
    ChangeThreshold(Arc<NoiseServer<ChangeThresholdStep>>),
}

impl ActiveWorkflow {
    /// Route a workflow-command message to the underlying typed server.
    pub async fn handle_command(
        &self,
        peer_id: CantonId,
        message: Message,
    ) -> Result<Message, NoiseError> {
        match self {
            Self::Onboarding(s) => s.handle_command(peer_id, message).await,
            Self::Kick(s) => s.handle_command(peer_id, message).await,
            Self::Contracts(s) => s.handle_command(peer_id, message).await,
            Self::Dars(s) => s.handle_command(peer_id, message).await,
            Self::AddParty(s) => s.handle_command(peer_id, message).await,
            Self::ChangeThreshold(s) => s.handle_command(peer_id, message).await,
        }
    }

    /// Resolve once this run can no longer finish, with the reason. See
    /// [`NoiseServer::wait_until_stranded`].
    pub async fn wait_until_stranded(&self) -> String {
        match self {
            Self::Onboarding(s) => s.wait_until_stranded().await,
            Self::Kick(s) => s.wait_until_stranded().await,
            Self::Contracts(s) => s.wait_until_stranded().await,
            Self::Dars(s) => s.wait_until_stranded().await,
            Self::AddParty(s) => s.wait_until_stranded().await,
            Self::ChangeThreshold(s) => s.wait_until_stranded().await,
        }
    }

    /// The peers that have joined this run. `WaitingForPeers` gates on this
    /// set rather than on `completed_peers`, so it is the join progress the
    /// runs API reports while a run waits for its invitees.
    pub async fn connected_peers(&self) -> HashSet<CantonId> {
        match self {
            Self::Onboarding(s) => s.workflow_state.connected_peers().await,
            Self::Kick(s) => s.workflow_state.connected_peers().await,
            Self::Contracts(s) => s.workflow_state.connected_peers().await,
            Self::Dars(s) => s.workflow_state.connected_peers().await,
            Self::AddParty(s) => s.workflow_state.connected_peers().await,
            Self::ChangeThreshold(s) => s.workflow_state.connected_peers().await,
        }
    }

    /// Live ACS export progress for this run, when a transfer is open.
    ///
    /// Only AddParty replicates a party, so every other kind answers `None`
    /// without touching its state.
    pub async fn acs_export_progress(&self) -> Option<AcsTransferProgress> {
        match self {
            Self::AddParty(s) => s.workflow_state.acs_export_progress().await,
            _ => None,
        }
    }
}
