//! CIP-104 Mode A coupon-reassignment e2e (M4, Task 9) — **devnet-only,
//! pending a live run**.
//!
//! Exercises the **delegation model** end-to-end. One threshold governance
//! vote (`SetupCouponReassignmentDelegation`) records the split and the
//! authorized `assigners` once, into an on-ledger `CouponReassignmentDelegation`.
//! From then on there is **no per-round voting**: each node's background
//! reward-automation loop (`run_reward_automation_loop`, spawned in
//! `start_server`) reads the active delegation and — if this node's member
//! party is a listed assigner — exercises `Delegation_Assign` directly to
//! reassign the decparty's ripe unassigned `RewardCouponV2` coupons to the
//! baked-in beneficiaries. Only ONE member instance needs to run to reassign
//! (contrast the deleted auto-confirm engine, which required >= threshold
//! confirmers per round).
//!
//! ## Why this cannot run in normal CI (and how it stays harmless)
//!
//! Gated two ways:
//!   1. **Opt-in env var** `DECPM_IT_REWARD` in `governance_workflows.rs` — the
//!      phase is not called at all unless that is set.
//!   2. **Runtime precondition skip** (below) — if the decparty has no
//!      `RewardCouponV2` coupons, the phase logs a SKIP line and returns
//!      `Ok(())`, so it is a no-op on localnet / any decparty without coupons.
//!
//! To actually observe reassignment on devnet, operational preconditions
//! (spec §13, plan Task 9) must hold — none are reproducible from this harness:
//!   - The decparty (`f.party_id()`) must be an app-provider whose coupons
//!     carry `provider == decparty` and are **unassigned** (`beneficiary =
//!     null`). On devnet that is `cbtc-network`; a fresh harness-allocated
//!     decparty earns no coupons, so the live run must target a decparty that
//!     does.
//!   - **Mode-B collection must be paused** for that decparty (coordinate with
//!     the team) so coupons re-accumulate unassigned instead of being swept to
//!     0. Coupons reappear within ~one round.
//!   - The test nodes must run with a **short reward-automation interval** so
//!     the loop reassigns within the poll deadline. The default is 300s; set
//!     `DECPM_REWARD_AUTOMATION_INTERVAL_SECS` (or `--reward-automation-interval-secs`,
//!     e.g. 15-30s) on the test nodes.
//!   - At least one of the delegation's `assigners` must be a member party this
//!     node holds credentials for (else the loop skips the decparty). Here the
//!     assigners are `[p1_member, p2_member]`; on the live `cbtc-network` run
//!     they are the active attestors (`attestor-1`, `attestor-2`).
//!
//! ## Field-level split assertions need PQS, not this harness
//!
//! The DecMan `/contracts/query` HTTP endpoint returns only `{contract_id}` per
//! contract (`ContractsQueryResponse`) — it does **not** expose decoded fields.
//! So this phase observes, at the HTTP layer: delegation **presence** (the
//! keyless-singleton invariant: exactly one `CouponReassignmentDelegation`) and
//! coupon **archival** (an originally-visible unassigned coupon cid is gone
//! after a tick). It **cannot** assert, at the HTTP layer, that each resulting
//! coupon carries a specific `beneficiary` or the 0.8 / 0.2 `amount` share
//! (Task 9 step 1(b)). Those per-beneficiary field checks must be verified
//! against devnet PQS `pqs_cbtc` on the real run — see the TODO on the final
//! assertion. Beneficiary self-minting (spec §4.3) is a separate precondition
//! (the beneficiaries' own agents) and is likewise verified out-of-band.
//!
//! ## Security property (Task 9 step 2)
//!
//! The split is baked into the delegation and `Delegation_Assign` reads it, so
//! a caller cannot alter it, and only a listed `assigner` may exercise the
//! choice. There are no proposals anymore, so there is no "craft a mismatched
//! proposal" case. The authoritative coverage is the DAML unit test
//! `test_non_assigner_cannot_reassign` (Task 1) plus the baked-split assertion.
//! The devnet ledger-level negative (submit `Delegation_Assign` as a party not
//! in `assigners`, expect ledger rejection) is **not expressible through this
//! HTTP harness**: DecMan exposes no endpoint to submit an arbitrary ledger
//! command as a chosen party — the automation only ever submits as an
//! authorized assigner — so `run_negative_case` documents it as a manual
//! pre-merge ops step rather than inventing a harness capability. See the
//! module-level report note.

use std::{collections::HashSet, time::Duration};

use serde_json::json;
use tracing::{info, warn};

use crate::common::{
    Fixture, governance::propose_confirm_execute, scenario::Scenario, types::ContractsQueryResponse,
};

/// `#splice-api-reward-assignment-v1`, URL-encoded — the `RewardCoupon`
/// interface package (concrete implementer on devnet: `RewardCouponV2`).
const REWARD_ASSIGN_PKG: &str = "%23splice-api-reward-assignment-v1";
/// `#governance-rewards-v1`, URL-encoded — holds the
/// `CouponReassignmentDelegation` template.
const GOVERNANCE_REWARDS_PKG: &str = "%23governance-rewards-v1";

/// Generous ceiling for the reassignment step. Under a short
/// `DECPM_REWARD_AUTOMATION_INTERVAL_SECS` (see the module doc) the loop
/// reassigns within seconds; this only bites when the nodes are misconfigured
/// (still on the 300s default) or paused Mode-B collection was not arranged.
const REASSIGN_TIMEOUT: Duration = Duration::from_secs(600);

fn reward_coupon_query_path(party_id: &str) -> String {
    format!(
        "/contracts/query?party_id={party_id}&package_id={REWARD_ASSIGN_PKG}\
         &module_name=Splice.Api.RewardAssignmentV1&entity_name=RewardCoupon&interface=true"
    )
}

fn delegation_query_path(party_id: &str) -> String {
    format!(
        "/contracts/query?party_id={party_id}&package_id={GOVERNANCE_REWARDS_PKG}\
         &module_name=Governance.Rewards.CouponReassignmentDelegation\
         &entity_name=CouponReassignmentDelegation"
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

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: coupon_reassignment (CIP-104 Mode A delegation model, Task 9 — devnet-only)");

    let decparty = f.party_id()?.to_string();

    // ------------------------------------------------------------------
    // Precondition skip (the runtime half of the gate). Guards the WHOLE
    // phase — including the governance setup below — so it is harmless on
    // localnet / a decparty with no coupons. (Ordered before the delegation
    // setup so the skip truly makes the phase a no-op rather than doing
    // governance work first.)
    // ------------------------------------------------------------------
    let initial_coupon_cids = query_reward_coupons(f, &decparty).await?;
    if initial_coupon_cids.is_empty() {
        warn!(
            "coupon_reassignment IT SKIPPED: no unassigned RewardCouponV2 for {decparty} — \
             needs live coupons with Mode-B collection paused (Task 9 precondition)"
        );
        return Ok(());
    }
    info!(
        "coupon_reassignment: {} candidate coupon(s) visible for {decparty}",
        initial_coupon_cids.len()
    );

    // ------------------------------------------------------------------
    // Given: create the delegation with ONE threshold governance vote
    // (propose -> confirm -> execute), recording the assigners and the
    // baked-in 0.8 / 0.2 split. `prior_delegation = null` (first delegation).
    //
    // Party roles (harness stand-ins; the live cbtc-network run uses the real
    // devnet parties per the operational preconditions):
    //   assigners      = [p1_member, p2_member]  -> attestor-1, attestor-2
    //   beneficiaries  = [p2_member @ 0.8, p3_member @ 0.2] -> cbtc-beneficiary,
    //                     operator
    // p3_member is deliberately NOT an assigner — it is the non-assigner used
    // by the security note below. (A beneficiary is not thereby an assigner.)
    // ------------------------------------------------------------------
    let assigner_a = f.p1_member_party()?.to_string();
    let assigner_b = f.p2_member_party()?.to_string();
    let benef_a = f.p2_member_party()?.to_string();
    let benef_b = f.p3_member_party()?.to_string();
    propose_confirm_execute(
        "SetupCouponReassignmentDelegation",
        json!({
            "type": "setup_coupon_reassignment_delegation",
            "assigners": [assigner_a, assigner_b],
            "new_beneficiaries": [
                {"beneficiary": benef_a, "percentage": "0.8"},
                {"beneficiary": benef_b, "percentage": "0.2"},
            ],
            "prior_delegation": null,
        }),
    )
    .run(f)
    .await?;

    Scenario::new("CouponReassignmentDelegation present")
        .then(
            "exactly one CouponReassignmentDelegation",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let r: ContractsQueryResponse = f
                        .get_json(f.p1.http, &delegation_query_path(party_id))
                        .await
                        .ok()?;
                    (r.contracts.len() == 1).then_some(Ok(()))
                })
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // When + Then (happy path): with the delegation in place and Mode-B
    // collection paused, each node's background loop reads the delegation and
    // (for a node whose member is a listed assigner) exercises
    // Delegation_Assign on its own — no vote per round. Proof-at-the-HTTP-layer:
    // at least one originally-visible unassigned coupon cid is now archived.
    // ------------------------------------------------------------------
    Scenario::new("delegation-model reassignment")
        .then(
            "at least one candidate coupon archived by Delegation_Assign",
            REASSIGN_TIMEOUT,
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
                        // Delegation_Assign archives each targeted unassigned
                        // coupon and creates one per beneficiary.
                        //
                        // TODO(devnet/PQS): the per-beneficiary field-level
                        // checks (Task 9 step 1(b): one RewardCouponV2 per
                        // beneficiary with `beneficiary ∈ {benef_a, benef_b}` and
                        // the 0.8 / 0.2 `amount` shares) require decoded reads not
                        // exposed by /contracts/query — verify them against devnet
                        // PQS `pqs_cbtc` on the real run.
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
    // Negative (security property, Task 9 step 2). See the module doc: the
    // authoritative coverage is DAML (`test_non_assigner_cannot_reassign` +
    // the baked-split assertion). The devnet ledger-level negative is a manual
    // ops step because this HTTP harness cannot submit Delegation_Assign as a
    // non-assigner party.
    // ------------------------------------------------------------------
    run_negative_case(f, &decparty).await
}

/// Documents the security-property negative (Task 9 step 2) rather than
/// exercising it here.
///
/// The property — only a listed `assigner` may exercise `Delegation_Assign`,
/// and the split is baked in so a caller cannot alter it — is enforced in DAML
/// and covered by the `test_non_assigner_cannot_reassign` unit test (Task 1).
/// The devnet ledger-level assertion (submit `Delegation_Assign` as
/// `p3_member`, a party **not** in `assigners`, and expect the ledger to reject
/// it) is **not expressible through this harness**: DecMan exposes no endpoint
/// to submit an arbitrary ledger command as a chosen party (the reward
/// automation only ever submits as an authorized assigner). Inventing such an
/// endpoint is out of scope for Task 9 (no new runtime code), so the devnet
/// negative is deferred to the pre-merge ops run (submit via the ledger API /
/// a daml script as the non-assigner and confirm rejection).
async fn run_negative_case(f: &Fixture, decparty: &str) -> anyhow::Result<()> {
    let non_assigner = f.p3_member_party()?;
    info!(
        "coupon_reassignment security property: enforced in DAML \
         (test_non_assigner_cannot_reassign, Task 1). Devnet ledger-level negative — \
         submit Delegation_Assign for {decparty} as non-assigner {non_assigner}, expect \
         rejection — is a manual pre-merge ops step (no HTTP path to submit as an \
         arbitrary party)."
    );
    Ok(())
}
