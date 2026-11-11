use sqlx::SqlitePool;
use tokio::time::Duration;

use super::{
    failure::{report_departure, retry_peer_step},
    party_change::Membership,
    steps::{command_matches_kind, persist_peer_step},
};
use crate::{
    auth::WorkflowAuth,
    config::{NodeConfig, Peer},
    consts::{MAX_CONSECUTIVE_NO_WORKFLOW_POLLS, peer_wait_poll_delay_ms},
    error::Result,
    noise::{Message, MessageType, NoiseError, client::NoiseClient},
    server::DeclineInvitationPayload,
    workflow::validation::PeerExpectations,
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
pub(super) enum StepError {
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

/// The result of one command.
pub(super) type StepResult<T = ()> = Result<T, StepError>;

/// What the loop does after a command.
enum Next {
    /// Poll the coordinator for the next command.
    Poll,
    /// The coordinator disconnected this peer: the run is over.
    Disconnect,
}

/// One peer run: what its commands need, and the counters that decide when
/// it gives up.
pub(super) struct PeerRun {
    pub(super) node_config: NodeConfig,
    pub(super) client: NoiseClient,
    pub(super) db: SqlitePool,
    pub(super) instance_name: String,
    pub(super) expectations: PeerExpectations,
    pub(super) workflow_auth: Option<WorkflowAuth>,
    /// Polls in a row that did not reach the coordinator.
    consecutive_errors: i32,
    /// Polls in a row that the coordinator answered with no active workflow.
    consecutive_no_workflow: usize,
    /// Steps in a row that failed, whatever the cause.
    pub(super) consecutive_step_failures: usize,
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
    async fn run_command(&mut self, command: MessageType, payload: &[u8]) -> StepResult<Next> {
        match command {
            MessageType::Wait => {
                tracing::trace!("Received command: Wait");
                // Coordinator says to wait, poll again after delay
                tokio::time::sleep(Duration::from_millis(peer_wait_poll_delay_ms())).await;
            }
            MessageType::RetireLegacyKeys => self.retire_legacy_keys().await?,
            MessageType::Disconnect => {
                tracing::info!("Received disconnect command, shutting down");
                return Ok(Next::Disconnect);
            }
            MessageType::UploadDars => self.upload_dars(payload).await?,
            MessageType::GenerateKeys => self.generate_keys(payload).await?,
            MessageType::SignDns => self.sign_dns(payload).await?,
            MessageType::SignP2p => self.sign_p2p(payload).await?,
            MessageType::SignSubmissions => self.sign_submissions(payload).await?,
            MessageType::SignKick => self.sign_membership(Membership::Kick, payload).await?,
            MessageType::SignChangeThreshold => {
                self.sign_membership(Membership::ChangeThreshold, payload)
                    .await?
            }
            MessageType::GenerateAddPartyKeys => self.generate_add_party_keys(payload).await?,
            MessageType::SignAddParty => {
                self.sign_membership(Membership::AddParty, payload).await?
            }
            MessageType::ImportAcs => self.import_acs(payload).await?,
            MessageType::ClearOnboardingFlag => self.clear_onboarding_flag(payload).await?,
            MessageType::SignClearOnboarding => self.sign_clear_onboarding(payload).await?,
            _ => {
                tracing::warn!("Unexpected message type: {command:?}");
            }
        }
        Ok(Next::Poll)
    }
}
