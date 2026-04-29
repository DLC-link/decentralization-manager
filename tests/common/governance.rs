use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};

use crate::common::{scenario::Scenario, types::GovernanceState};

#[derive(Default)]
pub struct ProposalCycleCtx {
    pub proposal_cid: Option<String>,
    pub confirmation_cids: Vec<String>,
}

/// Build a scenario that drives a single propose → confirm → execute cycle:
/// P1 proposes, P2 confirms, P3 executes, asserting no pending actions remain.
///
/// Includes cross-participant ACS visibility gates so the WHEN steps don't fire
/// before the confirmer/executor has observed the proposal/confirmations on its
/// own ledger view.
pub fn propose_confirm_execute(label: &str, proposal: Value) -> Scenario<ProposalCycleCtx> {
    let label = label.to_string();
    Scenario::with_ctx(label.clone(), ProposalCycleCtx::default())
        .when(format!("P1 proposes {label}"), {
            let proposal = proposal.clone();
            move |f, _ctx| {
                let proposal = proposal.clone();
                Box::pin(async move {
                    let req = json!({
                        "party_id": f.party_id()?,
                        "rules_contract_id": f.rules_contract_id()?,
                        "proposal": proposal,
                    });
                    let _: Value = f.post_json(f.p1.http, "/governance/propose", &req).await?;
                    Ok(())
                })
            }
        })
        .then_eventually(
            "proposal visible in confirmations on P1",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    if s.domain_actions.len() != 1 {
                        return None;
                    }
                    let action = s.domain_actions.into_iter().next()?;
                    ctx.proposal_cid = Some(action.proposal_cid);
                    Some(Ok(()))
                })
            },
        )
        .given_eventually(
            "proposal visible on P2",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let cid = match ctx.proposal_cid.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            return Some(Err(anyhow::anyhow!(
                                "proposal_cid not set by previous step"
                            )));
                        }
                    };
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p2.http, &path).await.ok()?;
                    s.domain_actions
                        .iter()
                        .any(|a| a.proposal_cid == cid)
                        .then_some(Ok(()))
                })
            },
        )
        .when("P2 confirms", |f, ctx| {
            Box::pin(async move {
                let proposal_cid = ctx
                    .proposal_cid
                    .as_deref()
                    .context("proposal_cid not set")?
                    .to_string();
                let req = json!({
                    "party_id": f.party_id()?, "rules_contract_id": f.rules_contract_id()?,
                    "action": {"type": "governance_set_threshold", "new_threshold": 0},
                    "governance_type": "core_domain", "proposal_cid": proposal_cid,
                });
                let _: Value = f.post_json(f.p2.http, "/governance/confirm", &req).await?;
                Ok(())
            })
        })
        .then_eventually(
            "can_execute=true on P1",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    let action = s.domain_actions.into_iter().find(|a| a.can_execute)?;
                    ctx.confirmation_cids = action
                        .confirmations
                        .iter()
                        .map(|c| c.contract_id.clone())
                        .collect();
                    Some(Ok(()))
                })
            },
        )
        .given_eventually(
            "proposal + confirmations visible on P3",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let cid = match ctx.proposal_cid.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            return Some(Err(anyhow::anyhow!(
                                "proposal_cid not set by previous step"
                            )));
                        }
                    };
                    let party_id = match f.party_id() {
                        Ok(p) => p,
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
            },
        )
        .when("P3 executes", |f, ctx| {
            Box::pin(async move {
                let proposal_cid = ctx
                    .proposal_cid
                    .as_deref()
                    .context("proposal_cid not set")?
                    .to_string();
                let confirmation_cids = ctx.confirmation_cids.clone();
                let req = json!({
                    "party_id": f.party_id()?, "rules_contract_id": f.rules_contract_id()?,
                    "action": {"type": "governance_set_threshold", "new_threshold": 0},
                    "confirmation_cids": confirmation_cids, "disclosed_contracts": [],
                    "governance_type": "core_domain", "proposal_cid": proposal_cid,
                });
                let _: Value = f.post_json(f.p3.http, "/governance/execute", &req).await?;
                Ok(())
            })
        })
        .then_eventually(
            "no pending domain actions",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    s.domain_actions.is_empty().then_some(Ok(()))
                })
            },
        )
}
