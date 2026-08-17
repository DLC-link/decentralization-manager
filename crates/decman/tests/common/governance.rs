use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};

use crate::common::{Fixture, scenario::Scenario, types::GovernanceState};

#[derive(Default)]
pub struct ProposalCycleCtx {
    pub proposal_cid: Option<String>,
    pub confirmation_cids: Vec<String>,
}

/// Which decparty a governance cycle runs on.
#[derive(Clone)]
pub enum CycleParty {
    /// The suite's main decparty. Each step reads `f.party_id` and
    /// `f.rules_contract_id` when it runs.
    Primary,
    /// Any other decparty. The caller supplies both ids.
    Named {
        party_id: String,
        rules_contract_id: String,
    },
}

impl CycleParty {
    /// Returns the party id and the rules contract id for this cycle.
    fn resolve(&self, f: &Fixture) -> anyhow::Result<(String, String)> {
        match self {
            CycleParty::Primary => Ok((
                f.party_id()?.to_string(),
                f.rules_contract_id()?.to_string(),
            )),
            CycleParty::Named {
                party_id,
                rules_contract_id,
            } => Ok((party_id.clone(), rules_contract_id.clone())),
        }
    }
}

/// Build a scenario that drives one propose → confirm → execute cycle on the
/// named decparty: P1 proposes, P2 confirms, P3 executes. The scenario asserts
/// that no pending action remains for this proposal.
///
/// The scenario includes cross-participant visibility steps. They stop a WHEN
/// step from running before the confirmer or the executor sees the proposal on
/// its own ledger view.
pub fn propose_confirm_execute_on(
    party: CycleParty,
    label: &str,
    proposal: Value,
) -> Scenario<ProposalCycleCtx> {
    let label = label.to_string();
    Scenario::with_ctx(label.clone(), ProposalCycleCtx::default())
        .given("party and governance rules contract present", {
            let party = party.clone();
            move |f, _ctx| {
                let party = party.clone();
                Box::pin(async move {
                    party.resolve(f)?;
                    Ok(())
                })
            }
        })
        .when(format!("P1 proposes {label}"), {
            let proposal = proposal.clone();
            let party = party.clone();
            move |f, _ctx| {
                let proposal = proposal.clone();
                let party = party.clone();
                Box::pin(async move {
                    let (party_id, rules_contract_id) = party.resolve(f)?;
                    let req = json!({
                        "party_id": party_id,
                        "rules_contract_id": rules_contract_id,
                        "proposal": proposal,
                    });
                    let _: Value = f.post_json(f.p1.http, "/governance/propose", &req).await?;
                    Ok(())
                })
            }
        })
        .then(
            "proposal visible in confirmations on P1",
            Duration::from_secs(60),
            {
                let label_p1 = label.clone();
                let party = party.clone();
                move |f, ctx| {
                    let label_p1 = label_p1.clone();
                    let party = party.clone();
                    Box::pin(async move {
                        let party_id = match party.resolve(f) {
                            Ok((p, _)) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let path = format!("/governance/confirmations?party_id={party_id}");
                        let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                        // Match THIS cycle's proposal by action label rather than
                        // assuming it is the only pending domain action. A prior
                        // phase (e.g. notification_feed) can leave an unrelated
                        // proposal pending, so `len() == 1` is not a safe gate.
                        let action = s
                            .domain_actions
                            .into_iter()
                            .find(|a| a.action_label == label_p1)?;
                        ctx.proposal_cid = Some(action.proposal_cid);
                        Some(Ok(()))
                    })
                }
            },
        )
        .then("proposal visible on P2", Duration::from_secs(60), {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let cid = match ctx.proposal_cid.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            return Some(Err(anyhow::anyhow!(
                                "proposal_cid not set by previous step"
                            )));
                        }
                    };
                    let party_id = match party.resolve(f) {
                        Ok((p, _)) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p2.http, &path).await.ok()?;
                    s.domain_actions
                        .iter()
                        .any(|a| a.proposal_cid == cid)
                        .then_some(Ok(()))
                })
            }
        })
        .when("P2 confirms", {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let proposal_cid = ctx
                        .proposal_cid
                        .as_deref()
                        .context("proposal_cid not set")?
                        .to_string();
                    let (party_id, rules_contract_id) = party.resolve(f)?;
                    let req = json!({
                        "party_id": party_id, "rules_contract_id": rules_contract_id,
                        "action": {"type": "governance_set_threshold", "new_threshold": 1},
                        "governance_type": "core_domain", "proposal_cid": proposal_cid,
                    });
                    let _: Value = f.post_json(f.p2.http, "/governance/confirm", &req).await?;
                    Ok(())
                })
            }
        })
        .then("can_execute=true on P1", Duration::from_secs(60), {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let party_id = match party.resolve(f) {
                        Ok((p, _)) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    let our_cid = ctx.proposal_cid.clone();
                    let action = s
                        .domain_actions
                        .into_iter()
                        .find(|a| Some(&a.proposal_cid) == our_cid.as_ref() && a.can_execute)?;
                    ctx.confirmation_cids = action
                        .confirmations
                        .iter()
                        .map(|c| c.contract_id.clone())
                        .collect();
                    Some(Ok(()))
                })
            }
        })
        .then(
            "proposal + confirmations visible on P3",
            Duration::from_secs(60),
            {
                let party = party.clone();
                move |f, ctx| {
                    let party = party.clone();
                    Box::pin(async move {
                        let cid = match ctx.proposal_cid.as_ref() {
                            Some(c) => c.clone(),
                            None => {
                                return Some(Err(anyhow::anyhow!(
                                    "proposal_cid not set by previous step"
                                )));
                            }
                        };
                        let party_id = match party.resolve(f) {
                            Ok((p, _)) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let path = format!("/governance/confirmations?party_id={party_id}");
                        let s: GovernanceState = f.get_json(f.p3.http, &path).await.ok()?;
                        let action = s
                            .domain_actions
                            .into_iter()
                            .find(|a| a.proposal_cid == cid && a.can_execute)?;
                        ctx.confirmation_cids = action
                            .confirmations
                            .iter()
                            .map(|c| c.contract_id.clone())
                            .collect();
                        Some(Ok(()))
                    })
                }
            },
        )
        .when("P3 executes", {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let proposal_cid = ctx
                        .proposal_cid
                        .as_deref()
                        .context("proposal_cid not set")?
                        .to_string();
                    let confirmation_cids = ctx.confirmation_cids.clone();
                    let (party_id, rules_contract_id) = party.resolve(f)?;
                    let req = json!({
                        "party_id": party_id, "rules_contract_id": rules_contract_id,
                        "action": {"type": "governance_set_threshold", "new_threshold": 1},
                        "confirmation_cids": confirmation_cids, "disclosed_contracts": [],
                        "governance_type": "core_domain", "proposal_cid": proposal_cid,
                    });
                    let _: Value = f.post_json(f.p3.http, "/governance/execute", &req).await?;
                    Ok(())
                })
            }
        })
        .then(
            "this proposal no longer pending",
            Duration::from_secs(60),
            {
                let party = party.clone();
                move |f, ctx| {
                    let party = party.clone();
                    Box::pin(async move {
                        let party_id = match party.resolve(f) {
                            Ok((p, _)) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let path = format!("/governance/confirmations?party_id={party_id}");
                        let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                        // This cycle is done when ITS proposal is gone (executed).
                        // Don't assert a globally empty slate — an unrelated prior
                        // proposal may still be pending (see the P1 visibility note).
                        let our_cid = ctx.proposal_cid.clone();
                        (!s.domain_actions
                            .iter()
                            .any(|a| Some(&a.proposal_cid) == our_cid.as_ref()))
                        .then_some(Ok(()))
                    })
                }
            },
        )
}

/// Drive one cycle on the decparty the fixture holds.
pub fn propose_confirm_execute(label: &str, proposal: Value) -> Scenario<ProposalCycleCtx> {
    propose_confirm_execute_on(CycleParty::Primary, label, proposal)
}
