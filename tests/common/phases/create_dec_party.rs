use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
    types::DecentralizedPartiesResponse,
};

async fn next_test_network_prefix(f: &Fixture) -> anyhow::Result<String> {
    let r: DecentralizedPartiesResponse = f
        .get_json(f.p1.http, "/decentralized-parties")
        .await
        .context("listing parties")?;
    let max = r
        .parties
        .iter()
        .filter_map(|p| {
            let id = p.party_id.split("::").next()?;
            let n = id.strip_prefix("test-network-")?;
            n.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    Ok(format!("test-network-{}", max + 1))
}

async fn ensure_no_party_with_prefix(f: &Fixture, prefix: &str) -> anyhow::Result<()> {
    let r: DecentralizedPartiesResponse = f.get_json(f.p1.http, "/decentralized-parties").await?;
    if r.parties.iter().any(|p| p.party_id.starts_with(prefix)) {
        anyhow::bail!("party with prefix {prefix} already exists");
    }
    Ok(())
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: create_dec_party");
    let prefix = next_test_network_prefix(f).await?;
    info!("Using prefix: {prefix}");

    Scenario::with_ctx(
        format!("create decentralized party {prefix}"),
        InvitationIds::default(),
    )
    .given("no party at this prefix yet", {
        let prefix = prefix.clone();
        move |f, _| {
            let prefix = prefix.clone();
            Box::pin(async move { ensure_no_party_with_prefix(f, &prefix).await })
        }
    })
    .when("P1 posts /onboarding", {
        let prefix = prefix.clone();
        move |f, _| {
            let prefix = prefix.clone();
            Box::pin(async move {
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
                ctx.p2 = Some(id);
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
                ctx.p3 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 + P3 accept Onboarding invitations", |f, ctx| {
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
            r2.context("accept on P2")?;
            r3.context("accept on P3")?;
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
        "Onboarding completed run visible in /workflows on P1 (Coordinator)",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(f, f.p1.http, "Onboarding", "Coordinator", "completed")
                    .await
            })
        },
    )
    .then(
        "Onboarding completed run visible in /workflows on P2 (Attestor)",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(f, f.p2.http, "Onboarding", "Attestor", "completed")
                    .await
            })
        },
    )
    .then(
        "Onboarding completed run visible in /workflows on P3 (Attestor)",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(f, f.p3.http, "Onboarding", "Attestor", "completed")
                    .await
            })
        },
    )
    .then(
        "party visible in /decentralized-parties",
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
                    f.party_id = Some(party.party_id);
                    f.party_prefix = Some(prefix.clone());
                    Some(Ok(()))
                })
            }
        },
    )
    .run(f)
    .await
}
