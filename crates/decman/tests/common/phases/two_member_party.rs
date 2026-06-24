//! Regression: a decentralized party with only **one** invited peer.
//!
//! The bug report: "if we create a dec party with 2 members it hangs forever
//! then fails" with `Coordinator stalled in step WaitingForPeers ...; peers
//! likely unreachable`. The coordinator used to derive its `WaitingForPeers`
//! wait-set from the *full configured mesh* (here P2 **and** P3) instead of
//! the invited members (P2 only), so it waited for P3 — never invited, never
//! connects — until the (since-removed) 90s staleness watchdog failed the
//! run.
//!
//! The happy-path `create_dec_party` phase can't catch this because it invites
//! P2 **and** P3, i.e. the full mesh, where invited == configured. This phase
//! invites a strict subset (just P2), reproducing the "1 coordinator + 1
//! regular peer" party from the report. It must now reach `completed`.

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    chaos::fresh_prefix,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
    types::DecentralizedPartiesResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: two_member_party");
    // Unique per run so re-running the e2e against the same localnet never
    // collides on the party prefix (the handler 409s on a duplicate prefix).
    let prefix = fresh_prefix("two-member");
    info!("Using prefix: {prefix}");

    Scenario::with_ctx(
        format!("create two-member decentralized party {prefix}"),
        InvitationIds::default(),
    )
    .when("P1 posts /onboarding inviting only P2", {
        let prefix = prefix.clone();
        move |f, _| {
            let prefix = prefix.clone();
            Box::pin(async move {
                // Only P2 is a member — P3 is in P1's network config but is
                // deliberately NOT invited. Pre-fix, the coordinator still
                // waited for P3 here and stalled.
                let req = json!({
                    "party_id_prefix": prefix,
                    "peer_ids": [&f.p2.participant_id],
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
    .when("P2 accepts the Onboarding invitation", |f, ctx| {
        Box::pin(async move {
            let p2_id = ctx
                .p2
                .as_deref()
                .context("P2 invitation id not set")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &p2_id)
                .await
                .context("accept on P2")
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
        "Onboarding completed run visible in /workflows on P2 (Peer)",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(f, f.p2.http, "Onboarding", "Peer", "completed").await
            })
        },
    )
    .then(
        "two-member party visible in /decentralized-parties",
        Duration::from_secs(30),
        {
            let prefix = prefix.clone();
            move |f, _| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, "/decentralized-parties").await.ok()?;
                    r.parties
                        .into_iter()
                        .find(|p| p.party_id.starts_with(&prefix))?;
                    Some(Ok(()))
                })
            }
        },
    )
    .run(f)
    .await
}
