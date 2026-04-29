use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{Fixture, scenario::Scenario, types::GovernanceState};

#[derive(Default)]
pub struct TokenCustodyCtx {
    pub proposal_cid: Option<String>,
    pub confirmation_cids: Vec<String>,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: token_custody");

    Scenario::with_ctx("token custody", TokenCustodyCtx::default())
        .given("rules contract deployed", |f, _| {
            Box::pin(async move {
                f.rules_contract_id()?;
                Ok(())
            })
        })
        .when("P1 proposes SetupCcPreapproval", |f, _| {
            Box::pin(async move {
                let req = json!({
                    "party_id": f.party_id()?,
                    "rules_contract_id": f.rules_contract_id()?,
                    "proposal": {
                        "type": "setup_cc_preapproval",
                        "provider": f.p1_member_party()?,
                        "expected_dso": f.p1_member_party()?,
                    },
                });
                let _: Value = f.post_json(f.p1.http, "/governance/propose", &req).await?;
                Ok(())
            })
        })
        .then_eventually(
            "proposal visible in confirmations",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/governance/confirmations?party_id={party_id}");
                    let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                    let action = s.domain_actions.into_iter().next()?;
                    ctx.proposal_cid = Some(action.proposal_cid);
                    Some(Ok(()))
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
        .then_eventually("can_execute=true", Duration::from_secs(60), |f, ctx| {
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
        })
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
        .run(f)
        .await
}
