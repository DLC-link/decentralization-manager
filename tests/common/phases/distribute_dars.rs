use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    http::probe_workflow_status,
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

const DAR_FILES: &[&str] = &[
    "governance-core-v0-rc4-0.1.0.dar",
    "governance-token-custody-v0-rc4-0.1.0.dar",
    "governance-utility-onboarding-v0-rc4-0.1.0.dar",
];

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: distribute_dars");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dars_dir = Path::new(manifest_dir).join("releases/v0/rc4");

    let mut entries = Vec::with_capacity(DAR_FILES.len());
    for filename in DAR_FILES {
        let path = dars_dir.join(filename);
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("reading DAR {}", path.display()))?;
        entries.push(json!({ "filename": filename, "data": B64.encode(&bytes) }));
    }
    let req = json!({ "dar_files": entries });

    Scenario::with_ctx("distribute DARs", InvitationIds::default())
        .given("3 DAR files on disk", |_f, _| {
            Box::pin(async move { Ok(()) })
        })
        .when("P1 uploads and distributes DARs", {
            let req = req.clone();
            move |f, _| {
                let req = req.clone();
                Box::pin(async move {
                    let _: Value = f
                        .post_json(f.p1.http, "/dars/upload", &req)
                        .await
                        .context("POST /dars/upload")?;
                    let _: Value = f
                        .post_json(f.p1.http, "/dars/distribute", &req)
                        .await
                        .context("POST /dars/distribute")?;
                    Ok(())
                })
            }
        })
        .then(
            "Dars invitation visible on P2",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id = probe_pending_invitation(f, f.p2.http, "Dars").await?;
                    ctx.p2 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .then(
            "Dars invitation visible on P3",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id = probe_pending_invitation(f, f.p3.http, "Dars").await?;
                    ctx.p3 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .when("P2 + P3 accept Dars invitations", |f, ctx| {
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
                r2.context("accept Dars on P2")?;
                r3.context("accept Dars on P3")?;
                Ok(())
            })
        })
        .then(
            "dars/distribute workflow reaches completed",
            Duration::from_secs(240),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_status(
                        &*f,
                        f.p1.http,
                        "/dars/distribute/status",
                        "dars/distribute",
                    )
                    .await
                })
            },
        )
        .run(f)
        .await
}
