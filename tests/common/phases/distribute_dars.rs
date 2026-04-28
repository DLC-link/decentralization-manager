use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, http::poll_workflow_status, invitations::accept_invitation, scenario::Scenario,
};

const DAR_FILES: &[&str] = &[
    "governance-core-v0-rc3-0.1.0.dar",
    "governance-token-custody-v0-rc3-0.1.0.dar",
    "governance-utility-onboarding-v0-rc3-0.1.0.dar",
];

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: distribute_dars");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dars_dir = std::path::Path::new(manifest_dir).join("releases/v0/rc3");

    let mut entries = Vec::with_capacity(DAR_FILES.len());
    for filename in DAR_FILES {
        let path = dars_dir.join(filename);
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("reading DAR {}", path.display()))?;
        entries.push(json!({ "filename": filename, "data": B64.encode(&bytes) }));
    }
    let req = json!({ "dar_files": entries });

    Scenario::new("distribute DARs")
        .given("3 DAR files on disk", |_f, _| {
            Box::pin(async move { Ok(()) })
        })
        .when(
            "P1 uploads + distributes DARs, P2/P3 accept, status reaches completed",
            {
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
                        let p2_accept = accept_invitation(&*f, f.p2.http, "participant-2", "Dars");
                        let p3_accept = accept_invitation(&*f, f.p3.http, "participant-3", "Dars");
                        let (r2, r3) = tokio::join!(p2_accept, p3_accept);
                        r2.context("accept Dars on P2")?;
                        r3.context("accept Dars on P3")?;
                        poll_workflow_status(
                            &*f,
                            f.p1.http,
                            "/dars/distribute/status",
                            "dars/distribute",
                        )
                        .await
                    })
                }
            },
        )
        .run(f)
        .await
}
