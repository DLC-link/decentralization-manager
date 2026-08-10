//! CIP-104 Mode A coupon-reassignment e2e. Runs on **localnet**
//! every CI run (the harness seeds its own coupons — see `seed_reward_coupons`)
//! and, opt-in, against **devnet**.
//!
//! Exercises the **delegation model** end-to-end. One threshold governance
//! vote (`SetupCouponReassignmentDelegation`) records the split and the
//! authorized `assigners` once, into an on-ledger `CouponReassignmentDelegation`.
//! From then on there is **no per-round voting**: each node's background
//! reward-automation loop (`run_reward_automation_loop`, spawned in
//! `start_server`) reads the active delegation and — if this node's member
//! party is a listed assigner — exercises `Delegation_Assign` directly to
//! reassign the decparty's unassigned `RewardCouponV2` coupons to the
//! baked-in beneficiaries. Only ONE member instance needs to run to reassign.
//!
//! ## Why this cannot run in normal CI (and how it stays harmless)
//!
//! Gated two ways:
//!   1. **Opt-in env var** `DECPM_IT_REWARD` in `governance_workflows.rs` — the
//!      phase is not called at all unless that is set.
//!   2. **Runtime precondition skip** (below) — **devnet-only**: if the
//!      decparty has no `RewardCouponV2` coupons, the phase logs a SKIP line
//!      and returns `Ok(())`. On **localnet** the phase seeds its own coupons
//!      (`seed_reward_coupons`) and hard-fails instead of skipping if none are
//!      visible after a short poll — there is no silent no-op path there.
//!
//! To actually observe reassignment on devnet, operational preconditions
//! (design §13) must hold — none are reproducible from this harness:
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
//! coupon carries a specific `beneficiary` or the 0.8 / 0.2 `amount` shares.
//!
//! On **localnet**, the split IS asserted by value: each beneficiary party is
//! an observer of its own assigned `RewardCouponV2`, so the phase reads the
//! decoded amount via the JSON Ledger API (`active_reward_coupons`) and checks
//! the 80.0 / 20.0 shares directly — no PQS needed. On **devnet**, the
//! per-beneficiary field checks still require decoded reads not exposed by
//! `/contracts/query` and must be verified against devnet PQS `pqs_cbtc` on the
//! real run (issue #271) — see the TODO on the final assertion. Beneficiary
//! self-minting (design §4.3) is a separate precondition (the beneficiaries' own
//! agents) and is likewise verified out-of-band.
//!
//! ## Security property
//!
//! The split is baked into the delegation and `Delegation_Assign` reads it, so
//! a caller cannot alter it, and only a listed `assigner` may exercise the
//! choice. The per-round path involves no proposal at all, so there is no
//! "craft a mismatched proposal" case. The authoritative coverage is the DAML unit test
//! `test_non_assigner_cannot_reassign` plus the baked-split assertion.
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
    Fixture,
    governance::propose_confirm_execute,
    ledger_api::P1_JSON_API,
    phases::seed_reward_coupons::{SEED_AMOUNT, SEED_COUPON_COUNT},
    scenario::Scenario,
    types::{ActiveDelegationResponse, ContractsQueryResponse},
};

/// `#splice-api-reward-assignment-v1`, URL-encoded — the `RewardCoupon`
/// interface package (concrete implementer on devnet: `RewardCouponV2`).
const REWARD_ASSIGN_PKG: &str = "%23splice-api-reward-assignment-v1";
/// `#governance-rewards-automation-v1-rc2`, URL-encoded — holds the
/// `CouponReassignmentDelegation` template.
const GOVERNANCE_REWARDS_PKG: &str = "%23governance-rewards-automation-v1-rc2";

/// Generous ceiling for the reassignment step. Under a short
/// `DECPM_REWARD_AUTOMATION_INTERVAL_SECS` (see the module doc) the loop
/// reassigns within seconds; this only bites when the nodes are misconfigured
/// (still on the 300s default) or paused Mode-B collection was not arranged.
const REASSIGN_TIMEOUT: Duration = Duration::from_secs(600);

/// Devnet query: the `RewardCoupon` *interface* (matches any implementer; on
/// cbtc-network that is `RewardCouponV2`). Real auth supports InterfaceFilter.
fn reward_coupon_query_path(party_id: &str) -> String {
    format!(
        "/contracts/query?party_id={party_id}&package_id={REWARD_ASSIGN_PKG}\
         &module_name=Splice.Api.RewardAssignmentV1&entity_name=RewardCoupon&interface=true"
    )
}

/// Localnet query: the *concrete* `Splice.Amulet:RewardCouponV2` template.
/// Localnet builds DecMan with `--features test-mode`, where `/contracts/query`
/// falls back to a `WildcardFilter` ACS read and matches results by concrete
/// `module_name`/`entity_name` (mock auth can't use real Template/Interface
/// filters). An interface query (`RewardCoupon`) therefore never matches and
/// returns empty — we must query the concrete implementing template.
fn reward_coupon_v2_query_path(party_id: &str) -> String {
    format!(
        "/contracts/query?party_id={party_id}&package_id=%23splice-amulet\
         &module_name=Splice.Amulet&entity_name=RewardCouponV2&interface=false"
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
    let path = match f.target {
        crate::common::TestTarget::Localnet => reward_coupon_v2_query_path(party_id),
        crate::common::TestTarget::Devnet => reward_coupon_query_path(party_id),
    };
    let r: ContractsQueryResponse = f.get_json(f.p1.http, &path).await?;
    Ok(r.contracts.into_iter().map(|c| c.contract_id).collect())
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: coupon_reassignment (CIP-104 Mode A delegation model)");

    let decparty = f.party_id()?.to_string();

    // ------------------------------------------------------------------
    // Precondition check (the runtime half of the gate). Target-aware:
    // - Localnet: seed_reward_coupons committed coupons; empty here is a
    //   hard failure after brief poll (no silent no-op).
    // - Devnet: empty coupons log SKIP and return Ok(()).
    // (Ordered before delegation setup so early exit avoids governance work.)
    // ------------------------------------------------------------------
    let initial_coupon_cids = match f.target {
        crate::common::TestTarget::Localnet => {
            // seed_reward_coupons committed the coupons synchronously; poll only
            // to absorb any ledger->DecMan read lag, then hard-fail. A silent
            // skip here would turn the whole phase into a false-positive no-op.
            //
            // Wait for the FULL seeded set, not merely a non-empty one: the seed
            // commits in several transactions, so an early read can return a
            // subset, and the "every seeded coupon archived" assertion below
            // would then be evaluated over that subset instead of all of them.
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            let cids = loop {
                let cids = query_reward_coupons(f, &decparty).await?;
                if cids.len() >= SEED_COUPON_COUNT || std::time::Instant::now() >= deadline {
                    break cids;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            };
            anyhow::ensure!(
                cids.len() >= SEED_COUPON_COUNT,
                "coupon_reassignment (localnet): saw {} unassigned RewardCouponV2 for {decparty}, \
                 expected the {SEED_COUPON_COUNT} seeded — seed_reward_coupons must run first \
                 and commit them all",
                cids.len()
            );
            cids
        }
        crate::common::TestTarget::Devnet => {
            let cids = query_reward_coupons(f, &decparty).await?;
            if cids.is_empty() {
                warn!(
                    "coupon_reassignment IT SKIPPED: no unassigned RewardCouponV2 for {decparty} — \
                     needs live coupons with Mode-B collection paused (precondition)"
                );
                return Ok(());
            }
            cids
        }
    };
    info!(
        "coupon_reassignment: {} candidate coupon(s) visible for {decparty}",
        initial_coupon_cids.len()
    );

    // ------------------------------------------------------------------
    // Given: create the delegation with ONE threshold governance vote
    // (propose -> confirm -> execute), recording the assigners and the
    // baked-in 0.8 / 0.2 split. `prior_delegation = null` (first delegation).
    //
    // Party roles:
    //   On localnet, assigners = [p1_member, p2_member], and beneficiaries are
    //   the two dedicated non-assigner parties the seed phase allocated
    //   ([reward_beneficiary_party @ 0.8, reward_operator_party @ 0.2]) — kept
    //   disjoint from the assigners so the split-by-value assertion below
    //   observes each beneficiary's own coupons unambiguously.
    //
    //   On devnet (harness stand-ins; the live cbtc-network run uses the real
    //   devnet parties per the operational preconditions):
    //     assigners      = [p1_member, p2_member]  -> attestor-1, attestor-2
    //     beneficiaries  = [p2_member @ 0.8, p3_member @ 0.2] -> cbtc-beneficiary,
    //                       operator
    //   p3_member is deliberately NOT an assigner — it is the non-assigner used
    //   by the security note below. (A beneficiary is not thereby an assigner.)
    // ------------------------------------------------------------------
    let dso_party = f.p1_member_party()?.to_string(); // localnet DSO stand-in
    let assigner_a = f.p1_member_party()?.to_string();
    let assigner_b = f.p2_member_party()?.to_string();
    // Beneficiaries must be disjoint from assigners. On localnet the seed phase
    // allocated two dedicated non-assigner parties; on devnet keep the existing
    // stand-ins (issue #271 wires the real cbtc-beneficiary/operator).
    let (benef_a, benef_b) = match f.target {
        crate::common::TestTarget::Localnet => (
            f.reward_beneficiary_party()?.to_string(),
            f.reward_operator_party()?.to_string(),
        ),
        crate::common::TestTarget::Devnet => (
            f.p2_member_party()?.to_string(),
            f.p3_member_party()?.to_string(),
        ),
    };
    propose_confirm_execute(
        "SetupCouponReassignmentDelegation",
        json!({
            "type": "setup_coupon_reassignment_delegation",
            // On localnet the harness substitutes p1_member for the DSO, which is
            // the party seed_reward_coupons mints the coupons as.
            "dso": dso_party,
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

    // The proposal form prefills "Replaces Delegation" from this endpoint. A cid
    // that disagrees with the ledger would aim the next vote at a contract that
    // is not the live one, so assert the two agree rather than trusting the read.
    Scenario::new("active-delegation endpoint agrees with the ledger")
        .then(
            "GET /coupon-reassignment-delegation returns the live delegation's cid",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let acs: ContractsQueryResponse = f
                        .get_json(f.p1.http, &delegation_query_path(party_id))
                        .await
                        .ok()?;
                    let [only] = acs.contracts.as_slice() else {
                        return None;
                    };
                    let r: ActiveDelegationResponse = f
                        .get_json(
                            f.p1.http,
                            &format!("/coupon-reassignment-delegation?party_id={party_id}"),
                        )
                        .await
                        .ok()?;
                    let [active] = r.delegations.as_slice() else {
                        return Some(Err(anyhow::anyhow!(
                            "endpoint reported {} delegations while the ACS holds exactly one ({})",
                            r.delegations.len(),
                            only.contract_id
                        )));
                    };
                    if active.cid != only.contract_id {
                        return Some(Err(anyhow::anyhow!(
                            "endpoint returned {} but the ACS holds {}",
                            active.cid,
                            only.contract_id
                        )));
                    }
                    // Decoded fields the cid-only `/contracts/query` cannot show.
                    if active.assigners.is_empty() || active.beneficiary_count == 0 {
                        return Some(Err(anyhow::anyhow!(
                            "delegation decoded as {} assigners / {} beneficiaries",
                            active.assigners.len(),
                            active.beneficiary_count
                        )));
                    }
                    Some(Ok(()))
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
    // On localnet the seeded set spans more than one chunk (see
    // `seed_reward_coupons::SEED_COUPON_COUNT`), so requiring *every* seeded
    // coupon to be archived exercises the drain loop across chunk boundaries —
    // a single-chunk assertion would pass while later chunks were dropped. On
    // devnet the coupon set is whatever the ledger happens to hold and other
    // automation may touch it, so one archived coupon remains the bar there.
    let require_all_archived = f.target == crate::common::TestTarget::Localnet;
    let archived_criterion = if require_all_archived {
        "every seeded coupon archived by Delegation_Assign (spans >1 chunk)"
    } else {
        "at least one candidate coupon archived by Delegation_Assign"
    };
    Scenario::new("delegation-model reassignment")
        .then(archived_criterion, REASSIGN_TIMEOUT, {
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
                    // checks (one RewardCouponV2 per
                    // beneficiary with `beneficiary ∈ {benef_a, benef_b}` and
                    // the 0.8 / 0.2 `amount` shares) require decoded reads not
                    // exposed by /contracts/query — verify them against devnet
                    // PQS `pqs_cbtc` on the real run.
                    let mut gone = initial.iter().filter(|c| !current.contains(*c));
                    if require_all_archived {
                        gone.count().eq(&initial.len()).then_some(Ok(()))
                    } else {
                        gone.next().map(|_| Ok(()))
                    }
                })
            }
        })
        .run(f)
        .await?;

    // Localnet: assert the 0.8 / 0.2 split BY VALUE (the automated replacement
    // for the devnet/PQS TODO). Each beneficiary party is an observer of its own
    // assigned RewardCouponV2, so we read the decoded amount via the JSON Ledger
    // API. On devnet this stays a PQS check on the real run (issue #271).
    if f.target == crate::common::TestTarget::Localnet {
        let benef = f.reward_beneficiary_party()?.to_string();
        let operator = f.reward_operator_party()?.to_string();
        // Totals over the whole seeded set, so the split is checked across chunk
        // boundaries rather than on one coupon.
        let seeded_total = SEED_COUPON_COUNT as f64 * SEED_AMOUNT;
        let expect_benef = seeded_total * 0.8;
        let expect_operator = seeded_total * 0.2;
        Scenario::new("0.8/0.2 split by value")
            .then(
                "beneficiary total ~4x operator total across all seeded coupons",
                Duration::from_secs(120),
                move |f, _| {
                    let benef = benef.clone();
                    let operator = operator.clone();
                    Box::pin(async move {
                        // Log a hard read error (e.g. an ACS shape mismatch) on
                        // each attempt instead of silently swallowing it into a
                        // retry that ends in a generic timeout — makes the first
                        // live/CI run diagnosable. Still returns None to retry
                        // (the 120s deadline bounds it).
                        let b = match f.active_reward_coupons(P1_JSON_API, &benef).await {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("split assertion: reading beneficiary coupons failed: {e:#}");
                                return None;
                            }
                        };
                        let o = match f.active_reward_coupons(P1_JSON_API, &operator).await {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("split assertion: reading operator coupons failed: {e:#}");
                                return None;
                            }
                        };
                        let sum = |v: &[(Option<String>, String)], who: &str| -> f64 {
                            v.iter()
                                .filter(|(bene, _)| bene.as_deref() == Some(who))
                                .filter_map(|(_, amt)| amt.parse::<f64>().ok())
                                .sum()
                        };
                        let b_total = sum(&b, &benef);
                        let o_total = sum(&o, &operator);
                        // Wait until both beneficiary coupons exist, then assert.
                        if b_total <= 0.0 || o_total <= 0.0 {
                            return None;
                        }
                        // Wait for the full seeded total before judging: a
                        // partial read mid-drain would look like a bad split.
                        if b_total + o_total < seeded_total - 0.05 {
                            return None;
                        }
                        let ok = crate::common::ledger_api::split_ok(b_total, o_total, 0.01)
                            && (b_total - expect_benef).abs() < 0.05
                            && (o_total - expect_operator).abs() < 0.05;
                        Some(if ok {
                            Ok(())
                        } else {
                            Err(anyhow::anyhow!(
                                "split mismatch: beneficiary={b_total} operator={o_total} \
                                 (expected {expect_benef} / {expect_operator})"
                            ))
                        })
                    })
                },
            )
            .run(f)
            .await?;
    }

    // ------------------------------------------------------------------
    // Negative (security property). See the module doc: the
    // authoritative coverage is DAML (`test_non_assigner_cannot_reassign` +
    // the baked-split assertion). The devnet ledger-level negative is a manual
    // ops step because this HTTP harness cannot submit Delegation_Assign as a
    // non-assigner party.
    // ------------------------------------------------------------------
    run_negative_case(f, &decparty).await
}

/// Documents the security-property negative rather than
/// exercising it here.
///
/// The property — only a listed `assigner` may exercise `Delegation_Assign`,
/// and the split is baked in so a caller cannot alter it — is enforced in DAML
/// and covered by the `test_non_assigner_cannot_reassign` unit test.
/// The devnet ledger-level assertion (submit `Delegation_Assign` as
/// `p3_member`, a party **not** in `assigners`, and expect the ledger to reject
/// it) is **not expressible through this harness**: DecMan exposes no endpoint
/// to submit an arbitrary ledger command as a chosen party (the reward
/// automation only ever submits as an authorized assigner). Inventing such an
/// endpoint is out of scope here (no new runtime code), so the devnet
/// negative is deferred to the pre-merge ops run (submit via the ledger API /
/// a daml script as the non-assigner and confirm rejection).
async fn run_negative_case(f: &Fixture, decparty: &str) -> anyhow::Result<()> {
    let non_assigner = f.p3_member_party()?;
    info!(
        "coupon_reassignment security property: enforced in DAML \
         (test_non_assigner_cannot_reassign). Devnet ledger-level negative — \
         submit Delegation_Assign for {decparty} as non-assigner {non_assigner}, expect \
         rejection — is a manual pre-merge ops step (no HTTP path to submit as an \
         arbitrary party)."
    );
    Ok(())
}
