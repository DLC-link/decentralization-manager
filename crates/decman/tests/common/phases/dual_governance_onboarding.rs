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

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    chaos::fresh_prefix,
    http::probe_workflow_status,
    invitations::{post_accept_invitation, probe_pending_invitation},
    phases::deploy_gov_core::{configure_party_on_nodes, post_governance_rules},
    scenario::Scenario,
    types::{DecentralizedPartiesResponse, GovernanceStateLookup},
};

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
                let id = probe_pending_invitation(f, f.p2.http, "Onboarding").await?;
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
                let id = probe_pending_invitation(f, f.p3.http, "Onboarding").await?;
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
                        .find(|p| p.party_id.starts_with(&prefix))?;
                    f.registrar_party_id = Some(party.party_id);
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
                let id = probe_pending_invitation(f, f.p2.http, "Contracts").await?;
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
                let id = probe_pending_invitation(f, f.p3.http, "Contracts").await?;
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
                let r: GovernanceStateLookup = f.get_json(f.p1.http, &path).await.ok()?;
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

    Ok(())
}
