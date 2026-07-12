//! Decentrally-hosted external-party onboarding phase.
//!
//! Drives `POST /external-party` on P1 naming P2 + P3 as additional hosts at a
//! 2-of-3 confirmation threshold. P2 and P3 accept the hosting invitations, then
//! each authorizes hosting on its own participant. Asserts the coordinator run
//! completes, both peer runs complete, and the party surfaces with a well-formed
//! id.

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
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: external_party");
    // Unique per run so re-running against the same localnet never collides.
    let hint = fresh_prefix("ext-party");
    info!("Using party hint: {hint}");

    Scenario::with_ctx(
        format!("onboard decentrally-hosted external party {hint} across P1+P2+P3"),
        InvitationIds::default(),
    )
    .when("P1 posts /external-party naming P2 + P3 as hosts (2-of-3)", {
        let hint = hint.clone();
        move |f, _| {
            let hint = hint.clone();
            Box::pin(async move {
                let req = json!({
                    "party_hint": hint,
                    "hosting_peers": [&f.p2.participant_id, &f.p3.participant_id],
                    "confirmation_threshold": 2,
                });
                let _: Value = f.post_json(f.p1.http, "/external-party", &req).await?;
                Ok(())
            })
        }
    })
    .then(
        "ExternalParty invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, "ExternalParty").await?;
                ctx.p2 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .then(
        "ExternalParty invitation visible on P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p3.http, "ExternalParty").await?;
                ctx.p3 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 + P3 accept ExternalParty invitations", |f, ctx| {
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
        "external-party workflow reaches completed on P1 (Coordinator)",
        Duration::from_secs(180),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(&*f, f.p1.http, "/external-party/status", "external-party")
                    .await
            })
        },
    )
    .then(
        "external-party peer run completed on P2 (host authorized)",
        Duration::from_secs(60),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(f, f.p2.http, "ExternalParty", "Peer", "completed").await
            })
        },
    )
    .then(
        "external-party peer run completed on P3 (host authorized)",
        Duration::from_secs(60),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(f, f.p3.http, "ExternalParty", "Peer", "completed").await
            })
        },
    )
    .then(
        "external-party completed run visible in /workflows on P1 (Coordinator)",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(
                    f,
                    f.p1.http,
                    "ExternalParty",
                    "Coordinator",
                    "completed",
                )
                .await
            })
        },
    )
    .then(
        "onboarded party listed on P1 with a well-formed, self-consistent id",
        Duration::from_secs(30),
        {
            let hint = hint.clone();
            move |f, _| {
                let hint = hint.clone();
                Box::pin(async move {
                    let resp: Value = f.get_json(f.p1.http, "/external-parties").await.ok()?;
                    let parties = resp.get("parties").and_then(Value::as_array)?;
                    // Find the party this run onboarded by its hint segment.
                    let party = parties.iter().find(|p| {
                        p.get("party_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| id.starts_with(&format!("{hint}::")))
                    })?;
                    let party_id = party.get("party_id").and_then(Value::as_str).unwrap_or("");
                    let fingerprint =
                        party.get("fingerprint").and_then(Value::as_str).unwrap_or("");
                    // A Canton namespace fingerprint is the `1220` SHA-256
                    // multihash prefix + 32-byte hash, hex-encoded = 68 chars.
                    if !fingerprint.starts_with("1220") || fingerprint.len() != 68 {
                        return Some(Err(anyhow::anyhow!(
                            "external party fingerprint malformed: {fingerprint}"
                        )));
                    }
                    // The listed id must be exactly `{hint}::{fingerprint}`.
                    if party_id != format!("{hint}::{fingerprint}") {
                        return Some(Err(anyhow::anyhow!(
                            "external party id/fingerprint inconsistent: {party_id} vs {fingerprint}"
                        )));
                    }
                    Some(Ok(()))
                })
            }
        },
    )
    .run(f)
    .await
}
