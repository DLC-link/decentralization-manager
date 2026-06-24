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

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: kick");

    Scenario::with_ctx("kick participant-3", InvitationIds::default())
        .given("party + member parties present", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                f.party_prefix()?;
                Ok(())
            })
        })
        .then(
            "P3 owner_key resolved via Noise",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let prefix = f.party_prefix().ok()?.to_string();
                    let p3_uid = f.p3.participant_id.clone();
                    let path = format!("/decentralized-parties?prefix={prefix}");
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, &path).await.ok()?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.starts_with(&prefix))?;
                    let pi = party
                        .participants
                        .into_iter()
                        .find(|p| p.participant_uid == p3_uid)?;
                    pi.owner_key.map(|_| Ok(()))
                })
            },
        )
        .when("P1 posts /kick", |f, _| {
            Box::pin(async move {
                let party_id = f.party_id()?.to_string();
                let p3_uid = f.p3.participant_id.clone();

                // The server derives `namespace_fingerprint` from its
                // participant cache; the THEN above already proves it has
                // resolved P3's owner_key, so /kick won't 409.
                let req = json!({
                    "decentralized_party_id": party_id,
                    "participant_id": p3_uid,
                    "new_threshold": 2_i64,
                });
                let _: Value = f
                    .post_json(f.p1.http, "/kick", &req)
                    .await
                    .context("POST /kick")?;
                Ok(())
            })
        })
        .then(
            "Kick invitation visible on P2",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id = probe_pending_invitation(f, f.p2.http, "Kick").await?;
                    ctx.p2 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .when("P2 accepts Kick invitation", |f, ctx| {
            Box::pin(async move {
                let id = ctx
                    .p2
                    .as_deref()
                    .context("P2 invitation id not set")?
                    .to_string();
                post_accept_invitation(f, f.p2.http, &id)
                    .await
                    .context("accept Kick on P2")
            })
        })
        .then(
            "kick workflow reaches completed",
            Duration::from_secs(240),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_status(&*f, f.p1.http, "/kick/status", "kick").await
                })
            },
        )
        .then(
            "Kick completed run visible in /workflows on P1 (Coordinator)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p1.http, "Kick", "Coordinator", "completed")
                        .await
                })
            },
        )
        .then(
            "Kick completed run visible in /workflows on P2 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p2.http, "Kick", "Peer", "completed").await
                })
            },
        )
        .then(
            "kick removed P3 from the party and lowered the threshold",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let prefix = f.party_prefix().ok()?.to_string();
                    let p3_uid = f.p3.participant_id.clone();
                    // `refresh=true` forces a fresh Canton fetch so we assert
                    // the real topology, not the up-to-60s-stale cache that
                    // would still list P3.
                    let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, &path).await.ok()?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.starts_with(&prefix))?;
                    let p3_present = party
                        .participants
                        .iter()
                        .any(|p| p.participant_uid == p3_uid);
                    // Retry until the topology change has propagated (the kick
                    // already reached `completed` above, so this converges
                    // promptly); a lingering P3 or wrong threshold then surfaces
                    // as a timeout.
                    if p3_present || party.threshold != 2 {
                        return None;
                    }
                    Some(Ok(()))
                })
            },
        )
        .run(f)
        .await
}
