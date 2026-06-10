use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

const DAR_FILES: &[&str] = &[
    "governance-action-v1-0.1.0.dar",
    "governance-core-v1-0.1.0.dar",
    "governance-token-custody-v1-0.1.0.dar",
    "governance-utility-onboarding-v1-0.1.0.dar",
];

/// Map of expected package_id → (module_name, entity_name) of a well-known
/// template inside it. A successful `/contracts/query` (200 with `contracts: []`
/// or `contracts: [...]`) means the package is installed; an error response
/// means it's missing.
///
/// `governance-action` is intentionally absent: it's a library-only package
/// (exports the `GovernableAction` interface, no templates), so the
/// "probe a known template" pattern doesn't apply. It's a transitive
/// build-time dependency of the other three packages — if those install,
/// `governance-action` necessarily installs too.
const DAR_PROBES: &[(&str, &str, &str)] = &[
    (
        "%23governance-core-v1",
        "Governance.Rules",
        "GovernanceRules",
    ),
    (
        "%23governance-token-custody-v1",
        "Governance.TokenCustody.SetupTokenPreapproval",
        "SetupTokenPreapproval",
    ),
    (
        "%23governance-utility-onboarding-v1",
        "Governance.UtilityOnboarding.SetupUtility",
        "SetupUtility",
    ),
];

async fn dar_present_on(f: &Fixture, port: u16, pkg: &str, module: &str, entity: &str) -> bool {
    let party_id = match f.party_id() {
        Ok(p) => p.to_string(),
        Err(_) => return false,
    };
    let path = format!(
        "/contracts/query?party_id={party_id}&package_id={pkg}\
         &module_name={module}&entity_name={entity}"
    );
    f.get_json::<serde_json::Value>(port, &path).await.is_ok()
}

async fn all_dars_present(f: &Fixture) -> bool {
    for (pkg, module, entity) in DAR_PROBES {
        for port in [f.p1.http, f.p2.http, f.p3.http] {
            if !dar_present_on(f, port, pkg, module, entity).await {
                return false;
            }
        }
    }
    true
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: distribute_dars");
    if all_dars_present(f).await {
        info!("All expected DARs already present on all participants; skipping distribute_dars");
        return Ok(());
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dars_dir = Path::new(manifest_dir).join("releases/v0/release");

    let mut entries = Vec::with_capacity(DAR_FILES.len());
    for filename in DAR_FILES {
        let path = dars_dir.join(filename);
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("reading DAR {}", path.display()))?;
        entries.push(json!({ "filename": filename, "data": B64.encode(&bytes) }));
    }
    let upload_req = json!({ "dar_files": entries });

    Scenario::with_ctx("distribute DARs", InvitationIds::default())
        .given("3 DAR files on disk", |_f, _| {
            Box::pin(async move { Ok(()) })
        })
        .when("P1 uploads and distributes DARs", {
            let upload_req = upload_req.clone();
            let entries = entries.clone();
            move |f, _| {
                let upload_req = upload_req.clone();
                let distribute_req = json!({
                    "dar_files": entries,
                    "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                });
                Box::pin(async move {
                    let upload_res: anyhow::Result<Value> = f
                        .post_json(f.p1.http, "/dars/upload", &upload_req)
                        .await;
                    if let Err(e) = &upload_res {
                        let msg = format!("{e:#}");
                        if msg.contains("PERMISSION_DENIED") || msg.contains("UNAUTHENTICATED") {
                            anyhow::bail!(
                                "test M2M client lacks DAR upload privileges on participant P1; \
                                 either upload the DARs out-of-band or grant the client admin scope \
                                 on the participant. Underlying error: {msg}"
                            );
                        }
                    }
                    let _: Value = upload_res.context("POST /dars/upload")?;
                    let distribute_res: anyhow::Result<Value> = f
                        .post_json(f.p1.http, "/dars/distribute", &distribute_req)
                        .await;
                    if let Err(e) = &distribute_res {
                        let msg = format!("{e:#}");
                        if msg.contains("PERMISSION_DENIED") || msg.contains("UNAUTHENTICATED") {
                            anyhow::bail!(
                                "test M2M client lacks DAR distribute privileges on participant P1; \
                                 either distribute the DARs out-of-band or grant the client admin scope \
                                 on the participant. Underlying error: {msg}"
                            );
                        }
                    }
                    let _: Value = distribute_res.context("POST /dars/distribute")?;
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
        .then(
            "Dars completed run visible in /workflows on P1 (Coordinator)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p1.http, "Dars", "Coordinator", "completed")
                        .await
                })
            },
        )
        .then(
            "Dars completed run visible in /workflows on P2 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p2.http, "Dars", "Peer", "completed").await
                })
            },
        )
        .then(
            "Dars completed run visible in /workflows on P3 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p3.http, "Dars", "Peer", "completed").await
                })
            },
        )
        .run(f)
        .await
}
