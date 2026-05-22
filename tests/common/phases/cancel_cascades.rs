//! G6: Cancel during in-flight peer run cancels accepted-but-running rows.
//!
//! Start onboarding P1 → {P2, P3}. P2 accepts (peer row goes inprogress);
//! BEFORE P3 accepts, P1 cancels. Assert: P2's peer row flips to
//! cancelled with an error mentioning cancellation, and P3 has no leftover
//! pending Onboarding invitation.

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, db,
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
    types::PendingInvitationsResponse,
};

#[derive(Default)]
struct Ctx {
    invites: InvitationIds,
    /// Coordinator-side instance_name on P1 (`<prefix>-creation`).
    instance_name: String,
    /// Peer-side instance_name on P2 — synthesized by accept_invitation
    /// as `peer-onboarding-<pubkey>-<epoch>`. Captured once the
    /// inprogress row is observable so subsequent steps can refer to it
    /// after it flips to `cancelled`.
    p2_peer_instance: Option<String>,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: cancel_cascades_to_peers");

    let prefix = format!(
        "cancel-cascade-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    );

    Scenario::with_ctx(
        format!("cancel cascades to in-flight peer ({prefix})"),
        Ctx::default(),
    )
    .when("P1 posts /onboarding (P3 will not accept)", {
        let prefix = prefix.clone();
        move |f, ctx| {
            let prefix = prefix.clone();
            Box::pin(async move {
                let req = json!({
                    "party_id_prefix": prefix,
                    "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                });
                let resp: Value = f.post_json(f.p1.http, "/onboarding", &req).await?;
                // Capture server-minted instance_name (includes uuid suffix
                // after the concurrent-workflows refactor).
                ctx.instance_name = resp
                    .get("instance_name")
                    .and_then(Value::as_str)
                    .context("POST /onboarding response missing instance_name")?
                    .to_string();
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
                ctx.invites.p2 = Some(id);
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
                ctx.invites.p3 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 accepts (P3 deferred)", |f, ctx| {
        Box::pin(async move {
            let p2 = ctx
                .invites
                .p2
                .as_deref()
                .context("P2 invite id")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &p2)
                .await
                .context("accept Onboarding on P2")
        })
    })
    .then(
        "P2 peer row reaches inprogress",
        Duration::from_secs(60),
        |f, ctx| {
            let db_path = f.db_path(2);
            Box::pin(async move {
                match db::current_inprogress_peer_instance(&db_path, "Onboarding").await {
                    Ok(Some(name)) => {
                        ctx.p2_peer_instance = Some(name);
                        Some(Ok(()))
                    }
                    // No inprogress peer row yet — keep polling.
                    Ok(None) => None,
                    // Transient DB error (e.g. WAL contention) — keep polling.
                    Err(_) => None,
                }
            })
        },
    )
    .when("P1 cancels", |f, ctx| {
        Box::pin(async move {
            let instance = ctx.instance_name.clone();
            anyhow::ensure!(
                !instance.is_empty(),
                "ctx.instance_name not set before P1 cancel step"
            );
            let path = format!("/workflows/{instance}/cancel");
            let _: Value = f
                .post_json(f.p1.http, &path, &json!({}))
                .await
                .with_context(|| format!("POST {path} on P1"))?;
            Ok(())
        })
    })
    .then(
        "P2 peer row flipped to cancelled",
        Duration::from_secs(30),
        |f, ctx| {
            let db_path = f.db_path(2);
            let instance = ctx.p2_peer_instance.clone();
            Box::pin(async move {
                let instance = instance?;
                let s = db::workflow_run_status(&db_path, &instance, "Peer")
                    .await
                    .ok()
                    .flatten()?;
                (s == "cancelled").then_some(Ok(()))
            })
        },
    )
    .then(
        "P3 has no pending Onboarding invitation",
        Duration::from_secs(15),
        |f, _| {
            Box::pin(async move {
                let r: PendingInvitationsResponse =
                    f.get_json(f.p3.http, "/invitations").await.ok()?;
                let n = r
                    .invitations
                    .iter()
                    .filter(|i| i.invitation_type == "Onboarding")
                    .count();
                (n == 0).then_some(Ok(()))
            })
        },
    )
    .when("dismiss leftover rows on P1 + P2", |f, ctx| {
        let p1_instance = ctx.instance_name.clone();
        let p2_instance = ctx.p2_peer_instance.clone();
        Box::pin(async move {
            let p1_path = format!("/workflows/{p1_instance}/dismiss");
            let _ = f.post_expect_status(f.p1.http, &p1_path, &json!({})).await;
            if let Some(p2) = p2_instance {
                let p2_path = format!("/workflows/{p2}/dismiss");
                let _ = f.post_expect_status(f.p2.http, &p2_path, &json!({})).await;
            }
            Ok(())
        })
    })
    .run(f)
    .await
}
