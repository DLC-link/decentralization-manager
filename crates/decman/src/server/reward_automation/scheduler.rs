// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The reward automation loop and its per-party pass.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use chrono::{DateTime, Utc};

use super::{
    assign::{fold_oldest_expiry, package_absent, run_reassign_once},
    coupons::for_each_unassigned_coupon,
    delegation::{ActiveDelegation, active_delegation, delegated_decparties},
    metrics::{
        ASSIGNER_NOT_MEMBER, AUTH_UNREGISTERED, CREDENTIALS_UNAVAILABLE, ENABLEMENT_UNREADABLE,
        EXPIRY_READS, HEARTBEAT, OLDEST_EXPIRES_IN, SWEEP_FAILED, SWEEPS, ensure_outcome_series,
    },
};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    server::{
        AppState,
        handlers::{get_party_credentials, packages},
        package_inventory::fetch_package_id_to_name,
        queries::read_governance_state,
    },
};

// ============================================================================
// Background loop + registration
// ============================================================================

/// The loop's wake interval, independent of the reward cadence.
///
/// The heartbeat and the sweep share this one task, so a sweep that blocks for N
/// minutes costs N heartbeats, and one blocking past the stall rule's 15-minute
/// window pages `decman-reward-automation-stalled` even though nothing crashed.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Whether an auth failure for this decparty is worth counting (pure).
///
/// Only a decparty this node is a named assigner for can lose value when its
/// credentials fail. One that was never enabled loses nothing, so counting it
/// would alert on every decparty a participant happens to hold credentials for.
/// `enabled` of `None` means no delegation read has succeeded, and an unread set
/// counts nothing rather than guessing either way.
fn counts_as_auth_fault(
    outcome: &SweepOutcome,
    decparty: &CantonId,
    enabled: Option<&HashSet<CantonId>>,
) -> bool {
    matches!(
        outcome,
        SweepOutcome::CredentialsUnavailable | SweepOutcome::AuthUnregistered
    ) && enabled.is_some_and(|set| set.contains(decparty))
}

/// What one pass learned about the decparties whose delegations name this node.
#[derive(Debug, PartialEq, Eq)]
enum Enablement {
    /// The decparties this node is a named assigner for.
    Read(HashSet<CantonId>),
    /// This participant holds no rewards DAR, so no delegation can name it.
    Off,
    /// No credential could read the set.
    Unreadable,
}

/// Whether a pass that could not read the enablement set is a fault (pure).
///
/// [`counts_as_auth_fault`] attributes an auth fault only to a decparty in the
/// set, so a pass holding no set attributes nothing. A node whose every
/// credential fails would then count no auth fault at all, while the loop keeps
/// beating. This counter is that pass's only signal.
///
/// A node serving no decparty has nothing to read, and a node without the
/// rewards DAR is off by design, so neither counts.
fn counts_as_unreadable_enablement(read: &Enablement, served: usize) -> bool {
    *read == Enablement::Unreadable && served > 0
}

/// How long a read that only feeds a diagnostic may take. These reads run on the
/// task that carries the heartbeat, so a hung one must not stop the sweep.
const DIAGNOSTIC_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// What a pass that no token could read concludes from the participant's
/// packages, given as `package id -> name` (pure).
///
/// Only a confirmed absence is [`Enablement::Off`]. A present package leaves the
/// set unknown. `rewards` is a `#name` reference or a package id, as in
/// [`PackageConfig`].
fn enablement_without_a_token(
    rewards: Option<&str>,
    packages: &HashMap<String, String>,
) -> Enablement {
    let present = match rewards {
        // No rewards package is configured, so no delegation can name this node.
        None => false,
        Some(reference) => match reference.strip_prefix('#') {
            Some(name) => packages.values().any(|n| n == name),
            None => packages.contains_key(reference),
        },
    };
    if present {
        Enablement::Unreadable
    } else {
        Enablement::Off
    }
}

/// The decparties this node is a named assigner for.
///
/// Tries each served decparty in turn: the read needs any one working token, not
/// that decparty's own. A node whose every credential fails cannot ask, and
/// [`Enablement::Unreadable`] says so rather than claiming an empty set. An
/// empty set would read as "nothing is enabled" and silence a real fault.
///
/// When no token can read the set, the participant's admin API still lists its
/// packages without one. A participant without the rewards DAR is then
/// [`Enablement::Off`]. A failed package list stays [`Enablement::Unreadable`].
async fn read_enablement(
    data: &actix_web::web::Data<AppState>,
    parties: &[CantonId],
) -> Enablement {
    let pkgs = packages();
    for decparty in parties {
        let Ok(Some((token, member))) = get_party_credentials(data, decparty).await else {
            continue;
        };
        match delegated_decparties(&data.config, &pkgs, data.test_mode, &member, &token).await {
            Ok(set) => return Enablement::Read(set),
            // The package store belongs to the participant, so every other
            // token gets the same answer.
            Err(e) if package_absent(&e) => {
                tracing::trace!(
                    %member,
                    error = %e,
                    "reward automation is off: this participant holds no rewards DAR"
                );
                return Enablement::Off;
            }
            Err(e) => {
                // Another decparty's token may still work, so try the rest.
                tracing::warn!(%member, error = %e, "reading the delegated decparties failed");
            }
        }
    }
    let listed = tokio::time::timeout(
        DIAGNOSTIC_READ_TIMEOUT,
        fetch_package_id_to_name(&data.config),
    )
    .await;
    match listed {
        Ok(Ok(packages)) => {
            enablement_without_a_token(pkgs.governance_rewards.as_deref(), &packages)
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "listing the participant's packages failed");
            Enablement::Unreadable
        }
        Err(_) => {
            tracing::debug!("listing the participant's packages timed out");
            Enablement::Unreadable
        }
    }
}

/// Per-node background loop. It records a heartbeat every [`HEARTBEAT_INTERVAL`] and
/// sweeps each decparty it holds credentials for every
/// `reward_automation_interval_secs`. Enablement is on-ledger: a decparty with no
/// active delegation is skipped.
///
/// The heartbeat shares this task with the sweep deliberately, so a sweep that
/// hangs stops the beat and becomes visible.
pub(crate) async fn run_reward_automation_loop(data: actix_web::web::Data<AppState>) {
    let interval_secs = data.config.reward_automation_interval_secs;
    if interval_secs == 0 {
        tracing::warn!("reward_automation_interval_secs is 0; using 1s instead");
    }
    let sweep_interval = Duration::from_secs(interval_secs.max(1));
    let read_secs = data.config.reward_expiry_read_interval_secs;
    if read_secs == 0 {
        tracing::warn!("reward_expiry_read_interval_secs is 0; using 1s instead");
    }
    let read_interval = Duration::from_secs(read_secs.max(1));

    // The shortest of the three: a configured interval below HEARTBEAT_INTERVAL must
    // still beat at its own rate, or `is_due` only ever sees HEARTBEAT_INTERVAL-sized
    // steps and a short interval is silently floored to it. Beating faster is
    // safe for the stall alert, whose `increase < 1 over 15m` is a lower bound.
    let mut heartbeat =
        tokio::time::interval(HEARTBEAT_INTERVAL.min(sweep_interval).min(read_interval));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Records the wake time, not the tick's own deadline, so scheduling jitter can
    // stretch an interval by up to one heartbeat — benign against a 36h coupon TTL.
    let mut last_sweep: Option<tokio::time::Instant> = None;
    let mut last_read: Option<tokio::time::Instant> = None;
    // Recomputed into the gauge on every heartbeat.
    let mut oldest: HashMap<CantonId, DateTime<Utc>> = HashMap::new();
    // The decparties whose delegations name this node, from the last pass that
    // could read them. Kept across passes so a later credentials outage does not
    // stop the loop classifying: a set read once stays truer than no set at all.
    let mut enabled: Option<HashSet<CantonId>> = None;

    loop {
        heartbeat.tick().await;
        HEARTBEAT.inc();
        report_oldest_expiry(&oldest, Utc::now());

        let now = tokio::time::Instant::now();
        let Some(pass) = due_pass(
            last_sweep.map(|t| now.duration_since(t)),
            sweep_interval,
            last_read.map(|t| now.duration_since(t)),
            read_interval,
        ) else {
            continue;
        };
        if pass == Pass::Sweep {
            last_sweep = Some(now);
        }
        // A sweep reads the ledger too, so it satisfies the read clock.
        last_read = Some(now);

        let parties: Vec<CantonId> = data
            .party_credentials
            .read()
            .await
            .iter()
            .map(|p| p.dec_party_id.clone())
            .collect();

        prune_unserved(&mut oldest, &parties);

        let read = read_enablement(&data, &parties).await;
        if counts_as_unreadable_enablement(&read, parties.len()) {
            ENABLEMENT_UNREADABLE.inc();
            tracing::warn!(
                served = parties.len(),
                "no credential could read the delegated decparties; until one can, this node \
                 attributes no auth fault to any of them"
            );
        }
        if let Enablement::Read(set) = read {
            enabled = Some(set);
        }

        for decparty in parties {
            let label = decparty.to_string();
            // Only a sweep moves the sweep counters. A read that fails is not a
            // sweep that failed, and counting it would fire the sweep-failure
            // alert on a signal-path problem.
            match pass {
                Pass::Sweep => {
                    SWEEPS.with_label_values(&[&label]).inc();
                    ensure_outcome_series(&label);
                }
                Pass::ExpiryRead => {
                    EXPIRY_READS.with_label_values(&[&label]).inc();
                }
            }
            let outcome = match run_once_for_party(&data, &decparty, pass).await {
                Ok(outcome) => outcome,
                Err(e) if package_absent(&e) => {
                    tracing::trace!(
                        %decparty,
                        error = %e,
                        "reward automation is off: this participant holds no rewards DAR"
                    );
                    SweepOutcome::Off
                }
                Err(e) => {
                    if pass == Pass::Sweep {
                        SWEEP_FAILED.with_label_values(&[&label]).inc();
                    }
                    match pass {
                        Pass::Sweep => {
                            tracing::warn!(%decparty, error = %e, "reward automation sweep failed");
                        }
                        Pass::ExpiryRead => {
                            tracing::warn!(%decparty, error = %e, "reward expiry read failed");
                        }
                    }
                    SweepOutcome::Failed
                }
            };
            if counts_as_auth_fault(&outcome, &decparty, enabled.as_ref()) {
                match outcome {
                    SweepOutcome::CredentialsUnavailable => {
                        CREDENTIALS_UNAVAILABLE.with_label_values(&[&label]).inc();
                    }
                    SweepOutcome::AuthUnregistered => {
                        AUTH_UNREGISTERED.with_label_values(&[&label]).inc();
                    }
                    _ => unreachable!("counts_as_auth_fault admits only the two auth arms"),
                }
            }
            apply_sweep_outcome(&mut oldest, &decparty, outcome);
        }
    }
}

/// Whether an interval has elapsed (pure). `None` is always due: a fresh pod
/// drains its backlog rather than waiting out an interval.
fn is_due(since_last: Option<Duration>, interval: Duration) -> bool {
    since_last.is_none_or(|elapsed| elapsed >= interval)
}

/// What this node may do for a decparty whose delegation it has just read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    /// The delegation names this node, so it reads the backlog and assigns it.
    Assign,
    /// The delegation does not name this node. It reads the backlog anyway, so
    /// the expiry gauge keeps reporting, and assigns nothing.
    ReportOnly,
}

/// Whether the delegation lets this node assign (pure).
///
/// A delegation that exists and does not name this node is a governance mistake
/// far more often than an intention, and the coupons expire either way. So the
/// backlog still reaches the gauge, and only the assigning stops.
pub(super) fn role_for(delegation: &ActiveDelegation, member: &CantonId) -> Role {
    if delegation.assigners.contains(member) {
        Role::Assign
    } else {
        Role::ReportOnly
    }
}

/// The delegation's assigners that are not governance members (pure).
///
/// `Delegation_Assign` checks the assigner against the list fixed by the vote, so
/// a removed member keeps assigning until a new vote replaces the delegation
/// (#285). The reverse is not reported: a member need not be an assigner, and a
/// decparty may name a single assigner on purpose.
fn assigners_not_members(assigners: &[CantonId], members: &[CantonId]) -> Vec<CantonId> {
    assigners
        .iter()
        .filter(|a| !members.contains(a))
        .cloned()
        .collect()
}

/// Counts and logs a sweep whose delegation names an assigner that is no longer
/// a governance member. A failed or slow rules read reports nothing, because
/// the membership is then unknown.
async fn report_assigners_not_members(
    data: &actix_web::web::Data<AppState>,
    decparty: &CantonId,
    delegation: &ActiveDelegation,
    token: &str,
    pkgs: &PackageConfig,
) {
    let read = tokio::time::timeout(
        DIAGNOSTIC_READ_TIMEOUT,
        read_governance_state(&data.config, decparty, Some(token.to_string()), pkgs),
    )
    .await;
    let rules = match read {
        Ok(Ok(Some(rules))) => rules,
        Ok(Ok(None)) => return,
        Ok(Err(e)) => {
            tracing::debug!(%decparty, error = %e, "reading the governance members failed");
            return;
        }
        Err(_) => {
            tracing::debug!(%decparty, "reading the governance members timed out");
            return;
        }
    };
    let stale = assigners_not_members(&delegation.assigners, &rules.members);
    if !stale.is_empty() {
        ASSIGNER_NOT_MEMBER
            .with_label_values(&[&decparty.to_string()])
            .inc();
        tracing::warn!(
            %decparty,
            assigners = ?stale,
            "the delegation still names assigners that are no longer governance members; \
             a new SetupCouponReassignmentDelegation vote removes them"
        );
    }
}

/// What a pass over the served decparties does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    /// Read the backlog and assign every assignable coupon.
    Sweep,
    /// Read the backlog only, to refresh the expiry gauge between sweeps.
    ExpiryRead,
}

/// Which pass this tick owes, if any (pure).
///
/// A sweep wins whenever both are due, because a sweep reads the ledger anyway
/// and running both would pay for the same read twice.
fn due_pass(
    since_sweep: Option<Duration>,
    sweep_interval: Duration,
    since_read: Option<Duration>,
    read_interval: Duration,
) -> Option<Pass> {
    if is_due(since_sweep, sweep_interval) {
        Some(Pass::Sweep)
    } else if is_due(since_read, read_interval) {
        Some(Pass::ExpiryRead)
    } else {
        None
    }
}

/// A sweep that found coupons to assign and assigned none of them (pure).
///
/// Keying on the backlog rather than on the refusal tally is what makes the
/// contention `break` count: that path ends the sweep leaving both tallies at
/// zero, so a refusal-only test reads it as a sweep with nothing to do.
pub(super) fn sweep_assigned_nothing(assignable: usize, assigned: usize) -> bool {
    assignable > 0 && assigned == 0
}

/// Seconds from `now` until `expires_at` (pure). Negative once the moment has
/// passed, and sub-second precise.
fn seconds_until(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    (expires_at - now).num_milliseconds() as f64 / 1000.0
}

/// Publishes the expiry countdown for every remembered decparty.
fn report_oldest_expiry(oldest: &HashMap<CantonId, DateTime<Utc>>, now: DateTime<Utc>) {
    for (decparty, expires_at) in oldest {
        OLDEST_EXPIRES_IN
            .with_label_values(&[&decparty.to_string()])
            .set(seconds_until(*expires_at, now));
    }
}

/// What one sweep learned about a decparty's unassigned backlog.
enum SweepOutcome {
    /// The oldest unassigned coupon expires at this instant.
    Backlog(DateTime<Utc>),
    /// Nothing unassigned, or the automation is switched off for this decparty.
    Empty,
    /// The sweep errored before it could tell.
    Failed,
    /// Party credentials were not available this sweep — e.g. a Keycloak token
    /// acquisition failure. Distinct from `Empty`: this decparty is still meant
    /// to be served (it holds a row in `party_credentials`), so this is an
    /// error condition rather than the automation being off for it. Whether it
    /// moves a counter depends on enablement — see [`counts_as_auth_fault`].
    CredentialsUnavailable,
    /// The auth registry holds no entry for this decparty, so this node cannot
    /// read its backlog at all. `AuthRegistry::new` skips a party whose token
    /// manager fails to initialise and leaves its `party_credentials` row in
    /// place, so a bad Keycloak secret lands here on every pass until the
    /// registry is rebuilt. Distinct from `CredentialsUnavailable`, which is one
    /// pass's token failure and cures itself. A decparty this node genuinely
    /// stopped serving loses its `party_credentials` row and `prune_unserved`
    /// drops it, so this outcome is always a fault.
    AuthUnregistered,
    /// This participant holds no rewards DAR, so it cannot host a delegation and
    /// the automation is off for it. Distinct from `Empty`, which asserts that
    /// nothing is left to save and drops the gauge series: this outcome asserts
    /// nothing and changes nothing.
    Off,
}

/// Applies one sweep's outcome to the remembered timestamps. The arms are
/// asymmetrical on purpose: dropping the series stops both expiry rules, so only
/// an outcome that asserts the backlog is empty may do it. Every failure keeps
/// the countdown falling, because a coupon nobody can read still expires.
fn apply_sweep_outcome(
    oldest: &mut HashMap<CantonId, DateTime<Utc>>,
    decparty: &CantonId,
    outcome: SweepOutcome,
) {
    match outcome {
        SweepOutcome::Backlog(expires_at) => {
            oldest.insert(decparty.clone(), expires_at);
        }
        // Only `Empty` asserts nothing is left to save, so only `Empty` may drop
        // the series.
        SweepOutcome::Empty => {
            oldest.remove(decparty);
            let _ = OLDEST_EXPIRES_IN.remove_label_values(&[&decparty.to_string()]);
        }
        SweepOutcome::Failed
        | SweepOutcome::CredentialsUnavailable
        | SweepOutcome::AuthUnregistered
        | SweepOutcome::Off => {}
    }
}

/// Forgets every decparty this node no longer holds credentials for, dropping its
/// gauge series with it. A series left behind would count down forever.
fn prune_unserved(oldest: &mut HashMap<CantonId, DateTime<Utc>>, served: &[CantonId]) {
    oldest.retain(|decparty, _| {
        let still_served = served.contains(decparty);
        if !still_served {
            let _ = OLDEST_EXPIRES_IN.remove_label_values(&[&decparty.to_string()]);
        }
        still_served
    });
}

/// One reassign pass for a single decparty under the delegation model. A no-op
/// outcome (`SweepOutcome::Empty`) unless the decparty has an active
/// `CouponReassignmentDelegation` (the enablement signal) naming this node's
/// member party as an assigner. Credentials being unavailable is a distinct
/// outcome (`SweepOutcome::CredentialsUnavailable`): unlike the two enablement
/// no-ops, it is not this decparty's automation being switched off. A missing
/// registry entry is a third (`SweepOutcome::AuthUnregistered`), and it persists
/// where a token failure cures itself.
async fn run_once_for_party(
    data: &actix_web::web::Data<AppState>,
    decparty: &CantonId,
    pass: Pass,
) -> anyhow::Result<SweepOutcome> {
    let pkgs = packages();
    let (token, member) = match get_party_credentials(data, decparty).await {
        Ok(Some(creds)) => creds,
        // The callee logged the cause. One failed fetch is one pass's problem and
        // the decparty is still served, so the countdown keeps falling. Every pass
        // failing is not, and it looks the same in the log: the counter is what
        // separates them, and it is the only signal for a decparty broken from
        // boot, which never read a backlog and so has no gauge series.
        Err(_) => {
            tracing::warn!(%decparty, "party credentials unavailable for reward sweep");
            return Ok(SweepOutcome::CredentialsUnavailable);
        }
        // This node holds a `party_credentials` row for the decparty and the auth
        // registry has no entry for it, so every pass lands here until the
        // registry is rebuilt. The coupons keep expiring meanwhile, so the
        // countdown stays, and the caller counts the fault when a delegation
        // names this node for the decparty: one whose auth failed at boot never
        // read a backlog, so it has no gauge series for a rule to fire on.
        Ok(None) => {
            tracing::warn!(
                %decparty,
                "no auth registered for this decparty; its coupons will expire unassigned"
            );
            return Ok(SweepOutcome::AuthUnregistered);
        }
    };
    // Enablement: an active delegation. None => off (no-op).
    let Some(delegation) =
        active_delegation(&data.config, &pkgs, data.test_mode, decparty, &token).await?
    else {
        return Ok(SweepOutcome::Empty);
    };
    if pass == Pass::Sweep {
        report_assigners_not_members(data, decparty, &delegation, &token, &pkgs).await;
    }
    let role = role_for(&delegation, &member);
    if role == Role::ReportOnly {
        tracing::warn!(
            %decparty,
            %member,
            assigners = ?delegation.assigners,
            "the delegation does not name this node as an assigner; reporting the backlog without assigning"
        );
    }
    let expires_at = match (role, pass) {
        (Role::Assign, Pass::Sweep) => {
            run_reassign_once(
                &data.config,
                decparty,
                &member,
                &token,
                &delegation,
                data.test_mode,
                &pkgs,
            )
            .await?
        }
        (Role::Assign, Pass::ExpiryRead) | (Role::ReportOnly, _) => {
            read_oldest_expiry(
                &data.config,
                decparty,
                &delegation.dso,
                &token,
                data.test_mode,
                &pkgs,
            )
            .await?
        }
    };
    Ok(match expires_at {
        Some(t) => SweepOutcome::Backlog(t),
        None => SweepOutcome::Empty,
    })
}

/// The expiry of the oldest coupon this decparty still has time to save, read
/// without assigning anything.
///
/// This is the read half of [`run_reassign_once`]. It exists so the gauge can
/// refresh faster than the sweep cadence: the sweep interval is sized to fill a
/// `Delegation_Assign` chunk, which is a transaction-cost choice, and detection
/// should not inherit it.
///
/// The live/expired cutoff is `read_started_at`, one instant for the whole
/// stream, so every coupon is judged against the same clock. A coupon that
/// expires *during* the read therefore still counts, and the reported countdown
/// reaches this node slightly negative rather than the coupon vanishing from
/// the gauge — the honest signal for one just lost, and the next read drops it.
/// Only a coupon that has already been dead for a while is excluded, which is
/// what the rule is for (see [`oldest_expiry`]).
async fn read_oldest_expiry(
    config: &NodeConfig,
    decparty: &CantonId,
    dso: &CantonId,
    token: &str,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let read_started_at = Utc::now();
    let mut oldest = None;
    for_each_unassigned_coupon(
        config,
        decparty,
        dso,
        Some(token.to_string()),
        test_mode,
        packages,
        |coupon| {
            oldest = fold_oldest_expiry(oldest, coupon.expires_at, read_started_at);
            Ok(())
        },
    )
    .await?;
    Ok(oldest)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        super::test_support::{ALICE, BOB, CAROL, DAVE, EVE, GOV, dt},
        *,
    };
    use crate::{
        auth::{AuthRegistry, WorkflowAuth},
        config::{KeycloakConfig, PartyCredentials},
        server::AppState,
    };

    #[test]
    fn a_removed_member_is_reported_while_the_delegation_names_it() -> anyhow::Result<()> {
        // DAVE left the committee after the vote that named him.
        let assigners = [CantonId::parse(ALICE)?, CantonId::parse(DAVE)?];
        let members = [CantonId::parse(ALICE)?, CantonId::parse(BOB)?];

        assert_eq!(
            assigners_not_members(&assigners, &members),
            vec![CantonId::parse(DAVE)?]
        );
        Ok(())
    }

    #[test]
    fn a_member_who_is_not_an_assigner_is_not_reported() -> anyhow::Result<()> {
        // A delegation may name one assigner on purpose, as mainnet's
        // cbtc-network does. A new member is not named until a new vote either.
        let assigners = [CantonId::parse(ALICE)?];
        let members = [
            CantonId::parse(ALICE)?,
            CantonId::parse(BOB)?,
            CantonId::parse(EVE)?,
        ];

        assert!(assigners_not_members(&assigners, &members).is_empty());
        Ok(())
    }

    #[test]
    fn the_expiry_countdown_falls_with_real_time_not_with_the_sweep() {
        // Falls with real time, not with the sweep cadence.
        let decparty = CantonId::parse(GOV).expect("GOV is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();
        oldest.insert(decparty.clone(), expires_at);

        report_oldest_expiry(&oldest, dt("2026-07-20T12:00:00Z"));
        let six_hours = OLDEST_EXPIRES_IN
            .get_metric_with_label_values(&[&decparty.to_string()])
            .expect("label values are valid")
            .get();
        assert_eq!(six_hours, 6.0 * 3600.0);

        // Twenty minutes later, with no sweep in between.
        report_oldest_expiry(&oldest, dt("2026-07-20T12:20:00Z"));
        let after = OLDEST_EXPIRES_IN
            .get_metric_with_label_values(&[&decparty.to_string()])
            .expect("label values are valid")
            .get();
        assert_eq!(after, 6.0 * 3600.0 - 1200.0);
    }

    #[test]
    fn a_failed_sweep_keeps_the_countdown_running() {
        // A failure must leave the timestamp alone.
        let decparty = CantonId::parse(ALICE).expect("ALICE is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Backlog(expires_at));
        assert_eq!(oldest.get(&decparty), Some(&expires_at), "a backlog writes");

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Failed);
        assert_eq!(
            oldest.get(&decparty),
            Some(&expires_at),
            "a failure must leave the timestamp in place"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Empty);
        assert!(oldest.is_empty(), "an empty backlog removes");
    }

    #[test]
    fn unavailable_credentials_keep_the_countdown_running() {
        // A Keycloak token failure must not read as "safely off": the timestamp
        // must survive exactly as it does on `Failed`.
        // Its own decparty fixture, since these tests share a process-wide
        // metrics registry and run in parallel.
        let decparty = CantonId::parse(CAROL).expect("CAROL is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Backlog(expires_at));
        assert_eq!(oldest.get(&decparty), Some(&expires_at), "a backlog writes");

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::CredentialsUnavailable);
        assert_eq!(
            oldest.get(&decparty),
            Some(&expires_at),
            "unavailable credentials must leave the timestamp in place"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Empty);
        assert!(oldest.is_empty(), "an empty backlog removes");
    }

    #[test]
    fn a_missing_dar_changes_nothing() {
        // A participant with no rewards DAR is a no-op, not a failure and not an
        // empty backlog. `Empty` would drop the gauge series, which asserts that
        // nothing is left to save.
        let decparty = CantonId::parse(EVE).expect("EVE is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Backlog(expires_at));
        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Off);
        assert_eq!(
            oldest.get(&decparty),
            Some(&expires_at),
            "a missing DAR must leave the timestamp exactly as it was"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Empty);
        assert!(oldest.is_empty(), "an empty backlog still removes");
    }

    /// Both auth arms are reachable and each is reached its own way, so each gets
    /// its own case: one passing test would leave the other arm unexercised.
    #[tokio::test]
    async fn a_missing_registry_entry_is_its_own_outcome() -> anyhow::Result<()> {
        // `auth: None` is the same `Ok(None)` a decparty gets when
        // `AuthRegistry::new` skipped it, which is the case this arm exists for.
        let data = AppState::for_test(None).await?;
        let decparty = CantonId::parse(DAVE)?;

        let outcome = run_once_for_party(&data, &decparty, Pass::Sweep).await?;

        assert!(matches!(outcome, SweepOutcome::AuthUnregistered));
        Ok(())
    }

    /// The party authenticates at startup (`expires_in: 0` puts its token on the
    /// refresh path at once), then the Keycloak stand-in fails the next request —
    /// the one this pass makes.
    #[tokio::test]
    async fn a_failed_token_fetch_is_its_own_outcome() -> anyhow::Result<()> {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path_regex},
        };

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token": "t", "expires_in": 0})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let decparty = CantonId::parse(EVE)?;
        let registry = AuthRegistry::new(&[PartyCredentials {
            dec_party_id: decparty.clone(),
            member_party_id: CantonId::parse(BOB)?,
            user_id: "user".to_string(),
            keycloak: KeycloakConfig {
                url: server.uri(),
                realm: "test-realm".to_string(),
                client_id: "decman".to_string(),
                client_secret: Some("secret".to_string()),
                ..KeycloakConfig::default()
            },
            auth0: None,
            packages: PackageConfig::default(),
        }])
        .await?;
        let data = AppState::for_test(Some(WorkflowAuth::Keycloak(Arc::new(registry)))).await?;

        let outcome = run_once_for_party(&data, &decparty, Pass::Sweep).await?;

        assert!(matches!(outcome, SweepOutcome::CredentialsUnavailable));
        Ok(())
    }

    #[test]
    fn an_auth_failure_counts_only_for_a_decparty_this_node_assigns_for() {
        // A decparty nobody enabled loses nothing when its credentials fail, and
        // devnet held 28 such pairs against one real delegation. Counting them
        // made every alert a false positive.
        let enabled_party = CantonId::parse(GOV).expect("GOV is a valid canton id");
        let stranger = CantonId::parse(DAVE).expect("DAVE is a valid canton id");
        let enabled: HashSet<CantonId> = std::iter::once(enabled_party.clone()).collect();

        for outcome in [
            SweepOutcome::CredentialsUnavailable,
            SweepOutcome::AuthUnregistered,
        ] {
            assert!(
                counts_as_auth_fault(&outcome, &enabled_party, Some(&enabled)),
                "a delegation names this node for it, so its coupons are at risk"
            );
            assert!(
                !counts_as_auth_fault(&outcome, &stranger, Some(&enabled)),
                "no delegation names this node for it, so nothing is at risk"
            );
        }
    }

    #[test]
    fn an_unread_enablement_set_counts_nothing() {
        // `None` is "we could not ask", not "nothing is enabled". Counting on it
        // would alert for every decparty on a node whose first read has not
        // landed yet.
        let decparty = CantonId::parse(GOV).expect("GOV is a valid canton id");
        assert!(!counts_as_auth_fault(
            &SweepOutcome::AuthUnregistered,
            &decparty,
            None
        ));
        assert!(!counts_as_auth_fault(
            &SweepOutcome::CredentialsUnavailable,
            &decparty,
            None
        ));
    }

    #[test]
    fn an_unreadable_enablement_set_counts_on_a_serving_node() {
        // The blind pass attributes no auth fault to any decparty. Without this
        // counter, a node whose every credential fails reports nothing.
        assert!(counts_as_unreadable_enablement(&Enablement::Unreadable, 17));
    }

    #[test]
    fn an_unreadable_enablement_set_counts_nothing_on_an_idle_node() {
        // A node serving no decparty has nothing to read.
        assert!(!counts_as_unreadable_enablement(&Enablement::Unreadable, 0));
    }

    #[test]
    fn a_node_without_the_rewards_dar_counts_no_unreadable_enablement() {
        // Such a node is off by design. Counting it would page every node that
        // never took part in reward reassignment.
        assert!(!counts_as_unreadable_enablement(&Enablement::Off, 17));
    }

    #[test]
    fn a_read_enablement_set_counts_nothing() {
        let decparty = CantonId::parse(GOV).expect("GOV is a valid canton id");
        let enabled: HashSet<CantonId> = std::iter::once(decparty).collect();
        assert!(!counts_as_unreadable_enablement(
            &Enablement::Read(enabled),
            17
        ));
        assert!(!counts_as_unreadable_enablement(
            &Enablement::Read(HashSet::new()),
            17
        ));
    }

    /// `auth: None` gives every decparty the `Ok(None)` of a party whose auth
    /// failed at boot, so no credential can ask. The admin API port is closed,
    /// so the package check fails too, and nothing confirms that rewards are off.
    #[tokio::test]
    async fn a_node_that_can_neither_ask_nor_list_packages_cannot_read_its_enablement()
    -> anyhow::Result<()> {
        let closed_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.local_addr()?.port()
        };
        let mut state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("the test state is shared"))?;
        state.config.canton.admin_api_host = "127.0.0.1".to_string();
        state.config.canton.admin_api_port = closed_port;
        let data = actix_web::web::Data::new(state);
        let parties = [CantonId::parse(DAVE)?, CantonId::parse(EVE)?];

        assert_eq!(
            read_enablement(&data, &parties).await,
            Enablement::Unreadable
        );
        Ok(())
    }

    fn package_list(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect()
    }

    #[test]
    fn a_participant_without_the_rewards_package_is_off_without_a_token() {
        let packages = package_list(&[("abc123", "governance-core")]);
        assert_eq!(
            enablement_without_a_token(Some("#governance-rewards-automation-v1"), &packages),
            Enablement::Off
        );
        assert_eq!(
            enablement_without_a_token(Some("def456"), &packages),
            Enablement::Off
        );
    }

    #[test]
    fn a_participant_with_the_rewards_package_stays_unreadable_without_a_token() {
        // The DAR is there, so a delegation may name this node. Only a token can
        // tell, and none worked.
        let packages = package_list(&[("def456", "governance-rewards-automation-v1")]);
        assert_eq!(
            enablement_without_a_token(Some("#governance-rewards-automation-v1"), &packages),
            Enablement::Unreadable
        );
        assert_eq!(
            enablement_without_a_token(Some("def456"), &packages),
            Enablement::Unreadable
        );
    }

    #[test]
    fn no_configured_rewards_package_is_off_without_a_token() {
        assert_eq!(
            enablement_without_a_token(None, &package_list(&[])),
            Enablement::Off
        );
    }

    #[test]
    fn no_other_outcome_counts_as_an_auth_fault() {
        // The two auth arms are the only ones these counters may move on. A
        // failed sweep has its own counter and an empty backlog is not a fault.
        let decparty = CantonId::parse(GOV).expect("GOV is a valid canton id");
        let enabled: HashSet<CantonId> = std::iter::once(decparty.clone()).collect();
        for outcome in [
            SweepOutcome::Empty,
            SweepOutcome::Failed,
            SweepOutcome::Off,
            SweepOutcome::Backlog(dt("2026-07-20T18:00:00Z")),
        ] {
            assert!(
                !counts_as_auth_fault(&outcome, &decparty, Some(&enabled)),
                "only the two auth arms may move these counters"
            );
        }
    }

    #[test]
    fn losing_auth_for_a_decparty_keeps_its_countdown() {
        // `AuthRegistry::new` skips a party whose token manager fails to
        // initialise and leaves its `party_credentials` row alone, so a bad
        // Keycloak secret puts a decparty in the loop's party list and out of the
        // registry until a restart. `prune_unserved` cannot catch it, because that
        // keys on the rows, which still exist. The coupons expire meanwhile, so
        // both failures have to keep the countdown falling.
        let decparty = CantonId::parse(BOB).expect("BOB is a valid canton id");
        let mut oldest = HashMap::new();
        oldest.insert(decparty.clone(), dt("2026-07-20T18:00:00Z"));

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::CredentialsUnavailable);
        assert!(
            oldest.contains_key(&decparty),
            "one pass's token failure is transient, so the countdown keeps running"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::AuthUnregistered);
        assert!(
            oldest.contains_key(&decparty),
            "a missing registry entry does not empty the backlog, so the countdown stays"
        );
    }

    #[test]
    fn a_decparty_this_node_stopped_serving_is_forgotten() {
        // Otherwise the countdown outlives the credentials.
        let decparty = CantonId::parse(BOB).expect("BOB is a valid canton id");
        let mut oldest = HashMap::new();
        oldest.insert(decparty.clone(), dt("2026-07-20T18:00:00Z"));

        prune_unserved(&mut oldest, std::slice::from_ref(&decparty));
        assert!(
            oldest.contains_key(&decparty),
            "still served, so still tracked"
        );

        prune_unserved(&mut oldest, &[]);
        assert!(oldest.is_empty(), "no longer served, so forgotten");
    }

    #[test]
    fn the_first_sweep_of_a_process_is_always_due() {
        assert!(is_due(None, Duration::from_secs(21_600)));
    }

    #[test]
    fn a_sweep_is_due_once_the_interval_has_elapsed() {
        let interval = Duration::from_secs(600);
        assert!(!is_due(Some(Duration::from_secs(599)), interval));
        assert!(is_due(Some(Duration::from_secs(600)), interval));
        assert!(is_due(Some(Duration::from_secs(601)), interval));
    }

    // ---- the two clocks (due_pass) -------------------------------------------

    /// Devnet's cadence: a 6h sweep against the default hourly read.
    const SWEEP: Duration = Duration::from_secs(21_600);
    const READ: Duration = Duration::from_secs(3_600);

    fn secs(n: u64) -> Option<Duration> {
        Some(Duration::from_secs(n))
    }

    // ---- the assigner role (role_for) ---------------------------------------

    fn delegation_naming(assigners: &[&str]) -> ActiveDelegation {
        ActiveDelegation {
            cid: "00cid".to_string(),
            dso: CantonId::parse(GOV).expect("GOV is a valid canton id"),
            assigners: assigners
                .iter()
                .map(|a| CantonId::parse(a).expect("valid canton id"))
                .collect(),
            beneficiary_count: 2,
        }
    }

    #[test]
    fn a_listed_assigner_may_assign() {
        let d = delegation_naming(&[ALICE, BOB]);
        let me = CantonId::parse(ALICE).expect("ALICE is a valid canton id");
        assert_eq!(role_for(&d, &me), Role::Assign);
    }

    #[test]
    fn a_node_the_delegation_does_not_name_still_reports() {
        // A vote naming the wrong party is a mistake, not an off switch, so the
        // backlog must keep reaching the expiry gauge.
        let d = delegation_naming(&[ALICE, BOB]);
        let me = CantonId::parse(CAROL).expect("CAROL is a valid canton id");
        assert_eq!(role_for(&d, &me), Role::ReportOnly);
    }

    #[test]
    fn a_delegation_naming_nobody_reports() {
        let d = delegation_naming(&[]);
        let me = CantonId::parse(ALICE).expect("ALICE is a valid canton id");
        assert_eq!(role_for(&d, &me), Role::ReportOnly);
    }

    /// Shares ALICE's namespace, as every party allocated on one participant
    /// does.
    const ALICE_SIBLING: &str =
        "alice-ops::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// Shares ALICE's hint under a different participant's namespace.
    const ALICE_ON_ANOTHER_PARTICIPANT: &str =
        "alice::1220ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    #[test]
    fn only_the_exact_named_party_may_assign() -> anyhow::Result<()> {
        // `Delegation_Assign` requires the assigner to be an element of
        // `assigners`, which compares whole party ids. A node that matched on
        // namespace or hint alone would submit an assignment the ledger refuses
        // on every sweep.
        let d = delegation_naming(&[ALICE]);
        assert_eq!(role_for(&d, &CantonId::parse(ALICE)?), Role::Assign);
        assert_eq!(
            role_for(&d, &CantonId::parse(ALICE_SIBLING)?),
            Role::ReportOnly,
            "a sibling party on the assigner's participant is not the named assigner"
        );
        assert_eq!(
            role_for(&d, &CantonId::parse(ALICE_ON_ANOTHER_PARTICIPANT)?),
            Role::ReportOnly,
            "a namesake on another participant is not the named assigner"
        );
        Ok(())
    }

    // ---- the reward status (reward_status_from) ------------------------------

    #[test]
    fn the_first_tick_sweeps_rather_than_only_reading() {
        assert_eq!(due_pass(None, SWEEP, None, READ), Some(Pass::Sweep));
    }

    #[test]
    fn a_due_sweep_wins_over_a_due_read() {
        // A sweep reads the ledger anyway, so running both would pay for the
        // same read twice.
        assert_eq!(
            due_pass(secs(21_600), SWEEP, secs(3_600), READ),
            Some(Pass::Sweep)
        );
    }

    #[test]
    fn a_read_runs_between_sweeps() {
        assert_eq!(
            due_pass(secs(3_600), SWEEP, secs(3_600), READ),
            Some(Pass::ExpiryRead)
        );
    }

    #[test]
    fn neither_clock_due_runs_nothing() {
        assert_eq!(due_pass(secs(600), SWEEP, secs(600), READ), None);
    }

    #[test]
    fn a_read_interval_longer_than_the_sweep_never_binds() {
        // Mainnet's 300s default against an hourly read: every pass is a sweep,
        // so the read clock costs nothing.
        let sweep = Duration::from_secs(300);
        assert_eq!(
            due_pass(secs(300), sweep, secs(300), READ),
            Some(Pass::Sweep)
        );
        assert_eq!(due_pass(secs(120), sweep, secs(120), READ), None);
    }

    #[test]
    fn seconds_until_counts_down_to_the_expiry() {
        let now = dt("2026-07-20T12:00:00Z");
        assert_eq!(seconds_until(dt("2026-07-20T18:00:00Z"), now), 6.0 * 3600.0);
    }

    #[test]
    fn seconds_until_is_negative_for_an_expired_coupon() {
        // Negative rather than clamped: the value is lost, and by how long matters.
        let now = dt("2026-07-20T12:00:00Z");
        assert_eq!(seconds_until(dt("2026-07-20T11:00:00Z"), now), -3600.0);
    }

    #[test]
    fn seconds_until_keeps_sub_second_precision() {
        // The gauge is compared across samples minutes apart, so a truncated
        // second would read as drift.
        let now = dt("2026-07-20T12:00:00Z");
        assert_eq!(seconds_until(dt("2026-07-20T12:00:00.500Z"), now), 0.5);
    }

    #[test]
    fn a_sweep_that_paid_nothing_while_every_coupon_was_refused_counts() {
        assert!(sweep_assigned_nothing(3, 0));
    }

    #[test]
    fn a_sweep_that_ended_before_it_paid_anything_counts() {
        // The contention `break` ends a sweep with both tallies at zero while a
        // backlog is still waiting.
        assert!(sweep_assigned_nothing(9, 0));
    }

    #[test]
    fn a_sweep_that_paid_something_does_not_count() {
        assert!(!sweep_assigned_nothing(10, 7));
    }

    #[test]
    fn a_sweep_with_nothing_to_do_does_not_count() {
        assert!(!sweep_assigned_nothing(0, 0));
    }
}
