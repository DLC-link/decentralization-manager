//! CIP-104 Mode A coupon-reassignment e2e. Runs on **localnet** every CI run
//! (the harness seeds its own coupons — see `seed_reward_coupons`). Devnet has
//! its own phase, `coupon_reassignment_deployed`, which reads a delegation that
//! is already deployed instead of voting one in.
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
//! ## Field-level split assertions read the JSON Ledger API
//!
//! The DecMan `/contracts/query` HTTP endpoint returns only `{contract_id}` per
//! contract (`ContractQueryResponse`) — it does **not** expose decoded fields.
//! So this phase observes, at the HTTP layer: delegation **presence** (the
//! keyless-singleton invariant: exactly one `CouponReassignmentDelegation`) and
//! coupon **archival** (an originally-visible unassigned coupon cid is gone
//! after a sweep).
//!
//! The split IS asserted by value: each beneficiary party is an observer of its
//! own assigned `RewardCouponV2`, so the phase reads the decoded amount via the
//! JSON Ledger API (`active_reward_coupons`) and checks the 80.0 / 20.0 shares
//! directly. Devnet tunnels no JSON Ledger API, so its phase reads PQS instead.
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
//!
//! ## Foreign-DSO coupon (localnet)
//!
//! `dso` is `RewardCouponV2`'s only signatory, so any party can mint a coupon
//! that names itself `dso` and the decparty as `provider`. The seed phase mints
//! one such coupon as a member party, while the delegation names a dedicated
//! DSO party. The phase asserts that the drain assigns every other coupon and
//! leaves that one active. It also asserts that the expiry gauge ignores it.
//! Only the gauge check pins the coupon reader's dso filter: without the
//! filter, the dso assert in `Delegation_Assign` would still reject the coupon.

use std::{collections::HashSet, time::Duration};

use common::api::{ActiveCouponReassignmentDelegation, ContractQueryResponse};
use serde_json::json;
use tracing::{info, warn};

use crate::common::{
    Fixture,
    governance::propose_confirm_execute,
    ledger_api::{ActiveCoupon, P1_JSON_API},
    phases::seed_reward_coupons::{
        FOREIGN_TTL_HOURS, SEED_AMOUNT, SEED_COUPON_COUNT, SEED_TTL_HOURS, SEEDED_SURVIVORS,
        UNASSIGNABLE_AMOUNT,
    },
    scenario::Scenario,
};

/// `#governance-rewards-automation-v1`, URL-encoded — holds the
/// `CouponReassignmentDelegation` template.
const GOVERNANCE_REWARDS_PKG: &str = "%23governance-rewards-automation-v1";

/// Generous ceiling for the reassignment step. Under a short
/// `DECPM_REWARD_AUTOMATION_INTERVAL_SECS` the loop reassigns within seconds —
/// the loop's heartbeat timer scales down to match rather than flooring at its
/// own 60s cadence; this only bites when the nodes are misconfigured (still on
/// the 300s default) or, on devnet, Mode-B collection was not paused.
pub(crate) const REASSIGN_TIMEOUT: Duration = Duration::from_secs(600);

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

/// Contract ids of the decparty's `RewardCouponV2` contracts (cid-only; the
/// HTTP endpoint does not surface decoded fields, so we cannot filter by
/// `beneficiary == null` here — see the module doc).
async fn query_reward_coupons(f: &Fixture, party_id: &str) -> anyhow::Result<HashSet<String>> {
    let r: ContractQueryResponse = f
        .get_json(f.p1.http, &reward_coupon_v2_query_path(party_id))
        .await?;
    Ok(r.contracts.into_iter().map(|c| c.contract_id).collect())
}

async fn probe_reward_coupons(f: &Fixture, party_id: &str) -> Option<HashSet<String>> {
    let r: ContractQueryResponse = f
        .probe_get_json(f.p1.http, &reward_coupon_v2_query_path(party_id))
        .await?;
    Some(r.contracts.into_iter().map(|c| c.contract_id).collect())
}

/// Sums one metric family across the e2e nodes. A node serving no such line has
/// never moved that counter, which sums as zero.
///
/// Matches the family exactly. A bare prefix test would also sum a longer family
/// name that happens to start with this one.
async fn counter_total(f: &Fixture, ports: &[u16], name: &str) -> anyhow::Result<f64> {
    let mut total = 0.0;
    for port in ports {
        let body = f.get_text(*port, "/metrics").await?;
        for line in body.lines() {
            let is_family = line
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '));
            if is_family {
                total += line
                    .split_whitespace()
                    .last()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or_default();
            }
        }
    }
    Ok(total)
}

/// Removing the files on every exit prevents a failed assertion from leaving
/// the node's background loop faulted during subsequent diagnostics.
struct AssignmentFault(Vec<std::path::PathBuf>);

impl AssignmentFault {
    fn install(f: &Fixture, party: &str) -> anyhow::Result<Self> {
        let fault = Self(
            (1..=2)
                .map(|i| {
                    f.dev_dir
                        .join(format!("participant-{i}/reward-assign-failure"))
                })
                .collect(),
        );
        fault.set(party, "LOCAL_VERDICT_LOCKED_CONTRACTS")?;
        Ok(fault)
    }

    fn set(&self, party: &str, error_id: &str) -> anyhow::Result<()> {
        for path in &self.0 {
            let staging = path.with_extension("tmp");
            std::fs::write(&staging, format!("{party} {error_id}"))?;
            std::fs::rename(staging, path)?;
        }
        Ok(())
    }
}

impl Drop for AssignmentFault {
    fn drop(&mut self) {
        for path in &self.0 {
            if let Err(error) = std::fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                warn!(%error, path = %path.display(), "removing assignment test fault failed");
            }
        }
    }
}

#[derive(Debug)]
struct AssignmentMetrics {
    zero: f64,
    skipped: f64,
    assigned: f64,
    expiry: Option<f64>,
}

/// One decparty's value of the metric `name` in a `/metrics` body. `None` when
/// the node has no such series yet.
pub(crate) fn decparty_metric(body: &str, name: &str, party: &str) -> anyhow::Result<Option<f64>> {
    let prefix = format!("{name}{{decparty=\"{party}\"}} ");
    body.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.parse::<f64>().map_err(Into::into))
        .transpose()
}

async fn assignment_metrics(f: &Fixture, party: &str) -> anyhow::Result<Vec<AssignmentMetrics>> {
    let mut values = Vec::new();
    for port in [f.p1.metrics, f.p2.metrics] {
        let body = f.get_text(port, "/metrics").await?;
        let metric = |name: &str| decparty_metric(&body, name, party);
        values.push(AssignmentMetrics {
            zero: metric("decman_reward_sweep_zero_assigned_total")?.unwrap_or(0.0),
            skipped: metric("decman_reward_coupons_skipped_total")?.unwrap_or(0.0),
            assigned: metric("decman_reward_coupons_assigned_total")?.unwrap_or(0.0),
            expiry: metric("decman_reward_oldest_unassigned_expires_in_seconds")?,
        });
    }
    Ok(values)
}

async fn assert_assignment_fault(
    f: &Fixture,
    party: &str,
    baseline: &[AssignmentMetrics],
    transient: bool,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    loop {
        let current = assignment_metrics(f, party).await?;
        let mut ready = true;
        for (now, before) in current.iter().zip(baseline) {
            anyhow::ensure!(
                now.assigned == before.assigned,
                "faulted assignment reached the ledger: {now:?}"
            );
            if transient {
                anyhow::ensure!(
                    now.skipped == before.skipped,
                    "contention counted coupons as rejected: {now:?}"
                );
            }
            ready &= now.zero > before.zero
                && (transient || now.skipped > before.skipped)
                && now
                    .expiry
                    .is_some_and(|expiry| expiry.is_finite() && expiry > 0.0);
        }
        if ready {
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "assignment failure metrics did not converge (transient={transient}): {current:?}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: coupon_reassignment (CIP-104 Mode A delegation model)");
    anyhow::ensure!(
        f.target == crate::common::TestTarget::Localnet,
        "coupon_reassignment seeds coupons and votes in its own delegation, so it runs on \
         localnet only; devnet runs coupon_reassignment_deployed (devnet_reward_e2e)"
    );

    let decparty = f.party_id()?.to_string();
    // The seeded coupon a member minted as `dso`. No sweep may assign it (see
    // `seed_reward_coupons::FOREIGN_TTL_HOURS`).
    let foreign_cid = f.reward_foreign_coupon_cid()?.to_string();

    // ------------------------------------------------------------------
    // Precondition check, ordered before delegation setup so a failure here
    // costs no governance work. seed_reward_coupons committed the coupons
    // synchronously; poll only to absorb any ledger->DecMan read lag, then
    // hard-fail. A silent skip here would turn the whole phase into a
    // false-positive no-op.
    //
    // Wait for the FULL seeded set, not merely a non-empty one: the seed
    // commits in several transactions, so an early read can return a subset,
    // and the "every seeded coupon archived" assertion below would then be
    // evaluated over that subset instead of all of them.
    // ------------------------------------------------------------------
    let initial_coupon_cids = {
        let seeded = SEED_COUPON_COUNT + SEEDED_SURVIVORS;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let cids = loop {
            let cids = query_reward_coupons(f, &decparty).await?;
            if cids.len() >= seeded || std::time::Instant::now() >= deadline {
                break cids;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        };
        anyhow::ensure!(
            cids.len() >= seeded,
            "coupon_reassignment (localnet): saw {} unassigned RewardCouponV2 for {decparty}, \
             expected the {seeded} seeded — seed_reward_coupons must run first \
             and commit them all",
            cids.len()
        );
        anyhow::ensure!(
            cids.contains(&foreign_cid),
            "coupon_reassignment (localnet): the foreign-DSO coupon {foreign_cid} is not \
             among the {} coupons DecMan reports for {decparty}",
            cids.len()
        );
        cids
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
    // Party roles: assigners = [p1_member, p2_member], and beneficiaries are
    // the two dedicated non-assigner parties the seed phase allocated
    // ([reward_beneficiary_party @ 0.8, reward_operator_party @ 0.2]) — kept
    // disjoint from the assigners so the split-by-value assertion below
    // observes each beneficiary's own coupons unambiguously. The dso is the
    // dedicated party the seed phase minted its coupons as. It differs from
    // p1_member, which minted the one foreign-DSO coupon. p3_member is
    // deliberately NOT an assigner — it is the non-assigner used by the
    // security note below.
    // ------------------------------------------------------------------
    let assigner_a = f.p1_member_party()?.to_string();
    let assigner_b = f.p2_member_party()?.to_string();
    let dso_party = f.reward_dso_party()?.to_string();
    let benef_a = f.reward_beneficiary_party()?.to_string();
    let benef_b = f.reward_operator_party()?.to_string();
    // Install the fault on both assigners before enabling the loop, so no
    // healthy node can drain the coupons before the failure assertions run.
    let fault = AssignmentFault::install(f, &decparty)?;
    let baseline = assignment_metrics(f, &decparty).await?;
    propose_confirm_execute(
        "SetupCouponReassignmentDelegation",
        json!({
            "type": "setup_coupon_reassignment_delegation",
            // On localnet, the party seed_reward_coupons minted every coupon as,
            // except the foreign-DSO one.
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

    assert_assignment_fault(f, &decparty, &baseline, true).await?;
    let after_contention = assignment_metrics(f, &decparty).await?;
    fault.set(&decparty, "COMMAND_PREPROCESSING_FAILED")?;
    assert_assignment_fault(f, &decparty, &after_contention, false).await?;
    drop(fault);
    info!("assignment failure metrics verified; restored real ledger submission");

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
                    let r: ContractQueryResponse = f
                        .probe_get_json(f.p1.http, &delegation_query_path(party_id))
                        .await?;
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
                    let acs: ContractQueryResponse = f
                        .probe_get_json(f.p1.http, &delegation_query_path(party_id))
                        .await?;
                    let [only] = acs.contracts.as_slice() else {
                        return None;
                    };
                    let r: ActiveCouponReassignmentDelegation = f
                        .probe_get_json(
                            f.p1.http,
                            &format!("/coupon-reassignment-delegation?party_id={party_id}"),
                        )
                        .await?;
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
    // When + Then (happy path): with the delegation in place, each node's
    // background loop reads the delegation and (for a node whose member is a
    // listed assigner) exercises Delegation_Assign on its own — no vote per
    // round. Proof-at-the-HTTP-layer: the seeded coupon cids are now archived.
    // ------------------------------------------------------------------
    // The seeded set spans more than one chunk (see
    // `seed_reward_coupons::SEED_COUPON_COUNT`), so requiring *every* seeded
    // coupon to be archived exercises the drain loop across chunk boundaries —
    // a single-chunk assertion would pass while later chunks were dropped.
    //
    // The seed also plants ONE coupon the ledger refuses to assign
    // (`UNASSIGNABLE_AMOUNT`), so the bar is every seeded coupon *except that
    // one*. Requiring all of them would fail by construction, and requiring
    // merely "one or more" would pass while the fan-out dropped a whole chunk.
    // Exempting exactly one is what proves a bad coupon costs only itself.
    //
    // The seed plants one more survivor: a coupon a member minted as `dso`,
    // while the delegation names another DSO. No sweep may assign it, so the bar
    // also exempts it, by contract id. It is the most urgent coupon a reader
    // could admit, so a sweep that wrongly assigns it does so before the rest
    // drain. Its archival therefore fails this step at once, not at the deadline.
    Scenario::new("delegation-model reassignment")
        .then(
            "every seeded coupon but the unassignable and foreign-DSO ones archived (spans >1 chunk)",
            REASSIGN_TIMEOUT,
            {
                let initial = initial_coupon_cids.clone();
                let foreign = foreign_cid.clone();
                move |f, _| {
                    let initial = initial.clone();
                    let foreign = foreign.clone();
                    Box::pin(async move {
                        let party_id = match f.party_id() {
                            Ok(p) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let current = probe_reward_coupons(f, party_id).await?;
                        // Delegation_Assign archives each targeted unassigned
                        // coupon and creates one per beneficiary. The split is
                        // asserted by value in the next scenario.
                        if !current.contains(&foreign) {
                            return Some(Err(anyhow::anyhow!(
                                "the foreign-DSO coupon {foreign} was archived: a sweep assigned \
                                 a coupon whose dso is not the delegation's"
                            )));
                        }
                        // Every seeded coupon but the unassignable and
                        // foreign-DSO ones.
                        initial
                            .iter()
                            .filter(|c| !current.contains(*c))
                            .count()
                            .eq(&(initial.len() - SEEDED_SURVIVORS))
                            .then_some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    // Assert the 0.8 / 0.2 split BY VALUE. Each beneficiary party is an observer
    // of its own assigned RewardCouponV2, so we read the decoded amount via the
    // JSON Ledger API.
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
                    let b = f.probe_diag.ok(
                        "split assertion: read beneficiary coupons",
                        f.active_reward_coupons(P1_JSON_API, &benef).await,
                    )?;
                    let o = f.probe_diag.ok(
                        "split assertion: read operator coupons",
                        f.active_reward_coupons(P1_JSON_API, &operator).await,
                    )?;
                    let sum = |v: &[ActiveCoupon], who: &str| -> f64 {
                        v.iter()
                            .filter(|c| c.beneficiary.as_deref() == Some(who))
                            .filter_map(|c| c.amount.parse::<f64>().ok())
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

    // The bad coupon costs only itself. The seed planted one coupon the
    // ledger admits and then refuses to assign, so `Delegation_Assign` failed
    // for the chunk holding it and the drain re-submitted that chunk one
    // coupon at a time. What proves the fan-out worked is the pair of facts:
    // every healthy coupon was paid (asserted above, and by the split totals
    // which already reconcile to the full seeded amount), and this one is
    // still sitting there unassigned rather than having wedged the sweep.
    // Only the delegation's DSO's coupons count: the foreign-DSO coupon is
    // unassigned too, by design, and the drain step already pinned it.
    let dso = dso_party.clone();
    Scenario::new("an unassignable coupon is isolated, not fatal")
        .then(
            "exactly one of the DSO's coupons remains, unassigned, at the unassignable amount",
            Duration::from_secs(120),
            move |f, _| {
                let dso = dso.clone();
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let all = f.probe_diag.ok(
                        "unassignable assertion: read coupons",
                        f.active_reward_coupons(P1_JSON_API, party_id).await,
                    )?;
                    let unassigned: Vec<&String> = all
                        .iter()
                        .filter(|c| c.dso == dso && c.beneficiary.is_none())
                        .map(|c| &c.amount)
                        .collect();
                    // The drain may still be mid-sweep; keep polling until the
                    // healthy set has drained away.
                    if unassigned.len() > 1 {
                        return None;
                    }
                    let [amount] = unassigned.as_slice() else {
                        return Some(Err(anyhow::anyhow!(
                            "no unassigned coupon left; the unassignable one should survive \
                             every sweep, since nothing quarantines it"
                        )));
                    };
                    let wanted: f64 = UNASSIGNABLE_AMOUNT.parse().unwrap_or(f64::NAN);
                    let got: f64 = amount.parse().unwrap_or(f64::NAN);
                    Some(if (got - wanted).abs() < 1e-12 {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!(
                            "the surviving unassigned coupon is {amount}, not the \
                             unassignable {UNASSIGNABLE_AMOUNT} — a healthy coupon was \
                             skipped instead"
                        ))
                    })
                })
            },
        )
        .run(f)
        .await?;

    // The coupon reader drops the foreign-DSO coupon. The drain step cannot
    // show that alone: if the reader admitted the coupon, the dso assert in
    // `Delegation_Assign` would still reject it, and it would survive either
    // way. The expiry gauge tells the two apart. The foreign coupon expires
    // first, so a reader that admitted it would report its expiry. The
    // DSO's own survivor, the unassignable coupon, expires much later.
    let floor_secs = ((SEED_TTL_HOURS + FOREIGN_TTL_HOURS) * 3600 / 2) as f64;
    Scenario::new("the coupon reader ignores a foreign-DSO coupon")
        .then(
            "every assigner's expiry gauge is past the foreign coupon's expiry",
            Duration::from_secs(120),
            move |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let metrics = f.probe_diag.ok(
                        "foreign-DSO gauge assertion: read /metrics",
                        assignment_metrics(f, party_id).await,
                    )?;
                    let mut expiries = Vec::with_capacity(metrics.len());
                    for node in &metrics {
                        // Wait for every assigner to publish the gauge.
                        expiries.push(node.expiry?);
                    }
                    Some(if expiries.iter().all(|e| *e > floor_secs) {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!(
                            "oldest-unassigned gauge {expiries:?} is not past {floor_secs}s: \
                             the reader counted the foreign-DSO coupon, which expires \
                             {FOREIGN_TTL_HOURS}h after seeding"
                        ))
                    })
                })
            },
        )
        .run(f)
        .await?;

    // The instruments the alerts read, proven end to end: a real sweep
    // assigned real coupons and the counters moved.
    let assigner_metrics = [f.p1.metrics, f.p2.metrics];
    Scenario::new("the reward counters move")
        .then(
            "assigned counts every healthy coupon, and the refused one is counted skipped",
            Duration::from_secs(120),
            move |f, _| {
                Box::pin(async move {
                    let assigned = match counter_total(
                        f,
                        &assigner_metrics,
                        "decman_reward_coupons_assigned_total",
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            warn!("counter assertion: reading /metrics failed: {e:#}");
                            return None;
                        }
                    };
                    // The tail of `drain_assignable` runs just after the
                    // commit the split assertion already saw, so keep polling.
                    if assigned < SEED_COUPON_COUNT as f64 {
                        return None;
                    }
                    let skipped = match counter_total(
                        f,
                        &assigner_metrics,
                        "decman_reward_coupons_skipped_total",
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            warn!("counter assertion: reading /metrics failed: {e:#}");
                            return None;
                        }
                    };
                    if skipped >= 1.0 {
                        Some(Ok(()))
                    } else {
                        // Reachable, not just theoretical: the drain's fan-out can
                        // `break 'chunks` on a transient error, and if that lands on
                        // the unassignable coupon's isolated submission after the
                        // healthy coupons already committed, the sweep ends with
                        // `assigned` complete and `skipped` still 0. Nothing
                        // quarantines that coupon, so retry within the deadline
                        // instead of failing the run — the next sweep re-finds it.
                        warn!(
                            "counter assertion: assigned reached {assigned} but skipped \
                             is still {skipped}; retrying"
                        );
                        None
                    }
                })
            },
        )
        .run(f)
        .await?;

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

#[cfg(test)]
mod tests {
    use super::decparty_metric;

    const BODY: &str = "\
# TYPE decman_reward_coupons_assigned_total counter
decman_reward_coupons_assigned_total{decparty=\"other::1220\"} 9
decman_reward_coupons_assigned_total{decparty=\"gov::1220\"} 3
decman_reward_coupons_assigned_total_extra{decparty=\"gov::1220\"} 7
decman_reward_oldest_unassigned_expires_in_seconds{decparty=\"gov::1220\"} bad
";

    #[test]
    fn reads_one_decpartys_series_of_one_family() {
        let got = decparty_metric(BODY, "decman_reward_coupons_assigned_total", "gov::1220")
            .expect("parses");
        assert_eq!(got, Some(3.0));
    }

    #[test]
    fn a_missing_series_is_none_and_a_bad_value_is_an_error() {
        let missing = decparty_metric(BODY, "decman_reward_coupons_assigned_total", "new::1220");
        assert_eq!(missing.expect("parses"), None);
        let bad = decparty_metric(
            BODY,
            "decman_reward_oldest_unassigned_expires_in_seconds",
            "gov::1220",
        );
        assert!(bad.is_err());
    }
}
