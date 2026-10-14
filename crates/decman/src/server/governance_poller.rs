//! Pending-governance gauges, kept current by a background poller.
//!
//! The approvals feed reads the ledger only while someone has the UI open, so a
//! gauge fed from it would go quiet exactly when nobody is looking. This loop
//! reads each served decparty's queue on its own cadence, through the query the
//! feed uses, so the gauges agree with what the feed would show.

use std::{collections::HashMap, sync::LazyLock, time::Duration};

use actix_web::web;
use decman_lib::catalog::interpret;
use prometheus::{GaugeVec, IntCounterVec, IntGaugeVec};

use crate::canton_id::CantonId;

use super::{
    AppState,
    handlers::{get_party_credentials, packages, rules_and_threshold},
    ledger_paging::FETCH_CHUNK,
    queries::{GovernanceQueue, get_governance_confirmations},
};

/// How often each served decparty's queue is read. An open approvals feed reads
/// the same queue every 10 s, so this costs about a sixth of one viewer.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Proposals one read covers: one Canton page. Reading every proposal each
/// minute would bring back the unbounded scan #424 removed, so a party with
/// more open proposals reports a lower bound.
const READ_LIMIT: usize = FETCH_CHUNK as usize;

static PENDING: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    prometheus::register_int_gauge_vec!(
        "decman_governance_pending",
        "Open governance actions at the last successful read. awaiting_approvals needs more confirmations; executable has enough to execute.",
        &["decparty", "state"]
    )
    .expect("metric name is a unique literal")
});

static OLDEST_EXPIRES_IN: LazyLock<GaugeVec> = LazyLock::new(|| {
    prometheus::register_gauge_vec!(
        "decman_governance_oldest_pending_expires_in_seconds",
        "Seconds until the earliest live confirmation on an open governance action expires. Absent when no open action has a live confirmation. Keeps falling while reads fail.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static POLLS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_governance_poll_total",
        "Passes of the governance poller, one a minute while it lives. error means a decparty's read failed and its gauges kept their last values.",
        &["outcome"]
    )
    .expect("metric name is a unique literal")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingState {
    AwaitingApprovals,
    Executable,
}

impl PendingState {
    const ALL: [Self; 2] = [Self::AwaitingApprovals, Self::Executable];

    fn label(self) -> &'static str {
        match self {
            Self::AwaitingApprovals => "awaiting_approvals",
            Self::Executable => "executable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollOutcome {
    Ok,
    Error,
}

impl PollOutcome {
    const ALL: [Self; 2] = [Self::Ok, Self::Error];

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// Registers every family at startup and exports both poll outcomes at zero.
pub(crate) fn register_metrics() {
    LazyLock::force(&PENDING);
    LazyLock::force(&OLDEST_EXPIRES_IN);
    for outcome in PollOutcome::ALL {
        POLLS.with_label_values(&[outcome.label()]).inc_by(0);
    }
}

/// What one read learned about a decparty's queue.
#[derive(Debug, Default, PartialEq)]
struct PendingSummary {
    awaiting_approvals: i64,
    executable: i64,
    /// Unix seconds of the earliest live confirmation expiry.
    oldest_expires_at: Option<i64>,
}

/// Counts the open actions in a queue and finds their earliest live expiry
/// (pure).
///
/// An `expires_at` of 0 is unresolved: it counts as live, as it does on the
/// feed, but it has no countdown. An expiry at or before `now` has passed and
/// never sets the countdown.
fn summarize(queue: &GovernanceQueue, now: i64) -> PendingSummary {
    // Only its confirmations keep a self-management action open. Once they
    // have all expired, it waits for cleanup, not for approvals.
    let self_actions = queue.actions.iter().map(|a| {
        let expiries: Vec<i64> = a.confirmations.iter().map(|c| c.expires_at).collect();
        let open = interpret::live_count(&expiries, |e| *e, now) > 0;
        (open, a.can_execute, expiries)
    });
    // A proposal stays open until it is executed or cancelled. An orphan's
    // proposal is already gone.
    let proposals = queue.domain_actions.iter().map(|d| {
        let expiries = d.confirmations.iter().map(|c| c.expires_at).collect();
        (!d.orphaned, d.can_execute, expiries)
    });

    let mut summary = PendingSummary::default();
    for (open, can_execute, expiries) in self_actions.chain(proposals) {
        if !open {
            continue;
        }
        if can_execute {
            summary.executable += 1;
        } else {
            summary.awaiting_approvals += 1;
        }
        let soonest = expiries.into_iter().filter(|&e| e > now).min();
        summary.oldest_expires_at = summary.oldest_expires_at.into_iter().chain(soonest).min();
    }
    summary
}

/// Reads one decparty's queue the way the approvals feed does. The feed shows
/// an incomplete read as a shorter queue; this returns it as an error, so an
/// outage cannot read as an empty queue.
async fn read_queue(
    data: &web::Data<AppState>,
    decparty: &CantonId,
) -> anyhow::Result<GovernanceQueue> {
    let Some((token, _member)) = get_party_credentials(data, decparty).await? else {
        anyhow::bail!("no auth registered for this decparty");
    };
    let pkgs = packages();
    let (rules, threshold) = rules_and_threshold(data, decparty, Some(token.clone()), &pkgs).await;
    let queue = get_governance_confirmations(
        &data.config,
        decparty,
        threshold,
        rules.as_ref(),
        Some(token),
        &pkgs,
        READ_LIMIT,
        None,
    )
    .await?;
    anyhow::ensure!(queue.complete, "a ledger read behind the queue failed");
    Ok(queue)
}

/// How the last read of a decparty went. Logging only on a change keeps a
/// lasting fault to one line instead of one a minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadHealth {
    Complete,
    Truncated,
    Failed,
}

#[derive(Default)]
struct PollState {
    /// Earliest live expiry per decparty. Republished every pass, so the
    /// countdown keeps falling while that decparty's reads fail.
    oldest: HashMap<CantonId, i64>,
    health: HashMap<CantonId, ReadHealth>,
}

impl PollState {
    /// Publishes a successful read. Only a read that found no live expiry drops
    /// the countdown series; a failed read never reaches here.
    fn apply(&mut self, decparty: &CantonId, summary: &PendingSummary) {
        let label = decparty.to_string();
        for (state, count) in [
            (PendingState::AwaitingApprovals, summary.awaiting_approvals),
            (PendingState::Executable, summary.executable),
        ] {
            PENDING
                .with_label_values(&[label.as_str(), state.label()])
                .set(count);
        }
        match summary.oldest_expires_at {
            Some(expires_at) => {
                self.oldest.insert(decparty.clone(), expires_at);
            }
            None => {
                self.oldest.remove(decparty);
                let _ = OLDEST_EXPIRES_IN.remove_label_values(&[label.as_str()]);
            }
        }
    }

    /// Forgets every decparty this node no longer serves, with its series. A
    /// series left behind would report a queue nobody reads.
    fn prune_unserved(&mut self, served: &[CantonId]) {
        self.health.retain(|decparty, _| {
            let still_served = served.contains(decparty);
            if !still_served {
                let label = decparty.to_string();
                for state in PendingState::ALL {
                    let _ = PENDING.remove_label_values(&[label.as_str(), state.label()]);
                }
                let _ = OLDEST_EXPIRES_IN.remove_label_values(&[label.as_str()]);
            }
            still_served
        });
        self.oldest.retain(|decparty, _| served.contains(decparty));
    }

    /// Publishes the countdown for every decparty with a live expiry.
    fn report_oldest_expiry(&self, now: i64) {
        for (decparty, expires_at) in &self.oldest {
            OLDEST_EXPIRES_IN
                .with_label_values(&[decparty.to_string()])
                .set((expires_at - now) as f64);
        }
    }
}

/// One pass over every served decparty. A failed read leaves that decparty's
/// gauges as they were, and the pass moves on to the next decparty.
async fn poll_once(data: &web::Data<AppState>, state: &mut PollState) -> PollOutcome {
    let served: Vec<CantonId> = data
        .party_credentials
        .read()
        .await
        .iter()
        .map(|p| p.dec_party_id.clone())
        .collect();
    state.prune_unserved(&served);

    let mut outcome = PollOutcome::Ok;
    for decparty in served {
        let previous = state.health.get(&decparty).copied();
        let current = match read_queue(data, &decparty).await {
            Ok(queue) => {
                state.apply(
                    &decparty,
                    &summarize(&queue, chrono::Utc::now().timestamp()),
                );
                if queue.next_cursor.is_none() {
                    if previous == Some(ReadHealth::Failed) {
                        tracing::info!(%decparty, "governance queue read recovered");
                    }
                    ReadHealth::Complete
                } else {
                    if previous != Some(ReadHealth::Truncated) {
                        tracing::warn!(
                            %decparty,
                            limit = READ_LIMIT,
                            "more open proposals than one read covers; the pending gauges are a lower bound"
                        );
                    }
                    ReadHealth::Truncated
                }
            }
            Err(e) => {
                outcome = PollOutcome::Error;
                if previous == Some(ReadHealth::Failed) {
                    tracing::debug!(%decparty, error = %e, "governance queue read still failing");
                } else {
                    tracing::warn!(
                        %decparty,
                        error = %e,
                        "governance queue read failed; its gauges keep their last values"
                    );
                }
                ReadHealth::Failed
            }
        };
        state.health.insert(decparty, current);
    }
    state.report_oldest_expiry(chrono::Utc::now().timestamp());
    outcome
}

/// Reads every served decparty's queue once per [`POLL_INTERVAL`], and counts
/// each finished pass. A read that hangs stops the count, so a stalled poller
/// shows as a flat counter rather than as a frozen gauge.
pub(crate) async fn run_governance_poller(data: web::Data<AppState>) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut state = PollState::default();
    loop {
        interval.tick().await;
        let outcome = poll_once(&data, &mut state).await;
        POLLS.with_label_values(&[outcome.label()]).inc();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Context;

    use super::*;
    use crate::{
        auth::{MockAuthRegistry, WorkflowAuth},
        config::{KeycloakConfig, PackageConfig, PartyCredentials},
        server::types::{
            ActionType, DomainConfirmation, DomainGovernanceAction, GovernanceAction,
            GovernanceConfirmation,
        },
    };

    const NOW: i64 = 1_800_000_000;
    const OLDEST: &str = "decman_governance_oldest_pending_expires_in_seconds";

    // Each test that moves a series names its own decparty: every test in this
    // binary shares one registry, and they run in parallel.
    fn party(prefix: &str) -> anyhow::Result<CantonId> {
        CantonId::parse(&format!("{prefix}::{}", "0".repeat(68)))
    }

    fn summary(awaiting: i64, executable: i64, oldest: Option<i64>) -> PendingSummary {
        PendingSummary {
            awaiting_approvals: awaiting,
            executable,
            oldest_expires_at: oldest,
        }
    }

    fn self_action(can_execute: bool, expiries: &[i64]) -> anyhow::Result<GovernanceAction> {
        let action = ActionType::GovernanceSetThreshold { new_threshold: 2 };
        let confirmer = party("member")?;
        let confirmation = |expires_at| GovernanceConfirmation {
            contract_id: "00c".to_string(),
            action: action.clone(),
            confirming_party: confirmer.clone(),
            created_at: 0,
            expires_at,
        };
        Ok(GovernanceAction {
            action_hash: "hash".to_string(),
            confirmations: expiries.iter().copied().map(confirmation).collect(),
            action: action.clone(),
            confirmation_count: 0,
            executable_confirmation_cids: Vec::new(),
            can_execute,
            last_confirmation_at: 0,
        })
    }

    fn proposal(
        can_execute: bool,
        orphaned: bool,
        expiries: &[i64],
    ) -> anyhow::Result<DomainGovernanceAction> {
        let confirmer = party("member")?;
        let confirmation = |expires_at| DomainConfirmation {
            contract_id: "00c".to_string(),
            confirming_party: confirmer.clone(),
            created_at: 0,
            expires_at,
        };
        Ok(DomainGovernanceAction {
            proposal_cid: "00p".to_string(),
            action_label: "GenericVote".to_string(),
            description: None,
            confirmations: expiries.iter().copied().map(confirmation).collect(),
            confirmation_count: 0,
            executable_confirmation_cids: Vec::new(),
            can_execute,
            orphaned,
            transfer_details: None,
            accept_transfer_details: None,
            service_request_details: None,
            proposer: None,
            proposer_not_authorized: false,
            created_at: None,
        })
    }

    fn queue(
        actions: Vec<GovernanceAction>,
        domain_actions: Vec<DomainGovernanceAction>,
    ) -> GovernanceQueue {
        GovernanceQueue {
            actions,
            domain_actions,
            next_cursor: None,
            complete: true,
        }
    }

    /// How many series `family` exports for `decparty`. Reading through the
    /// registry, unlike `get_metric_with_label_values`, never creates one.
    fn series_count(family: &str, decparty: &CantonId) -> usize {
        let label = decparty.to_string();
        prometheus::gather()
            .iter()
            .filter(|f| f.name() == family)
            .flat_map(|f| f.get_metric())
            .filter(|m| m.get_label().iter().any(|l| l.value() == label))
            .count()
    }

    #[test]
    fn labels_are_the_strings_dashboards_query() {
        assert_eq!(
            PendingState::ALL.map(PendingState::label),
            ["awaiting_approvals", "executable"]
        );
        assert_eq!(PollOutcome::ALL.map(PollOutcome::label), ["ok", "error"]);
    }

    #[test]
    fn open_actions_split_by_state_and_count_down_to_the_earliest_expiry() -> anyhow::Result<()> {
        let q = queue(
            vec![self_action(true, &[NOW + 600])?],
            vec![
                proposal(true, false, &[NOW + 900, NOW + 120])?,
                proposal(false, false, &[NOW + 300])?,
            ],
        );
        assert_eq!(summarize(&q, NOW), summary(1, 2, Some(NOW + 120)));
        Ok(())
    }

    /// An unresolved expiry keeps a self action open. A proposal stays open
    /// after its confirmations expire. Neither has a countdown left.
    #[test]
    fn an_unresolved_or_passed_expiry_never_sets_the_countdown() -> anyhow::Result<()> {
        let q = queue(
            vec![self_action(false, &[0])?],
            vec![proposal(false, false, &[NOW - 10, NOW])?],
        );
        assert_eq!(summarize(&q, NOW), summary(2, 0, None));
        Ok(())
    }

    #[test]
    fn expired_self_actions_and_orphans_are_not_pending() -> anyhow::Result<()> {
        let q = queue(
            vec![self_action(false, &[NOW - 10])?],
            vec![proposal(true, true, &[NOW + 60])?],
        );
        assert_eq!(summarize(&q, NOW), PendingSummary::default());
        Ok(())
    }

    #[test]
    fn the_poll_counter_exists_before_the_first_pass() -> anyhow::Result<()> {
        register_metrics();
        let family = prometheus::gather()
            .into_iter()
            .find(|f| f.name() == "decman_governance_poll_total")
            .context("the poll counter is not registered")?;
        for outcome in PollOutcome::ALL {
            assert!(
                family
                    .get_metric()
                    .iter()
                    .any(|m| m.get_label().iter().any(|l| l.value() == outcome.label())),
                "no series for {outcome:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_read_sets_the_gauges_and_the_countdown_falls_until_the_next() -> anyhow::Result<()> {
        let decparty = party("applied")?;
        let label = decparty.to_string();
        let mut state = PollState::default();

        state.apply(&decparty, &summary(2, 1, Some(NOW + 600)));
        state.report_oldest_expiry(NOW + 60);

        for (pending_state, count) in [
            (PendingState::AwaitingApprovals, 2),
            (PendingState::Executable, 1),
        ] {
            let gauge =
                PENDING.get_metric_with_label_values(&[label.as_str(), pending_state.label()])?;
            assert_eq!(gauge.get(), count);
        }
        let countdown = OLDEST_EXPIRES_IN.get_metric_with_label_values(&[label.as_str()])?;
        assert_eq!(countdown.get(), 540.0);

        // Only a read that found no live expiry may drop the series.
        state.apply(&decparty, &summary(2, 1, None));
        state.report_oldest_expiry(NOW + 120);
        assert_eq!(series_count(OLDEST, &decparty), 0);
        Ok(())
    }

    #[test]
    fn an_unserved_decparty_loses_every_series() -> anyhow::Result<()> {
        let decparty = party("pruned")?;
        let mut state = PollState::default();
        state.apply(&decparty, &summary(1, 0, Some(NOW + 60)));
        state.health.insert(decparty.clone(), ReadHealth::Complete);
        state.report_oldest_expiry(NOW);

        state.prune_unserved(&[]);

        assert_eq!(series_count("decman_governance_pending", &decparty), 0);
        assert_eq!(series_count(OLDEST, &decparty), 0);
        assert!(state.oldest.is_empty() && state.health.is_empty());
        Ok(())
    }

    /// The query layer turns a failed ledger read into a shorter queue. Here
    /// the Ledger and Admin APIs sit on port 1, where nothing listens, and the
    /// pass must report a failure, not an empty queue.
    #[tokio::test]
    async fn a_ledger_outage_reads_as_a_failure_not_an_empty_queue() -> anyhow::Result<()> {
        let decparty = party("outage")?;
        let mut data = AppState::for_test(None).await?.into_inner();
        let canton = &mut Arc::get_mut(&mut data)
            .context("a fresh test state has one owner")?
            .config
            .canton;
        canton.ledger_api_port = 1;
        canton.admin_api_port = 1;
        data.party_credentials.write().await.push(PartyCredentials {
            dec_party_id: decparty.clone(),
            member_party_id: party("member")?,
            user_id: "user".to_string(),
            keycloak: KeycloakConfig::default(),
            auth0: None,
            packages: PackageConfig::default(),
        });
        let mock = MockAuthRegistry::new(data.party_credentials.clone());
        *data.auth.write().await = Some(WorkflowAuth::Mock(Arc::new(mock)));
        let data = web::Data::from(data);

        let mut state = PollState::default();
        let expires_at = chrono::Utc::now().timestamp() + 600;
        state.apply(&decparty, &summary(3, 0, Some(expires_at)));

        let outcome = poll_once(&data, &mut state).await;

        assert_eq!(outcome, PollOutcome::Error);
        assert_eq!(state.health.get(&decparty), Some(&ReadHealth::Failed));
        assert_eq!(state.oldest.get(&decparty), Some(&expires_at));
        let awaiting = PENDING.get_metric_with_label_values(&[
            decparty.to_string().as_str(),
            PendingState::AwaitingApprovals.label(),
        ])?;
        assert_eq!(awaiting.get(), 3, "a failed read keeps the last count");
        Ok(())
    }
}
