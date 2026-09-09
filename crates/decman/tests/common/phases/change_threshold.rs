use std::time::Duration;

use anyhow::Context;
use common::{
    api::DecentralizedPartiesResponse,
    types::{InvitationType, WorkflowKind, WorkflowProgress, WorkflowRole},
};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

/// Change the party's threshold, then put it back.
///
/// Runs directly after `add_party`, so the party is known to hold P1 + P2 +
/// P3 at threshold 2. It goes down to 1 and back to 2, which leaves the
/// party exactly as the later phases expect to find it. Lowering first is
/// deliberate: the round that restores 2 then only needs the single
/// signature the lowered threshold asks for, so neither round depends on
/// full-threshold authorization.
///
/// `PartyToParticipant` carries two thresholds — how many hosting
/// participants must confirm, and how many of the party's signing keys
/// authorize a transaction for it — and this workflow exists to move the
/// threshold, so it has to move both. A run that moves only the hosting one
/// leaves the signing threshold at its old value, and every peer refuses to
/// sign a proposal whose threshold is not the one its operator accepted. The
/// workflow then never completes, which is what #428 hit on devnet.
pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: change_threshold");

    round("lower the threshold to 1", 2, 1).run(f).await?;
    round("restore the threshold to 2", 1, 2).run(f).await?;
    Ok(())
}

/// One threshold change, start to landed: post it, have both other members
/// accept, and assert the party ends up on `to`.
fn round(name: &'static str, from: i64, to: i64) -> Scenario<InvitationIds> {
    Scenario::with_ctx(name, InvitationIds::default())
        .given("party present with all three members", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                f.party_prefix()?;
                Ok(())
            })
        })
        .when("P1 posts /change-threshold", move |f, _| {
            Box::pin(async move {
                let req = json!({
                    "decentralized_party_id": f.party_id()?.to_string(),
                    "new_threshold": to,
                    "previous_threshold": from,
                });
                let _: Value = f
                    .post_json(f.p1.http, "/change-threshold", &req)
                    .await
                    .context("POST /change-threshold")?;
                Ok(())
            })
        })
        .then(
            "ChangeThreshold invitation visible on P2",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id =
                        probe_pending_invitation(f, f.p2.http, InvitationType::ChangeThreshold)
                            .await?;
                    ctx.p2 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .when("P2 accepts ChangeThreshold invitation", |f, ctx| {
            Box::pin(async move {
                let id = ctx
                    .p2
                    .as_deref()
                    .context("P2 invitation id not set")?
                    .to_string();
                post_accept_invitation(f, f.p2.http, &id)
                    .await
                    .context("accept ChangeThreshold on P2")
            })
        })
        .then(
            "ChangeThreshold invitation visible on P3",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id =
                        probe_pending_invitation(f, f.p3.http, InvitationType::ChangeThreshold)
                            .await?;
                    ctx.p3 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .when("P3 accepts ChangeThreshold invitation", |f, ctx| {
            Box::pin(async move {
                let id = ctx
                    .p3
                    .as_deref()
                    .context("P3 invitation id not set")?
                    .to_string();
                post_accept_invitation(f, f.p3.http, &id)
                    .await
                    .context("accept ChangeThreshold on P3")
            })
        })
        .then(
            "change-threshold workflow reaches completed",
            Duration::from_secs(240),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_status(
                        &*f,
                        f.p1.http,
                        "/change-threshold/status",
                        "change-threshold",
                    )
                    .await
                })
            },
        )
        .then(
            "ChangeThreshold completed run visible in /workflows on P2 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(
                        f,
                        f.p2.http,
                        WorkflowKind::ChangeThreshold,
                        WorkflowRole::Peer,
                        WorkflowProgress::Completed,
                    )
                    .await
                })
            },
        )
        .then(
            "the party's threshold is the new one",
            Duration::from_secs(60),
            move |f, _| {
                Box::pin(async move {
                    let prefix = match f.party_prefix() {
                        Ok(v) => v.to_string(),
                        Err(e) => return Some(Err(e)),
                    };
                    // `refresh=true` forces a fresh Canton fetch, so this
                    // asserts the real topology rather than the up-to-60s
                    // stale cache that would still carry the old threshold.
                    let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
                    let r: DecentralizedPartiesResponse =
                        f.probe_get_json(f.p1.http, &path).await?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.prefix == prefix)?;
                    // Retry until the topology change has propagated; a
                    // threshold stuck on the old value surfaces as a timeout.
                    if i64::from(party.threshold) != to {
                        return None;
                    }
                    Some(Ok(()))
                })
            },
        )
}
