//! Making THIS node leave an external party on its own.
//!
//! Canton lets a host leave without the party. `PartyToParticipant.requiredAuth`
//! has a branch for exactly this shape: the threshold is unchanged, no host is
//! added, and one host is removed. Such a write needs **either** the party's
//! namespace **or** the removed participant's, so this node's own key completes
//! it and no wallet signature is involved.
//!
//! The node also created Splice contracts for the party while it served as the
//! party's validator, and it controls both cleanups alone:
//! `ValidatorRight_ArchiveAsValidator` (controller `validator`) and
//! `TransferPreapproval_Cancel` (controller the receiver or the provider, before
//! `expiresAt`). Both run as the node's validator operator party.
//!
//! The steps run in this order, and each one is safe to repeat:
//!
//! 1. **Splice cleanup.** It runs before any topology write, so a failure
//!    leaves the party fully hosted and the operator free to retry.
//! 2. **Topology.** The serial-N+1 mapping without this node, authorized with
//!    this node's key and checked against head state at submission.
//! 3. **Audit.** [`super::unhost_audit`] holds one row per attempt. The row is
//!    written first and updated at every step, so a crash leaves it open at the
//!    step it reached, and the next attempt resumes from there.

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        Command, Commands, CreatedEvent, ExerciseCommand, Identifier, Record, RecordField,
        SubmitAndWaitRequest, Value, command, value,
    },
    digitalasset::canton::{
        protocol::v30::{
            PartyToParticipant, TopologyMapping,
            enums::{ParticipantPermission, TopologyChangeOp},
            party_to_participant::HostingParticipant,
            topology_mapping,
        },
        topology::admin::v30::{AuthorizeRequest, authorize_request},
    },
};
use common::{api::ExternalPartyUnhostPreview, types::WorkflowProgress};
use decman_lib::framework::{
    filters::{party_event_format, template_filter},
    record::{field_party, field_timestamp},
};
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::NodeConfig,
    consts::topology_retry_delay_secs,
    server::ledger_paging::fetch_active_contracts_filtered,
    utils,
    workflow::{
        external_party::{
            add_hosts::{CurrentPartyTopology, instance_is_for_party, read_party_to_participant},
            unhost_audit::{
                self, AuditStatus, NewAttempt, RequestedVia, UnhostAuditRow, UnhostStep,
            },
        },
        party_replication::relay::{HoldRefused, PartyHold, RelaySessions},
        topology,
    },
};

/// The Splice templates are named by package so Canton matches the contracts of
/// every vetted `splice-amulet` version, not just one package id.
const SPLICE_AMULET: &str = "#splice-amulet";

/// How many head-state reads the topology step makes, one topology retry delay
/// apart, before it reports the write as accepted but not yet effective. Kept
/// short so the whole call returns inside a wallet client's 60 s timeout.
const EFFECTIVE_POLLS: u32 = 10;

/// Why an unhost was refused. Each variant maps to the status code that tells
/// the caller what to do next.
#[derive(Debug, thiserror::Error)]
pub enum UnhostError {
    /// This node holds no authorized mapping that names it (404).
    #[error("This host does not host external party {party}")]
    NotHosted { party: String },
    /// The pinned serial disagrees with head state (409).
    #[error(
        "this host reads serial {found} for {party}, not the {pinned} the request pinned; \
         re-read the party and retry"
    )]
    StaleSerial {
        party: String,
        pinned: u32,
        found: u32,
    },
    /// The departure would leave the party unable to transact (400).
    #[error("{0}")]
    Refused(String),
    /// Something already running on this node blocks the departure (409).
    #[error("{0}")]
    Busy(String),
    /// The node is not set up for the Splice cleanup the request asked for (422).
    #[error("{0}")]
    NotConfigured(String),
    /// A read failed before anything was written (500).
    #[error("{0:#}")]
    Internal(#[source] anyhow::Error),
}

/// What the caller asked for.
pub struct UnhostRequest {
    pub party_id: String,
    pub base_serial: u32,
    pub splice_cleanup: bool,
    pub requested_via: RequestedVia,
    /// The admin user, or `tenant-key-N`. Never a key.
    pub requested_by: String,
}

/// What the workflow reads and writes through.
pub struct UnhostEnv<'a> {
    pub config: &'a NodeConfig,
    pub db: &'a SqlitePool,
    pub relay_sessions: &'a Arc<RelaySessions>,
    pub auth: Option<&'a WorkflowAuth>,
}

/// What an attempt did, once it got past its preconditions.
#[derive(Debug)]
pub struct UnhostOutcome {
    /// `Completed`: head state no longer lists this node. `InProgress`: the
    /// write was accepted but is not effective yet. `Failed`: a step failed
    /// after work started; the audit row holds the reason.
    pub status: WorkflowProgress,
    /// The serial this node reads for the party.
    pub serial: u32,
    pub validator_rights_archived: Vec<String>,
    pub preapprovals_cancelled: Vec<String>,
    pub audit_id: i64,
}

/// Make this node leave `request.party_id`.
///
/// # Errors
/// [`UnhostError`] when the attempt is refused, or when a read fails before
/// anything was written. A failure after work started is an
/// [`UnhostOutcome`] with status `Failed`, because the caller needs the
/// contract ids already archived.
pub async fn unhost(
    env: &UnhostEnv<'_>,
    request: &UnhostRequest,
) -> Result<UnhostOutcome, UnhostError> {
    let party_id = request.party_id.as_str();
    let me = env.config.participant_id().to_string();

    let audit_id = unhost_audit::insert_attempt(
        env.db,
        &NewAttempt {
            party_id,
            participant_id: &me,
            requested_via: request.requested_via,
            requested_by: &request.requested_by,
            splice_cleanup: request.splice_cleanup,
        },
    )
    .await
    .map_err(UnhostError::Internal)?;

    // Held until the node is gone or the attempt ends. A second unhost of the
    // party is refused, and so is any ACS relay for it, because a relay running
    // while the node leaves would stream or import against a mapping that no
    // longer holds. Taking the hold and starting a relay share a lock, so
    // neither can miss the other.
    let hold = match env
        .relay_sessions
        .hold(party_id, |instance| {
            instance_is_for_party(instance, party_id)
        })
        .await
    {
        Ok(hold) => hold,
        Err(refused) => {
            let error = match refused {
                HoldRefused::AlreadyHeld => already_running(party_id),
                HoldRefused::RelayOpen => relay_busy(party_id),
            };
            tracing::info!(%party_id, audit_id, "external-party unhost refused: {error}");
            let message = error.to_string();
            if let Err(e) =
                unhost_audit::finish(env.db, audit_id, AuditStatus::Failed, None, Some(&message))
                    .await
            {
                tracing::error!(%party_id, audit_id, "failed to close the unhost audit row: {e:#}");
            }
            return Err(error);
        }
    };

    let mut progress = Progress {
        audit_id,
        serial_before: request.base_serial,
        validator_rights_archived: Vec::new(),
        preapprovals_cancelled: Vec::new(),
    };
    let result = run(env, request, &me, &mut progress).await;

    let (status, serial, error) = match &result {
        Ok(Finished::Completed { serial }) => {
            // The node is gone whatever happens to this write, so a failure
            // here must not turn the outcome into a failure.
            if let Err(e) = unhost_audit::set_step(env.db, audit_id, UnhostStep::Done).await {
                tracing::error!(%party_id, audit_id, "failed to record the unhost as done: {e:#}");
            }
            (AuditStatus::Completed, Some(*serial), None)
        }
        Ok(Finished::Pending { submitted }) => (AuditStatus::InProgress, Some(*submitted), None),
        Err(Stop::Refused(error @ UnhostError::Internal(_))) => {
            tracing::error!(%party_id, audit_id, "external-party unhost could not start: {error:#}");
            (AuditStatus::Failed, None, Some(error.to_string()))
        }
        Err(Stop::Refused(error)) => {
            tracing::info!(%party_id, audit_id, "external-party unhost refused: {error}");
            (AuditStatus::Failed, None, Some(error.to_string()))
        }
        Err(Stop::Failed(error)) => {
            tracing::error!(%party_id, audit_id, "external-party unhost failed: {error:#}");
            (AuditStatus::Failed, None, Some(format!("{error:#}")))
        }
    };
    if let Err(e) = unhost_audit::finish(env.db, audit_id, status, serial, error.as_deref()).await {
        tracing::error!(%party_id, audit_id, "failed to close the unhost audit row: {e:#}");
    }

    let outcome = |status, serial| UnhostOutcome {
        status,
        serial,
        validator_rights_archived: progress.validator_rights_archived.clone(),
        preapprovals_cancelled: progress.preapprovals_cancelled.clone(),
        audit_id,
    };
    match result {
        Ok(Finished::Completed { serial }) => {
            tracing::info!(%party_id, serial, audit_id, "external-party unhost completed");
            Ok(outcome(WorkflowProgress::Completed, serial))
        }
        Ok(Finished::Pending { .. }) => {
            tracing::info!(
                %party_id,
                audit_id,
                "external-party unhost accepted; the write is not effective yet"
            );
            // The removal is scheduled but not effective, so this node still
            // appears in the mapping and a relay could otherwise start. The
            // hold moves to a watcher that releases it once the node is gone.
            tokio::spawn(watch_departure(
                env.config.clone(),
                env.db.clone(),
                hold,
                party_id.to_string(),
                me,
                audit_id,
            ));
            Ok(outcome(
                WorkflowProgress::InProgress,
                progress.serial_before,
            ))
        }
        Err(Stop::Refused(error)) => Err(error),
        Err(Stop::Failed(_)) => Ok(outcome(WorkflowProgress::Failed, progress.serial_before)),
    }
}

/// How many head-state reads [`watch_departure`] makes, one topology retry delay
/// apart: ten minutes with the default delay.
const WATCH_POLLS: u32 = 300;

/// Wait for an accepted removal to take effect, keeping the party held until it
/// does. Then close the attempt's audit row as completed.
///
/// A repeat of the unhost meanwhile is refused as already running, and answers
/// 200 once the hold is gone. If the removal never shows, the hold is released
/// and the row stays open for the next attempt to settle.
async fn watch_departure(
    config: NodeConfig,
    db: SqlitePool,
    hold: PartyHold,
    party_id: String,
    me: String,
    audit_id: i64,
) {
    let delay = Duration::from_secs(topology_retry_delay_secs());
    for _ in 0..WATCH_POLLS {
        tokio::time::sleep(delay).await;
        match read_hosted(&config, &party_id).await {
            Ok(current) if !hosts(&current.mapping, &me) => {
                let closed = async {
                    unhost_audit::set_step(&db, audit_id, UnhostStep::Done).await?;
                    unhost_audit::finish(
                        &db,
                        audit_id,
                        AuditStatus::Completed,
                        Some(current.serial),
                        None,
                    )
                    .await
                }
                .await;
                if let Err(e) = closed {
                    tracing::error!(%party_id, audit_id, "failed to close the unhost audit row: {e:#}");
                }
                tracing::info!(
                    %party_id,
                    serial = current.serial,
                    audit_id,
                    "external-party unhost completed after it was accepted"
                );
                drop(hold);
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(%party_id, "external-party unhost: head-state read failed: {e:#}");
            }
        }
    }
    tracing::warn!(
        %party_id,
        audit_id,
        "external-party unhost: the accepted removal did not take effect in time; releasing the \
         party for the next attempt"
    );
    drop(hold);
}

/// What one attempt has done so far, for the audit row and the response.
struct Progress {
    audit_id: i64,
    serial_before: u32,
    validator_rights_archived: Vec<String>,
    preapprovals_cancelled: Vec<String>,
}

/// How an attempt that was not refused ended.
enum Finished {
    /// Head state no longer lists this node, at `serial`.
    Completed { serial: u32 },
    /// The write at `submitted` was accepted but is not effective yet.
    Pending { submitted: u32 },
}

/// Why an attempt stopped early.
enum Stop {
    /// A precondition refused it; the caller gets a 4xx.
    Refused(UnhostError),
    /// A step failed after work may have started.
    Failed(anyhow::Error),
}

impl From<UnhostError> for Stop {
    fn from(error: UnhostError) -> Self {
        Self::Refused(error)
    }
}

/// Write the step to the audit row. A failed audit write stops the attempt:
/// going on would do work the row does not record.
async fn record_step(
    env: &UnhostEnv<'_>,
    progress: &Progress,
    step: UnhostStep,
) -> Result<(), Stop> {
    unhost_audit::set_step(env.db, progress.audit_id, step)
        .await
        .map_err(Stop::Failed)
}

async fn run(
    env: &UnhostEnv<'_>,
    request: &UnhostRequest,
    me: &str,
    progress: &mut Progress,
) -> Result<Finished, Stop> {
    let party_id = request.party_id.as_str();

    // ---- Preconditions, against this node's own head-state read ----------
    let current = read_hosted(env.config, party_id)
        .await
        .map_err(UnhostError::from)?;
    progress.serial_before = current.serial;
    unhost_audit::set_serial_before(env.db, progress.audit_id, current.serial)
        .await
        .map_err(UnhostError::Internal)?;

    // Settled against head state, so an earlier attempt that answered 202 and
    // has since landed is recorded as the departure it was.
    let departed_at = (!hosts(&current.mapping, me)).then_some(current.serial);
    unhost_audit::close_open_attempts(env.db, party_id, me, progress.audit_id, departed_at)
        .await
        .map_err(UnhostError::Internal)?;

    if !hosts(&current.mapping, me) {
        // A retry after success. The mapping exists and this node is gone.
        return Ok(Finished::Completed {
            serial: current.serial,
        });
    }
    if current.serial != request.base_serial {
        return Err(UnhostError::StaleSerial {
            party: party_id.to_string(),
            pinned: request.base_serial,
            found: current.serial,
        }
        .into());
    }
    leave_mapping(&current.mapping, me)?;

    let earlier = unhost_audit::latest_with_work(env.db, party_id, me)
        .await
        .map_err(UnhostError::Internal)?;
    let resumed_from = resumes(earlier.as_ref(), current.serial);
    let cleanup_as = if request.splice_cleanup && resumed_from.is_none() {
        Some(cleanup_party(env)?)
    } else {
        None
    };

    if let Some(earlier) = resumed_from {
        tracing::info!(
            %party_id,
            earlier,
            "external-party unhost: resuming after an earlier attempt finished the Splice cleanup"
        );
        unhost_audit::set_resumed_from(env.db, progress.audit_id, earlier)
            .await
            .map_err(UnhostError::Internal)?;
    }

    // ---- Step 1: Splice cleanup, before any topology write ---------------
    if let Some(operator) = cleanup_as {
        record_step(env, progress, UnhostStep::SpliceCleanup).await?;
        splice_cleanup(env, party_id, operator, progress)
            .await
            .map_err(Stop::Failed)?;
    }

    // ---- Step 2: topology -----------------------------------------------
    record_step(env, progress, UnhostStep::Topology).await?;
    leave_topology(env.config, party_id, me, request.base_serial).await
}

/// The party's mapping in head state, or [`UnhostError::NotHosted`] when there
/// is none.
async fn read_hosted(config: &NodeConfig, party_id: &str) -> anyhow::Result<CurrentPartyTopology> {
    read_party_to_participant(config, party_id)
        .await?
        .ok_or_else(|| {
            anyhow::Error::new(UnhostError::NotHosted {
                party: party_id.to_string(),
            })
        })
}

impl From<anyhow::Error> for UnhostError {
    /// Keeps a [`UnhostError`] that travelled inside an `anyhow::Error` as
    /// itself, so a 404 does not turn into a 500 on the way out.
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<UnhostError>() {
            Ok(unhost) => unhost,
            Err(other) => Self::Internal(other),
        }
    }
}

/// Does the mapping list `me`, at any permission?
fn hosts(mapping: &PartyToParticipant, me: &str) -> bool {
    mapping.participants.iter().any(|p| p.participant_uid == me)
}

/// Can this host act for the party? Unmarked, and at Confirmation or
/// Submission. The same rule add-hosts applies: a marked host holds none of the
/// party's contracts, and an Observation host confirms nothing.
fn can_act(host: &HostingParticipant) -> bool {
    host.onboarding.is_none()
        && (host.permission == ParticipantPermission::Confirmation as i32
            || host.permission == ParticipantPermission::Submission as i32)
}

/// The serial-N+1 mapping without `me`: same party, same threshold, same keys,
/// and every other host in its current order.
///
/// # Errors
/// [`UnhostError::Refused`] when `me` is the last host, or when the hosts left
/// could not meet the threshold.
fn leave_mapping(
    current: &PartyToParticipant,
    me: &str,
) -> Result<PartyToParticipant, UnhostError> {
    let party = &current.party;
    let remaining: Vec<HostingParticipant> = current
        .participants
        .iter()
        .filter(|p| p.participant_uid != me)
        .cloned()
        .collect();
    if remaining.len() == current.participants.len() {
        return Err(UnhostError::NotHosted {
            party: party.clone(),
        });
    }
    if remaining.is_empty() {
        return Err(UnhostError::Refused(format!(
            "this host is the last host of {party}; Canton refuses a mapping with no hosts"
        )));
    }
    let able = remaining.iter().filter(|p| can_act(p)).count();
    if (able as u32) < current.threshold {
        return Err(UnhostError::Refused(format!(
            "{party} needs {threshold} confirmations, and only {able} of the hosts that would \
             remain can give one; the party's key must lower the threshold first \
             (/v0/tenant/threshold/prepare, then /v0/tenant/threshold/execute)",
            threshold = current.threshold
        )));
    }
    Ok(PartyToParticipant {
        party: party.clone(),
        threshold: current.threshold,
        participants: remaining,
        party_signing_keys: current.party_signing_keys.clone(),
    })
}

async fn relay_open(sessions: &RelaySessions, party_id: &str) -> bool {
    sessions
        .any_session(|instance| instance_is_for_party(instance, party_id))
        .await
}

fn already_running(party_id: &str) -> UnhostError {
    UnhostError::Busy(format!(
        "an unhost of {party_id} is already running on this host; wait for it to finish"
    ))
}

fn relay_busy(party_id: &str) -> UnhostError {
    UnhostError::Busy(format!(
        "an ACS relay for {party_id} is open on this host; let it finish or time out first"
    ))
}

/// The earlier attempt this one resumes, if any.
///
/// Only an attempt that ran its Splice cleanup, reached the topology step and
/// did not complete, read at the serial head state still reports. An attempt
/// sent with `splice_cleanup: false` reaches the topology step too, so the flag
/// is what proves the cleanup ran. A different serial means the party moved
/// since, so the cleanup runs again: it is cheap, and archived contracts are
/// simply not found.
fn resumes(earlier: Option<&UnhostAuditRow>, head_serial: u32) -> Option<i64> {
    let row = earlier?;
    (row.splice_cleanup
        && row.status != AuditStatus::Completed.as_str()
        && row.step == UnhostStep::Topology.as_str()
        && row.serial_before == Some(i64::from(head_serial)))
    .then_some(row.id)
}

/// The party the Splice cleanup acts as.
///
/// # Errors
/// [`UnhostError::NotConfigured`] when `DECPM_VALIDATOR_OPERATOR_PARTY` is
/// unset, or names a party this node holds no credential for.
fn cleanup_party<'a>(env: &UnhostEnv<'a>) -> Result<&'a CantonId, UnhostError> {
    let Some(operator) = env.config.validator_operator_party.as_ref() else {
        return Err(UnhostError::NotConfigured(
            "this host has no DECPM_VALIDATOR_OPERATOR_PARTY, so it cannot archive the party's \
             Splice contracts; send splice_cleanup: false if it never acted as the party's \
             Splice validator"
                .to_string(),
        ));
    };
    if !env.auth.is_some_and(|auth| auth.has_credentials(operator)) {
        return Err(UnhostError::NotConfigured(format!(
            "this host holds no party credential for its validator operator party {operator}"
        )));
    }
    Ok(operator)
}

// ============================================================================
// Splice cleanup
// ============================================================================

fn validator_right_template() -> Identifier {
    Identifier {
        package_id: SPLICE_AMULET.to_string(),
        module_name: "Splice.Amulet".to_string(),
        entity_name: "ValidatorRight".to_string(),
    }
}

fn transfer_preapproval_template() -> Identifier {
    Identifier {
        package_id: SPLICE_AMULET.to_string(),
        module_name: "Splice.AmuletRules".to_string(),
        entity_name: "TransferPreapproval".to_string(),
    }
}

/// A `TransferPreapproval` the operator provides for the party.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Preapproval {
    contract_id: String,
    /// Microseconds since the epoch, as Daml stores `Time`.
    expires_at: i64,
}

/// The party's Splice contracts that the operator holds.
#[derive(Debug, Default, PartialEq, Eq)]
struct SpliceContracts {
    validator_rights: Vec<String>,
    preapprovals: Vec<Preapproval>,
}

enum SpliceContract {
    ValidatorRight(String),
    Preapproval(Preapproval),
}

/// Keep a contract only when it ties `party` to `operator`: a `ValidatorRight`
/// with `user == party` and `validator == operator`, or a `TransferPreapproval`
/// with `receiver == party` and `provider == operator`.
///
/// The operator sees these contracts for every user it serves, so the filter
/// is what keeps the cleanup to the party leaving.
fn classify(created: &CreatedEvent, party: &str, operator: &str) -> Option<SpliceContract> {
    let template = created.template_id.as_ref()?;
    let args = created.create_arguments.as_ref()?;
    let is = |label: &str, want: &str| field_party(args, label).as_deref() == Some(want);
    match template.entity_name.as_str() {
        "ValidatorRight" if is("user", party) && is("validator", operator) => {
            Some(SpliceContract::ValidatorRight(created.contract_id.clone()))
        }
        "TransferPreapproval" if is("receiver", party) && is("provider", operator) => {
            Some(SpliceContract::Preapproval(Preapproval {
                contract_id: created.contract_id.clone(),
                expires_at: field_timestamp(args, "expiresAt")?,
            }))
        }
        _ => None,
    }
}

/// Read the party's `ValidatorRight` and `TransferPreapproval` contracts as the
/// operator, which is a signatory on both and so sees them without any right
/// over the party.
async fn find_splice_contracts(
    config: &NodeConfig,
    party_id: &str,
    operator: &CantonId,
    token: &str,
) -> anyhow::Result<SpliceContracts> {
    let format = party_event_format(
        operator,
        vec![
            template_filter(validator_right_template(), false),
            template_filter(transfer_preapproval_template(), false),
        ],
        true,
    );
    let operator_id = operator.to_string();
    let found = match fetch_active_contracts_filtered(
        config,
        Some(token.to_string()),
        format,
        |c| classify(&c, party_id, &operator_id),
    )
    .await
    {
        Ok(found) => found,
        // A participant without splice-amulet has no such contracts to archive.
        Err(e) if format!("{e:#}").contains("NO_TEMPLATES_FOR_PACKAGE_NAME_AND_QUALIFIED_NAME") => {
            tracing::info!(
                %party_id,
                "external-party unhost: splice-amulet is not on this participant; nothing to archive"
            );
            Vec::new()
        }
        Err(e) => return Err(e.context("reading the party's Splice contracts as the operator")),
    };

    let mut contracts = SpliceContracts::default();
    for contract in found {
        match contract {
            SpliceContract::ValidatorRight(cid) => contracts.validator_rights.push(cid),
            SpliceContract::Preapproval(p) => contracts.preapprovals.push(p),
        }
    }
    Ok(contracts)
}

async fn operator_token(env: &UnhostEnv<'_>, operator: &CantonId) -> anyhow::Result<String> {
    let auth = env
        .auth
        .context("this host has no party credentials to act as its validator operator party")?;
    Ok(auth
        .get_credentials(operator)
        .await
        .with_context(|| format!("minting a ledger token for the validator operator {operator}"))?
        .token)
}

/// Exercise `choice` on one contract as the operator, and wait for the result.
async fn exercise_as_operator(
    config: &NodeConfig,
    token: &str,
    operator: &CantonId,
    template_id: Identifier,
    contract_id: &str,
    choice: &str,
    argument: Record,
) -> anyhow::Result<()> {
    let mut client = utils::create_command_client(config, Some(token.to_string())).await?;
    let commands = Commands {
        command_id: uuid::Uuid::new_v4().to_string(),
        act_as: vec![operator.to_string()],
        commands: vec![Command {
            command: Some(command::Command::Exercise(ExerciseCommand {
                template_id: Some(template_id),
                contract_id: contract_id.to_string(),
                choice: choice.to_string(),
                choice_argument: Some(Value {
                    sum: Some(value::Sum::Record(argument)),
                }),
            })),
        }],
        ..Default::default()
    };
    client
        .submit_and_wait(tonic::Request::new(SubmitAndWaitRequest {
            commands: Some(commands),
        }))
        .await
        .with_context(|| format!("{choice} on {contract_id}"))?;
    Ok(())
}

/// Archive every `ValidatorRight` and cancel every live `TransferPreapproval`
/// the operator holds for the party. Each archive is written to the audit row
/// as it happens.
async fn splice_cleanup(
    env: &UnhostEnv<'_>,
    party_id: &str,
    operator: &CantonId,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let token = operator_token(env, operator).await?;
    let found = find_splice_contracts(env.config, party_id, operator, &token).await?;
    if found == SpliceContracts::default() {
        tracing::info!(
            %party_id,
            "external-party unhost: this host holds no Splice contracts for the party"
        );
    }

    for cid in found.validator_rights {
        exercise_as_operator(
            env.config,
            &token,
            operator,
            validator_right_template(),
            &cid,
            "ValidatorRight_ArchiveAsValidator",
            Record::default(),
        )
        .await?;
        progress.validator_rights_archived.push(cid);
        save_cleanup(env, progress).await?;
    }

    for preapproval in found.preapprovals {
        // Past `expiresAt` the cancel choice refuses, and the DSO expires the
        // contract itself, so an expired one counts as done.
        if preapproval.expires_at <= now_micros() {
            tracing::info!(
                %party_id,
                contract_id = %preapproval.contract_id,
                "external-party unhost: TransferPreapproval already expired; leaving it to the DSO"
            );
            continue;
        }
        let cancelled = exercise_as_operator(
            env.config,
            &token,
            operator,
            transfer_preapproval_template(),
            &preapproval.contract_id,
            "TransferPreapproval_Cancel",
            Record {
                record_id: None,
                fields: vec![RecordField {
                    label: "p".to_string(),
                    value: Some(Value {
                        sum: Some(value::Sum::Party(operator.to_string())),
                    }),
                }],
            },
        )
        .await;
        match cancelled {
            Ok(()) => {
                progress
                    .preapprovals_cancelled
                    .push(preapproval.contract_id);
                save_cleanup(env, progress).await?;
            }
            // It expired between the read and the cancel.
            Err(e) if preapproval.expires_at <= now_micros() => {
                tracing::info!(
                    %party_id,
                    contract_id = %preapproval.contract_id,
                    "external-party unhost: TransferPreapproval expired during the cancel: {e:#}"
                );
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

async fn save_cleanup(env: &UnhostEnv<'_>, progress: &Progress) -> anyhow::Result<()> {
    unhost_audit::set_cleanup(
        env.db,
        progress.audit_id,
        &progress.validator_rights_archived,
        &progress.preapprovals_cancelled,
    )
    .await
}

fn now_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

// ============================================================================
// Topology
// ============================================================================

/// Write the mapping without this node and wait, briefly, for it to take
/// effect.
///
/// Head state is read first, so a repeat after the write landed reads as
/// success, and a party that moved since the preconditions is a stale pin.
/// The serial at submission is `base_serial + 1`, so of two concurrent writers
/// only one can succeed.
async fn leave_topology(
    config: &NodeConfig,
    party_id: &str,
    me: &str,
    base_serial: u32,
) -> Result<Finished, Stop> {
    let current = read_hosted(config, party_id).await.map_err(Stop::Failed)?;
    if !hosts(&current.mapping, me) {
        return Ok(Finished::Completed {
            serial: current.serial,
        });
    }
    if current.serial != base_serial {
        return Err(UnhostError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        }
        .into());
    }
    let mapping = leave_mapping(&current.mapping, me)?;
    let submitted = base_serial.checked_add(1).ok_or_else(|| {
        UnhostError::Refused(format!(
            "{party_id} is at serial {base_serial}, which cannot be advanced"
        ))
    })?;
    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(Stop::Failed)?;

    let authorized = topology::authorize_with_topology_retry(
        config,
        AuthorizeRequest {
            r#type: Some(authorize_request::Type::Proposal(
                authorize_request::Proposal {
                    change: TopologyChangeOp::AddReplace as i32,
                    serial: submitted,
                    mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                        mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
                    })),
                },
            )),
            // This node's namespace alone authorizes its own departure, so the
            // write must be complete when this returns.
            must_fully_authorize: true,
            force_changes: vec![],
            signed_by: vec![],
            store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            wait_to_become_effective: None,
        },
        "external-party unhost authorize",
    )
    .await;

    if let Err(error) = authorized {
        // The write may have landed before the error, or another writer may
        // have taken the serial. Head state says which.
        return match read_hosted(config, party_id).await {
            Ok(after) if !hosts(&after.mapping, me) => Ok(Finished::Completed {
                serial: after.serial,
            }),
            Ok(after) if after.serial != base_serial => Err(UnhostError::StaleSerial {
                party: party_id.to_string(),
                pinned: base_serial,
                found: after.serial,
            }
            .into()),
            // Unchanged head state does not prove a rejection when the answer
            // was lost: Canton may have scheduled the removal anyway. The
            // watcher keeps the party held until it knows.
            _ if outcome_unknown(&error) => {
                tracing::warn!(
                    %party_id,
                    "external-party unhost: the topology write's outcome is unknown: {error:#}"
                );
                Ok(Finished::Pending { submitted })
            }
            _ => Err(Stop::Failed(error)),
        };
    }

    let delay = Duration::from_secs(topology_retry_delay_secs());
    for _ in 0..EFFECTIVE_POLLS {
        match read_hosted(config, party_id).await {
            Ok(after) if !hosts(&after.mapping, me) => {
                return Ok(Finished::Completed {
                    serial: after.serial,
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(%party_id, "external-party unhost: head-state read failed: {e:#}")
            }
        }
        tokio::time::sleep(delay).await;
    }
    Ok(Finished::Pending { submitted })
}

/// Whether a failed topology write may still have been scheduled.
///
/// A gRPC error that says the call never finished cannot tell a rejection from
/// a lost answer. An error that is not a gRPC status failed before anything
/// was sent, and a status such as `INVALID_ARGUMENT` is Canton's refusal.
fn outcome_unknown(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<tonic::Status>())
        .is_some_and(|status| {
            matches!(
                status.code(),
                tonic::Code::Unavailable
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::Cancelled
                    | tonic::Code::Unknown
                    | tonic::Code::Internal
                    | tonic::Code::Aborted
            )
        })
}

// ============================================================================
// Preview
// ============================================================================

/// What [`unhost`] would do for `party_id` right now. Nothing is written.
///
/// A precondition that would refuse the unhost is reported as `blocker` rather
/// than as an error, so the operator sees it before confirming.
///
/// # Errors
/// [`UnhostError::NotHosted`] when this node does not host the party, and
/// [`UnhostError::Internal`] when a read fails.
pub async fn preview(
    env: &UnhostEnv<'_>,
    party_id: &str,
) -> Result<ExternalPartyUnhostPreview, UnhostError> {
    let me = env.config.participant_id().to_string();
    let current = read_hosted(env.config, party_id).await?;
    if !hosts(&current.mapping, &me) {
        return Err(UnhostError::NotHosted {
            party: party_id.to_string(),
        });
    }

    let mut blocker = leave_mapping(&current.mapping, &me).err();
    if blocker.is_none() && env.relay_sessions.is_held(party_id) {
        blocker = Some(already_running(party_id));
    }
    if blocker.is_none() && relay_open(env.relay_sessions, party_id).await {
        blocker = Some(relay_busy(party_id));
    }

    let operator = env.config.validator_operator_party.as_ref();
    let mut contracts = SpliceContracts::default();
    if operator.is_some() {
        match cleanup_party(env) {
            Ok(operator) => {
                let token = operator_token(env, operator).await?;
                contracts = find_splice_contracts(env.config, party_id, operator, &token).await?;
            }
            Err(not_configured) => {
                blocker.get_or_insert(not_configured);
            }
        }
    }
    let now = now_micros();
    let (live, expired): (Vec<_>, Vec<_>) = contracts
        .preapprovals
        .into_iter()
        .partition(|p| p.expires_at > now);

    Ok(ExternalPartyUnhostPreview {
        party_id: party_id.to_string(),
        serial: current.serial,
        threshold: current.mapping.threshold,
        remaining_hosts: current
            .mapping
            .participants
            .iter()
            .filter(|p| p.participant_uid != me)
            .map(|p| p.participant_uid.clone())
            .collect(),
        validator_operator_party: operator.map(ToString::to_string),
        validator_rights: contracts.validator_rights,
        preapprovals_to_cancel: live.into_iter().map(|p| p.contract_id).collect(),
        preapprovals_expired: expired.into_iter().map(|p| p.contract_id).collect(),
        blocker: blocker.map(|b| b.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::hosting_participant;

    use super::*;

    const ME: &str = "participant-3::1220cc";
    const PARTY: &str = "alice::1220aa";
    const OPERATOR: &str = "validator::1220dd";

    fn host(uid: &str, permission: ParticipantPermission, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: uid.to_string(),
            permission: permission as i32,
            onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
        }
    }

    /// A 2-of-3 party hosted by P1, ME and P2, in that order.
    fn two_of_three() -> PartyToParticipant {
        PartyToParticipant {
            party: PARTY.to_string(),
            threshold: 2,
            participants: vec![
                host(
                    "participant-1::1220aa",
                    ParticipantPermission::Confirmation,
                    false,
                ),
                host(ME, ParticipantPermission::Confirmation, false),
                host(
                    "participant-2::1220bb",
                    ParticipantPermission::Confirmation,
                    false,
                ),
            ],
            party_signing_keys: None,
        }
    }

    fn refusal(result: Result<PartyToParticipant, UnhostError>) -> String {
        match result {
            Err(UnhostError::Refused(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The write Canton authorizes on this node's key alone: the same
    /// threshold, no host added, exactly one removed. Anything more needs the
    /// party's key.
    #[test]
    fn the_new_mapping_drops_exactly_this_host() {
        let current = two_of_three();
        let Ok(next) = leave_mapping(&current, ME) else {
            panic!("a 2-of-3 party can lose one host");
        };
        let uids: Vec<&str> = next
            .participants
            .iter()
            .map(|p| p.participant_uid.as_str())
            .collect();
        assert_eq!(uids, vec!["participant-1::1220aa", "participant-2::1220bb"]);
        assert_eq!(next.threshold, current.threshold);
        assert_eq!(next.party, current.party);
        assert_eq!(next.participants[0], current.participants[0]);
        assert_eq!(next.participants[1], current.participants[2]);
        assert_eq!(next.party_signing_keys, current.party_signing_keys);
    }

    #[test]
    fn the_last_host_cannot_leave() {
        let current = PartyToParticipant {
            threshold: 1,
            participants: vec![host(ME, ParticipantPermission::Confirmation, false)],
            ..two_of_three()
        };
        assert!(refusal(leave_mapping(&current, ME)).contains("last host"));
    }

    /// On 2 of 2 neither host may leave: the one left could not meet the
    /// threshold. The message names the remedy.
    #[test]
    fn no_host_may_leave_a_party_at_full_threshold() {
        let mut current = two_of_three();
        current.participants.remove(2);
        let message = refusal(leave_mapping(&current, ME));
        assert!(message.contains("threshold/prepare"), "{message}");
        assert!(message.contains("only 1"), "{message}");
    }

    /// A host still carrying the onboarding marker holds none of the party's
    /// contracts and confirms nothing, so it does not count as a host that
    /// remains.
    #[test]
    fn a_marked_host_does_not_count_as_remaining() {
        let mut current = two_of_three();
        current.participants[2].onboarding = Some(hosting_participant::Onboarding {});
        refusal(leave_mapping(&current, ME));
    }

    #[test]
    fn an_observation_host_does_not_count_as_remaining() {
        let mut current = two_of_three();
        current.participants[2].permission = ParticipantPermission::Observation as i32;
        refusal(leave_mapping(&current, ME));
    }

    /// A Submission host exists only at threshold 1 and acts alone, so it is
    /// enough to stay behind.
    #[test]
    fn a_submission_host_counts_as_remaining() {
        let current = PartyToParticipant {
            threshold: 1,
            participants: vec![
                host(
                    "participant-1::1220aa",
                    ParticipantPermission::Submission,
                    false,
                ),
                host(ME, ParticipantPermission::Submission, false),
            ],
            ..two_of_three()
        };
        assert!(leave_mapping(&current, ME).is_ok());
    }

    #[test]
    fn a_mapping_without_this_host_is_not_hosted() {
        let mut current = two_of_three();
        current.participants.remove(1);
        assert!(matches!(
            leave_mapping(&current, ME),
            Err(UnhostError::NotHosted { .. })
        ));
        assert!(!hosts(&current, ME));
    }

    /// A retry after success must read as success, so the check for "this node
    /// is gone" must not care what permission it had.
    #[test]
    fn this_host_is_present_at_any_permission() {
        let mut current = two_of_three();
        current.participants[1].permission = ParticipantPermission::Observation as i32;
        assert!(hosts(&current, ME));
    }

    fn row(status: AuditStatus, step: UnhostStep, serial_before: Option<i64>) -> UnhostAuditRow {
        UnhostAuditRow {
            id: 7,
            party_id: PARTY.to_string(),
            participant_id: ME.to_string(),
            requested_via: "tenant".to_string(),
            requested_by: "tenant-key-0".to_string(),
            serial_before,
            serial_after: None,
            validator_rights_archived: "[]".to_string(),
            preapprovals_cancelled: "[]".to_string(),
            splice_cleanup: true,
            step: step.as_str().to_string(),
            resumed_from: None,
            started_at: 0,
            finished_at: None,
            status: status.as_str().to_string(),
            error: None,
        }
    }

    /// An attempt that finished the cleanup and stopped at the topology step
    /// is resumed, as long as the party has not moved since.
    #[test]
    fn an_attempt_stopped_at_topology_is_resumed() {
        let earlier = row(AuditStatus::Failed, UnhostStep::Topology, Some(4));
        assert_eq!(resumes(Some(&earlier), 4), Some(7));
        let crashed = row(AuditStatus::InProgress, UnhostStep::Topology, Some(4));
        assert_eq!(resumes(Some(&crashed), 4), Some(7));
    }

    #[test]
    fn nothing_else_is_resumed() {
        assert_eq!(resumes(None, 4), None);
        let moved = row(AuditStatus::Failed, UnhostStep::Topology, Some(3));
        assert_eq!(resumes(Some(&moved), 4), None, "the party moved since");
        let mid_cleanup = row(AuditStatus::Failed, UnhostStep::SpliceCleanup, Some(4));
        assert_eq!(
            resumes(Some(&mid_cleanup), 4),
            None,
            "the cleanup must finish"
        );
        let done = row(AuditStatus::Completed, UnhostStep::Done, Some(4));
        assert_eq!(
            resumes(Some(&done), 4),
            None,
            "a finished unhost starts over"
        );
    }

    /// An attempt sent with `splice_cleanup: false` also reaches the topology
    /// step. A later attempt that asks for the cleanup must run it, or the node
    /// leaves with its Splice contracts still live.
    #[test]
    fn an_attempt_that_skipped_the_cleanup_is_not_resumed() {
        let mut skipped = row(AuditStatus::Failed, UnhostStep::Topology, Some(4));
        skipped.splice_cleanup = false;
        assert_eq!(resumes(Some(&skipped), 4), None);
    }

    fn party_field(label: &str, party: &str) -> RecordField {
        RecordField {
            label: label.to_string(),
            value: Some(Value {
                sum: Some(value::Sum::Party(party.to_string())),
            }),
        }
    }

    fn created(entity: &str, fields: Vec<RecordField>) -> CreatedEvent {
        CreatedEvent {
            contract_id: format!("{entity}-cid"),
            template_id: Some(Identifier {
                package_id: "pkg".to_string(),
                module_name: "Splice.Amulet".to_string(),
                entity_name: entity.to_string(),
            }),
            create_arguments: Some(Record {
                record_id: None,
                fields,
            }),
            ..Default::default()
        }
    }

    fn validator_right(user: &str, validator: &str) -> CreatedEvent {
        created(
            "ValidatorRight",
            vec![
                party_field("dso", "dso::1220ee"),
                party_field("user", user),
                party_field("validator", validator),
            ],
        )
    }

    fn preapproval(receiver: &str, provider: &str) -> CreatedEvent {
        let mut event = created(
            "TransferPreapproval",
            vec![
                party_field("receiver", receiver),
                party_field("provider", provider),
            ],
        );
        if let Some(args) = event.create_arguments.as_mut() {
            args.fields.push(RecordField {
                label: "expiresAt".to_string(),
                value: Some(Value {
                    sum: Some(value::Sum::Timestamp(1_700_000_000_000_000)),
                }),
            });
        }
        event
    }

    /// The operator sees these contracts for every user it serves. Only the
    /// ones tying the leaving party to this operator may be archived.
    #[test]
    fn only_the_partys_contracts_with_this_operator_are_kept() {
        assert!(matches!(
            classify(&validator_right(PARTY, OPERATOR), PARTY, OPERATOR),
            Some(SpliceContract::ValidatorRight(cid)) if cid == "ValidatorRight-cid"
        ));
        assert!(classify(&validator_right("bob::1220ff", OPERATOR), PARTY, OPERATOR).is_none());
        assert!(classify(&validator_right(PARTY, "other::1220ff"), PARTY, OPERATOR).is_none());

        assert!(matches!(
            classify(&preapproval(PARTY, OPERATOR), PARTY, OPERATOR),
            Some(SpliceContract::Preapproval(p)) if p.expires_at == 1_700_000_000_000_000
        ));
        assert!(classify(&preapproval(PARTY, "other::1220ff"), PARTY, OPERATOR).is_none());
        assert!(classify(&preapproval("bob::1220ff", OPERATOR), PARTY, OPERATOR).is_none());
    }

    #[test]
    fn other_templates_are_ignored() {
        let event = created(
            "FeaturedAppRight",
            vec![
                party_field("user", PARTY),
                party_field("validator", OPERATOR),
            ],
        );
        assert!(classify(&event, PARTY, OPERATOR).is_none());
    }

    /// A lost answer must not read as a rejection: the removal may be scheduled,
    /// and releasing the hold would let a relay start under it. Canton's own
    /// refusals, and failures before anything was sent, are definite.
    #[test]
    fn only_an_unfinished_call_leaves_the_outcome_unknown() {
        let status = |code| anyhow::Error::new(tonic::Status::new(code, "x"));
        for code in [
            tonic::Code::Unavailable,
            tonic::Code::DeadlineExceeded,
            tonic::Code::Cancelled,
            tonic::Code::Unknown,
        ] {
            assert!(outcome_unknown(&status(code)), "{code:?}");
        }
        for code in [
            tonic::Code::InvalidArgument,
            tonic::Code::FailedPrecondition,
            tonic::Code::PermissionDenied,
            tonic::Code::AlreadyExists,
        ] {
            assert!(!outcome_unknown(&status(code)), "{code:?}");
        }
        let wrapped = status(tonic::Code::Unavailable).context("authorize");
        assert!(
            outcome_unknown(&wrapped),
            "context must not hide the status"
        );
        assert!(!outcome_unknown(&anyhow::anyhow!(
            "no channel to the admin API"
        )));
    }

    /// A 404 that travelled through `anyhow` must still be a 404.
    #[test]
    fn a_not_hosted_error_survives_anyhow() {
        let wrapped = anyhow::Error::new(UnhostError::NotHosted {
            party: PARTY.to_string(),
        });
        assert!(matches!(
            UnhostError::from(wrapped),
            UnhostError::NotHosted { .. }
        ));
        assert!(matches!(
            UnhostError::from(anyhow::anyhow!("rpc failed")),
            UnhostError::Internal(_)
        ));
    }
}
