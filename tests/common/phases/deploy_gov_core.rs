use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    MemberCreds,
    TestTarget,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
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
        .post_json(port, "/v2/parties", &req)
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
        .post_json(port, "/v2/users/ledger-api-user/rights", &req)
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
    member_creds: Option<&MemberCreds>,
) -> anyhow::Result<()> {
    let (user_id, keycloak_url, keycloak_realm, keycloak_client_id, keycloak_client_secret) =
        if let Some(c) = member_creds {
            (
                c.user_id.clone(),
                std::env::var("DECPM_KEYCLOAK_URL")
                    .context("DECPM_KEYCLOAK_URL not set on devnet")?,
                std::env::var("DECPM_KEYCLOAK_REALM")
                    .context("DECPM_KEYCLOAK_REALM not set on devnet")?,
                c.keycloak_client_id.clone(),
                Some(c.keycloak_client_secret.clone()),
            )
        } else {
            (
                "ledger-api-user".to_string(),
                String::new(),
                String::new(),
                String::new(),
                None,
            )
        };

    let mut req = json!({
        "dec_party_id": party_id,
        "member_party_id": member,
        "user_id": user_id,
        "keycloak_url": keycloak_url,
        "keycloak_realm": keycloak_realm,
        "keycloak_client_id": keycloak_client_id,
        "packages": {
            "governance_action": "#governance-action-v0",
            "governance_core": "#governance-core-v0",
            "governance_token_custody": "#governance-token-custody-v0",
            "governance_utility_onboarding": "#governance-utility-onboarding-v0",
            "utility_registry": "#utility-registry-app-v0",
        },
    });
    if let Some(secret) = keycloak_client_secret {
        req["keycloak_client_secret"] = serde_json::Value::String(secret);
    }

    let _: Value = f
        .put_json(port, "/party-config", &req)
        .await
        .with_context(|| format!("PUT /party-config on {name}"))?;
    Ok(())
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: deploy_gov_core");

    Scenario::with_ctx("deploy gov core", InvitationIds::default())
        .given("party + DARs present", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                Ok(())
            })
        })
        .when(
            "member parties allocated, rights granted, configs PUT, contracts posted",
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

                    // On localnet we allocate fresh gov-member-pN parties on each
                    // participant's JSON Ledger API and grant ledger-api-user the
                    // rights to act/read as them (and as the decentralized party).
                    // On devnet the member parties already exist with their rights
                    // managed via Keycloak/IDP — see development/remote/participant-N/.env
                    // for the P{N}_MEMBER_PARTY_ID values. We reuse those directly
                    // and skip both the JSON-API allocate and the ledger-api-user
                    // grants (the JSON Ledger API isn't tunneled on devnet, and
                    // ledger-api-user doesn't exist there anyway).
                    let (p1m, p2m, p3m) = match f.target {
                        TestTarget::Localnet => {
                            let p1m = allocate_party(&*f, P1_JSON_API, "gov-member-p1", "participant-1").await?;
                            let p2m = allocate_party(&*f, P2_JSON_API, "gov-member-p2", "participant-2").await?;
                            let p3m = allocate_party(&*f, P3_JSON_API, "gov-member-p3", "participant-3").await?;
                            grant_rights(&*f, P1_JSON_API, &p1m, "participant-1").await?;
                            grant_rights(&*f, P2_JSON_API, &p2m, "participant-2").await?;
                            grant_rights(&*f, P3_JSON_API, &p3m, "participant-3").await?;
                            grant_rights(&*f, P1_JSON_API, &party_id, "participant-1").await?;
                            grant_rights(&*f, P2_JSON_API, &party_id, "participant-2").await?;
                            grant_rights(&*f, P3_JSON_API, &party_id, "participant-3").await?;
                            (p1m, p2m, p3m)
                        }
                        TestTarget::Devnet => {
                            let p1m = f.p1_member_creds.as_ref()
                                .context("P1 member creds missing on devnet")?.party_id.clone();
                            let p2m = f.p2_member_creds.as_ref()
                                .context("P2 member creds missing on devnet")?.party_id.clone();
                            let p3m = f.p3_member_creds.as_ref()
                                .context("P3 member creds missing on devnet")?.party_id.clone();
                            (p1m, p2m, p3m)
                        }
                    };

                    update_party_config(&*f, f.p1.http, &party_id, &p1m, "participant-1", f.p1_member_creds.as_ref()).await?;
                    update_party_config(&*f, f.p2.http, &party_id, &p2m, "participant-2", f.p2_member_creds.as_ref()).await?;
                    update_party_config(&*f, f.p3.http, &party_id, &p3m, "participant-3", f.p3_member_creds.as_ref()).await?;

                    let req = json!({
                        "decentralized_party_id": party_id,
                        "participant_ids": [p1_uid, p2_uid, p3_uid],
                        "participant_parties": [&p1m, &p2m, &p3m],
                        "operator_party": &p1m,
                        "contracts": [{
                            "id": "governance-rules",
                            "name": "GovernanceRules",
                            "package_id": "#governance-core-v0",
                            "module_name": "Governance.Rules",
                            "entity_name": "GovernanceRules",
                            "fields": [
                                {"type": "decentralized_party"},
                                {"type": "party_set", "parties": [&p1m, &p2m, &p3m]},
                                {"type": "int64", "value": 2},
                                {"type": "rel_time", "microseconds": 1800000000_i64},
                                {"type": "none"},
                            ],
                        }],
                    });
                    let _: Value = f
                        .post_json(f.p1.http, "/contracts", &req)
                        .await
                        .context("POST /contracts")?;

                    f.p1_member_party = Some(p1m);
                    f.p2_member_party = Some(p2m);
                    f.p3_member_party = Some(p3m);
                    Ok(())
                })
            },
        )
        .then(
            "Contracts invitation visible on P2",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id = probe_pending_invitation(f, f.p2.http, "Contracts").await?;
                    ctx.p2 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .then(
            "Contracts invitation visible on P3",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id = probe_pending_invitation(f, f.p3.http, "Contracts").await?;
                    ctx.p3 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .when("P2 + P3 accept Contracts invitations", |f, ctx| {
            Box::pin(async move {
                let p2_id = ctx
                    .p2
                    .as_deref()
                    .context("P2 invitation id not set")?
                    .to_string();
                let p3_id = ctx
                    .p3
                    .as_deref()
                    .context("P3 invitation id not set")?
                    .to_string();
                let p2_accept = post_accept_invitation(f, f.p2.http, &p2_id);
                let p3_accept = post_accept_invitation(f, f.p3.http, &p3_id);
                let (r2, r3) = tokio::join!(p2_accept, p3_accept);
                r2.context("accept Contracts on P2")?;
                r3.context("accept Contracts on P3")?;
                Ok(())
            })
        })
        .then(
            "contracts workflow reaches completed",
            Duration::from_secs(240),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_status(&*f, f.p1.http, "/contracts/status", "contracts").await
                })
            },
        )
        .then(
            "Contracts completed run visible in /workflows on P1 (Coordinator)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(
                        f,
                        f.p1.http,
                        "Contracts",
                        "Coordinator",
                        "completed",
                    )
                    .await
                })
            },
        )
        .then(
            "Contracts completed run visible in /workflows on P2 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p2.http, "Contracts", "Peer", "completed").await
                })
            },
        )
        .then(
            "Contracts completed run visible in /workflows on P3 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p3.http, "Contracts", "Peer", "completed").await
                })
            },
        )
        .then(
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
