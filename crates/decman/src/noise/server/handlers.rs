use std::time::Instant;

use crate::{
    canton_id::CantonId,
    noise::{
        CHUNK_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType, NoiseError, acs_block_size,
        parse_public_key, send_noise_message,
    },
    server::{DeclineInvitationPayload, WorkflowKind, peer_status::bump},
    workflow::{
        party_replication::pipe::{PipeBlock, encode_block},
        state::WorkflowStep,
    },
};

use super::NoiseServer;

/// How often the source logs ACS export progress, in blocks. The target is
/// disconnected from every synchronizer for the whole transfer, so both sides
/// need to show movement in production logs.
const ACS_PROGRESS_EVERY_BLOCKS: u64 = 64;

/// Whether an outgoing command should carry the workflow's `command_payload`.
///
/// `Disconnect` is a pure control signal marking the end of a workflow and
/// must never inherit the residual payload from an earlier step (e.g. the
/// DAR bundle from `UploadDars`). Shipping it would turn a small control
/// message into a multi-MB chunked transfer and delay the peer's invite
/// listener from resuming, which has caused Contracts-invite races in CI.
const fn command_carries_payload(command: MessageType) -> bool {
    !matches!(
        command,
        MessageType::Disconnect | MessageType::RetireLegacyKeys
    )
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

impl<S: WorkflowStep + 'static> NoiseServer<S> {
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
            MessageType::GetNextAcsBlock => {
                self.handle_get_next_acs_block(peer_id, message.payload)
                    .await
            }
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

    /// Serve the next block of this run's open ACS export.
    ///
    /// Nothing is stored on either side: the block is read from the live Canton
    /// export stream on demand, so the source's memory is one block regardless
    /// of how large the party is.
    async fn handle_get_next_acs_block(
        &self,
        peer_id: CantonId,
        request_payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        if request_payload.len() < 8 {
            return Err(NoiseError::InvalidMessage);
        }
        let mut seq_bytes = [0u8; 8];
        seq_bytes.copy_from_slice(&request_payload[..8]);
        let seq = u64::from_be_bytes(seq_bytes);

        let block = self
            .workflow_state
            .next_acs_block(&peer_id, seq, acs_block_size())
            .await?;

        if let PipeBlock::Data { .. } = &block
            && seq.is_multiple_of(ACS_PROGRESS_EVERY_BLOCKS)
        {
            tracing::info!(
                "ACS export progress: block {seq}, {mib} MiB served",
                mib = self.workflow_state.acs_export_served().await / (1024 * 1024)
            );
        }
        if let PipeBlock::End { trailer, .. } = &block {
            tracing::info!(
                "ACS export exhausted at block {seq}: {total} bytes, sha256 {digest}",
                total = trailer.total_len,
                digest = trailer.sha256
            );
        }

        let (is_end, payload) = encode_block(&block);
        let msg_type = if is_end {
            MessageType::AcsBlockEnd
        } else {
            MessageType::AcsBlock
        };
        Ok(Message::new(msg_type, payload))
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
    /// An `abandoned` decline comes from a peer that accepted and later gave
    /// up. It only counts that peer out; [`Self::wait_until_stranded`] fails
    /// the run if the rest cannot finish it. The other peers keep their runs,
    /// so a coordinator retry can still re-drive them.
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

        if payload.abandoned {
            let reason = payload.reason.as_deref().unwrap_or("no reason given");
            tracing::warn!(
                "Peer {peer_id} gave up on run {}: {reason}",
                self.workflow_state.instance_name()
            );
            self.departed
                .write()
                .await
                .insert(peer_id, format!("gave up: {reason}"));
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
        assert!(!command_carries_payload(MessageType::RetireLegacyKeys));
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
            abandoned: false,
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
