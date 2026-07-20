//! CIP-104 Mode A reward-assignment e2e (M3+M4, Task 10) — **devnet-only,
//! pending a live run**.
//!
//! Exercises the full auto-confirmation path end-to-end: set the on-ledger
//! `RewardSplitConfig` via `SetRewardSplit`, then let each node's background
//! reward-automation loop (spawned in `start_server`) discover the decparty's
//! unassigned `RewardCouponV2` coupons, propose `AssignRewardBeneficiaries`,
//! auto-confirm to threshold, and execute — with no human clicking confirm.
//! Finishes with a negative case proving the default-deny confirmer refuses a
//! crafted proposal whose split does not match the config.
//!
//! ## Why this cannot run in normal CI (and how it stays harmless)
//!
//! This phase is gated two ways:
//!   1. **Opt-in env var** `DECPM_IT_REWARD` in `governance_workflows.rs` — it is
//!      not called at all unless that is set.
//!   2. **Runtime precondition skip** (below) — if the decparty has no
//!      `RewardCouponV2` coupons, the phase logs a SKIP line and returns `Ok(())`.
//!
//! To actually observe assignment on devnet, three operational preconditions
//! (spec §13, plan Task 10) must hold — none are reproducible from this harness:
//!   - The decparty (`f.party_id()`) must be an app-provider whose coupons carry
//!     `provider == decparty` and are **unassigned**. On devnet that is
//!     `cbtc-network`; a fresh harness-allocated decparty earns no coupons, so
//!     the run must target a decparty that does (point the harness at
//!     `cbtc-network`, or arrange coupons for the harness decparty).
//!   - **Mode-B collection must be paused** for that decparty (coordinate with
//!     Robert) so coupons re-accumulate unassigned instead of being swept to 0.
//!   - The test nodes must run with a **short `reward_automation_interval_secs`**
//!     (default is 300s — far too slow for a test). See the `NOTE(interval)`
//!     below: there is currently no env/config override for this field, so
//!     either one must be added, or the operator must configure it, before the
//!     automation steps here can complete within the poll deadlines.
//!
//! ## Field-level assertions need PQS, not this harness
//!
//! The DecMan `/contracts/query` HTTP endpoint returns only `{contract_id}` per
//! contract (`ContractWithBlob`, blob discarded by the harness type) — it does
//! **not** expose decoded fields. So this phase can observe, at the HTTP layer:
//! coupon **presence/archival** (by cid) and the governance lifecycle (proposal
//! appears → reaches threshold → executes, via `/governance/confirmations`). It
//! **cannot** assert, at the HTTP layer, that each resulting coupon carries a
//! specific `beneficiary` or `amount` share (Task 10 step 1(d)). Those must be
//! verified against devnet PQS `pqs_cbtc` on the real run — see the TODO on the
//! final assertion.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use anyhow::Context;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::common::{
    Fixture,
    governance::propose_confirm_execute,
    scenario::Scenario,
    types::{ContractsQueryResponse, GovernanceState},
};

/// `#splice-api-reward-assignment-v1`, URL-encoded — the `RewardCoupon`
/// interface package (concrete implementer on devnet: `RewardCouponV2`).
const REWARD_ASSIGN_PKG: &str = "%23splice-api-reward-assignment-v1";
/// `#governance-rewards-v1`, URL-encoded — holds `RewardSplitConfig` and the
/// `AssignRewardBeneficiaries` proposal template.
const GOVERNANCE_REWARDS_PKG: &str = "%23governance-rewards-v1";

const ASSIGN_ACTION_LABEL: &str = "AssignRewardBeneficiaries";

/// Generous ceiling for each automation step. The loop's default cadence is
/// `reward_automation_interval_secs = 300`; a propose → confirm → execute chain
/// spans multiple ticks, so this is deliberately large. It only bites when the
/// nodes are misconfigured — under a short interval each step passes in seconds.
///
/// NOTE(interval): there is currently no env/config override for
/// `reward_automation_interval_secs` (main.rs builds `NodeConfig::default()`),
/// so the harness cannot shorten it from here. Until such an override exists,
/// the devnet run MUST configure a short interval (e.g. 15-30s) on the test
/// nodes, or these steps will approach this ceiling before completing.
const AUTOMATION_TIMEOUT: Duration = Duration::from_secs(600);

/// How long the negative case holds while confirming the honest nodes never
/// push a crafted mismatched proposal to threshold.
const NEGATIVE_HOLD: Duration = Duration::from_secs(60);

fn reward_coupon_query_path(party_id: &str) -> String {
    format!(
        "/contracts/query?party_id={party_id}&package_id={REWARD_ASSIGN_PKG}\
         &module_name=Splice.Api.RewardAssignmentV1&entity_name=RewardCoupon&interface=true"
    )
}

fn split_config_query_path(party_id: &str) -> String {
    format!(
        "/contracts/query?party_id={party_id}&package_id={GOVERNANCE_REWARDS_PKG}\
         &module_name=Governance.Rewards.RewardSplitConfig&entity_name=RewardSplitConfig"
    )
}

fn assign_proposal_query_path(party_id: &str) -> String {
    format!(
        "/contracts/query?party_id={party_id}&package_id={GOVERNANCE_REWARDS_PKG}\
         &module_name=Governance.Rewards.AssignRewardBeneficiaries\
         &entity_name=AssignRewardBeneficiaries"
    )
}

/// Contract ids of the decparty's `RewardCoupon` contracts (cid-only; the HTTP
/// endpoint does not surface decoded fields, so we cannot filter by
/// `beneficiary == null` here — see the module doc).
async fn query_reward_coupons(f: &Fixture, party_id: &str) -> anyhow::Result<HashSet<String>> {
    let r: ContractsQueryResponse = f
        .get_json(f.p1.http, &reward_coupon_query_path(party_id))
        .await?;
    Ok(r.contracts.into_iter().map(|c| c.contract_id).collect())
}

/// Contract ids of pending `AssignRewardBeneficiaries` proposal contracts.
async fn query_assign_proposals(f: &Fixture, party_id: &str) -> anyhow::Result<HashSet<String>> {
    let r: ContractsQueryResponse = f
        .get_json(f.p1.http, &assign_proposal_query_path(party_id))
        .await?;
    Ok(r.contracts.into_iter().map(|c| c.contract_id).collect())
}

#[derive(Default)]
struct AssignCtx {
    /// Proposal cid of the first automation-produced `AssignRewardBeneficiaries`
    /// we follow through confirm → execute.
    proposal_cid: Option<String>,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: reward_assignment (CIP-104 Mode A, Task 10 — devnet-only)");

    let decparty = f.party_id()?.to_string();

    // ------------------------------------------------------------------
    // Precondition skip (the runtime half of the gate). Guards the WHOLE
    // phase — including the governance setup below — so it is harmless on
    // localnet / a decparty with no coupons. (Ordered before the split-set,
    // unlike the plan's prose numbering, so the skip truly makes the phase a
    // no-op rather than doing governance work first.)
    // ------------------------------------------------------------------
    let initial_coupon_cids = query_reward_coupons(f, &decparty).await?;
    if initial_coupon_cids.is_empty() {
        warn!(
            "reward_assignment IT SKIPPED: no unassigned RewardCouponV2 for {decparty} — \
             needs live coupons with Mode-B collection paused (Task 10 precondition)"
        );
        return Ok(());
    }
    info!(
        "reward_assignment: {} candidate coupon(s) visible for {decparty}",
        initial_coupon_cids.len()
    );

    // ------------------------------------------------------------------
    // Given: set the on-ledger split (two beneficiaries at 0.8 / 0.2) via the
    // SetRewardSplit GovernableAction, driven through the same propose → confirm
    // → execute helper every other phase uses.
    // ------------------------------------------------------------------
    let benef_a = f.p2_member_party()?.to_string();
    let benef_b = f.p3_member_party()?.to_string();
    propose_confirm_execute(
        "SetRewardSplit",
        json!({
            "type": "set_reward_split",
            "new_beneficiaries": [
                {"beneficiary": benef_a, "percentage": "0.8"},
                {"beneficiary": benef_b, "percentage": "0.2"},
            ],
            "prior_config": null,
        }),
    )
    .run(f)
    .await?;

    Scenario::new("RewardSplitConfig present")
        .then(
            "exactly one RewardSplitConfig",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let r: ContractsQueryResponse = f
                        .get_json(f.p1.http, &split_config_query_path(party_id))
                        .await
                        .ok()?;
                    (r.contracts.len() == 1).then_some(Ok(()))
                })
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // When + Then (happy path): the automation proposes → auto-confirms to
    // threshold → executes AssignRewardBeneficiaries, all on its own.
    // ------------------------------------------------------------------
    Scenario::with_ctx("Mode A auto-assignment", AssignCtx::default())
        .then(
            "an AssignRewardBeneficiaries proposal appears",
            AUTOMATION_TIMEOUT,
            |f, ctx| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    let action = s
                        .domain_actions
                        .into_iter()
                        .find(|a| a.action_label == ASSIGN_ACTION_LABEL)?;
                    info!(
                        "observed proposal {} ({ASSIGN_ACTION_LABEL})",
                        action.proposal_cid
                    );
                    ctx.proposal_cid = Some(action.proposal_cid);
                    Some(Ok(()))
                })
            },
        )
        .then(
            "proposal reaches >= threshold auto-confirmations",
            AUTOMATION_TIMEOUT,
            |f, ctx| {
                Box::pin(async move {
                    let cid = match ctx.proposal_cid.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            return Some(Err(anyhow::anyhow!(
                                "proposal_cid not set by prior step"
                            )));
                        }
                    };
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    let threshold = s.threshold;
                    let action = s.domain_actions.iter().find(|a| a.proposal_cid == cid)?;
                    (action.confirmation_count >= threshold || action.can_execute).then_some(Ok(()))
                })
            },
        )
        .then(
            "proposal executes (consumed off the pending set)",
            AUTOMATION_TIMEOUT,
            |f, ctx| {
                Box::pin(async move {
                    let cid = match ctx.proposal_cid.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            return Some(Err(anyhow::anyhow!(
                                "proposal_cid not set by prior step"
                            )));
                        }
                    };
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    // Gone from the pending set == executed (or expired). Combined
                    // with the reached-threshold step above, this means executed.
                    s.domain_actions
                        .iter()
                        .all(|a| a.proposal_cid != cid)
                        .then_some(Ok(()))
                })
            },
        )
        .then(
            "at least one candidate coupon archived",
            AUTOMATION_TIMEOUT,
            {
                let initial = initial_coupon_cids.clone();
                move |f, _| {
                    let initial = initial.clone();
                    Box::pin(async move {
                        let party_id = match f.party_id() {
                            Ok(p) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let current = query_reward_coupons(f, party_id).await.ok()?;
                        // Assignment archives each targeted unassigned coupon and
                        // creates one per beneficiary. Proof-at-the-HTTP-layer: at
                        // least one originally-visible coupon cid is now gone.
                        //
                        // TODO(devnet/PQS): the per-beneficiary field-level checks
                        // (Task 10 step 1(d): one RewardCouponV2 per beneficiary with
                        // `beneficiary ∈ {benef_a, benef_b}` and the 0.8/0.2 `amount`
                        // shares) require decoded reads not exposed by /contracts/query
                        // — verify them against devnet PQS `pqs_cbtc` on the real run.
                        initial
                            .iter()
                            .any(|c| !current.contains(c))
                            .then_some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // Negative (security property): a crafted AssignRewardBeneficiaries proposal
    // whose split does NOT match the RewardSplitConfig must be refused by the
    // honest nodes' confirmers — it never reaches threshold.
    // ------------------------------------------------------------------
    run_negative_case(f, &decparty).await
}

/// Submits a mismatched `AssignRewardBeneficiaries` proposal on one node,
/// self-confirms it once so `get_governance_confirmations` surfaces it, then
/// asserts it never reaches threshold within a bounded window (the honest
/// confirmers refuse it — `is_confirmable` → false on split mismatch).
async fn run_negative_case(f: &mut Fixture, decparty: &str) -> anyhow::Result<()> {
    // Need a real (unassigned) coupon cid for `primary_coupon`.
    let coupons = query_reward_coupons(f, decparty).await?;
    let Some(coupon_cid) = coupons.into_iter().next() else {
        warn!(
            "reward_assignment negative case SKIPPED: no coupon left for {decparty} to craft a \
             mismatched proposal against (all assigned/archived)"
        );
        return Ok(());
    };

    // Crafted split: a single beneficiary at 1.0 — structurally valid (sums to
    // 1.0, so it passes the boundary `validate()`), but deliberately != the
    // configured 0.8/0.2 two-party set, so the confirmer must refuse it.
    let bad_beneficiary = f.p1_member_party()?.to_string();
    let crafted = json!({
        "type": "assign_reward_beneficiaries",
        "primary_coupon": coupon_cid,
        "additional_coupons": [],
        "new_beneficiaries": [{"beneficiary": bad_beneficiary, "percentage": "1.0"}],
    });

    // Snapshot existing proposal cids so we can spot the one we create.
    // TODO(devnet): the background proposer may create its own (valid) proposal
    // concurrently; under a short interval, disambiguate the crafted proposal
    // from a concurrent valid one via PQS on the distinctive single-beneficiary
    // split, or quiesce the proposer for this step.
    let before = query_assign_proposals(f, decparty).await?;

    let req = json!({
        "party_id": decparty,
        "rules_contract_id": f.rules_contract_id()?,
        "proposal": crafted,
    });
    let _: Value = f.post_json(f.p1.http, "/governance/propose", &req).await?;
    info!("negative case: submitted crafted mismatched {ASSIGN_ACTION_LABEL} on P1");

    // Find the crafted proposal's cid (first new one after our propose).
    let crafted_cid = {
        let deadline = Duration::from_secs(60);
        let start = Instant::now();
        loop {
            let now = query_assign_proposals(f, decparty).await?;
            if let Some(cid) = now.difference(&before).next() {
                break cid.clone();
            }
            if start.elapsed() >= deadline {
                anyhow::bail!("crafted proposal contract never appeared within {deadline:?}");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };

    // Self-confirm once on P1 so get_governance_confirmations surfaces it (it
    // only lists proposals with >= 1 confirmation). Placeholder `action` mirrors
    // propose_confirm_execute — the CoreDomain branch derives the choice arg from
    // proposal_cid and ignores it.
    let confirm_req = json!({
        "party_id": decparty,
        "rules_contract_id": f.rules_contract_id()?,
        "action": {"type": "governance_set_threshold", "new_threshold": 1},
        "governance_type": "core_domain",
        "proposal_cid": crafted_cid,
    });
    let _: Value = f
        .post_json(f.p1.http, "/governance/confirm", &confirm_req)
        .await
        .context("self-confirm crafted proposal")?;
    info!("negative case: self-confirmed crafted proposal {crafted_cid} (1 confirmation)");

    // Hold: the crafted proposal must stay below threshold for the whole window.
    // Honest confirmers (P2/P3 automation) refuse it on split mismatch.
    let start = Instant::now();
    loop {
        let path = format!("/governance/confirmations?party_id={decparty}");
        let s: GovernanceState = f.get_json(f.p1.http, &path).await?;
        let threshold = s.threshold;
        if let Some(a) = s
            .domain_actions
            .iter()
            .find(|a| a.proposal_cid == crafted_cid)
            && (a.confirmation_count >= threshold || a.can_execute)
        {
            anyhow::bail!(
                "SECURITY VIOLATION: crafted mismatched {ASSIGN_ACTION_LABEL} reached threshold \
                 ({} >= {threshold}) — honest confirmers wrongly auto-confirmed a split that does \
                 not match RewardSplitConfig",
                a.confirmation_count
            );
        }
        if start.elapsed() >= NEGATIVE_HOLD {
            break;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    info!(
        "negative case held: crafted proposal stayed below threshold for {NEGATIVE_HOLD:?} — \
         default-deny confirmer refused the mismatched split"
    );
    Ok(())
}
