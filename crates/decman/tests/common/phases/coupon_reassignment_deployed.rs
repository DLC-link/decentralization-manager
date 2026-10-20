//! CIP-104 Mode A coupon reassignment against a delegation already deployed on
//! devnet. Only `devnet_reward_e2e` runs this phase.
//!
//! The phase never votes, onboards or submits a command itself. It reads the
//! decparty's active `CouponReassignmentDelegation` from PQS and registers the
//! decparty on the three harness nodes. Each node's reward loop then reads the
//! same delegation. A node whose member party is a listed assigner exercises
//! `Delegation_Assign`. The phase checks the outcome in two places:
//!
//! - the assigner nodes' `decman_reward_coupons_assigned_total`, which counts
//!   the coupons that node assigned and the ledger accepted;
//! - PQS, where the coupons created with a beneficiary since the phase began
//!   must match the delegation's split by value.
//!
//! The run needs these conditions, which this harness cannot create:
//!
//! - Mode-B collection is paused for the decparty, so coupons stay unassigned.
//! - Unassigned coupons have accumulated since the pause.
//! - No other DecMan assigns the decparty's coupons during the run.
//! - The harness nodes sweep on a short interval. `devnet.env.sh` sets
//!   `DECPM_REWARD_AUTOMATION_INTERVAL_SECS=15`.
//! - The member users already hold act-as and read-as rights on the decparty.
//!   The phase grants no rights.

use std::time::Duration;

use anyhow::Context;
use common::api::ActiveCouponReassignmentDelegation;
use tracing::{info, warn};

use crate::common::{
    Fixture, TestTarget,
    phases::{
        coupon_reassignment::{REASSIGN_TIMEOUT, decparty_metric},
        deploy_gov_core::update_party_config,
    },
    pqs::{DeployedDelegation, Pqs, check_split},
    probe::Class,
    scenario::Scenario,
};

/// The full party id of the decparty whose deployed delegation the phase uses.
pub const DECPARTY_VAR: &str = "DECPM_IT_REWARD_DECPARTY";
/// A Postgres URL for the PQS database that projects that decparty.
pub const PQS_URL_VAR: &str = "DECPM_IT_PQS_URL";

const ASSIGNED_METRIC: &str = "decman_reward_coupons_assigned_total";

/// What the steps learn and hand to later steps.
#[derive(Default)]
struct Run {
    delegation: Option<DeployedDelegation>,
    /// `(http, metrics)` ports of the harness nodes the delegation lists.
    assigner_nodes: Vec<(u16, u16)>,
    /// PQS's ledger offset just before the harness nodes start to sweep.
    start_offset: i64,
    /// Coupons the harness nodes had assigned when the counter step passed.
    assigned_by_harness: f64,
}

impl Run {
    fn delegation(&self) -> anyhow::Result<&DeployedDelegation> {
        self.delegation
            .as_ref()
            .context("delegation not read — the first Given must run first")
    }
}

fn read_input(var: &str) -> anyhow::Result<String> {
    required(var, std::env::var(var).ok())
}

/// `value` of the input `var`, where blank counts as unset (pure).
fn required(var: &str, value: Option<String>) -> anyhow::Result<String> {
    value
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("{var} is not set; devnet_reward_e2e needs it (see README)"))
}

/// The harness nodes whose member party the delegation lists as an assigner,
/// as `(http, metrics)` ports (pure).
fn assigner_nodes(
    nodes: &[(String, u16, u16)],
    delegation: &DeployedDelegation,
) -> anyhow::Result<Vec<(u16, u16)>> {
    let listed: Vec<(u16, u16)> = nodes
        .iter()
        .filter(|(member, _, _)| delegation.assigners.contains(member))
        .map(|(_, http, metrics)| (*http, *metrics))
        .collect();
    anyhow::ensure!(
        !listed.is_empty(),
        "delegation {} lists assigners {:?}, and none is a harness member party ({:?}); the \
         harness nodes cannot exercise Delegation_Assign",
        delegation.cid,
        delegation.assigners,
        nodes.iter().map(|(m, _, _)| m).collect::<Vec<_>>()
    );
    Ok(listed)
}

/// Whether DecMan's own ledger read names the delegation PQS holds (pure).
/// `Ok(false)` means DecMan has not answered with a delegation yet.
fn decman_agrees(
    decman: &ActiveCouponReassignmentDelegation,
    pqs: &DeployedDelegation,
) -> anyhow::Result<bool> {
    let Some(first) = decman.delegations.first() else {
        return Ok(false);
    };
    anyhow::ensure!(
        first.cid == pqs.cid,
        "DecMan acts on delegation {}, but PQS shows {} as the newest",
        first.cid,
        pqs.cid
    );
    let mut theirs: Vec<String> = first.assigners.iter().map(ToString::to_string).collect();
    let mut ours = pqs.assigners.clone();
    theirs.sort();
    ours.sort();
    anyhow::ensure!(
        theirs == ours,
        "DecMan reads assigners {theirs:?} on {}, PQS reads {ours:?}",
        pqs.cid
    );
    anyhow::ensure!(
        first.dso.to_string() == pqs.dso,
        "DecMan reads dso {} on {}, PQS reads {}",
        first.dso,
        pqs.cid,
        pqs.dso
    );
    Ok(true)
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: coupon_reassignment_deployed (CIP-104 Mode A on a deployed delegation)");
    anyhow::ensure!(
        f.target == TestTarget::Devnet,
        "coupon_reassignment_deployed reads a delegation deployed on devnet; it does not run on \
         localnet"
    );
    let decparty = read_input(DECPARTY_VAR)?;
    let pqs = Pqs::connect(&read_input(PQS_URL_VAR)?).await?;

    let member = |creds: &Option<crate::common::MemberCreds>, n: u8| {
        creds
            .as_ref()
            .map(|c| c.party_id.clone())
            .with_context(|| format!("P{n} member creds missing on devnet"))
    };
    let members = [
        member(&f.p1_member_creds, 1)?,
        member(&f.p2_member_creds, 2)?,
        member(&f.p3_member_creds, 3)?,
    ];
    f.party_id = Some(decparty.clone());
    f.p1_member_party = Some(members[0].clone());
    f.p2_member_party = Some(members[1].clone());
    f.p3_member_party = Some(members[2].clone());
    let nodes = vec![
        (members[0].clone(), f.p1.http, f.p1.metrics),
        (members[1].clone(), f.p2.http, f.p2.metrics),
        (members[2].clone(), f.p3.http, f.p3.metrics),
    ];

    Scenario::with_ctx(format!("Delegation_Assign on {decparty}"), Run::default())
        .given("PQS holds the decparty's active delegation", {
            let pqs = pqs.clone();
            let decparty = decparty.clone();
            move |_, ctx| {
                let pqs = pqs.clone();
                let decparty = decparty.clone();
                Box::pin(async move {
                    let mut active = pqs.active_delegations(&decparty).await?;
                    anyhow::ensure!(
                        !active.is_empty(),
                        "PQS shows no active CouponReassignmentDelegation for {decparty}; one \
                         must be voted in first, and PQS must index \
                         governance-rewards-automation-v1"
                    );
                    if active.len() > 1 {
                        warn!(
                            "{} active delegations for {decparty}; the automation acts on the \
                             newest",
                            active.len()
                        );
                    }
                    let delegation = active.swap_remove(0);
                    info!(
                        "delegation {}: assigners {:?}, split {:?}",
                        delegation.cid,
                        delegation.assigners,
                        delegation
                            .split
                            .iter()
                            .map(|s| format!("{} @ {}", s.beneficiary, s.percentage))
                            .collect::<Vec<_>>()
                    );
                    ctx.delegation = Some(delegation);
                    Ok(())
                })
            }
        })
        .given("the delegation names the network's DSO", |f, ctx| {
            Box::pin(async move {
                let delegation = ctx.delegation()?;
                let network = f
                    .dso_party
                    .as_deref()
                    .context("dso_party not set — discover_network_parties must run first")?;
                anyhow::ensure!(
                    delegation.dso == network,
                    "delegation {} names dso {}, but the network's DSO is {network}; no live \
                     coupon could be assigned through it",
                    delegation.cid,
                    delegation.dso
                );
                Ok(())
            })
        })
        .given(
            "a harness node's member party is a listed assigner",
            move |_, ctx| {
                let nodes = nodes.clone();
                Box::pin(async move {
                    ctx.assigner_nodes = assigner_nodes(&nodes, ctx.delegation()?)?;
                    Ok(())
                })
            },
        )
        .given("unassigned coupons are waiting", {
            let pqs = pqs.clone();
            let decparty = decparty.clone();
            move |_, ctx| {
                let pqs = pqs.clone();
                let decparty = decparty.clone();
                Box::pin(async move {
                    // Read the offset first: a coupon assigned after it counts.
                    ctx.start_offset = pqs.latest_offset().await?;
                    let dso = ctx.delegation()?.dso.clone();
                    let (count, total) = pqs.unassigned_coupons(&decparty, &dso).await?;
                    anyhow::ensure!(
                        count > 0,
                        "PQS shows no unassigned RewardCouponV2 for {decparty} from {dso}; pause \
                         Mode-B collection and let coupons accumulate (see README)"
                    );
                    info!(
                        "{count} unassigned coupon(s) worth {total} wait at offset {}",
                        ctx.start_offset
                    );
                    Ok(())
                })
            }
        })
        .when("the harness nodes serve the decparty", {
            let decparty = decparty.clone();
            move |f, _| {
                let decparty = decparty.clone();
                Box::pin(async move {
                    let f: &Fixture = f;
                    for (n, port, member, creds) in [
                        (
                            1,
                            f.p1.http,
                            f.p1_member_party()?,
                            f.p1_member_creds.as_ref(),
                        ),
                        (
                            2,
                            f.p2.http,
                            f.p2_member_party()?,
                            f.p2_member_creds.as_ref(),
                        ),
                        (
                            3,
                            f.p3.http,
                            f.p3_member_party()?,
                            f.p3_member_creds.as_ref(),
                        ),
                    ] {
                        let name = format!("participant-{n}");
                        update_party_config(f, port, &decparty, member, &name, creds).await?;
                    }
                    Ok(())
                })
            }
        })
        .then(
            "DecMan's ledger read agrees with PQS",
            Duration::from_secs(60),
            {
                let decparty = decparty.clone();
                move |f, ctx| {
                    let decparty = decparty.clone();
                    Box::pin(async move {
                        let (http, _) = *ctx.assigner_nodes.first()?;
                        let path = format!("/coupon-reassignment-delegation?party_id={decparty}");
                        let read: ActiveCouponReassignmentDelegation =
                            f.probe_get_json(http, &path).await?;
                        let delegation = match ctx.delegation() {
                            Ok(d) => d,
                            Err(e) => return Some(Err(e)),
                        };
                        match decman_agrees(&read, delegation) {
                            Ok(true) => Some(Ok(())),
                            Ok(false) => {
                                f.probe_diag.record(
                                    "delegation agreement",
                                    Class::Transient,
                                    format!(
                                        "DecMan lists no delegation for {decparty} in its \
                                         configured rewards package"
                                    ),
                                );
                                None
                            }
                            Err(e) => Some(Err(e)),
                        }
                    })
                }
            },
        )
        .then(
            "a harness assigner exercised Delegation_Assign",
            REASSIGN_TIMEOUT,
            {
                let decparty = decparty.clone();
                move |f, ctx| {
                    let decparty = decparty.clone();
                    Box::pin(async move {
                        let mut assigned = 0.0;
                        for (_, metrics) in &ctx.assigner_nodes {
                            let body = f.probe_diag.ok(
                                "assigned counter: read /metrics",
                                f.get_text(*metrics, "/metrics").await,
                            )?;
                            let value = f.probe_diag.ok(
                                "assigned counter: parse /metrics",
                                decparty_metric(&body, ASSIGNED_METRIC, &decparty),
                            )?;
                            assigned += value.unwrap_or(0.0);
                        }
                        if assigned < 1.0 {
                            f.probe_diag.record(
                                "assigned counter",
                                Class::Transient,
                                format!("{ASSIGNED_METRIC} is still 0 on the harness assigners"),
                            );
                            return None;
                        }
                        info!("the harness assigners assigned {assigned} coupon(s)");
                        ctx.assigned_by_harness = assigned;
                        Some(Ok(()))
                    })
                }
            },
        )
        .then(
            "PQS shows one coupon per beneficiary at the delegation's split",
            Duration::from_secs(180),
            move |f, ctx| {
                let pqs = pqs.clone();
                let decparty = decparty.clone();
                Box::pin(async move {
                    let delegation = match ctx.delegation() {
                        Ok(d) => d,
                        Err(e) => return Some(Err(e)),
                    };
                    let assigned = f.probe_diag.ok(
                        "split: read PQS",
                        pqs.assigned_since(&decparty, &delegation.dso, ctx.start_offset)
                            .await,
                    )?;
                    // PQS may lag the ledger. Wait until it holds at least the
                    // coupons the harness counted for every beneficiary.
                    let fewest = assigned.iter().map(|a| a.coupons).min().unwrap_or(0);
                    if (fewest as f64) < ctx.assigned_by_harness {
                        f.probe_diag.record(
                            "split: PQS lag",
                            Class::Transient,
                            format!(
                                "PQS shows {fewest} assigned coupon(s) for some beneficiary; the \
                                 harness assigned {}",
                                ctx.assigned_by_harness
                            ),
                        );
                        return None;
                    }
                    Some(check_split(&delegation.split, &assigned).map(|seen| {
                        info!(
                            "{} assigned coupon(s) per beneficiary, worth {} in total",
                            seen.coupons_per_beneficiary, seen.total
                        );
                    }))
                })
            },
        )
        .run(f)
        .await?;

    info!(
        "the non-assigner ledger negative stays a manual step; DAML covers it in \
         test_non_assigner_cannot_reassign"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use common::api::CouponReassignmentDelegationSummary;
    use common::canton_id::CantonId;

    use super::*;
    use crate::common::pqs::Share;

    const DSO: &str = "DSO::1220dededededededededededededededededededededededededededededededede";
    const A1: &str =
        "attestor-1::12200f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f";
    const A2: &str =
        "attestor-2::12201e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e";
    const A3: &str =
        "attestor-3::12202d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d";

    fn delegation(assigners: &[&str]) -> DeployedDelegation {
        DeployedDelegation {
            cid: "00del".to_string(),
            dso: DSO.to_string(),
            assigners: assigners.iter().map(|a| a.to_string()).collect(),
            split: vec![Share {
                beneficiary: "benef::1220".to_string(),
                percentage: "1.0".parse().expect("decimal"),
            }],
        }
    }

    fn nodes() -> Vec<(String, u16, u16)> {
        vec![
            (A1.to_string(), 8081, 9464),
            (A2.to_string(), 8082, 9465),
            (A3.to_string(), 8083, 9466),
        ]
    }

    fn decman_read(cid: &str, assigners: &[&str], dso: &str) -> ActiveCouponReassignmentDelegation {
        ActiveCouponReassignmentDelegation {
            delegations: vec![CouponReassignmentDelegationSummary {
                cid: cid.to_string(),
                dso: CantonId::parse(dso).expect("dso"),
                assigners: assigners
                    .iter()
                    .map(|a| CantonId::parse(a).expect("assigner"))
                    .collect(),
                beneficiary_count: 1,
            }],
        }
    }

    #[test]
    fn picks_the_harness_nodes_the_delegation_lists() {
        let got = assigner_nodes(&nodes(), &delegation(&[A2, A3, "other::1220"])).expect("some");
        assert_eq!(got, vec![(8082, 9465), (8083, 9466)]);
    }

    #[test]
    fn fails_when_no_harness_node_is_an_assigner() {
        let err = assigner_nodes(&nodes(), &delegation(&["other::1220"])).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("other::1220") && msg.contains(A1),
            "got: {msg}"
        );
    }

    #[test]
    fn decman_agrees_when_cid_assigners_and_dso_match() {
        let pqs = delegation(&[A1, A2]);
        // Assigner order does not matter.
        let read = decman_read("00del", &[A2, A1], DSO);
        assert!(decman_agrees(&read, &pqs).expect("agrees"));
    }

    #[test]
    fn decman_has_not_answered_until_it_lists_a_delegation() {
        let read = ActiveCouponReassignmentDelegation {
            delegations: vec![],
        };
        assert!(!decman_agrees(&read, &delegation(&[A1])).expect("no error"));
    }

    #[test]
    fn decman_disagreeing_with_pqs_is_an_error() {
        let pqs = delegation(&[A1, A2]);
        let other_cid = decman_read("00other", &[A1, A2], DSO);
        assert!(decman_agrees(&other_cid, &pqs).is_err());
        let other_assigners = decman_read("00del", &[A1], DSO);
        assert!(decman_agrees(&other_assigners, &pqs).is_err());
        let other_dso = decman_read("00del", &[A1, A2], A3);
        assert!(decman_agrees(&other_dso, &pqs).is_err());
    }

    #[test]
    fn a_blank_input_counts_as_unset() {
        for value in [None, Some(String::new()), Some("  ".to_string())] {
            let err = required(PQS_URL_VAR, value).unwrap_err();
            assert!(format!("{err:#}").contains(PQS_URL_VAR), "got: {err:#}");
        }
        assert_eq!(
            required(PQS_URL_VAR, Some("postgres://pqs".to_string())).expect("set"),
            "postgres://pqs"
        );
    }
}
