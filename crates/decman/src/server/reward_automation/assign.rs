// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The delegation-model assigner: the Delegation_Assign submit and the sweep.

use anyhow::Context;
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, ExerciseCommand, Identifier, Record, SubmitAndWaitRequest, Value, command,
    command_service_client::CommandServiceClient, value,
};
use chrono::{DateTime, Utc};
use decman_lib::framework::{
    commands::commands_envelope,
    encode::{field, make_contract_id, make_extra_args, make_list, make_party, make_text_map},
};

use super::{
    coupons::{CouponInfo, chunk_size, select_assignable, unassigned_coupons},
    delegation::ActiveDelegation,
    metrics::{ASSIGNED, SKIPPED, ZERO_ASSIGNED},
    scheduler::sweep_assigned_nothing,
};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    server::{package_inventory::fetch_package_id_to_name, queries::fetch_contract_template_id},
    utils,
};

// ============================================================================
// Delegation-model per-round assigner (`Delegation_Assign`)
// ============================================================================

/// Build the `Delegation_Assign` choice argument (pure): fields
/// `assigner, primaryCoupon, additionalCoupons, extraArgs`, in that order.
///
/// `extraArgs` is passed through to `RewardCoupon_AssignBeneficiaries`. splice
/// ignores it for `RewardCouponV2`, so an empty one is correct today; the
/// choice takes it so a later coupon version needing a context does not force
/// another DAML change.
fn build_delegation_assign_arg(
    assigner: &CantonId,
    primary: &str,
    additional: &[String],
) -> Record {
    Record {
        record_id: None,
        fields: vec![
            field("assigner", make_party(assigner)),
            field("primaryCoupon", make_contract_id(primary)),
            field(
                "additionalCoupons",
                make_list(additional.iter().map(|c| make_contract_id(c)).collect()),
            ),
            field("extraArgs", make_extra_args(make_text_map(vec![]))),
        ],
    }
}

/// Build the `Identifier` for the `CouponReassignmentDelegation` template
/// (pure). The DAML template lives in module `Governance.Rewards.
/// CouponReassignmentDelegation` (not the shorter `Governance.Rewards`) — the
/// Ledger API `module_name` must be the full module path or the exercise
/// resolves against the wrong (non-existent) template.
fn delegation_template_id(package_id: String) -> Identifier {
    Identifier {
        package_id,
        module_name: "Governance.Rewards.CouponReassignmentDelegation".to_string(),
        entity_name: "CouponReassignmentDelegation".to_string(),
    }
}

/// Localnet-only submit shim. The release binary does not compile this hook.
/// A per-node file lets the harness change faults without restarting the loop.
#[cfg(feature = "test-mode")]
async fn inject_assignment_failure(decparty: &CantonId) -> anyhow::Result<()> {
    let Some(path) = std::env::var_os("DECPM_TEST_REWARD_ASSIGN_FAILURE_FILE") else {
        return Ok(());
    };
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let Some((party, error_id)) = contents.trim().split_once(' ') else {
        anyhow::bail!("invalid reward assignment test fault");
    };
    if party != decparty.to_string() {
        return Ok(());
    }
    let status = match error_id {
        "LOCAL_VERDICT_LOCKED_CONTRACTS" => {
            tonic::Status::aborted(format!("{error_id}(10,test): injected contention"))
        }
        "COMMAND_PREPROCESSING_FAILED" => {
            tonic::Status::invalid_argument(format!("{error_id}(8,test): injected rejection"))
        }
        _ => anyhow::bail!("unsupported reward assignment test fault: {error_id}"),
    };
    Err(status.into())
}

/// Pin the exercise to the verified concrete package, including older versions
/// of the configured package name. Never trust module/entity names alone.
fn verified_delegation_template(
    actual: Identifier,
    configured: &str,
    package_name: Option<&str>,
) -> anyhow::Result<Identifier> {
    let expected = delegation_template_id(configured.to_string());
    let package_matches = match configured.strip_prefix('#') {
        Some(name) => package_name == Some(name),
        None => actual.package_id == configured,
    };
    anyhow::ensure!(
        package_matches
            && actual.module_name == expected.module_name
            && actual.entity_name == expected.entity_name,
        "refusing delegation from {}:{}:{}; expected {}:{}:{}",
        actual.package_id,
        actual.module_name,
        actual.entity_name,
        configured,
        expected.module_name,
        expected.entity_name,
    );
    Ok(actual)
}

/// Exercise `Delegation_Assign` as a plain ledger command (no governance
/// round). Adapted from `execute_confirm_action`
/// (`handlers/governance.rs:2107`); the differences are the target contract
/// (the delegation cid), the choice (`Delegation_Assign`), the template
/// (`Governance.Rewards.CouponReassignmentDelegation`), and `act_as =
/// [assigner]` / `read_as = [decparty]`, because the two are co-hosted.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn submit_delegation_assign(
    config: &NodeConfig,
    decparty: &CantonId,
    assigner: &CantonId,
    token: &str,
    delegation_cid: &str,
    primary: &str,
    additional: &[String],
    packages: &PackageConfig,
) -> anyhow::Result<()> {
    #[cfg(feature = "test-mode")]
    inject_assignment_failure(decparty).await?;

    let choice_argument = Value {
        sum: Some(value::Sum::Record(build_delegation_assign_arg(
            assigner, primary, additional,
        ))),
    };
    let fallback = packages
        .governance_rewards
        .as_deref()
        .context("governance_rewards package not configured")?;
    // A wildcard read under mock auth can find a look-alike delegation. Read
    // its actual template and fail closed before borrowing the member's authority.
    let actual =
        fetch_contract_template_id(config, decparty, Some(token.to_string()), delegation_cid)
            .await?
            .context("delegation created event has no template id")?;
    let package_name = if fallback.starts_with('#') {
        fetch_package_id_to_name(config)
            .await?
            .remove(&actual.package_id)
    } else {
        None
    };
    let template_id = verified_delegation_template(actual, fallback, package_name.as_deref())?;
    // `ledger_channel` (not a raw `Channel::from_shared`) so this respects the
    // configured Canton TLS/mTLS settings — the automation's own reads go through
    // it via `create_state_client`, and an assign that bypassed it would be the
    // one path unable to talk to a TLS-enabled ledger.
    let channel = config.ledger_channel().await?;
    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: delegation_cid.to_string(),
            choice: "Delegation_Assign".to_string(),
            choice_argument: Some(choice_argument),
        })),
    };
    // act_as = [assigner], read_as = [decparty] — the shared governance
    // envelope, with the assigner (not a governance member) in the actor slot.
    let commands = commands_envelope(
        uuid::Uuid::new_v4().to_string(),
        assigner,
        decparty,
        vec![cmd],
        vec![],
    );
    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    // A non-ASCII byte in the token makes this parse fail. The automation runs
    // in a detached task whose handle is dropped, so a panic here would kill
    // reward assignment silently while the process keeps serving HTTP.
    let auth = format!("Bearer {token}")
        .parse()
        .context("authorization header is not a valid metadata value")?;
    req.metadata_mut().insert("authorization", auth);
    client.submit_and_wait(req).await?; // an assign needs no created-cid readback
    Ok(())
}

/// The earliest expiry among unassigned coupons still worth saving (pure): every
/// coupon that has not yet expired counts, including ones inside the
/// assignability margin. An already-expired coupon is
/// excluded; the DSO's reaper does not archive it promptly (it can sit active
/// for weeks), so including it would pin the gauge to a corpse instead of the
/// backlog a responder can still act on.
fn oldest_expiry(coupons: &[CouponInfo], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    coupons.iter().fold(None, |oldest, c| {
        fold_oldest_expiry(oldest, c.expires_at, now)
    })
}

/// [`oldest_expiry`]'s rule for one coupon (pure), so a streaming read can hold
/// a running minimum instead of a list — see [`read_oldest_expiry`]. Folding in
/// any order gives the same answer as [`oldest_expiry`] over the whole set.
pub(super) fn fold_oldest_expiry(
    oldest: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if expires_at <= now {
        return oldest;
    }
    Some(match oldest {
        Some(current) => current.min(expires_at),
        None => expires_at,
    })
}

/// One reassign sweep for a decparty under the delegation model: read the
/// unassigned coupons and assign **all** the assignable ones, in successive
/// chunked `Delegation_Assign` transactions. Nothing assignable is a no-op.
///
/// A sweep drains the whole set rather than assigning one chunk and waiting for
/// the next sweep, so throughput does not depend on the sweep interval: the
/// interval is a latency/cost knob, not a safety-critical one. The chunk bounds
/// one *transaction* (see [`chunk_size`]).
///
/// A transient failure ends the sweep; a rejected command is isolated to the one
/// coupon at fault and the drain continues (see [`drain_assignable`]).
///
/// The create budget and the expiry margin come from [`NodeConfig`] so they can
/// be tuned against a live ledger without a rebuild.
pub(crate) async fn run_reassign_once(
    config: &NodeConfig,
    decparty: &CantonId,
    assigner: &CantonId,
    token: &str,
    delegation: &ActiveDelegation,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let expiry_margin =
        chrono::Duration::seconds(config.reward_min_expiry_margin_secs.min(i64::MAX as u64) as i64);

    let coupons = unassigned_coupons(
        config,
        decparty,
        &delegation.dso,
        Some(token.to_string()),
        test_mode,
        packages,
    )
    .await?;
    let now = Utc::now();
    let oldest = oldest_expiry(&coupons, now);
    let visible = coupons.len();
    let assignable = select_assignable(coupons, now, expiry_margin);
    if assignable.is_empty() {
        // `visible` is what the ACS read returned, so it separates "coupons
        // present but none assignable yet" from "nothing visible at all".
        //
        // It does NOT separate a decparty that earned nothing from one whose
        // coupons carry providerIsObserver = false (design §4): such a coupon
        // is absent from the ACS entirely, so both report 0. Telling those
        // apart needs a signal this node does not hold.
        tracing::debug!(%decparty, visible, "no assignable coupons this tick");
        return Ok(oldest);
    }

    let assigned = drain_assignable(
        &assignable,
        chunk_size(config.reward_max_creates, delegation.beneficiary_count),
        decparty,
        |primary, additional| {
            submit_delegation_assign(
                config,
                decparty,
                assigner,
                token,
                &delegation.cid,
                primary,
                additional,
                packages,
            )
        },
    )
    .await;
    if assigned > 0 {
        tracing::info!(%decparty, %assigner, count = assigned, "reassigned coupon batch");
    }
    Ok(oldest)
}

/// Canton error ids that mean this node's view is stale, or the network
/// faltered — not that the command itself was bad.
///
/// Keyed on the **error id**, never the gRPC status code. Devnet shows benign
/// contention arriving as three different codes: `LOCAL_VERDICT_LOCKED_CONTRACTS`
/// is `ABORTED`, `LOCAL_VERDICT_INACTIVE_CONTRACTS` is `NOT_FOUND`, and
/// `UNKNOWN_CONTRACT_SYNCHRONIZERS` is `FAILED_PRECONDITION`. A code-based rule
/// would read that last one as a bad command and fan out a chunk another node
/// had just assigned correctly. Canton's own retryability flag is no better: it
/// marks those categories non-retryable because *resubmitting* cannot work,
/// while this automation re-reads, which does fix them.
///
/// The timeouts come from the 2026-07-29 devnet outage, where five consecutive
/// sweeps each hit a different one and recovered unattended.
/// `MEDIATOR_SAYS_TX_TIMED_OUT` was observed on devnet on 2026-08-20: the coupon
/// it named assigned cleanly on the very next sweep, so the mediator's missing
/// confirmations were the fault and the coupon was never bad.
const TRANSIENT_ASSIGN_ERROR_IDS: &[&str] = &[
    "LOCAL_VERDICT_LOCKED_CONTRACTS",
    "LOCAL_VERDICT_INACTIVE_CONTRACTS",
    "UNKNOWN_CONTRACT_SYNCHRONIZERS",
    "CONTRACT_NOT_FOUND",
    "LOCAL_VERDICT_TIMEOUT",
    "NOT_SEQUENCED_TIMEOUT",
    "SEQUENCER_BACKPRESSURE",
    "REQUEST_TIME_OUT",
    "MEDIATOR_SAYS_TX_TIMED_OUT",
];

/// The Canton error id a failed submission carries, i.e. the message prefix
/// before `(`. `None` when the error is not a ledger rejection at all.
fn canton_error_id(e: &anyhow::Error) -> Option<String> {
    let status = e.downcast_ref::<tonic::Status>()?;
    let id = status.message().split('(').next()?.trim();
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
        .then(|| id.to_string())
}

/// Whether a read failed only because a package it names is absent from this
/// participant.
///
/// Both reads name their package by alias, so the ledger rejects the request
/// before it reads a contract. A node in that state fails identically on every
/// sweep and no operator can act on the line, so the loop logs it at `trace`.
///
/// One id covers both reads, because the delegation read runs first: a missing
/// `governance-rewards-automation-v1` fails there, and a missing
/// `splice-api-reward-assignment-v1` fails the same way on the coupon read.
pub(super) fn package_absent(e: &anyhow::Error) -> bool {
    canton_error_id(e).as_deref() == Some("PACKAGE_NAMES_NOT_FOUND")
}

/// The transient ids that another assigner causes by reaching the coupons first.
///
/// On a decparty with several assigners, every node but one loses each round.
/// That is the design working, so these log at `debug`. The other transient ids
/// are timeouts and backpressure. They stay at `warn`, because they point at the
/// synchronizer.
const CONTENTION_ERROR_IDS: &[&str] = &[
    "LOCAL_VERDICT_LOCKED_CONTRACTS",
    "LOCAL_VERDICT_INACTIVE_CONTRACTS",
    "UNKNOWN_CONTRACT_SYNCHRONIZERS",
    "CONTRACT_NOT_FOUND",
];

/// Whether a fresh read on the next sweep is the cure.
///
/// A non-ledger failure (config, transport, auth) is transient here too: it is
/// not attributable to any one coupon, so isolating one would be meaningless.
fn assign_failure_is_transient(e: &anyhow::Error) -> bool {
    match canton_error_id(e) {
        Some(id) => TRANSIENT_ASSIGN_ERROR_IDS.contains(&id.as_str()),
        None => true,
    }
}

/// Whether another assigner reaching the coupons first explains the failure.
fn assign_failure_is_contention(e: &anyhow::Error) -> bool {
    canton_error_id(e).is_some_and(|id| CONTENTION_ERROR_IDS.contains(&id.as_str()))
}

/// Logs a transient failure that ends the sweep: contention at `debug`, and
/// anything else at `warn`.
fn log_sweep_end(
    e: &anyhow::Error,
    decparty: &CantonId,
    coupon: &str,
    assigned: usize,
    remaining: usize,
    message: &str,
) {
    if assign_failure_is_contention(e) {
        tracing::debug!(%decparty, error = %e, coupon, assigned, remaining, "{message}");
    } else {
        tracing::warn!(%decparty, error = %e, coupon, assigned, remaining, "{message}");
    }
}

/// Assign `assignable` in `chunk`-sized batches via `submit`, returning how many
/// coupons were assigned. The submit step is injected so the chunking, the
/// failure rule and the fan-out are unit-testable without a ledger.
///
/// **A transient failure ends the drain; a rejected command does not.**
///
/// Contention is the overwhelmingly common failure — on a 3-assigner devnet two
/// nodes lose every round — and there a fresh read is the only cure, so the sweep
/// ends and the coupons keep their full TTL. That is [`TRANSIENT_ASSIGN_ERROR_IDS`].
///
/// Any other rejection is attributable to the batch's contents, so the drain
/// re-submits that chunk's coupons **one at a time**, logs each one the ledger
/// still refuses at ERROR, and carries on with the next chunk. splice fetches
/// and validates every `additionalCoupon`, so the culprit can sit anywhere in
/// the chunk; submitting each coupon alone pays every healthy one without
/// having to locate it first.
///
/// One rejected chunk of `n` costs `1 + n` submissions, and that ceiling holds
/// however many of its coupons the ledger refuses. It suits the causes a
/// rejection actually has, because each one is correlated across coupons rather
/// than isolated to a single coupon: a package skew on `RewardCouponV2`, a split
/// splice refuses, a choice context a later coupon version requires. Most or all
/// of a chunk then fails together, so the ERROR lines name every coupon at fault.
///
/// The skip lasts **one sweep only** — there is no cross-sweep quarantine, so the
/// drain stays stateless and a misclassified failure costs one sweep rather than
/// stranding a healthy coupon until restart. A genuinely un-exerciseable coupon
/// is therefore re-found every sweep, which is the point: it keeps producing an
/// ERROR for alerting while every healthy coupon still gets paid.
async fn drain_assignable<'a, F, Fut>(
    assignable: &'a [String],
    chunk: usize,
    decparty: &CantonId,
    mut submit: F,
) -> usize
where
    F: FnMut(&'a str, &'a [String]) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let size = chunk.max(1);
    let mut assigned = 0;
    let mut skipped = 0;

    // Chunks in most-urgent-first order. A rejected chunk is re-submitted one
    // coupon at a time, so a batch the ledger refuses costs 1+n, not a stall.
    'chunks: for lo in (0..assignable.len()).step_by(size) {
        let hi = (lo + size).min(assignable.len());
        let (primary, additional) = assignable[lo..hi]
            .split_first()
            .expect("ranges are built non-empty");

        match submit(primary, additional).await {
            Ok(()) => {
                assigned += hi - lo;
                continue;
            }
            Err(e) if assign_failure_is_transient(&e) => {
                log_sweep_end(
                    &e,
                    decparty,
                    primary,
                    assigned,
                    assignable.len() - lo,
                    "assign chunk failed; ending the sweep to re-read the ledger",
                );
                break;
            }
            // Already a single coupon: there is nothing to fan out to.
            Err(e) if hi - lo == 1 => {
                skipped += 1;
                tracing::error!(
                    %decparty,
                    error = %e,
                    coupon = %primary,
                    error_id = canton_error_id(&e).unwrap_or_default(),
                    "coupon rejected on its own; skipping it for this sweep"
                );
                continue;
            }
            Err(e) => tracing::warn!(
                %decparty,
                error = %e,
                error_id = canton_error_id(&e).unwrap_or_default(),
                coupons = hi - lo,
                "assign chunk rejected; re-submitting its coupons one at a time"
            ),
        }

        for (i, coupon) in assignable[lo..hi].iter().enumerate() {
            // An empty slice borrowed from `assignable` keeps `submit`'s lifetime.
            let alone = &assignable[lo + i..lo + i];
            match submit(coupon, alone).await {
                Ok(()) => assigned += 1,
                Err(e) if assign_failure_is_transient(&e) => {
                    log_sweep_end(
                        &e,
                        decparty,
                        coupon,
                        assigned,
                        assignable.len() - (lo + i),
                        "assign failed mid fan-out; ending the sweep to re-read the ledger",
                    );
                    break 'chunks;
                }
                Err(e) => {
                    skipped += 1;
                    tracing::error!(
                        %decparty,
                        error = %e,
                        coupon = %coupon,
                        error_id = canton_error_id(&e).unwrap_or_default(),
                        "coupon rejected on its own; skipping it for this sweep"
                    );
                }
            }
        }
    }

    let label = decparty.to_string();
    if assigned > 0 {
        ASSIGNED
            .with_label_values(&[&label])
            .inc_by(assigned as u64);
    }
    if skipped > 0 {
        SKIPPED.with_label_values(&[&label]).inc_by(skipped as u64);
        tracing::error!(
            %decparty,
            skipped,
            assigned,
            "some coupons could not be assigned; they will be retried next sweep"
        );
    }
    if sweep_assigned_nothing(assignable.len(), assigned) {
        ZERO_ASSIGNED.with_label_values(&[&label]).inc();
    }
    assigned
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use decman_lib::catalog::types::RewardBeneficiary;

    use super::{
        super::test_support::{DAVE, alice, canton_err, cids, coupon, dt},
        *,
    };

    /// Test-only helper mirroring `server::types` tests: builds a
    /// [`RewardBeneficiary`] from a Canton-ID prefix + a decimal percentage
    /// string. A fixed valid namespace keeps party ids parseable; distinct
    /// prefixes yield distinct parties.
    fn rb(prefix: &str, pct: &str) -> RewardBeneficiary {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        RewardBeneficiary {
            beneficiary: CantonId::parse(&format!("{prefix}::{ns}")).expect("valid canton id"),
            percentage: pct.parse().expect("valid decimal"),
        }
    }

    #[test]
    fn delegation_identity_rejects_lookalikes_and_missing_inventory() -> anyhow::Result<()> {
        let actual = delegation_template_id("concrete-id".into());
        assert!(
            verified_delegation_template(
                actual.clone(),
                "#governance-rewards-v1",
                Some("attacker-rewards")
            )
            .is_err()
        );
        assert!(
            verified_delegation_template(actual.clone(), "#governance-rewards-v1", None).is_err()
        );
        assert!(verified_delegation_template(actual.clone(), "other-id", None).is_err());
        let verified = verified_delegation_template(
            actual.clone(),
            "#governance-rewards-v1",
            Some("governance-rewards-v1"),
        )?;
        assert_eq!(verified.package_id, "concrete-id");
        assert!(verified_delegation_template(actual.clone(), "concrete-id", None).is_ok());
        let mut wrong_template = actual;
        wrong_template.entity_name = "DifferentChoiceCarrier".into();
        assert!(verified_delegation_template(wrong_template, "concrete-id", None).is_err());
        Ok(())
    }

    /// Sizes of the chunks a drain submitted, in order.
    type ChunkSizes = std::rc::Rc<std::cell::RefCell<Vec<usize>>>;

    /// Record each submitted chunk's size, answering with `outcomes` in order
    /// (`true` = Ok). Runs out of outcomes => Ok.
    fn recorder(
        outcomes: Vec<bool>,
    ) -> (
        ChunkSizes,
        impl FnMut(&str, &[String]) -> std::future::Ready<anyhow::Result<()>>,
    ) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let mut i = 0;
        let f = move |_p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            let ok = outcomes.get(i).copied().unwrap_or(true);
            i += 1;
            std::future::ready(if ok {
                Ok(())
            } else {
                Err(anyhow!("submission rejected"))
            })
        };
        (seen, f)
    }

    #[tokio::test]
    async fn drain_assigns_the_whole_set_in_one_pass() {
        // The point of draining: 120 coupons at a chunk of 50 is three
        // transactions in ONE sweep, not one transaction and a wait.
        let all = cids(120);
        let (seen, submit) = recorder(vec![]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 120);
        assert_eq!(*seen.borrow(), vec![50, 50, 20]);
    }

    #[tokio::test]
    async fn drain_ends_the_sweep_on_a_failed_chunk() {
        // A failure means the view is stale, and only a fresh read fixes that.
        // Exactly one attempt: no retry at a smaller size (a smaller chunk is
        // not a newer view) and no skip-and-continue (an assigner that took the
        // first coupon has most likely taken the rest).
        let all = cids(120);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![50], "one attempt, then end the sweep");
    }

    #[tokio::test]
    async fn drain_keeps_the_chunks_it_already_committed() {
        // Ending the sweep must not discard earlier successes: the first chunk is
        // assigned, the second fails, and the remainder waits for the next sweep
        // with its full TTL intact.
        let all = cids(120);
        let (seen, submit) = recorder(vec![true, false]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 50);
        assert_eq!(*seen.borrow(), vec![50, 50]);
    }

    #[tokio::test]
    async fn drain_attempts_once_when_the_set_is_smaller_than_the_chunk() {
        // One assignable coupon against a chunk of 50 is the shape devnet
        // contention actually takes. It must cost a single submission.
        let all = cids(1);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![1]);
    }

    // ---- failure classification + fan-out -----------------------------------

    /// Reject any chunk containing `poison` with `err_id`; accept every other
    /// chunk. Records the size of each attempted chunk.
    fn poisoned(
        poison: &'static str,
        err_id: &'static str,
    ) -> (
        ChunkSizes,
        impl FnMut(&str, &[String]) -> std::future::Ready<anyhow::Result<()>>,
    ) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let f = move |p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            let hit = p == poison || additional.iter().any(|c| c == poison);
            std::future::ready(if hit { Err(canton_err(err_id)) } else { Ok(()) })
        };
        (seen, f)
    }

    /// Reject every submission with `err_id`, recording each attempted size.
    fn all_rejected(
        err_id: &'static str,
    ) -> (
        ChunkSizes,
        impl FnMut(&str, &[String]) -> std::future::Ready<anyhow::Result<()>>,
    ) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let f = move |_p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            std::future::ready(Err(canton_err(err_id)))
        };
        (seen, f)
    }

    /// Expected attempt sizes for one chunk of `n` that fails and fans out.
    fn fanned(n: usize) -> Vec<usize> {
        std::iter::once(n)
            .chain(std::iter::repeat_n(1, n))
            .collect()
    }

    #[tokio::test]
    async fn contention_ends_the_sweep_without_fanning_out() {
        // The common case: two of three assigners lose every devnet round. A
        // fresh read is the only cure, so this must cost ONE submission —
        // fanning out here would burn n failing transactions per sweep.
        for id in [
            "LOCAL_VERDICT_LOCKED_CONTRACTS",
            "LOCAL_VERDICT_INACTIVE_CONTRACTS",
            "UNKNOWN_CONTRACT_SYNCHRONIZERS",
        ] {
            let all = cids(64);
            let (seen, submit) = poisoned("c0", id);
            let assigned = drain_assignable(&all, 64, &alice(), submit).await;
            assert_eq!(assigned, 0, "{id} must not assign");
            assert_eq!(*seen.borrow(), vec![64], "{id} must not fan out");
        }
    }

    #[tokio::test]
    async fn a_rejected_coupon_is_isolated_and_the_rest_still_drain() {
        // The head-of-line stall this fix exists for: one coupon the ledger
        // will never accept must not cost the other 63.
        let all = cids(64);
        let (seen, submit) = poisoned("c0", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 64, &alice(), submit).await;
        assert_eq!(assigned, 63, "every healthy coupon is assigned");
        // One batched attempt, then every coupon in that chunk on its own:
        // 1+n submissions, of which n-1 commit.
        assert_eq!(*seen.borrow(), fanned(64));
    }

    #[tokio::test]
    async fn a_rejected_coupon_is_found_wherever_it_sits() {
        // splice fetches and validates every additionalCoupon, so the culprit
        // need not be the primary. Dropping only the primary would advance one
        // coupon per sweep; submitting each on its own does not care where it sat.
        let all = cids(16);
        let (_, submit) = poisoned("c11", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 16, &alice(), submit).await;
        assert_eq!(assigned, 15);
    }

    #[tokio::test]
    async fn a_rejected_coupon_does_not_stop_later_chunks() {
        // The poison sits in chunk 1 of 3. Chunks 2 and 3 must still go, and
        // they must stay batched — only the rejected chunk fans out.
        let all = cids(30);
        let (seen, submit) = poisoned("c3", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 10, &alice(), submit).await;
        assert_eq!(assigned, 29);
        let mut expected = fanned(10);
        expected.extend([10, 10]);
        assert_eq!(*seen.borrow(), expected);
    }

    #[tokio::test]
    async fn a_rejected_singleton_chunk_is_not_resubmitted() {
        // A chunk of one has nothing to fan out to. Re-submitting it would
        // double the cost of the commonest rejection and change nothing.
        let all = cids(1);
        let (seen, submit) = poisoned("c0", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![1]);
    }

    #[tokio::test]
    async fn a_wholly_bad_batch_costs_one_pass_not_two() {
        // The realistic shape: the cause is correlated (a package skew, a split
        // splice refuses), so every coupon fails. The cost stays 1+n, and every
        // coupon at fault is named.
        let all = cids(8);
        let (seen, submit) = all_rejected("DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 8, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), fanned(8));
    }

    #[tokio::test]
    async fn contention_during_the_fan_out_ends_the_sweep() {
        // Another assigner took the rest of the chunk while we were fanning out.
        // A fresh read is the cure, so stop — and keep what already committed.
        let all = cids(4);
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let submit = move |_p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            let calls = log.borrow().len();
            std::future::ready(match calls {
                1 => Err(canton_err("DAML_INTERPRETATION_ERROR")), // the batch
                3 => Err(canton_err("LOCAL_VERDICT_LOCKED_CONTRACTS")),
                _ => Ok(()),
            })
        };
        let assigned = drain_assignable(&all, 4, &alice(), submit).await;
        assert_eq!(assigned, 1, "the singleton that committed is kept");
        assert_eq!(
            *seen.borrow(),
            vec![4, 1, 1],
            "stops at the transient error"
        );
    }

    #[tokio::test]
    async fn an_unrecognized_ledger_rejection_is_isolated_not_swallowed() {
        // An id we have never seen is treated as a bad command, so a coupon
        // that can never be exercised is still contained. The cost of being
        // wrong is one sweep: nothing is quarantined across sweeps.
        let all = cids(8);
        let (_, submit) = poisoned("c0", "SOME_FUTURE_CANTON_ERROR");
        let assigned = drain_assignable(&all, 8, &alice(), submit).await;
        assert_eq!(assigned, 7);
    }

    #[tokio::test]
    async fn a_non_ledger_failure_ends_the_sweep() {
        // A transport or config failure is not attributable to any one coupon,
        // so isolating one would be meaningless.
        let all = cids(8);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 8, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![8], "no fan-out on a non-ledger error");
    }

    #[test]
    fn transient_classification_keys_on_the_error_id_not_the_grpc_code() {
        // All three devnet contention errors arrive under DIFFERENT gRPC codes
        // (ABORTED / NOT_FOUND / FAILED_PRECONDITION), so a code-based rule
        // would misread one of them. Same code here, different ids.
        assert!(assign_failure_is_transient(&canton_err(
            "LOCAL_VERDICT_LOCKED_CONTRACTS"
        )));
        assert!(!assign_failure_is_transient(&canton_err(
            "DAML_INTERPRETATION_ERROR"
        )));
        // FAILED_PRECONDITION carrying benign contention — the case a
        // code-based classifier gets wrong.
        let fp = anyhow::Error::new(tonic::Status::failed_precondition(
            "UNKNOWN_CONTRACT_SYNCHRONIZERS(9,6a504b42): The following contracts have been archived",
        ));
        assert!(assign_failure_is_transient(&fp));
    }

    #[test]
    fn a_mediator_confirmation_timeout_is_transient() {
        // Devnet, 2026-08-20: this arrived as ABORTED and was counted a coupon
        // rejection, so it logged at ERROR and fired the coupon-rejected alert.
        // The same coupon assigned on the next sweep, so nothing was wrong with
        // it. Every other timeout in the list is classified the same way.
        let e = anyhow::Error::new(tonic::Status::aborted(
            "MEDIATOR_SAYS_TX_TIMED_OUT(2,0): Rejected transaction as the mediator did not \
             receive sufficient confirmations within the expected timeframe.",
        ));
        assert_eq!(
            canton_error_id(&e).as_deref(),
            Some("MEDIATOR_SAYS_TX_TIMED_OUT")
        );
        assert!(
            assign_failure_is_transient(&e),
            "a mediator timeout is not attributable to the coupon"
        );
    }

    #[test]
    fn contention_is_told_apart_from_the_other_transient_failures() {
        // Two of three devnet assigners lose every round, so contention must
        // not log at warn. A timeout still does: it points at the synchronizer.
        for id in CONTENTION_ERROR_IDS {
            let e = canton_err(id);
            assert!(assign_failure_is_contention(&e), "{id} is contention");
            assert!(
                assign_failure_is_transient(&e),
                "{id} must still end the sweep"
            );
        }
        for id in TRANSIENT_ASSIGN_ERROR_IDS
            .iter()
            .filter(|id| !CONTENTION_ERROR_IDS.contains(id))
        {
            assert!(
                !assign_failure_is_contention(&canton_err(id)),
                "{id} is not contention"
            );
        }
        assert!(!assign_failure_is_contention(&canton_err(
            "DAML_INTERPRETATION_ERROR"
        )));
        assert!(!assign_failure_is_contention(&anyhow::anyhow!(
            "connection refused"
        )));
    }

    #[test]
    fn canton_error_id_survives_added_context() {
        // A future `.context(..)` on the submit call must not silently turn
        // every rejection into an unclassifiable one.
        let e = Err::<(), _>(canton_err("LOCAL_VERDICT_LOCKED_CONTRACTS"))
            .context("submitting Delegation_Assign")
            .unwrap_err();
        assert_eq!(
            canton_error_id(&e).as_deref(),
            Some("LOCAL_VERDICT_LOCKED_CONTRACTS")
        );
        assert!(assign_failure_is_transient(&e));
    }

    #[test]
    fn only_an_absent_package_is_quiet() {
        // The devnet message, verbatim. The gRPC code here is deliberately not
        // the one Canton sends: the error id is what classifies.
        let absent = anyhow::Error::new(tonic::Status::internal(
            "PACKAGE_NAMES_NOT_FOUND(11,21a20a9f): The following package names do not match \
             upgradable packages uploaded on this participant: [governance-rewards-automation-v1].",
        ));
        assert!(package_absent(&absent));
        // A context added on the way up must not send the line back to warn.
        assert!(package_absent(
            &Err::<(), _>(absent)
                .context("reading the delegation")
                .unwrap_err()
        ));

        // Another ledger rejection, then a failure carrying no ledger status at
        // all, such as auth or transport.
        assert!(!package_absent(&canton_err("DAML_INTERPRETATION_ERROR")));
        assert!(!package_absent(&anyhow::anyhow!("connection refused")));
    }

    #[test]
    fn build_delegation_assign_arg_shape() {
        // rb(..).beneficiary yields a CantonId (this module has no canton_id helper);
        // rb takes a bare prefix and appends a fixed namespace itself.
        let rec =
            build_delegation_assign_arg(&rb("m1", "1.0").beneficiary, "00c1", &["00c2".into()]);
        let labels: Vec<&str> = rec.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "assigner",
                "primaryCoupon",
                "additionalCoupons",
                "extraArgs"
            ]
        );
    }

    #[test]
    fn delegation_template_id_uses_full_module_path() {
        // The DAML template lives in `Governance.Rewards.CouponReassignmentDelegation`,
        // not the shorter `Governance.Rewards` — a truncated module_name makes the
        // Ledger API resolve the exercise against a non-existent template.
        let id = delegation_template_id("pkg123".to_string());
        assert_eq!(id.package_id, "pkg123");
        assert_eq!(
            id.module_name,
            "Governance.Rewards.CouponReassignmentDelegation"
        );
        assert_eq!(id.entity_name, "CouponReassignmentDelegation");
    }

    #[test]
    fn oldest_expiry_takes_the_earliest_of_the_pile() {
        // Not the first read, and not the most urgent assignable one.
        let now = dt("2026-07-20T00:00:00Z");
        let coupons = vec![
            coupon("a", "2026-07-20T18:00:00Z"),
            coupon("b", "2026-07-20T12:00:00Z"),
            coupon("c", "2026-07-20T15:00:00Z"),
        ];
        assert_eq!(
            oldest_expiry(&coupons, now),
            Some(dt("2026-07-20T12:00:00Z"))
        );
    }

    #[test]
    fn oldest_expiry_is_none_for_an_empty_backlog() {
        // The loop reads this as "drop the series", so it must not be a zero.
        let now = dt("2026-07-20T00:00:00Z");
        assert_eq!(oldest_expiry(&[], now), None);
    }

    #[test]
    fn oldest_expiry_ignores_an_expired_coupon_in_favour_of_a_later_live_one() {
        // The expired coupon has the numerically smallest `expires_at` in the
        // pile — without the expiry filter it would win the minimum.
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![
            coupon("dead", "2026-07-19T00:00:00Z"),
            coupon("live", "2026-07-20T18:00:00Z"),
        ];
        assert_eq!(
            oldest_expiry(&coupons, now),
            Some(dt("2026-07-20T18:00:00Z"))
        );
    }

    #[test]
    fn oldest_expiry_is_none_when_every_coupon_has_expired() {
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![
            coupon("dead-1", "2026-07-19T00:00:00Z"),
            coupon("dead-2", "2026-07-18T00:00:00Z"),
        ];
        assert_eq!(oldest_expiry(&coupons, now), None);
    }

    #[test]
    fn folding_expiries_one_at_a_time_matches_reading_the_whole_pile() {
        // `read_oldest_expiry` never holds the pile: it folds each coupon as the
        // ACS streams past. This pins that fold to the same answer the collected
        // read gives, in either arrival order, so the two paths cannot drift.
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![
            coupon("dead", "2026-07-19T00:00:00Z"),
            coupon("late", "2026-07-24T00:00:00Z"),
            coupon("soon", "2026-07-21T00:00:00Z"),
        ];
        let expected = oldest_expiry(&coupons, now);
        assert_eq!(expected, Some(dt("2026-07-21T00:00:00Z")));

        let fold = |order: &[&CouponInfo]| {
            order
                .iter()
                .fold(None, |acc, c| fold_oldest_expiry(acc, c.expires_at, now))
        };
        let forward: Vec<&CouponInfo> = coupons.iter().collect();
        let backward: Vec<&CouponInfo> = coupons.iter().rev().collect();
        assert_eq!(fold(&forward), expected);
        assert_eq!(fold(&backward), expected);
    }

    #[test]
    fn folding_an_expired_coupon_leaves_the_running_oldest_alone() {
        // An already-expired coupon can sit active for weeks, so it must not
        // become the gauge's answer — not even when it arrives first.
        let now = dt("2026-07-20T12:00:00Z");
        let live = dt("2026-07-21T00:00:00Z");
        let dead = dt("2026-07-19T00:00:00Z");
        assert_eq!(fold_oldest_expiry(None, dead, now), None);
        assert_eq!(fold_oldest_expiry(Some(live), dead, now), Some(live));
        assert_eq!(
            fold_oldest_expiry(fold_oldest_expiry(None, dead, now), live, now),
            Some(live)
        );
    }

    #[tokio::test]
    async fn drain_counts_a_sweep_that_ended_before_assigning_anything() {
        // Its own decparty fixture, since these tests share a process-wide
        // metrics registry and run in parallel.
        let decparty = CantonId::parse(DAVE).expect("DAVE is a valid canton id");
        let label = decparty.to_string();
        let read = || {
            ZERO_ASSIGNED
                .get_metric_with_label_values(&[&label])
                .expect("label values are valid")
                .get()
        };
        let before = read();

        let all = cids(120);
        let (_seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 50, &decparty, submit).await;

        assert_eq!(assigned, 0);
        assert_eq!(read(), before + 1);
    }
}
