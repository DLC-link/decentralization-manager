use std::{sync::Arc, time::Duration};

use common::types::WorkflowProgress;
use sqlx::SqlitePool;

use super::{
    error::UnhostError,
    splice_cleanup::splice_cleanup,
    topology::{hosts, leave_mapping, leave_topology, refuse_a_decentralized_party},
};
use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::NodeConfig,
    consts::topology_retry_delay_secs,
    workflow::{
        external_party::{
            add_hosts::{CurrentPartyTopology, instance_is_for_party, read_party_to_participant},
            unhost_audit::{
                self, AuditStatus, NewAttempt, RequestedVia, UnhostAuditRow, UnhostStep,
            },
        },
        party_replication::relay::{HoldRefused, PartyHold, RelaySessions},
    },
};

/// What the caller asked for.
pub struct UnhostRequest {
    pub party_id: String,
    pub base_serial: u32,
    pub splice_cleanup: bool,
    pub requested_via: RequestedVia,
    /// The admin user, or `tenant-key-N`. Never a key.
    pub requested_by: String,
    /// The participant the caller means to remove. The tenant API names it, so
    /// a caller cannot mistake which host it is talking to. `None` means this
    /// node.
    pub named_participant: Option<CantonId>,
}

/// What the workflow reads and writes through.
pub struct UnhostEnv<'a> {
    pub config: &'a NodeConfig,
    pub db: &'a SqlitePool,
    pub relay_sessions: &'a Arc<RelaySessions>,
    pub auth: Option<&'a WorkflowAuth>,
    /// Called once this node is seen gone from the party, whether the call
    /// itself saw it or the watcher after a 202 did.
    pub on_departed: Option<Arc<dyn Fn() + Send + Sync>>,
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

    if let Some(named) = request
        .named_participant
        .as_ref()
        .filter(|named| named.to_string() != me)
    {
        let error = UnhostError::Refused(format!(
            "this host is {me}, not {named}; a host removes only itself, so send the request \
             to {named}'s own host"
        ));
        tracing::info!(%party_id, audit_id, "external-party unhost refused: {error}");
        let message = error.to_string();
        if let Err(e) =
            unhost_audit::finish(env.db, audit_id, AuditStatus::Failed, None, Some(&message)).await
        {
            tracing::error!(%party_id, audit_id, "failed to close the unhost audit row: {e:#}");
        }
        return Err(error);
    }

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
            if let Some(on_departed) = &env.on_departed {
                on_departed();
            }
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
                env.on_departed.clone(),
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
    on_departed: Option<Arc<dyn Fn() + Send + Sync>>,
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
                if let Some(on_departed) = on_departed {
                    on_departed();
                }
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
pub(super) struct Progress {
    pub(super) audit_id: i64,
    pub(super) serial_before: u32,
    pub(super) validator_rights_archived: Vec<String>,
    pub(super) preapprovals_cancelled: Vec<String>,
}

/// How an attempt that was not refused ended.
pub(super) enum Finished {
    /// Head state no longer lists this node, at `serial`.
    Completed { serial: u32 },
    /// The write at `submitted` was accepted but is not effective yet.
    Pending { submitted: u32 },
}

/// Why an attempt stopped early.
pub(super) enum Stop {
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
    refuse_a_decentralized_party(env.config, party_id).await?;
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
pub(super) async fn read_hosted(
    config: &NodeConfig,
    party_id: &str,
) -> anyhow::Result<CurrentPartyTopology> {
    read_party_to_participant(config, party_id)
        .await?
        .ok_or_else(|| {
            anyhow::Error::new(UnhostError::NotHosted {
                party: party_id.to_string(),
            })
        })
}

pub(super) async fn relay_open(sessions: &RelaySessions, party_id: &str) -> bool {
    sessions
        .any_session(|instance| instance_is_for_party(instance, party_id))
        .await
}

pub(super) fn already_running(party_id: &str) -> UnhostError {
    UnhostError::Busy(format!(
        "an unhost of {party_id} is already running on this host; wait for it to finish"
    ))
}

pub(super) fn relay_busy(party_id: &str) -> UnhostError {
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
pub(super) fn cleanup_party<'a>(env: &UnhostEnv<'a>) -> Result<&'a CantonId, UnhostError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::external_party::unhost::test_support::*;

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

    /// A request naming another participant is refused before anything else,
    /// and the refusal lands in the audit like every other. The audit is the
    /// record of what was asked of this node.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn naming_another_participant_is_refused_and_audited(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(CantonId::parse(&format!(
            "participant-3::1220{digest}",
            digest = "cc".repeat(32)
        ))?);
        let other = CantonId::parse(&format!(
            "participant-1::1220{digest}",
            digest = "aa".repeat(32)
        ))?;
        let relay_sessions = Arc::new(RelaySessions::new());
        let env = UnhostEnv {
            config: &config,
            db: &db,
            relay_sessions: &relay_sessions,
            auth: None,
            on_departed: None,
        };
        let request = UnhostRequest {
            party_id: PARTY.to_string(),
            base_serial: 4,
            splice_cleanup: false,
            requested_via: RequestedVia::Tenant,
            requested_by: "tenant-key-0".to_string(),
            named_participant: Some(other),
        };

        let Err(UnhostError::Refused(message)) = unhost(&env, &request).await else {
            panic!("a request naming another participant must be refused");
        };
        assert!(message.contains("removes only itself"), "{message}");

        let rows = unhost_audit::list_for_party(&db, PARTY, 10).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[0].requested_by, "tenant-key-0");
        assert_eq!(rows[0].error.as_deref(), Some(message.as_str()));
        assert!(!relay_sessions.is_held(PARTY), "a refusal takes no hold");
        Ok(())
    }
}
