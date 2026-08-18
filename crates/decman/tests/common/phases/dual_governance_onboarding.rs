//! Dual-decparty utility onboarding. A provider decparty accepts a registrar
//! decparty. The registrar then provisions an instrument and credentials its
//! issuers.
//!
//! The existing suite decparty acts as the provider. It already owns the
//! `ProviderService` that `utility_onboarding` created. This phase creates a
//! second decparty as the registrar.
//!
//! A polled observation cannot return a value, because `Scenario::run` drops
//! its context. The cycles therefore pass contract ids through the fixture, the
//! way `utility_onboarding` passes its allocation factory. Only this phase
//! reads those fields.
//!
//! ## Ordering
//!
//! The phase runs after `utility_onboarding`, which sets
//! `f.provider_service_cid`.
//!
//! It must run before `contracts_quorum_completes`. That phase leaves a
//! Contracts invitation on P3 that nobody accepts. It removes that invitation
//! only if it can. This phase accepts invitations by kind.
//! `probe_pending_invitation` returns the first invitation of a kind. A stale
//! invitation would therefore get accepted instead of this phase's own. The
//! whole chaos block is unsafe for the same reason.
//!
//! It must run before `kick`. `propose_confirm_execute` has P3 execute. The
//! provider decparty therefore needs all three members.
//!
//! ## Devnet
//!
//! Each run leaves a second decparty on the network. Devnet is shared. The
//! main sequence therefore gates this phase behind `DECPM_IT_DUAL_GOV`.
//!
//! No devnet run has exercised this phase yet. The first opt-in run must
//! confirm that the participant-admin client may grant rights on a second
//! decparty.

use std::time::Duration;

use anyhow::Context;
use common::api::{
    CredentialsResponse, DecentralizedPartiesResponse, GovernanceStateResponse,
    InstrumentsResponse, ProviderConfigurationsResponse, RegistrarServiceRequestsResponse,
    RegistrarServicesResponse,
};
use common::types::InvitationType;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, TestTarget,
    chaos::fresh_prefix,
    governance::{CycleParty, propose_confirm_execute_on},
    http::probe_workflow_status,
    invitations::{post_accept_invitation, probe_pending_invitation},
    phases::deploy_gov_core::{configure_party_on_nodes, post_governance_rules},
    scenario::Scenario,
};

/// The claim the provider decparty demands of a registrar. The provider issues
/// this credential itself during OnboardRegistrar.
const REGISTRAR_CLAIM: (&str, &str) = ("role", "registrar");

/// The claim the registrar decparty demands of an instrument issuer.
const ISSUER_CLAIM: (&str, &str) = ("role", "instrument-issuer");

/// Creates the registrar decparty and makes it able to vote. The function
/// leaves `f.registrar_party_id` and `f.registrar_rules_contract_id` set.
///
/// The steps repeat what `create_dec_party` and `deploy_gov_core` also do. The
/// repetition is deliberate. These steps carry no target branching. Two other
/// phases already repeat the onboarding steps the same way. Every target
/// branch lives in the two helpers this function calls.
async fn create_registrar_decparty(f: &mut Fixture) -> anyhow::Result<()> {
    let prefix = fresh_prefix("registrar");
    info!("Registrar decparty prefix: {prefix}");

    /// The two invitation ids this scenario collects. Each round of invites
    /// reuses both fields.
    #[derive(Default)]
    struct Ctx {
        p2_invite: Option<String>,
        p3_invite: Option<String>,
    }

    Scenario::with_ctx(
        format!("create registrar decparty {prefix}"),
        Ctx::default(),
    )
    .when("P1 posts /onboarding inviting P2 and P3", {
        let prefix = prefix.clone();
        move |f, _| {
            let prefix = prefix.clone();
            Box::pin(async move {
                // Omit the threshold. The party then takes the majority
                // default of 2 of 3. That matches the provider decparty.
                // One cycle helper therefore drives both.
                let req = json!({
                    "party_id_prefix": prefix,
                    "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                });
                let _: Value = f.post_json(f.p1.http, "/onboarding", &req).await?;
                Ok(())
            })
        }
    })
    .then(
        "Onboarding invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, InvitationType::Onboarding).await?;
                ctx.p2_invite = Some(id);
                Some(Ok(()))
            })
        },
    )
    .then(
        "Onboarding invitation visible on P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p3.http, InvitationType::Onboarding).await?;
                ctx.p3_invite = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 and P3 accept the Onboarding invitations", |f, ctx| {
        Box::pin(async move {
            let p2 = ctx
                .p2_invite
                .as_deref()
                .context("P2 invitation id not set")?
                .to_string();
            let p3 = ctx
                .p3_invite
                .as_deref()
                .context("P3 invitation id not set")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &p2)
                .await
                .context("accept Onboarding on P2")?;
            post_accept_invitation(f, f.p3.http, &p3)
                .await
                .context("accept Onboarding on P3")?;
            Ok(())
        })
    })
    .then(
        "onboarding workflow reaches completed",
        Duration::from_secs(240),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(&*f, f.p1.http, "/onboarding/status", "onboarding").await
            })
        },
    )
    .then(
        "registrar decparty visible in /decentralized-parties",
        Duration::from_secs(30),
        {
            let prefix = prefix.clone();
            move |f, _| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, "/decentralized-parties").await.ok()?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.prefix == prefix)?;
                    f.registrar_party_id = Some(party.party_id.to_string());
                    Some(Ok(()))
                })
            }
        },
    )
    .when(
        "registrar decparty registered on all three nodes",
        |f, _| {
            Box::pin(async move {
                let party_id = f
                    .registrar_party_id
                    .clone()
                    .context("registrar_party_id not set")?;
                configure_party_on_nodes(&*f, &party_id).await?;
                post_governance_rules(&*f, &party_id, "governance-rules-registrar").await?;
                Ok(())
            })
        },
    )
    .then(
        "Contracts invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, InvitationType::Contracts).await?;
                ctx.p2_invite = Some(id);
                Some(Ok(()))
            })
        },
    )
    .then(
        "Contracts invitation visible on P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p3.http, InvitationType::Contracts).await?;
                ctx.p3_invite = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 and P3 accept the Contracts invitations", |f, ctx| {
        Box::pin(async move {
            let p2 = ctx
                .p2_invite
                .as_deref()
                .context("P2 invitation id not set")?
                .to_string();
            let p3 = ctx
                .p3_invite
                .as_deref()
                .context("P3 invitation id not set")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &p2)
                .await
                .context("accept Contracts on P2")?;
            post_accept_invitation(f, f.p3.http, &p3)
                .await
                .context("accept Contracts on P3")?;
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
        "registrar GovernanceRules contract visible",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                let party_id = match f.registrar_party_id.clone() {
                    Some(p) => p,
                    None => {
                        return Some(Err(anyhow::anyhow!("registrar_party_id not set")));
                    }
                };
                let path = format!("/governance/state?party_id={party_id}");
                let r: GovernanceStateResponse = f.get_json(f.p1.http, &path).await.ok()?;
                f.registrar_rules_contract_id = Some(r.state?.contract_id);
                Some(Ok(()))
            })
        },
    )
    .run(f)
    .await
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: dual_governance_onboarding");

    f.provider_service_cid
        .as_deref()
        .context("provider_service_cid not set — utility_onboarding must run first")?;

    create_registrar_decparty(f).await?;
    let registrar_party_id = f
        .registrar_party_id
        .clone()
        .context("registrar_party_id not set")?;
    let registrar_rules_contract_id = f
        .registrar_rules_contract_id
        .clone()
        .context("registrar_rules_contract_id not set")?;
    info!("Registrar decparty ready: {registrar_party_id} rules={registrar_rules_contract_id}");

    let provider_party = f.party_id()?.to_string();
    let provider_service_cid = f
        .provider_service_cid
        .clone()
        .context("provider_service_cid not set")?;

    // SetupUtility already created a provider configuration with no
    // requirements, on this same ProviderService. The endpoint reports no
    // requirement data. The two rows therefore look identical. Record the
    // existing ids, then take the one that appears.
    let configurations_before: Vec<String> = {
        let path = format!("/provider-configurations?party_id={provider_party}");
        let r: ProviderConfigurationsResponse = f.get_json(f.p1.http, &path).await?;
        r.provider_configurations
            .into_iter()
            .map(|c| c.contract_id)
            .collect()
    };

    propose_confirm_execute_on(
        CycleParty::Primary,
        "CreateProviderConfiguration",
        json!({
            "type": "create_provider_configuration",
            "provider_service_cid": &provider_service_cid,
            "registrar_requirements": [{
                "issuer": &provider_party,
                "required_claims": [
                    {"property": REGISTRAR_CLAIM.0, "value": REGISTRAR_CLAIM.1},
                ],
            }],
            "holder_requirements": [],
        }),
    )
    .run(f)
    .await?;

    Scenario::new("provider configuration created")
        .then(
            "exactly one new provider configuration",
            Duration::from_secs(30),
            {
                let before = configurations_before.clone();
                let provider_party = provider_party.clone();
                move |f, _| {
                    let before = before.clone();
                    let provider_party = provider_party.clone();
                    Box::pin(async move {
                        let path = format!("/provider-configurations?party_id={provider_party}");
                        let r: ProviderConfigurationsResponse =
                            f.get_json(f.p1.http, &path).await.ok()?;
                        let fresh: Vec<String> = r
                            .provider_configurations
                            .into_iter()
                            .map(|c| c.contract_id)
                            .filter(|cid| !before.contains(cid))
                            .collect();
                        match fresh.len() {
                            0 => None,
                            1 => {
                                f.provider_configuration_cid = Some(fresh[0].clone());
                                Some(Ok(()))
                            }
                            n => Some(Err(anyhow::anyhow!(
                                "expected 1 new provider configuration, got {n}"
                            ))),
                        }
                    })
                }
            },
        )
        .run(f)
        .await?;

    let provider_configuration_cid = f
        .provider_configuration_cid
        .clone()
        .context("provider_configuration_cid not set")?;

    // The same operator selection deploy_gov_core and utility_onboarding make.
    // Localnet has nothing that separates the operator from a member party.
    // Devnet has a real operator identity.
    let operator = match f.target {
        TestTarget::Localnet => f.p1_member_party()?.to_string(),
        TestTarget::Devnet => f
            .operator_party
            .clone()
            .context("operator_party not set on devnet")?,
    };

    let registrar_cycle = CycleParty::Named {
        party_id: registrar_party_id.clone(),
        rules_contract_id: registrar_rules_contract_id.clone(),
    };

    propose_confirm_execute_on(
        registrar_cycle.clone(),
        "CreateRegistrarServiceRequest",
        json!({
            "type": "create_registrar_service_request",
            "operator": &operator,
            "provider": &provider_party,
            "create_transfer_rule": true,
            "create_allocation_factory": true,
        }),
    )
    .run(f)
    .await?;

    Scenario::new("registrar service request visible to the provider")
        .then(
            "request listed with the right parties and flags",
            Duration::from_secs(30),
            {
                let provider_party = provider_party.clone();
                let registrar_party = registrar_party_id.clone();
                let operator = operator.clone();
                move |f, _| {
                    let provider_party = provider_party.clone();
                    let registrar_party = registrar_party.clone();
                    let operator = operator.clone();
                    Box::pin(async move {
                        let path = format!(
                            "/registrar-service-requests?party_id={provider_party}"
                        );
                        let r: RegistrarServiceRequestsResponse =
                            f.get_json(f.p1.http, &path).await.ok()?;
                        let row = r
                            .registrar_service_requests
                            .into_iter()
                            .find(|q| q.registrar.to_string() == registrar_party)?;
                        if row.provider.to_string() != provider_party {
                            return Some(Err(anyhow::anyhow!(
                                "request provider is {got}, expected {provider_party}",
                                got = row.provider
                            )));
                        }
                        if row.operator.to_string() != operator {
                            return Some(Err(anyhow::anyhow!(
                                "request operator is {got}, expected {operator}",
                                got = row.operator
                            )));
                        }
                        // The template maps each Bool to an Optional Bool.
                        // The endpoint reads an absent flag as false. Assert
                        // both. A dropped flag then fails here.
                        if !row.create_transfer_rule || !row.create_allocation_factory {
                            return Some(Err(anyhow::anyhow!(
                                "flags are transfer_rule={t} allocation_factory={a}, expected both true",
                                t = row.create_transfer_rule,
                                a = row.create_allocation_factory
                            )));
                        }
                        f.registrar_service_request_cid = Some(row.contract_id);
                        Some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    let registrar_service_request_cid = f
        .registrar_service_request_cid
        .clone()
        .context("registrar_service_request_cid not set")?;

    propose_confirm_execute_on(
        CycleParty::Primary,
        "OnboardRegistrar",
        json!({
            "type": "onboard_registrar",
            "provider_service_cid": &provider_service_cid,
            "registrar_service_request_cid": &registrar_service_request_cid,
            "provider_configuration_cid": &provider_configuration_cid,
        }),
    )
    .run(f)
    .await?;

    Scenario::new("registrar onboarded")
        .then(
            "RegistrarService exists for the new registrar",
            Duration::from_secs(30),
            {
                let provider_party = provider_party.clone();
                let registrar_party = registrar_party_id.clone();
                move |f, _| {
                    let provider_party = provider_party.clone();
                    let registrar_party = registrar_party.clone();
                    Box::pin(async move {
                        let path = format!("/services/registrar?party_id={provider_party}");
                        let r: RegistrarServicesResponse =
                            f.get_json(f.p1.http, &path).await.ok()?;
                        let row = r
                            .services
                            .into_iter()
                            .find(|s| s.registrar.to_string() == registrar_party)?;
                        f.registrar_service_cid = Some(row.contract_id);
                        Some(Ok(()))
                    })
                }
            },
        )
        .then(
            "provider minted the registrar credential",
            Duration::from_secs(30),
            {
                let provider_party = provider_party.clone();
                let registrar_party = registrar_party_id.clone();
                move |f, _| {
                    let provider_party = provider_party.clone();
                    let registrar_party = registrar_party.clone();
                    Box::pin(async move {
                        let path = format!("/credentials?party_id={provider_party}");
                        let r: CredentialsResponse = f.get_json(f.p1.http, &path).await.ok()?;
                        r.credentials
                            .iter()
                            .find(|c| {
                                c.credential_id.starts_with("registrar-credential/")
                                    && c.claims.iter().any(|claim| {
                                        claim.subject == registrar_party
                                            && claim.property == REGISTRAR_CLAIM.0
                                            && claim.value == REGISTRAR_CLAIM.1
                                    })
                            })
                            .map(|_| Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    let registrar_service_cid = f
        .registrar_service_cid
        .clone()
        .context("registrar_service_cid not set")?;
    info!(
        "Registrar onboarded: configuration={provider_configuration_cid} \
         request={registrar_service_request_cid} service={registrar_service_cid}"
    );

    // Cycle 4: the registrar decparty provisions an instrument.
    // The second member party is the initial issuer.
    // The first member party is also the localnet operator.
    // A bug could read the operator's credential by mistake.
    // The two issuers must differ.
    // Cycle 6 revokes one and checks the other.
    let instrument_id = format!("{run}-DUAL-GOV-TOKEN", run = f.run_id);
    let issuer_a = f.p2_member_party()?.to_string();
    let issuer_b = f.p3_member_party()?.to_string();

    propose_confirm_execute_on(
        registrar_cycle.clone(),
        "ProvisionInstrument",
        json!({
            "type": "provision_instrument",
            "registrar_service_cid": &registrar_service_cid,
            "instrument_id_text": &instrument_id,
            "additional_identifiers": [],
            "issuer_requirements": [{
                "issuer": &registrar_party_id,
                "required_claims": [
                    {"property": ISSUER_CLAIM.0, "value": ISSUER_CLAIM.1},
                ],
            }],
            "holder_requirements": [],
            "initial_instrument_issuers": [&issuer_a],
        }),
    )
    .run(f)
    .await?;

    // Cycle 4 builds this tag from the proposal's instrument id.
    // Cycle 5 builds it from the stored configuration's default identifier.
    // The SDK copies the instrument id into that identifier unchanged.
    // Both cycles mint credentials under one prefix.
    let credential_prefix = format!("{instrument_id}-instrument-issuer-credential/");

    Scenario::new("instrument provisioned")
        .then("instrument listed by id", Duration::from_secs(30), {
            let registrar_party = registrar_party_id.clone();
            let instrument_id = instrument_id.clone();
            move |f, _| {
                let registrar_party = registrar_party.clone();
                let instrument_id = instrument_id.clone();
                Box::pin(async move {
                    let path = format!("/instruments?party_id={registrar_party}");
                    let r: InstrumentsResponse = f.get_json(f.p1.http, &path).await.ok()?;
                    let row = r
                        .instruments
                        .into_iter()
                        .find(|i| i.instrument_id == instrument_id)?;
                    // The row's contract_id is the InstrumentConfiguration id,
                    // which cycle 5 needs.
                    f.registrar_instrument_configuration_cid = Some(row.contract_id);
                    Some(Ok(()))
                })
            }
        })
        .then(
            "initial issuer holds an issuer credential",
            Duration::from_secs(30),
            {
                let registrar_party = registrar_party_id.clone();
                let prefix = credential_prefix.clone();
                let subject = issuer_a.clone();
                move |f, _| {
                    let registrar_party = registrar_party.clone();
                    let prefix = prefix.clone();
                    let subject = subject.clone();
                    Box::pin(async move {
                        let path = format!("/credentials?party_id={registrar_party}");
                        let r: CredentialsResponse = f.get_json(f.p1.http, &path).await.ok()?;
                        r.credentials
                            .iter()
                            .find(|c| {
                                c.credential_id.starts_with(&prefix)
                                    && c.claims.iter().any(|claim| {
                                        claim.subject == subject
                                            && claim.property == ISSUER_CLAIM.0
                                            && claim.value == ISSUER_CLAIM.1
                                    })
                            })
                            .map(|_| Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    let registrar_instrument_configuration_cid =
        f.registrar_instrument_configuration_cid
            .clone()
            .context("registrar_instrument_configuration_cid not set")?;

    // Cycle 5: the registrar decparty onboards the second member party.
    // This party becomes an additional instrument issuer.
    propose_confirm_execute_on(
        registrar_cycle.clone(),
        "OnboardInstrumentIssuers",
        json!({
            "type": "onboard_instrument_issuers",
            "instrument_configuration_cid": &registrar_instrument_configuration_cid,
            "instrument_issuers": [&issuer_b],
        }),
    )
    .run(f)
    .await?;

    // This probe captures the second issuer's credential ids.
    // Cycle 6 revokes exactly these ids.
    Scenario::new("second issuer onboarded")
        .then(
            "second issuer holds an issuer credential",
            Duration::from_secs(30),
            {
                let registrar_party = registrar_party_id.clone();
                let prefix = credential_prefix.clone();
                let subject = issuer_b.clone();
                move |f, _| {
                    let registrar_party = registrar_party.clone();
                    let prefix = prefix.clone();
                    let subject = subject.clone();
                    Box::pin(async move {
                        let path = format!("/credentials?party_id={registrar_party}");
                        let r: CredentialsResponse = f.get_json(f.p1.http, &path).await.ok()?;
                        let cids: Vec<String> = r
                            .credentials
                            .into_iter()
                            .filter(|c| {
                                c.credential_id.starts_with(&prefix)
                                    && c.claims.iter().any(|claim| {
                                        claim.subject == subject
                                            && claim.property == ISSUER_CLAIM.0
                                            && claim.value == ISSUER_CLAIM.1
                                    })
                            })
                            .map(|c| c.contract_id)
                            .collect();
                        if cids.is_empty() {
                            return None;
                        }
                        f.registrar_issuer_credential_cids = cids;
                        Some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    let revoke_cids = f.registrar_issuer_credential_cids.clone();
    anyhow::ensure!(
        !revoke_cids.is_empty(),
        "no credential captured for the second issuer"
    );

    // Cycle 6: the registrar decparty offboards the second issuer.
    // The payload groups credential ids under their issuer.
    // InstrumentIssuerCredentials carries instrument_issuer and credential_cids.
    propose_confirm_execute_on(
        registrar_cycle,
        "OffboardInstrumentIssuers",
        json!({
            "type": "offboard_instrument_issuers",
            "instrument_issuers": [{
                "instrument_issuer": &issuer_b,
                "credential_cids": &revoke_cids,
            }],
        }),
    )
    .run(f)
    .await?;

    // This probe checks two conditions.
    // The revoked credential ids are gone.
    // The first issuer's credential still exists.
    // The second check makes this cycle discriminating.
    // A bug that revokes every credential could not pass this check.
    Scenario::new("second issuer offboarded")
        .then(
            "revoked credentials gone, first issuer keeps its own",
            Duration::from_secs(30),
            {
                let registrar_party = registrar_party_id.clone();
                let prefix = credential_prefix.clone();
                let kept_subject = issuer_a.clone();
                let revoked = revoke_cids.clone();
                move |f, _| {
                    let registrar_party = registrar_party.clone();
                    let prefix = prefix.clone();
                    let kept_subject = kept_subject.clone();
                    let revoked = revoked.clone();
                    Box::pin(async move {
                        let path = format!("/credentials?party_id={registrar_party}");
                        let r: CredentialsResponse = f.get_json(f.p1.http, &path).await.ok()?;
                        let still_there: Vec<&String> = r
                            .credentials
                            .iter()
                            .map(|c| &c.contract_id)
                            .filter(|cid| revoked.contains(cid))
                            .collect();
                        if !still_there.is_empty() {
                            return None;
                        }
                        let kept = r.credentials.iter().any(|c| {
                            c.credential_id.starts_with(&prefix)
                                && c.claims.iter().any(|claim| {
                                    claim.subject == kept_subject
                                        && claim.property == ISSUER_CLAIM.0
                                        && claim.value == ISSUER_CLAIM.1
                                })
                        });
                        if !kept {
                            return Some(Err(anyhow::anyhow!(
                                "offboard revoked the first issuer's credential too"
                            )));
                        }
                        Some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    Ok(())
}
