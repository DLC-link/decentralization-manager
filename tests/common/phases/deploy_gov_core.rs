use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    http::poll_workflow_status,
    invitations::accept_invitation,
    scenario::Scenario,
    types::{AllocatePartyResponse, DecentralizedPartiesResponse, GovernanceStateLookup},
};

const P1_JSON_API: u16 = 3975;
const P2_JSON_API: u16 = 2975;
const P3_JSON_API: u16 = 4975;

async fn allocate_party(f: &Fixture, port: u16, hint: &str, name: &str) -> anyhow::Result<String> {
    info!("Allocating '{hint}' on {name} (port {port})");
    let req = json!({ "party_id_hint": hint, "local_metadata": { "annotations": {} } });
    let r: AllocatePartyResponse = f
        .post_json_auth(port, "/v2/parties", &req)
        .await
        .with_context(|| format!("allocate {hint} on {name}"))?;
    Ok(r.party_details.party)
}

async fn grant_rights(f: &Fixture, port: u16, party: &str, name: &str) -> anyhow::Result<()> {
    let req = json!({
        "userId": "ledger-api-user",
        "rights": [
            {"kind": {"CanActAs":  {"value": {"party": party}}}},
            {"kind": {"CanReadAs": {"value": {"party": party}}}},
        ],
        "identityProviderId": "",
    });
    let _: Value = f
        .post_json_auth(port, "/v2/users/ledger-api-user/rights", &req)
        .await
        .with_context(|| format!("grant rights on '{party}' on {name}"))?;
    Ok(())
}

async fn update_party_config(
    f: &Fixture,
    port: u16,
    party_id: &str,
    member: &str,
    name: &str,
) -> anyhow::Result<()> {
    let req = json!({
        "dec_party_id": party_id,
        "member_party_id": member,
        "user_id": "ledger-api-user",
        "keycloak_url": "",
        "keycloak_realm": "",
        "keycloak_client_id": "",
        "packages": {
            "governance_core": "#governance-core-v0-rc3",
            "governance_token_custody": "#governance-token-custody-v0-rc3",
            "governance_utility_onboarding": "#governance-utility-onboarding-v0-rc3",
            "utility_registry": "#utility-registry-app-v0",
        },
    });
    let _: Value = f
        .put_json(port, "/party-config", &req)
        .await
        .with_context(|| format!("PUT /party-config on {name}"))?;
    Ok(())
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: deploy_gov_core");

    Scenario::new("deploy gov core")
        .given("party + DARs present", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                Ok(())
            })
        })
        .when(
            "member parties allocated, rights granted, configs PUT, contracts deployed",
            |f, _| {
                Box::pin(async move {
                    let party_id = f.party_id()?.to_string();

                    // Re-fetch party to get participant_uids
                    let parties: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, "/decentralized-parties").await?;
                    let party = parties
                        .parties
                        .into_iter()
                        .find(|p| p.party_id == party_id)
                        .with_context(|| format!("party {party_id} not found"))?;
                    let p1_uid = party
                        .participants
                        .first()
                        .map(|p| p.participant_uid.clone())
                        .context("party has no p1")?;
                    let p2_uid = party
                        .participants
                        .get(1)
                        .map(|p| p.participant_uid.clone())
                        .context("party has no p2")?;
                    let p3_uid = party
                        .participants
                        .get(2)
                        .map(|p| p.participant_uid.clone())
                        .context("party has no p3")?;

                    let p1m =
                        allocate_party(&*f, P1_JSON_API, "gov-member-p1", "participant-1").await?;
                    let p2m =
                        allocate_party(&*f, P2_JSON_API, "gov-member-p2", "participant-2").await?;
                    let p3m =
                        allocate_party(&*f, P3_JSON_API, "gov-member-p3", "participant-3").await?;

                    grant_rights(&*f, P1_JSON_API, &p1m, "participant-1").await?;
                    grant_rights(&*f, P2_JSON_API, &p2m, "participant-2").await?;
                    grant_rights(&*f, P3_JSON_API, &p3m, "participant-3").await?;
                    grant_rights(&*f, P1_JSON_API, &party_id, "participant-1").await?;
                    grant_rights(&*f, P2_JSON_API, &party_id, "participant-2").await?;
                    grant_rights(&*f, P3_JSON_API, &party_id, "participant-3").await?;

                    update_party_config(&*f, f.p1.http, &party_id, &p1m, "participant-1").await?;
                    update_party_config(&*f, f.p2.http, &party_id, &p2m, "participant-2").await?;
                    update_party_config(&*f, f.p3.http, &party_id, &p3m, "participant-3").await?;

                    let req = json!({
                        "decentralized_party_id": party_id,
                        "participant_ids": [p1_uid, p2_uid, p3_uid],
                        "participant_parties": [&p1m, &p2m, &p3m],
                        "operator_party": &p1m,
                        "contracts": [{
                            "id": "governance-rules",
                            "name": "GovernanceRules",
                            "package_id": "#governance-core-v0-rc3",
                            "module_name": "Governance.Rules",
                            "entity_name": "GovernanceRules",
                            "fields": [
                                {"type": "decentralized_party"},
                                {"type": "party_set", "parties": [&p1m, &p2m, &p3m]},
                                {"type": "int64", "value": 2},
                                {"type": "rel_time", "microseconds": 1800000000_i64},
                            ],
                        }],
                    });
                    let _: Value = f
                        .post_json(f.p1.http, "/contracts", &req)
                        .await
                        .context("POST /contracts")?;

                    let p2_accept = accept_invitation(&*f, f.p2.http, "participant-2", "Contracts");
                    let p3_accept = accept_invitation(&*f, f.p3.http, "participant-3", "Contracts");
                    let (r2, r3) = tokio::join!(p2_accept, p3_accept);
                    r2.context("accept Contracts on P2")?;
                    r3.context("accept Contracts on P3")?;
                    poll_workflow_status(&*f, f.p1.http, "/contracts/status", "contracts").await?;

                    f.p1_member_party = Some(p1m);
                    f.p2_member_party = Some(p2m);
                    f.p3_member_party = Some(p3m);
                    Ok(())
                })
            },
        )
        .then_eventually(
            "GovernanceRules contract visible",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    let party_id = match f.party_id() {
                        Ok(p) => p.to_string(),
                        Err(e) => return Some(Err(e)),
                    };

                    // Primary: scan /decentralized-parties for the contract.
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, "/decentralized-parties").await.ok()?;
                    let cid = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id == party_id)
                        .and_then(|p| {
                            p.contracts
                                .into_iter()
                                .find(|c| c.template_id.contains("GovernanceRules"))
                                .map(|c| c.contract_id)
                        });

                    // Fallback: /governance/state, mirroring the bash workflow's safety net.
                    // If /decentralized-parties has not yet refreshed but the governance
                    // state already exposes the contract, accept that.
                    let cid = match cid {
                        Some(c) => c,
                        None => {
                            let path = format!("/governance/state?party_id={party_id}");
                            let r: GovernanceStateLookup =
                                f.get_json(f.p1.http, &path).await.ok()?;
                            r.state?.contract_id
                        }
                    };

                    f.rules_contract_id = Some(cid);
                    Some(Ok(()))
                })
            },
        )
        .run(f)
        .await
}
