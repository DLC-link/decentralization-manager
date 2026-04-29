use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    scenario::Scenario,
    types::{ContractsQueryResponse, GovernanceState, ProviderServicesResponse},
};

const UTILITY_APP_PKG: &str = "%23utility-registry-app-v0";
const UTILITY_REGISTRY_PKG: &str = "%23utility-registry-v0";

#[derive(Default)]
struct ProposalCycleCtx {
    proposal_cid: Option<String>,
    confirmation_cids: Vec<String>,
}

fn propose_confirm_execute(label: &str, proposal: Value) -> Scenario<ProposalCycleCtx> {
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
                    if s.domain_actions.len() != 1 {
                        return None;
                    }
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
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: utility_onboarding");

    if f.provider_service_cid.is_none() {
        propose_confirm_execute(
            "ProvisionProviderService",
            json!({"type": "provision_provider_service"}),
        )
        .run(f)
        .await?;
        Scenario::new("ProviderService visible")
            .then_eventually(
                "services/provider returns one",
                Duration::from_secs(30),
                |f, _| {
                    Box::pin(async move {
                        let party_id = match f.party_id() {
                            Ok(p) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let path = format!("/services/provider?party_id={party_id}");
                        let r: ProviderServicesResponse =
                            f.get_json(f.p1.http, &path).await.ok()?;
                        let cid = r.services.into_iter().next()?.contract_id;
                        f.provider_service_cid = Some(cid);
                        Some(Ok(()))
                    })
                },
            )
            .run(f)
            .await?;
    }

    let provider_cid = f
        .provider_service_cid
        .clone()
        .context("provider_service_cid not set after ProvisionProviderService")?;
    let p1_member = f.p1_member_party()?.to_string();
    let party_id = f.party_id()?.to_string();

    propose_confirm_execute(
        "SetupUtility",
        json!({
            "type": "setup_utility",
            "provider_service_cid": provider_cid,
            "operator": p1_member,
            "instrument_id_text": "TEST-E2E-TOKEN",
            "create_transfer_rule": true,
            "create_allocation_factory": true,
        }),
    )
    .run(f)
    .await?;

    Scenario::new("SetupUtility side effects")
        .then_eventually(
            "AllocationFactory visible",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!(
                        "/contracts/query?party_id={party_id}&package_id={UTILITY_APP_PKG}&module_name=Utility.Registry.App.V0.Service.AllocationFactory&entity_name=AllocationFactory"
                    );
                    let r: ContractsQueryResponse = f.get_json(f.p1.http, &path).await.ok()?;
                    let cid = r.contracts.into_iter().next()?.contract_id;
                    f.allocation_factory_cid = Some(cid);
                    Some(Ok(()))
                })
            },
        )
        .then_eventually(
            "InstrumentConfiguration visible",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!(
                        "/contracts/query?party_id={party_id}&package_id={UTILITY_REGISTRY_PKG}&module_name=Utility.Registry.V0.Configuration.Instrument&entity_name=InstrumentConfiguration"
                    );
                    let r: ContractsQueryResponse = f.get_json(f.p1.http, &path).await.ok()?;
                    let cid = r.contracts.into_iter().next()?.contract_id;
                    f.instrument_configuration_cid = Some(cid);
                    Some(Ok(()))
                })
            },
        )
        .run(f)
        .await?;

    let alloc = f
        .allocation_factory_cid
        .clone()
        .context("allocation_factory_cid not set")?;
    let inst = f
        .instrument_configuration_cid
        .clone()
        .context("instrument_configuration_cid not set")?;

    propose_confirm_execute(
        "Mint",
        json!({
            "type": "mint",
            "allocation_factory_cid": alloc,
            "instrument_id": {"admin": party_id, "id": "TEST-E2E-TOKEN"},
            "instrument_configuration_cid": inst,
            "recipient": p1_member,
            "amount": "100.0",
            "description": "E2E test mint",
        }),
    )
    .run(f)
    .await?;

    Scenario::new("Mint side effects")
        .then_eventually("MintOffer count >= 1", Duration::from_secs(30), |f, _| {
            Box::pin(async move {
                let party_id = match f.party_id() {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let path = format!(
                    "/contracts/query?party_id={party_id}&package_id={UTILITY_APP_PKG}&module_name=Utility.Registry.App.V0.Model.Mint&entity_name=MintOffer"
                );
                let r: ContractsQueryResponse = f.get_json(f.p1.http, &path).await.ok()?;
                (!r.contracts.is_empty()).then_some(Ok(()))
            })
        })
        .run(f)
        .await?;

    propose_confirm_execute(
        "Burn",
        json!({
            "type": "burn",
            "allocation_factory_cid": alloc,
            "instrument_id": {"admin": party_id, "id": "TEST-E2E-TOKEN"},
            "instrument_configuration_cid": inst,
            "holder": p1_member,
            "amount": "10.0",
            "description": "E2E test burn",
        }),
    )
    .run(f)
    .await?;

    Scenario::new("Burn side effects")
        .then_eventually("BurnOffer count >= 1", Duration::from_secs(30), |f, _| {
            Box::pin(async move {
                let party_id = match f.party_id() {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let path = format!(
                    "/contracts/query?party_id={party_id}&package_id={UTILITY_APP_PKG}&module_name=Utility.Registry.App.V0.Model.Burn&entity_name=BurnOffer"
                );
                let r: ContractsQueryResponse = f.get_json(f.p1.http, &path).await.ok()?;
                (!r.contracts.is_empty()).then_some(Ok(()))
            })
        })
        .run(f)
        .await?;

    Ok(())
}
