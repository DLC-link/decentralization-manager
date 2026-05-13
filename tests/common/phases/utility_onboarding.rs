use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tracing::info;

use crate::common::{
    Fixture,
    TestTarget,
    governance::propose_confirm_execute,
    operator::{OPERATOR_RESPONSE_TIMEOUT_DEVNET, await_operator_response},
    scenario::Scenario,
    types::{ContractsQueryResponse, ProviderServicesResponse},
};

const UTILITY_APP_PKG: &str = "%23utility-registry-app-v0";
const UTILITY_REGISTRY_PKG: &str = "%23utility-registry-v0";

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: utility_onboarding");

    if f.provider_service_cid.is_none() {
        match f.target {
            TestTarget::Localnet => {
                propose_confirm_execute(
                    "ProvisionProviderService",
                    json!({"type": "provision_provider_service"}),
                )
                .run(f)
                .await?;

                Scenario::new("ProviderService visible")
                    .then(
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
            TestTarget::Devnet => {
                let operator = f
                    .operator_party
                    .clone()
                    .context("operator_party not set on devnet — discover_network_parties must run first")?;
                let governance_party = f.party_id()?.to_string();
                propose_confirm_execute(
                    "CreateProviderServiceRequest",
                    json!({
                        "type": "create_provider_service_request",
                        "operator": operator,
                        "provider": governance_party,
                    }),
                )
                .run(f)
                .await?;

                info!("Awaiting operator-driven ProviderService for {governance_party}");
                let port = f.p1.http;
                let path = format!("/services/provider?party_id={governance_party}");
                let operator_for_match = operator.clone();
                let governance_for_match = governance_party.clone();
                let cid = await_operator_response::<ProviderServicesResponse, _>(
                    f,
                    port,
                    &path,
                    "CreateProviderServiceRequest",
                    "ProviderService",
                    OPERATOR_RESPONSE_TIMEOUT_DEVNET,
                    move |r| {
                        r.services
                            .into_iter()
                            .find(|s| {
                                s.operator.as_deref() == Some(operator_for_match.as_str())
                                    && s.provider.as_deref() == Some(governance_for_match.as_str())
                            })
                            .map(|s| s.contract_id)
                    },
                )
                .await?;
                f.provider_service_cid = Some(cid);
            }
        }
    }

    let provider_cid = f
        .provider_service_cid
        .clone()
        .context("provider_service_cid not set after provider service setup")?;
    let p1_member = f.p1_member_party()?.to_string();
    let party_id = f.party_id()?.to_string();

    propose_confirm_execute(
        "SetupUtility",
        json!({
            "type": "setup_utility",
            "provider_service_cid": provider_cid,
            "operator": p1_member,
            "instrument_id_text": format!("{}-TEST-E2E-TOKEN", f.run_id),
            "create_transfer_rule": true,
            "create_allocation_factory": true,
        }),
    )
    .run(f)
    .await?;

    Scenario::new("SetupUtility side effects")
        .then(
            "AllocationFactory visible",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!(
                        "/contracts/query?party_id={party_id}&package_id={UTILITY_APP_PKG}\
                         &module_name=Utility.Registry.App.V0.Service.AllocationFactory\
                         &entity_name=AllocationFactory"
                    );
                    let r: ContractsQueryResponse = f.get_json(f.p1.http, &path).await.ok()?;
                    let cid = r.contracts.into_iter().next()?.contract_id;
                    f.allocation_factory_cid = Some(cid);
                    Some(Ok(()))
                })
            },
        )
        .then(
            "InstrumentConfiguration visible",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!(
                        "/contracts/query?party_id={party_id}&package_id={UTILITY_REGISTRY_PKG}\
                         &module_name=Utility.Registry.V0.Configuration.Instrument\
                         &entity_name=InstrumentConfiguration"
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
            "instrument_id": {"admin": party_id, "id": format!("{}-TEST-E2E-TOKEN", f.run_id)},
            "instrument_configuration_cid": inst,
            "recipient": p1_member,
            "amount": "100.0",
            "description": "E2E test mint",
        }),
    )
    .run(f)
    .await?;

    Scenario::new("Mint side effects")
        .then("MintOffer count >= 1", Duration::from_secs(30), |f, _| {
            Box::pin(async move {
                let party_id = match f.party_id() {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let path = format!(
                    "/contracts/query?party_id={party_id}&package_id={UTILITY_APP_PKG}\
                     &module_name=Utility.Registry.App.V0.Model.Mint\
                     &entity_name=MintOffer"
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
            "instrument_id": {"admin": party_id, "id": format!("{}-TEST-E2E-TOKEN", f.run_id)},
            "instrument_configuration_cid": inst,
            "holder": p1_member,
            "amount": "10.0",
            "description": "E2E test burn",
        }),
    )
    .run(f)
    .await?;

    Scenario::new("Burn side effects")
        .then("BurnOffer count >= 1", Duration::from_secs(30), |f, _| {
            Box::pin(async move {
                let party_id = match f.party_id() {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let path = format!(
                    "/contracts/query?party_id={party_id}&package_id={UTILITY_APP_PKG}\
                     &module_name=Utility.Registry.App.V0.Model.Burn\
                     &entity_name=BurnOffer"
                );
                let r: ContractsQueryResponse = f.get_json(f.p1.http, &path).await.ok()?;
                (!r.contracts.is_empty()).then_some(Ok(()))
            })
        })
        .run(f)
        .await?;

    Ok(())
}
