use sqlx::SqlitePool;
use tokio::time::Duration;

use super::{
    failure::{report_departure, retry_peer_step},
    payload::{
        decode_add_party_config, extract_party_id_from_p2p_payload, is_new_member,
        save_prepared_submissions_from_payload, send_skip_status,
    },
    steps::{command_matches_kind, persist_peer_step},
};
use crate::{
    auth::WorkflowAuth,
    config::{NodeConfig, Peer},
    consts::{MAX_CONSECUTIVE_NO_WORKFLOW_POLLS, peer_wait_poll_delay_ms},
    error::Result,
    noise::{Message, MessageType, NoiseError, client::NoiseClient},
    server::DeclineInvitationPayload,
    utils,
    workflow::{
        add_party, change_threshold, contracts, kick, onboarding, party_replication,
        storage::{WorkflowStorage, artifact_kinds},
        topology,
        validation::PeerExpectations,
    },
};

/// Start node in peer mode (client)
/// Called when this node receives a workflow invite from the coordinator.
///
/// `instance_name` is the local peer-side `workflow_runs` row's primary
/// key — accept_invitation creates the row with a synthetic name (e.g.
/// `peer-onboarding-<pubkey>-<ts>`) and we use that same name for every
/// `workflow_artifacts` write, so the FK constraint to `workflow_runs` is
/// satisfied. The coordinator's logical instance_name (carried in
/// OnboardingConfig/ContractsConfig/KickConfig payloads) is the coordinator's
/// own primary key on its DB and is not used for storage on the peer side.
///
/// # Errors
///
/// Returns an error if the accepted invitation cannot be loaded, or if the
/// Noise client cannot read this node's keypair or parse the coordinator's key.
/// Returns an error if polling the coordinator fails three times in a row, or
/// if it reports no active workflow for `MAX_CONSECUTIVE_NO_WORKFLOW_POLLS`
/// polls. Returns an error after `MAX_CONSECUTIVE_STEP_FAILURES` step failures
/// in a row, such as commands or payloads the invitation does not allow.
/// Returns an error at once if the ACS import or the `RetireLegacyKeys` step
/// fails.
pub async fn start_peer(
    node_config: NodeConfig,
    coordinator: Peer,
    db: SqlitePool,
    instance_name: String,
    coordinator_instance: String,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    tracing::info!(
        "Initializing Noise client to connect to coordinator {participant_id}...",
        participant_id = coordinator.participant_id
    );

    // The invitation the operator accepted, read back before the first
    // command arrives. Every payload this loop is asked to act on is checked
    // against it, so a coordinator cannot widen the run beyond what was
    // agreed. Without it the peer cannot tell what it consented to, so the
    // run fails rather than signing blind.
    let coordinator_id = coordinator.participant_id.clone();
    let expectations =
        PeerExpectations::load(&db, &instance_name, &node_config, &coordinator_id).await?;

    let client = NoiseClient::new(
        node_config.clone(),
        coordinator,
        coordinator_instance.clone(),
    )
    .await?;

    tracing::info!("Noise client initialized, entering command polling loop");

    let kind = expectations.kind;
    let reporter = client.clone();
    let result = run_peer(
        node_config,
        client,
        db,
        instance_name,
        expectations,
        workflow_auth,
    )
    .await;
    if let Err(e) = &result {
        let report = DeclineInvitationPayload {
            kind,
            reason: Some(format!("{e:#}")),
            workflow_instance: (!coordinator_instance.is_empty()).then_some(coordinator_instance),
            abandoned: true,
        };
        tokio::spawn(report_departure(reporter, report));
    }
    result
}

/// Poll the coordinator and execute its commands until it disconnects this
/// peer, or until a failure ends the run.
async fn run_peer(
    node_config: NodeConfig,
    client: NoiseClient,
    db: SqlitePool,
    instance_name: String,
    expectations: PeerExpectations,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    PeerRun {
        node_config,
        client,
        db,
        instance_name,
        expectations,
        workflow_auth,
        consecutive_errors: 0,
        consecutive_no_workflow: 0,
        consecutive_step_failures: 0,
    }
    .run()
    .await
}

/// Why a command did not complete.
enum StepError {
    /// The step failed, and the coordinator may send the command again. The
    /// failure counts against the run's retry budget.
    Retry(String),
    /// The run cannot go on.
    Abort(anyhow::Error),
}

impl From<anyhow::Error> for StepError {
    fn from(e: anyhow::Error) -> Self {
        Self::Abort(e)
    }
}

impl From<NoiseError> for StepError {
    fn from(e: NoiseError) -> Self {
        Self::Abort(e.into())
    }
}

/// What the loop does after a command.
enum Next {
    /// Poll the coordinator for the next command.
    Poll,
    /// The coordinator disconnected this peer: the run is over.
    Disconnect,
}

/// One peer run: what its commands need, and the counters that decide when
/// it gives up.
struct PeerRun {
    node_config: NodeConfig,
    client: NoiseClient,
    db: SqlitePool,
    instance_name: String,
    expectations: PeerExpectations,
    workflow_auth: Option<WorkflowAuth>,
    /// Polls in a row that did not reach the coordinator.
    consecutive_errors: i32,
    /// Polls in a row that the coordinator answered with no active workflow.
    consecutive_no_workflow: usize,
    /// Steps in a row that failed, whatever the cause.
    consecutive_step_failures: usize,
}

impl PeerRun {
    async fn run(mut self) -> Result {
        let peer_kind = self.expectations.kind;

        // Command polling loop
        loop {
            // Poll coordinator for next command (with payload for commands that need data)
            let Some(message) = self.next_command().await? else {
                continue;
            };
            let command = message.msg_type;
            let payload = message.payload;

            // Only commands that belong to the accepted workflow kind are
            // executed. A Contracts invitation must not become a licence to sign
            // a kick's topology transactions.
            let outcome = if command_matches_kind(peer_kind, command) {
                persist_peer_step(&self.db, &self.instance_name, peer_kind, command).await;
                self.run_command(command, &payload).await
            } else {
                Err(StepError::Retry(format!(
                    "Refusing {command:?}: the accepted invitation is for a {peer_kind:?} workflow"
                )))
            };

            // Keep every failure path on the same counter and backoff policy.
            match outcome {
                Ok(Next::Poll) => {}
                Ok(Next::Disconnect) => break,
                Err(StepError::Retry(reason)) => {
                    retry_peer_step(&mut self.consecutive_step_failures, command, reason).await?;
                }
                Err(StepError::Abort(e)) => return Err(e),
            }
        }

        tracing::info!("Peer shutting down");
        Ok(())
    }

    /// Poll the coordinator once. `None` means the poll failed and has
    /// already waited before the next one.
    async fn next_command(&mut self) -> Result<Option<Message>> {
        match self.client.get_next_command_with_payload().await {
            Ok(msg) => {
                self.consecutive_errors = 0; // Reset error count on success
                self.consecutive_no_workflow = 0;
                Ok(Some(msg))
            }
            Err(e) => {
                // A 503 means the coordinator is reachable but has no active
                // workflow registered — either it is still resuming after a
                // restart (transient) or its workflow was cancelled / dismissed
                // while we were offline (permanent). The TCP round-trip
                // succeeded, so reset the connection-error count and instead
                // give the coordinator a bounded number of polls to
                // (re-)register before giving up, so a resumed run doesn't poll
                // a dead coordinator forever. The counter resets on any real
                // reply above, so a slow resume rides through.
                if matches!(&e, NoiseError::BadStatusCode(code, _) if code.as_u16() == 503) {
                    self.consecutive_errors = 0;
                    self.consecutive_no_workflow += 1;
                    if self.consecutive_no_workflow >= MAX_CONSECUTIVE_NO_WORKFLOW_POLLS {
                        tracing::error!(
                            "Coordinator reported no active workflow for \
                             {MAX_CONSECUTIVE_NO_WORKFLOW_POLLS} polls; the workflow was \
                             cancelled or dismissed. Aborting."
                        );
                        anyhow::bail!(
                            "Peer failed: coordinator has no active workflow (cancelled or dismissed)"
                        );
                    }
                    tracing::warn!(
                        "Poll {consecutive_no_workflow}/{MAX_CONSECUTIVE_NO_WORKFLOW_POLLS}: \
                         coordinator has no active workflow yet, retrying",
                        consecutive_no_workflow = self.consecutive_no_workflow
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    return Ok(None);
                }

                self.consecutive_errors += 1;

                // Per-attempt failures are WARN, not ERROR — the loop's design
                // is to tolerate transient blips (e.g. coordinator restarting
                // briefly during peer-config reload). Only the final-strike
                // bail is logged at ERROR. This keeps test logs readable when
                // a known restart cycle produces one or two retries that
                // succeed on the next attempt.
                tracing::warn!(
                    "Attempt {consecutive_errors}/3: failed to get next command: {e}",
                    consecutive_errors = self.consecutive_errors
                );

                // If we get multiple connection refused errors in a row,
                // the coordinator has likely shut down or there's a persistent error
                if self.consecutive_errors >= 3 {
                    tracing::error!(
                        "Failed to communicate with coordinator after 3 attempts. Aborting."
                    );
                    anyhow::bail!(
                        "Peer failed: persistent communication errors with coordinator: {e}"
                    );
                }

                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(None)
            }
        }
    }

    /// Execute one command of the accepted workflow kind.
    async fn run_command(
        &mut self,
        command: MessageType,
        payload: &[u8],
    ) -> std::result::Result<Next, StepError> {
        match command {
            MessageType::Wait => {
                tracing::trace!("Received command: Wait");
                // Coordinator says to wait, poll again after delay
                tokio::time::sleep(Duration::from_millis(peer_wait_poll_delay_ms())).await;
            }
            MessageType::RetireLegacyKeys => {
                // The target comes from the accepted invitation, never this command's payload.
                let party = self.expectations.dec_party_id.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("No accepted party for legacy key retirement")
                })?;
                let synchronizer_id = utils::get_synchronizer_id(&self.node_config).await?;
                topology::retire_legacy_keys(&self.node_config, &synchronizer_id, party).await?;
                self.consecutive_step_failures = 0;
                self.client
                    .send(
                        MessageType::StatusUpdate,
                        b"RetireLegacyKeys completed".to_vec(),
                    )
                    .await?;
            }
            MessageType::Disconnect => {
                tracing::info!("Received disconnect command, shutting down");
                return Ok(Next::Disconnect);
            }
            MessageType::UploadDars => {
                tracing::info!("Executing: Upload DARs");

                // Decode DAR files from payload and upload directly
                let dar_files = if payload.is_empty() {
                    Vec::new()
                } else {
                    match utils::decode_files(payload) {
                        Ok(files) => files,
                        Err(e) => {
                            return Err(StepError::Retry(format!(
                                "Failed to decode DARs from coordinator: {e}"
                            )));
                        }
                    }
                };

                // Vetting a DAR makes this participant willing to execute its
                // code, so the files must be the ones the operator accepted.
                if let Err(e) = self.expectations.check_dars(&dar_files) {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's DARs: {e}"
                    )));
                }

                if let Err(e) =
                    contracts::upload_dars_from_bytes(&self.node_config, dar_files).await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = self
                    .client
                    .send(MessageType::StatusUpdate, b"UploadDars completed".to_vec())
                    .await
                {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::GenerateKeys => {
                tracing::info!("Executing: Generate keys");
                // Deserialize onboarding config from payload (we still need
                // the prefix for namespace_key_name / daml_key_name; the
                // config's `instance_name` is the coordinator's view and is
                // intentionally unused here).
                let onboarding_config: onboarding::OnboardingConfig =
                    match serde_json::from_slice(payload) {
                        Ok(config) => config,
                        Err(e) => {
                            return Err(StepError::Retry(format!(
                                "Failed to deserialize onboarding config: {e}"
                            )));
                        }
                    };

                // The prefix names the vault keys this step creates or reuses,
                // so it is pinned before any key material is touched.
                if let Err(e) = self
                    .expectations
                    .check_onboarding_config(&onboarding_config.party_id_prefix)
                {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's onboarding config: {e}"
                    )));
                }

                if let Err(e) = onboarding::generate_keys(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &onboarding_config,
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = onboarding::peer::send_keys_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send keys to coordinator: {e}");
                }
            }
            MessageType::SignDns => {
                tracing::info!("Executing: Sign DNS proposal");
                if payload.is_empty() {
                    return Err(StepError::Retry(
                        "No DNS proposal payload received from coordinator".to_string(),
                    ));
                }
                match self
                    .expectations
                    .check_onboarding_dns(&self.node_config, &self.db, &self.instance_name, payload)
                    .await
                {
                    // Remember the namespace we authorized so the P2P
                    // proposal that follows can be pinned to the same party.
                    Ok(namespace) => {
                        // The P2P proposal is pinned to this namespace in the
                        // next step. Signing DNS without recording it would
                        // leave that cross-check silently disabled, so a write
                        // failure fails the step instead.
                        if let Err(e) = self
                            .db
                            .write_artifact(
                                &self.instance_name,
                                artifact_kinds::ACCEPTED_DNS_NAMESPACE,
                                None,
                                namespace.as_bytes(),
                            )
                            .await
                        {
                            return Err(StepError::Retry(format!(
                                "Refusing to sign DNS: the accepted namespace could not be \
                                 recorded, so the P2P cross-check would be lost: {e}"
                            )));
                        }
                    }
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Refusing the coordinator's DNS proposal: {e}"
                        )));
                    }
                }
                if let Err(e) = onboarding::sign_dns_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    payload,
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = onboarding::peer::send_dns_signature_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send DNS signature to coordinator: {e}");
                }
            }
            MessageType::SignP2p => {
                tracing::info!("Executing: Sign P2P proposals");
                if payload.is_empty() {
                    return Err(StepError::Retry(
                        "No P2P proposal payload received from coordinator".to_string(),
                    ));
                }
                // Absent means the DNS step ran on a build that did not record
                // the namespace yet — the mixed-version case, which degrades to
                // the prefix check with a warning inside check_onboarding_p2p.
                // A read error is different: it hides whether a namespace was
                // recorded at all, so it fails the step rather than skipping
                // the cross-check.
                let signed_namespace = match self
                    .db
                    .read_artifact(
                        &self.instance_name,
                        artifact_kinds::ACCEPTED_DNS_NAMESPACE,
                        None,
                    )
                    .await
                {
                    // Nothing recorded is the mixed-version case. Bytes that
                    // are not valid UTF-8 are not: something WAS recorded and
                    // cannot be read back, so treating it as absent would drop
                    // the cross-check on corrupt state.
                    Ok(None) => None,
                    Ok(Some(bytes)) => match String::from_utf8(bytes) {
                        Ok(namespace) => Some(namespace),
                        Err(e) => {
                            return Err(StepError::Retry(format!(
                                "Refusing to sign P2P: the recorded DNS namespace is not \
                                 readable, so the cross-check cannot be applied: {e}"
                            )));
                        }
                    },
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Refusing to sign P2P: cannot read the DNS namespace this run \
                             accepted, so the cross-check cannot be applied: {e}"
                        )));
                    }
                };
                if let Err(e) = self
                    .expectations
                    .check_onboarding_p2p(
                        &self.node_config,
                        &self.db,
                        &self.instance_name,
                        payload,
                        signed_namespace.as_deref(),
                    )
                    .await
                {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's P2P proposal: {e}"
                    )));
                }
                if let Err(e) = onboarding::sign_p2p_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    payload,
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = onboarding::peer::send_p2p_signatures_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send P2P signatures to coordinator: {e}");
                }

                // Identity hook (peer side): the SignP2p payload is a
                // SignedTopologyTransaction whose PartyToParticipant mapping
                // carries the resolved dec_party_id in its `party` field. By
                // now the namespace has been signed and submitted by the
                // coordinator, so we can persist this peer's keys +
                // participant id under the dec_party_identity table for use
                // by post-onboarding workflows on this node.
                match extract_party_id_from_p2p_payload(payload) {
                    Ok(dec_party_id) => {
                        if let Err(e) = self
                            .db
                            .write_run_party_id(&self.instance_name, &dec_party_id)
                            .await
                        {
                            tracing::error!(
                                "Failed to persist party id {dec_party_id} on peer run: {e}"
                            );
                        }
                        if let Err(e) = onboarding::peer::copy_self_identity_for_party(
                            &self.db,
                            &self.instance_name,
                            &self.node_config,
                            &dec_party_id,
                        )
                        .await
                        {
                            tracing::error!("Failed to copy self identity for {dec_party_id}: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to extract dec_party_id from P2P payload: {e}");
                    }
                }
            }
            MessageType::SignSubmissions => {
                tracing::info!("Executing: Sign submissions");

                if payload.is_empty() {
                    return Err(StepError::Retry(
                        "No submissions payload received from coordinator".to_string(),
                    ));
                }

                // Decode config and files from payload: [config_json, files_payload]
                let items = match utils::decode_length_prefixed(payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to decode SignSubmissions payload: {e}"
                        )));
                    }
                };

                let contracts_config: contracts::ContractsConfig =
                    match serde_json::from_slice(&items[0]) {
                        Ok(config) => config,
                        Err(e) => {
                            return Err(StepError::Retry(format!(
                                "Failed to deserialize contracts config: {e}"
                            )));
                        }
                    };

                let dec_party_id = contracts_config.decentralized_party_id.clone();
                if let Err(e) = self.expectations.check_dec_party(&dec_party_id) {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's contracts config: {e}"
                    )));
                }

                // Persist the prepared submissions sent by the coordinator into
                // this peer's workflow_artifacts so sign_submissions can
                // read them back via list_artifacts.
                if let Err(e) =
                    save_prepared_submissions_from_payload(&items[1], &self.db, &self.instance_name)
                        .await
                {
                    return Err(StepError::Retry(format!(
                        "Failed to save prepared submissions from coordinator: {e}"
                    )));
                }

                if let Err(e) = contracts::sign_submissions(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &dec_party_id,
                    &self.expectations.contract_intents,
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e:#}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = contracts::peer::send_submission_signatures_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send submission signatures to coordinator: {e}");
                }
            }
            MessageType::SignKick => {
                tracing::info!("Executing: Sign kick proposals");
                if payload.is_empty() {
                    return Err(StepError::Retry(
                        "No kick proposals payload received from coordinator".to_string(),
                    ));
                }

                let items = match utils::decode_length_prefixed(payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to decode SignKick payload: {e}"
                        )));
                    }
                };

                let _kick_config: kick::KickConfig = match serde_json::from_slice(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to deserialize kick config: {e}"
                        )));
                    }
                };

                if let Err(e) = self
                    .expectations
                    .check_party_proposals(
                        &self.node_config,
                        &self.db,
                        &self.instance_name,
                        &items[1],
                        &items[2],
                    )
                    .await
                {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's kick proposals: {e}"
                    )));
                }

                let kick_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) = kick::sign_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &kick_data,
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = kick::peer::send_kick_signatures_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send kick signatures to coordinator: {e}");
                }
            }
            MessageType::SignChangeThreshold => {
                tracing::info!("Executing: Sign change-threshold proposals");
                if payload.is_empty() {
                    return Err(StepError::Retry(
                        "No change-threshold proposals payload received from coordinator"
                            .to_string(),
                    ));
                }

                let items = match utils::decode_length_prefixed(payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to decode SignChangeThreshold payload: {e}"
                        )));
                    }
                };

                let _change_config: change_threshold::ChangeThresholdConfig =
                    match serde_json::from_slice(&items[0]) {
                        Ok(config) => config,
                        Err(e) => {
                            return Err(StepError::Retry(format!(
                                "Failed to deserialize change-threshold config: {e}"
                            )));
                        }
                    };

                if let Err(e) = self
                    .expectations
                    .check_party_proposals(
                        &self.node_config,
                        &self.db,
                        &self.instance_name,
                        &items[1],
                        &items[2],
                    )
                    .await
                {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's change-threshold proposals: {e}"
                    )));
                }

                let proposal_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) = change_threshold::sign_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &proposal_data,
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) =
                    change_threshold::peer::send_change_threshold_signatures_to_coordinator(
                        &self.client,
                        &self.db,
                        &self.instance_name,
                        &self.node_config,
                    )
                    .await
                {
                    tracing::error!(
                        "Failed to send change-threshold signatures to coordinator: {e}"
                    );
                }
            }
            MessageType::GenerateAddPartyKeys => {
                tracing::info!("Executing: Generate add-party keys");
                let add_party_config = match decode_add_party_config(payload) {
                    Ok(config) => config,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to deserialize add-party config: {e:#}"
                        )));
                    }
                };
                // Both the party and the new member are pinned before
                // `is_new_member` reads `new_participant_id` to decide this
                // node's role: naming this peer there would otherwise make it
                // take the new member's part in a run it joined as an existing
                // member.
                if let Err(e) = self
                    .expectations
                    .check_dec_party(&add_party_config.decentralized_party_id)
                    .and_then(|()| {
                        self.expectations
                            .check_new_participant(&add_party_config.new_participant_id)
                    })
                {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's add-party config: {e}"
                    )));
                }
                if !is_new_member(&self.node_config, &add_party_config) {
                    self.consecutive_step_failures = 0;
                    send_skip_status(&self.client, "GenerateAddPartyKeys").await;
                    return Ok(Next::Poll);
                }

                let ledger_token = add_party::resolve_ledger_token(
                    &self.workflow_auth,
                    &add_party_config.decentralized_party_id,
                )
                .await;
                if let Err(e) = add_party::generate_keys(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &add_party_config,
                    ledger_token.as_deref(),
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = add_party::peer::send_keys_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send add-party keys to coordinator: {e}");
                }
            }
            MessageType::SignAddParty => {
                tracing::info!("Executing: Sign add-party proposals");
                if payload.is_empty() {
                    return Err(StepError::Retry(
                        "No add-party proposals payload received from coordinator".to_string(),
                    ));
                }

                let items = match utils::decode_length_prefixed(payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to decode SignAddParty payload: {e}"
                        )));
                    }
                };
                let add_party_config = match decode_add_party_config(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to deserialize add-party config: {e:#}"
                        )));
                    }
                };

                let add_party_check = async {
                    self.expectations
                        .check_dec_party(&add_party_config.decentralized_party_id)?;
                    self.expectations
                        .check_new_participant(&add_party_config.new_participant_id)?;
                    self.expectations
                        .check_party_proposals(
                            &self.node_config,
                            &self.db,
                            &self.instance_name,
                            &items[1],
                            &items[2],
                        )
                        .await
                }
                .await;
                if let Err(e) = add_party_check {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's add-party proposals: {e}"
                    )));
                }

                if is_new_member(&self.node_config, &add_party_config)
                    && let Err(e) =
                        party_replication::check_vetted_packages_held(&self.node_config).await
                {
                    return Err(StepError::Retry(format!(
                        "Refusing to sign as the new member: {e:#}"
                    )));
                }

                let proposal_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) = add_party::sign_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &proposal_data,
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                // The new member leaves the synchronizer before its signature can
                // let the coordinator authorize the mapping. From the moment the
                // mapping is effective it receives the party's traffic, and it
                // must not journal any of it until the ACS import has landed.
                // Its own store has to hold the proposal first: disconnected, it
                // will not see the mapping take effect, and the import checks for it.
                if is_new_member(&self.node_config, &add_party_config)
                    && let Err(e) = async {
                        add_party::peer::wait_for_hosting_proposal(
                            &self.node_config,
                            &add_party_config.decentralized_party_id,
                        )
                        .await?;
                        party_replication::open_import_window(
                            &self.node_config,
                            &self.db,
                            &add_party_config.replication_target(&self.instance_name),
                        )
                        .await
                    }
                    .await
                {
                    return Err(StepError::Retry(format!(
                        "Failed to open the ACS import window: {e:#}"
                    )));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = add_party::peer::send_add_party_signatures_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send add-party signatures to coordinator: {e}");
                }

                // Identity hook (new member): same point in the protocol
                // where onboarding peers persist their identity — the party
                // id is authoritative from the config, and the keys were
                // written by GenerateAddPartyKeys.
                if is_new_member(&self.node_config, &add_party_config)
                    && let Err(e) = onboarding::peer::copy_self_identity_for_party(
                        &self.db,
                        &self.instance_name,
                        &self.node_config,
                        &add_party_config.decentralized_party_id,
                    )
                    .await
                {
                    tracing::error!(
                        "Failed to copy self identity for {party}: {e}",
                        party = add_party_config.decentralized_party_id
                    );
                }
            }
            MessageType::ImportAcs => {
                tracing::info!("Executing: Import party ACS");
                // [config, package_ids]. The snapshot is not in the payload: it
                // is pulled block by block straight into Canton's import, so
                // nothing proportional to the ACS is buffered or stored.
                let items = match utils::decode_length_prefixed(payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to decode ImportAcs payload: {e}"
                        )));
                    }
                };
                let add_party_config = match decode_add_party_config(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to deserialize add-party config: {e:#}"
                        )));
                    }
                };
                // Both the party and the new member are pinned before
                // `is_new_member` reads `new_participant_id` to decide this
                // node's role: naming this peer there would otherwise make it
                // take the new member's part in a run it joined as an existing
                // member.
                if let Err(e) = self
                    .expectations
                    .check_dec_party(&add_party_config.decentralized_party_id)
                    .and_then(|()| {
                        self.expectations
                            .check_new_participant(&add_party_config.new_participant_id)
                    })
                {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's ACS import: {e}"
                    )));
                }
                if !is_new_member(&self.node_config, &add_party_config) {
                    self.consecutive_step_failures = 0;
                    send_skip_status(&self.client, "ImportAcs").await;
                    return Ok(Next::Poll);
                }
                // Newline-joined package ids the party's contracts need; the
                // import preflight checks this participant has them all before
                // it disconnects anything.
                let required_package_ids: Vec<String> = String::from_utf8_lossy(&items[1])
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();

                let step_result = party_replication::import_party_acs(
                    &self.node_config,
                    &self.db,
                    &add_party_config.replication_target(&self.instance_name),
                    &required_package_ids,
                    party_replication::TransferSource::Coordinator,
                    |seq| {
                        let client = &self.client;
                        async move { client.request_next_acs_block(seq).await.map_err(Into::into) }
                    },
                )
                .await;

                // The import owns its retries and runs them inside one disconnect
                // window. Retrying the step here would reconnect the participant
                // between attempts, which replays the ACS journal — so a failure
                // that reaches this point ends the peer.
                if let Err(e) = step_result {
                    return Err(StepError::Abort(anyhow::anyhow!(
                        "Aborting peer: the ACS import failed: {e:#}"
                    )));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = self
                    .client
                    .send(MessageType::StatusUpdate, b"ImportAcs completed".to_vec())
                    .await
                {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::ClearOnboardingFlag => {
                tracing::info!("Executing: Clear onboarding flag");
                let add_party_config = match decode_add_party_config(payload) {
                    Ok(config) => config,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to deserialize add-party config: {e:#}"
                        )));
                    }
                };
                // Both the party and the new member are pinned before
                // `is_new_member` reads `new_participant_id` to decide this
                // node's role: naming this peer there would otherwise make it
                // take the new member's part in a run it joined as an existing
                // member.
                if let Err(e) = self
                    .expectations
                    .check_dec_party(&add_party_config.decentralized_party_id)
                    .and_then(|()| {
                        self.expectations
                            .check_new_participant(&add_party_config.new_participant_id)
                    })
                {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's clearing request: {e}"
                    )));
                }
                if !is_new_member(&self.node_config, &add_party_config) {
                    self.consecutive_step_failures = 0;
                    send_skip_status(&self.client, "ClearOnboardingFlag").await;
                    return Ok(Next::Poll);
                }

                match party_replication::clear_onboarding_flag(
                    &self.node_config,
                    &self.db,
                    &add_party_config.replication_target(&self.instance_name),
                )
                .await
                {
                    Ok(party_replication::ClearOutcome::Proposed) => {
                        // Canton requires the ONBOARDING PARTICIPANT to issue
                        // the flag-clear transaction — author it here and ship
                        // it to the coordinator for the threshold-signing
                        // round (the coordinator lacks an appropriate key).
                        match add_party::author_clear_proposal(&self.node_config, &add_party_config)
                            .await
                        {
                            Ok(Some(proposal)) => {
                                self.consecutive_step_failures = 0;
                                if let Err(e) = self
                                    .client
                                    .send(MessageType::AddPartyClearProposal, proposal)
                                    .await
                                {
                                    tracing::error!(
                                        "Failed to send clearing proposal to coordinator: {e}"
                                    );
                                }
                            }
                            Ok(None) => {
                                // Flag dropped between the poll and the
                                // authoring — report as cleared.
                                self.consecutive_step_failures = 0;
                                if let Err(e) = self
                                    .client
                                    .send(
                                        MessageType::StatusUpdate,
                                        b"ClearOnboardingFlag: Cleared".to_vec(),
                                    )
                                    .await
                                {
                                    tracing::error!("Failed to send completion status: {e}");
                                }
                            }
                            Err(e) => {
                                return Err(StepError::Retry(format!(
                                    "Step execution failed: {e}"
                                )));
                            }
                        }
                    }
                    Ok(party_replication::ClearOutcome::Cleared) => {
                        self.consecutive_step_failures = 0;
                        if let Err(e) = self
                            .client
                            .send(
                                MessageType::StatusUpdate,
                                b"ClearOnboardingFlag: Cleared".to_vec(),
                            )
                            .await
                        {
                            tracing::error!("Failed to send completion status: {e}");
                        }
                    }
                    Err(e) => {
                        return Err(StepError::Retry(format!("Step execution failed: {e}")));
                    }
                }
            }
            MessageType::SignClearOnboarding => {
                tracing::info!("Executing: Sign onboarding-flag clearing proposal");
                let items = match utils::decode_length_prefixed(payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to decode SignClearOnboarding payload: {e}"
                        )));
                    }
                };
                if items[1].is_empty() {
                    // Skip marker: the flag already cleared without a
                    // signing round (e.g. a single-owner-threshold party).
                    self.consecutive_step_failures = 0;
                    send_skip_status(&self.client, "SignClearOnboarding").await;
                    return Ok(Next::Poll);
                }

                if let Err(e) = self.expectations.check_clear_onboarding(&items[1]) {
                    return Err(StepError::Retry(format!(
                        "Refusing the coordinator's clearing proposal: {e}"
                    )));
                }

                if let Err(e) = add_party::sign_clear_proposal(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &items[1],
                )
                .await
                {
                    return Err(StepError::Retry(format!("Step execution failed: {e}")));
                }
                self.consecutive_step_failures = 0;
                if let Err(e) = add_party::peer::send_clear_signature_to_coordinator(
                    &self.client,
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                )
                .await
                {
                    tracing::error!("Failed to send clearing signature to coordinator: {e}");
                }
            }
            _ => {
                tracing::warn!("Unexpected message type: {command:?}");
            }
        }
        Ok(Next::Poll)
    }
}
