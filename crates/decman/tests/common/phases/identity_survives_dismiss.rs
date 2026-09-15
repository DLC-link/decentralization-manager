//! G5: a party's key cache survives onboarding completion + dismiss.
//!
//! Run a fresh onboarding to completion. Snapshot the `dec_party_participant`
//! rows that carry a key for the new party. Dismiss the onboarding
//! workflow_runs row. Re-read and assert the keys are preserved.
//!
//! Those rows are what a later kick reads to attribute an owner key to a
//! participant, and they outlive the run that created them. Dismissing a run
//! clears what the run owns and must not touch them.

use std::time::Duration;

use anyhow::Context;
use common::{
    api::DecentralizedPartiesResponse,
    types::{InvitationType, WorkflowKind, WorkflowProgress, WorkflowRole},
};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, db,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

#[derive(Default)]
struct Ctx {
    invites: InvitationIds,
    instance_name: String,
    dec_party_id: Option<String>,
    identity_before: i64,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: identity_survives_dismiss");

    let prefix = format!(
        "identity-keep-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    );
    let instance_name = format!("{prefix}-creation");

    Scenario::with_ctx(
        format!("party key cache preserved across dismiss ({prefix})"),
        Ctx {
            instance_name: instance_name.clone(),
            ..Default::default()
        },
    )
    .when("P1 posts /onboarding", {
        let prefix = prefix.clone();
        move |f, _ctx| {
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
                let id = probe_pending_invitation(f, f.p2.http, InvitationType::Onboarding).await?;
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
                let id = probe_pending_invitation(f, f.p3.http, InvitationType::Onboarding).await?;
                ctx.invites.p3 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 + P3 accept", |f, ctx| {
        Box::pin(async move {
            let p2 = ctx
                .invites
                .p2
                .as_deref()
                .context("P2 invite id")?
                .to_string();
            let p3 = ctx
                .invites
                .p3
                .as_deref()
                .context("P3 invite id")?
                .to_string();
            let r2 = post_accept_invitation(f, f.p2.http, &p2);
            let r3 = post_accept_invitation(f, f.p3.http, &p3);
            let (a, b) = tokio::join!(r2, r3);
            a.context("accept on P2")?;
            b.context("accept on P3")?;
            Ok(())
        })
    })
    .then(
        "onboarding workflow reaches completed",
        Duration::from_secs(240),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(f, f.p1.http, "/onboarding/status", "onboarding").await
            })
        },
    )
    .then(
        "Onboarding completed run visible on P1",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(
                    f,
                    f.p1.http,
                    WorkflowKind::Onboarding,
                    WorkflowRole::Coordinator,
                    WorkflowProgress::Completed,
                )
                .await
            })
        },
    )
    .then(
        "dec_party_id resolvable for the new prefix",
        Duration::from_secs(30),
        {
            let prefix = prefix.clone();
            move |f, ctx| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let path = format!("/decentralized-parties?prefix={prefix}");
                    let r: DecentralizedPartiesResponse =
                        f.probe_get_json(f.p1.http, &path).await?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.prefix == prefix)?;
                    ctx.dec_party_id = Some(party.party_id.to_string());
                    Some(Ok(()))
                })
            }
        },
    )
    .given(
        "snapshot the party's keyed participant rows pre-dismiss",
        |f, ctx| {
            let db_path = f.db_path(1);
            Box::pin(async move {
                let dec_party_id = ctx
                    .dec_party_id
                    .as_deref()
                    .context("dec_party_id not set")?
                    .to_string();
                let n = db::count_dec_party_participant_keys(&db_path, &dec_party_id).await?;
                anyhow::ensure!(
                    n >= 1,
                    "expected 1 or more keyed participant rows for {dec_party_id}, got {n}"
                );
                ctx.identity_before = n;
                info!("[G5] {n} keyed dec_party_participant rows pre-dismiss");
                Ok(())
            })
        },
    )
    .when("P1 dismisses the onboarding run", |f, ctx| {
        let instance = ctx.instance_name.clone();
        Box::pin(async move {
            let path = format!("/workflows/{instance}/dismiss");
            let _: Value = f.post_json(f.p1.http, &path, &json!({})).await?;
            Ok(())
        })
    })
    .then(
        "the party's key row count is unchanged",
        Duration::from_secs(15),
        |f, ctx| {
            let db_path = f.db_path(1);
            let diag = f.probe_diag.clone();
            Box::pin(async move {
                let dec_party_id = ctx.dec_party_id.as_deref()?.to_string();
                let after = diag.ok(
                    "count keyed dec_party_participant rows",
                    db::count_dec_party_participant_keys(&db_path, &dec_party_id).await,
                )?;
                if after != ctx.identity_before {
                    return Some(Err(anyhow::anyhow!(
                        "the party's key rows changed across dismiss ({} -> {})",
                        ctx.identity_before,
                        after
                    )));
                }
                info!("[G5] party key cache preserved across dismiss ({after} rows)");
                Some(Ok(()))
            })
        },
    )
    .run(f)
    .await
}
