use std::{collections::HashSet, marker::PhantomData, sync::Arc, time::Instant};

use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::SchemaRead,
    noise::{
        CHUNK_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType, NoiseError, NoiseKeypair,
        parse_public_key, send_noise_message,
    },
    server::{
        DeclineInvitationPayload, WorkflowKind, WorkflowProgress,
        peer_status::{LastSeen, bump},
    },
    workflow::{
        WorkflowState, add_party::AddPartyStep, change_threshold::ChangeThresholdStep,
        contracts::ContractsStep, dars::DarsStep, kick::KickStep, onboarding::OnboardingStep,
        state::WorkflowStep,
    },
};

/// Whether an outgoing command should carry the workflow's `command_payload`.
///
/// `Disconnect` is a pure control signal marking the end of a workflow and
/// must never inherit the residual payload from an earlier step (e.g. the
/// DAR bundle from `UploadDars`). Shipping it would turn a small control
/// message into a multi-MB chunked transfer and delay the peer's invite
/// listener from resuming, which has caused Contracts-invite races in CI.
const fn command_carries_payload(command: MessageType) -> bool {
    !matches!(command, MessageType::Disconnect)
}

/// Whether a peer's invitation decline targets THIS coordinator run. The
/// workflow kind must match, and when the decline echoes the run identity
/// (carried on invites since `workflow_instance` was added) it must match
/// the active run's instance name; declines from older peers without the
/// field pass on the kind check alone.
fn decline_matches_run(
    payload: &DeclineInvitationPayload,
    run_kind: WorkflowKind,
    run_instance: &str,
) -> bool {
    if payload.kind != run_kind {
        return false;
    }
    match &payload.workflow_instance {
        Some(instance) => instance == run_instance,
        None => true,
    }
}

/// Coordinator server that accepts connections from peers
pub struct NoiseServer<S: WorkflowStep + 'static> {
    node_config: Arc<NodeConfig>,
    keypair: Arc<NoiseKeypair>,
    /// Full peer records (address + port + public_key) kept around so the
    /// server can fan out broadcast notifications (e.g. CancelInvite after
    /// a DeclineInvitation) without going through the DB on the hot path.
    peers: Vec<Peer>,
    workflow_state: Arc<WorkflowState<S>>,
    last_seen: LastSeen,
    _p: PhantomData<S>,
}

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
}

impl<S: WorkflowStep + 'static> NoiseServer<S> {
    /// Create a new Noise server
    ///
    /// # Arguments
    /// * `node_config` - Node configuration
    /// * `network_config` - Network configuration
    /// * `db` - SQLite pool used to persist `WorkflowState` updates against the
    ///   matching `workflow_runs` row.
    /// * `instance_name` - Identifier for the persisted run (matches the row's
    ///   primary key).
    /// * `initial_step` - Initial workflow step
    /// * `exclude_participants` - Optional list of participant IDs to exclude from peers (e.g., participants being kicked)
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        node_config: NodeConfig,
        network_config: NetworkConfig,
        db: SqlitePool,
        instance_name: String,
        initial_step: S,
        exclude_participants: Option<Vec<String>>,
        last_seen: LastSeen,
    ) -> Result<Self, NoiseError> {
        let keypair = NoiseKeypair::from_file(&node_config.key_file_path()).await?;

        let excluded: HashSet<String> = exclude_participants
            .unwrap_or_default()
            .into_iter()
            .collect();

        if !excluded.is_empty() {
            tracing::info!(
                "Excluding {count} participant(s) from peers: {participants}",
                count = excluded.len(),
                participants = excluded.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }

        // Look up the persisted run up-front. Its `expected_peers` is the
        // authoritative invitee set the start handler selected for *this* run,
        // which can be a strict subset of the configured mesh — onboarding and
        // dars both invite a chosen subset of the peers this node knows about.
        // Deriving the wait set from the full configured mesh instead made the
        // coordinator wait in `WaitingForPeers` for peers it was never going to
        // invite and that can never connect, so any party smaller than the full
        // mesh (e.g. 1 coordinator + 1 peer) hung forever.
        // Prefer the persisted invitees; the coordinator always
        // inserts its row before constructing this server, so the
        // configured-mesh fallback only covers the (unexpected) no-row case.
        let persisted = match SchemaRead::get_workflow_run(&db, &instance_name).await {
            Ok(run) => run,
            Err(e) => {
                tracing::warn!(
                    "Failed to look up persisted workflow_runs row for {instance_name}: {e}; \
                     falling back to configured peers"
                );
                None
            }
        };

        let expected_peers: Vec<CantonId> = match persisted.as_ref() {
            Some(run) => run.expected_peers.clone(),
            None => network_config
                .peers
                .iter()
                .filter(|p| {
                    p.participant_id != *node_config.participant_id()
                        && !excluded.contains(&p.participant_id.to_string())
                })
                .map(|p| p.participant_id.clone())
                .collect(),
        };

        tracing::info!(
            "Expected {count} peer(s): {peers}",
            count = expected_peers.len(),
            peers = expected_peers
                .iter()
                .map(CantonId::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Scope the fan-out peer records to this run's participant set so
        // best-effort broadcasts (e.g. CancelInvite on decline) only reach the
        // peers actually invited to this run, never unrelated configured peers.
        let expected_set: HashSet<CantonId> = expected_peers.iter().cloned().collect();
        let peers = network_config
            .peers
            .iter()
            .filter(|p| expected_set.contains(&p.participant_id))
            .cloned()
            .collect();

        // Resume-aware construction: if an InProgress workflow_runs row already
        // exists for `instance_name` (we restarted mid-flight), re-hydrate the
        // state machine from its persisted `current_step` + already-completed
        // peers instead of starting fresh from `initial_step`. The
        // coordinator workflow loop is naturally driven by `current_step`, so
        // seeding it from the row picks the run back up at the right place.
        let workflow_state = match persisted {
            Some(run) if run.status == WorkflowProgress::InProgress => {
                match S::try_from_step_name(&run.current_step) {
                    Some(step) => {
                        tracing::info!(
                            "Resuming workflow {instance_name} at step {step:?} \
                             ({} of {} peers completed)",
                            run.completed_peers.len(),
                            expected_peers.len()
                        );
                        WorkflowState::from_persisted(
                            db,
                            instance_name,
                            step,
                            expected_peers,
                            run.completed_peers,
                        )
                    }
                    None => {
                        tracing::warn!(
                            "Persisted current_step {:?} for {instance_name} is not a valid \
                             {kind} step; starting fresh from {initial_step:?}",
                            run.current_step,
                            kind = std::any::type_name::<S>()
                        );
                        WorkflowState::new(db, instance_name, initial_step, expected_peers)
                    }
                }
            }
            _ => WorkflowState::new(db, instance_name, initial_step, expected_peers),
        };

        Ok(Self {
            node_config: Arc::new(node_config),
            keypair: Arc::new(keypair),
            peers,
            workflow_state,
            last_seen,
            _p: PhantomData,
        })
    }

    pub fn get_workflow_state(&self) -> Arc<WorkflowState<S>> {
        self.workflow_state.clone()
    }

    /// Handle a workflow-command message routed from the always-on listener.
    /// These are exactly the workflow arms of the former `handle_request`,
    /// relocated unchanged.
    pub async fn handle_command(
        &self,
        peer_id: CantonId,
        message: Message,
    ) -> Result<Message, NoiseError> {
        // Track liveness of the peer we exchange workflow commands with.
        {
            let now = Instant::now();
            let mut map = self.last_seen.write().await;
            bump(&mut map, peer_id.to_string(), now);
        }

        match message.msg_type {
            MessageType::GetNextCommand => self.handle_get_next_command(peer_id).await,
            MessageType::GetChunk => self.handle_get_chunk(message.payload).await,
            MessageType::KeysUpload => {
                self.handle_peer_data(peer_id, message.payload, "keys upload")
                    .await
            }
            MessageType::DnsSignature => {
                self.handle_peer_data(peer_id, message.payload, "DNS signature")
                    .await
            }
            MessageType::P2pSignatures => {
                self.handle_peer_data(peer_id, message.payload, "P2P signatures")
                    .await
            }
            MessageType::SubmissionSignatures => {
                self.handle_peer_data(peer_id, message.payload, "submission signatures")
                    .await
            }
            MessageType::KickSignatures => {
                self.handle_peer_data(peer_id, message.payload, "kick signatures")
                    .await
            }
            MessageType::AddPartyKeysUpload => {
                self.handle_peer_data(peer_id, message.payload, "add-party keys upload")
                    .await
            }
            MessageType::AddPartySignatures => {
                self.handle_peer_data(peer_id, message.payload, "add-party signatures")
                    .await
            }
            MessageType::AddPartyClearSignatures => {
                self.handle_peer_data(peer_id, message.payload, "add-party clearing signature")
                    .await
            }
            MessageType::AddPartyClearProposal => {
                self.handle_peer_data(peer_id, message.payload, "add-party clearing proposal")
                    .await
            }
            MessageType::ChangeThresholdSignatures => {
                self.handle_peer_data(peer_id, message.payload, "change-threshold signatures")
                    .await
            }
            MessageType::StatusUpdate => self.handle_status_update(peer_id, message.payload).await,
            MessageType::DeclineInvitation => Ok(self
                .handle_decline_invitation(peer_id, message.payload)
                .await),
            other => {
                tracing::warn!("Unsupported routed workflow message: {other:?}");
                Ok(Message::new(
                    MessageType::Error,
                    b"Unsupported message type".to_vec(),
                ))
            }
        }
    }

    /// Handle peer requesting next command
    async fn handle_get_next_command(&self, peer_id: CantonId) -> Result<Message, NoiseError> {
        // Mark peer as connected on first command poll
        self.workflow_state.peer_connected(peer_id.clone()).await;

        // Check if peer has already completed current step
        let has_completed = self.workflow_state.has_peer_completed(&peer_id).await;
        if has_completed {
            // Peer has completed current step, tell them to wait
            tracing::debug!("Sending Wait to {peer_id} (already completed current step)");
            return Ok(Message::new_empty(MessageType::Wait));
        }

        // Get current command from workflow state
        if let Some(command) = self.workflow_state.current_command().await {
            tracing::info!("Sending command {command:?} to {peer_id}");
            // Include payload only for commands that carry data (e.g. UploadDars,
            // SignDns). See `command_carries_payload` for why Disconnect is excluded.
            let payload = if command_carries_payload(command) {
                self.workflow_state.get_command_payload().await
            } else {
                Vec::new()
            };
            if payload.is_empty() {
                Ok(Message::new_empty(command))
            } else if payload.len() <= MAX_PAYLOAD_SIZE {
                // Small payload - send directly
                Ok(Message::new(command, payload))
            } else {
                // Large payload - use chunked transfer
                let total_size = payload.len() as u32;
                let chunk_count = payload.len().div_ceil(CHUNK_SIZE) as u32;
                tracing::info!(
                    "Payload too large ({total_size} bytes), using chunked transfer ({chunk_count} chunks)"
                );

                // Build ChunkedCommand payload: [command_type (2 bytes)] [total_size (4 bytes)] [chunk_count (4 bytes)]
                let mut meta = Vec::with_capacity(10);
                meta.extend_from_slice(&command.to_u16().to_be_bytes());
                meta.extend_from_slice(&total_size.to_be_bytes());
                meta.extend_from_slice(&chunk_count.to_be_bytes());

                Ok(Message::new(MessageType::ChunkedCommand, meta))
            }
        } else {
            // No command for peers right now (coordinator-only step)
            tracing::debug!("Sending Wait to {peer_id} (coordinator-only step)");
            Ok(Message::new_empty(MessageType::Wait))
        }
    }

    /// Handle chunk request from peer
    async fn handle_get_chunk(&self, request_payload: Vec<u8>) -> Result<Message, NoiseError> {
        if request_payload.len() < 4 {
            return Err(NoiseError::InvalidMessage);
        }

        let chunk_index = u32::from_be_bytes([
            request_payload[0],
            request_payload[1],
            request_payload[2],
            request_payload[3],
        ]) as usize;

        let payload = self.workflow_state.get_command_payload().await;
        let start = chunk_index * CHUNK_SIZE;

        if start >= payload.len() {
            return Err(NoiseError::InvalidMessage);
        }

        let end = std::cmp::min(start + CHUNK_SIZE, payload.len());
        let chunk_data = &payload[start..end];

        tracing::debug!(
            "Sending chunk {chunk_index} ({start}..{end}, {} bytes)",
            chunk_data.len()
        );

        // Build Chunk response: [chunk_index (4 bytes)] [chunk_data (variable)]
        let mut response = Vec::with_capacity(4 + chunk_data.len());
        response.extend_from_slice(&(chunk_index as u32).to_be_bytes());
        response.extend_from_slice(chunk_data);

        Ok(Message::new(MessageType::Chunk, response))
    }

    /// Handle peer data upload (keys, signatures, etc.)
    async fn handle_peer_data(
        &self,
        peer_id: CantonId,
        payload: Vec<u8>,
        data_type: &str,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling {data_type} from {peer_id}");

        self.workflow_state
            .store_peer_data(peer_id.clone(), payload)
            .await;

        self.workflow_state.peer_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle status update from peer
    async fn handle_status_update(
        &self,
        peer_id: CantonId,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        let status = String::from_utf8_lossy(&payload);
        tracing::info!("Handling status update from {peer_id}: {status}");

        // Mark peer as completed for this step
        self.workflow_state.peer_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle a peer-initiated decline of an outstanding invitation. Fails
    /// this coordinator run with a descriptive error so the UI doesn't have
    /// to wait for the timeout path, then broadcasts a `CancelInvite` to the
    /// remaining peers so their pending invites / in-flight peer runs roll
    /// back instead of hanging. Always replies with `Ack` — the declining
    /// peer treats the send as fire-and-forget.
    ///
    /// The decline is validated before failing the run: the always-on
    /// listener routes EVERY `DeclineInvitation` to whatever workflow is
    /// currently active, so a peer denying a STALE invitation card (left
    /// over from an earlier run that failed without a `CancelInvite`) must
    /// not kill an unrelated run that happens to be active now.
    async fn handle_decline_invitation(&self, peer_id: CantonId, payload: Vec<u8>) -> Message {
        let payload = serde_json::from_slice::<DeclineInvitationPayload>(&payload).ok();

        // Only peers this run actually invited may fail it.
        if !self.peers.iter().any(|p| p.participant_id == peer_id) {
            tracing::warn!(
                "Ignoring invitation decline from {peer_id}: not a participant of run {}",
                self.workflow_state.instance_name()
            );
            return Message::new_empty(MessageType::Ack);
        }

        let Some(payload) = payload else {
            tracing::warn!("Ignoring invitation decline from {peer_id}: unparseable payload");
            return Message::new_empty(MessageType::Ack);
        };

        if !decline_matches_run(&payload, S::kind(), self.workflow_state.instance_name()) {
            tracing::warn!(
                "Ignoring invitation decline from {peer_id} for {kind:?} run {instance:?}: \
                 active run is {active_kind:?} {active_instance}",
                kind = payload.kind,
                instance = payload.workflow_instance,
                active_kind = S::kind(),
                active_instance = self.workflow_state.instance_name()
            );
            return Message::new_empty(MessageType::Ack);
        }

        let msg = match &payload.reason {
            Some(r) => format!("Peer {peer_id} declined the invitation: {r}"),
            None => format!("Peer {peer_id} declined the invitation"),
        };
        tracing::warn!("{msg}");
        self.workflow_state.mark_failed(msg).await;

        self.broadcast_cancel_to_others(&peer_id).await;

        Message::new_empty(MessageType::Ack)
    }

    /// Best-effort fan-out of `CancelInvite` to every peer we expected to
    /// participate in this run *except* the one whose decline triggered the
    /// teardown. Their listener treats `CancelInvite` as "drop the invite
    /// and mark any in-progress peer run as Cancelled" (see
    /// `Triggers::cancel_peer_runs_from`), which is the same outcome we want
    /// here. Failures are logged and ignored.
    async fn broadcast_cancel_to_others(&self, declining_peer: &CantonId) {
        let identity = self.node_config.participant_id().to_string();
        let identity_bytes = identity.as_bytes();
        // Stamp this run's instance so peers cancel only THIS run's
        // invite/peer-run — a sibling concurrent run from the same
        // coordinator must survive the teardown.
        let message = Message::new_empty(MessageType::CancelInvite)
            .with_instance(self.workflow_state.instance_name());

        for peer in &self.peers {
            if &peer.participant_id == declining_peer {
                continue;
            }
            if peer.public_key.is_empty() {
                continue;
            }
            let Ok(peer_pub_key) = parse_public_key(&peer.public_key) else {
                continue;
            };
            let psk = self.keypair.derive_psk(&peer_pub_key);
            if let Err(e) =
                send_noise_message(&peer.address, peer.port, &psk, identity_bytes, &message).await
            {
                tracing::warn!(
                    "Best-effort CancelInvite to {} after decline failed: {e}",
                    peer.participant_id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_never_carries_a_payload() {
        assert!(!command_carries_payload(MessageType::Disconnect));
    }

    #[test]
    fn data_carrying_commands_preserve_their_payload() {
        assert!(command_carries_payload(MessageType::UploadDars));
        assert!(command_carries_payload(MessageType::SignDns));
        assert!(command_carries_payload(MessageType::SignP2p));
        assert!(command_carries_payload(MessageType::SignSubmissions));
        assert!(command_carries_payload(MessageType::SignKick));
        assert!(command_carries_payload(MessageType::GenerateKeys));
        assert!(command_carries_payload(MessageType::GenerateAddPartyKeys));
        assert!(command_carries_payload(MessageType::SignAddParty));
        assert!(command_carries_payload(MessageType::ImportAcs));
        assert!(command_carries_payload(MessageType::ClearOnboardingFlag));
        assert!(command_carries_payload(MessageType::SignClearOnboarding));
        assert!(command_carries_payload(MessageType::SignChangeThreshold));
    }

    fn decline(kind: WorkflowKind, workflow_instance: Option<&str>) -> DeclineInvitationPayload {
        DeclineInvitationPayload {
            kind,
            reason: None,
            workflow_instance: workflow_instance.map(str::to_string),
        }
    }

    #[test]
    fn decline_with_matching_kind_and_instance_matches() {
        let payload = decline(WorkflowKind::Kick, Some("acme-kick-1"));

        assert!(decline_matches_run(
            &payload,
            WorkflowKind::Kick,
            "acme-kick-1"
        ));
    }

    #[test]
    fn decline_with_wrong_kind_is_rejected() {
        // Stale Kick card denied while a Dars run is active.
        let payload = decline(WorkflowKind::Kick, None);

        assert!(!decline_matches_run(
            &payload,
            WorkflowKind::Dars,
            "dars-distribute-1"
        ));
    }

    #[test]
    fn decline_with_stale_instance_is_rejected() {
        // Stale Kick #1 card denied while Kick #2 (same kind, same party,
        // same member set) is active — only the instance tells them apart.
        let payload = decline(WorkflowKind::Kick, Some("acme-kick-1"));

        assert!(!decline_matches_run(
            &payload,
            WorkflowKind::Kick,
            "acme-kick-2"
        ));
    }

    #[test]
    fn decline_without_instance_passes_on_kind_alone() {
        // Invites from coordinators that predate `workflow_instance` carry
        // no run identity — the kind check alone must keep working.
        let payload = decline(WorkflowKind::Onboarding, None);

        assert!(decline_matches_run(
            &payload,
            WorkflowKind::Onboarding,
            "acme-creation"
        ));
    }
}
