//! G10: Start handler rejects a duplicate `instance_name`, but allows a
//! distinct concurrent run.
//!
//! Concurrent multi-instance workflows removed the global one-at-a-time gate:
//! the start handler now rejects only a run whose `instance_name` is already
//! registered (409), while a run with a *distinct* `instance_name` of the same
//! kind starts side-by-side (202).
//!
//! Drive the coordinator into an inprogress state by posting /onboarding
//! without accepting the invites on either peer — the coordinator stalls at
//! `WaitingForPeers`. A second POST with the SAME prefix (→ same
//! `{prefix}-creation` instance) must return 409; a POST with a DISTINCT prefix
//! must return 202 and run concurrently. /dars/distribute names are
//! timestamp-unique, so a second post always starts a concurrent run (202).
//! Cleanup drains and dismisses every leftover run so subsequent phases start
//! clean.
//!
//! Stalling is achieved by deferring `accept_invitation` rather than by
//! pausing peer processes — the start handler pre-flight peer-meshes
//! over Noise, so peers must remain responsive.

use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{Fixture, db, scenario::Scenario};

#[derive(Default)]
struct Ctx {
    onboarding_prefix: String,
    onboarding_instance: String,
    dars_instance: Option<String>,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: start_handler_conflict_409");

    let prefix = format!(
        "conflict-409-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    );
    let instance_name = format!("{prefix}-creation");

    // Pre-encode the DAR for the /dars/distribute portion.
    // DAR fixtures live at the workspace-root `releases/` (crate is at crates/decman).
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dars_dir = Path::new(manifest_dir).join("../../releases/v0/rc3");
    let dar_path = dars_dir.join("governance-core-v0-rc3-0.1.0.dar");
    let dar_b64 = B64.encode(
        tokio::fs::read(&dar_path)
            .await
            .with_context(|| format!("reading {}", dar_path.display()))?,
    );

    Scenario::with_ctx(
        format!("start handler 409 ({prefix})"),
        Ctx {
            onboarding_prefix: prefix.clone(),
            onboarding_instance: instance_name.clone(),
            dars_instance: None,
        },
    )
    .when("P1 posts first /onboarding (will stall)", {
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
        "first Onboarding row is inprogress on P1",
        Duration::from_secs(30),
        |f, _| {
            let db_path = f.db_path(1);
            Box::pin(async move {
                let n = db::count_workflow_runs_inprogress(&db_path, "Onboarding", "Coordinator")
                    .await
                    .ok()?;
                (n >= 1).then_some(Ok(()))
            })
        },
    )
    .when("P1 posts second /onboarding, SAME prefix — expect 409", {
        let prefix = prefix.clone();
        move |f, _| {
            let prefix = prefix.clone();
            Box::pin(async move {
                // Same prefix → same `{prefix}-creation` instance_name, which is
                // already registered → duplicate-instance rejection.
                let req = json!({
                    "party_id_prefix": prefix,
                    "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                });
                let (status, body) = f.post_expect_status(f.p1.http, "/onboarding", &req).await?;
                anyhow::ensure!(
                    status.as_u16() == 409,
                    "expected 409 for same-instance /onboarding, got {status}: {body}"
                );
                info!("[G10] same-instance /onboarding correctly rejected (409)");
                Ok(())
            })
        }
    })
    .when("P1 posts third /onboarding, DISTINCT prefix — expect 202", {
        let prefix = prefix.clone();
        move |f, _| {
            let prefix = prefix.clone();
            Box::pin(async move {
                // Distinct prefix → distinct instance → allowed to run
                // concurrently under the new multi-instance model.
                let req = json!({
                    "party_id_prefix": format!("{prefix}-concurrent"),
                    "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                });
                let (status, body) = f.post_expect_status(f.p1.http, "/onboarding", &req).await?;
                anyhow::ensure!(
                    status.as_u16() == 202,
                    "expected 202 for a distinct concurrent /onboarding, got {status}: {body}"
                );
                info!("[G10] distinct concurrent /onboarding accepted (202)");
                Ok(())
            })
        }
    })
    // Defensive precondition: if some earlier phase left dars_state in
    // InProgress (we've seen this happen post-cancel_cascades, despite the
    // happy-path distribute_dars completing successfully), cancel it so the
    // test starts from a clean slate. The dars_state.status InProgress
    // check in start_dars otherwise rejects our own first /dars/distribute.
    .given(
        "ensure no stale Dars workflow lingers from earlier phases",
        |f, _| {
            Box::pin(async move {
                #[derive(serde::Deserialize, Debug)]
                struct DarsStatus {
                    #[serde(default)]
                    status: Option<String>,
                }
                if let Ok(s) = f
                    .get_json::<DarsStatus>(f.p1.http, "/dars/distribute/status")
                    .await
                {
                    info!("[G10] pre-test dars status: {s:?}");
                    if matches!(s.status.as_deref(), Some("inprogress" | "InProgress")) {
                        info!("[G10] cancelling stale in-progress Dars before starting our test");
                        let _ = f
                            .post_expect_status(f.p1.http, "/dars/cancel", &json!({}))
                            .await;
                        // Brief settle so the abort + state flip lands.
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                Ok(())
            })
        },
    )
    .when("P1 posts first /dars/distribute (will stall)", {
        let dar_b64 = dar_b64.clone();
        move |f, _| {
            let dar_b64 = dar_b64.clone();
            Box::pin(async move {
                let req = json!({
                    "dar_files": [{
                        "filename": "governance-core-v0-rc3-0.1.0.dar",
                        "data": dar_b64,
                    }],
                    "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                });
                let _: Value = f.post_json(f.p1.http, "/dars/distribute", &req).await?;
                Ok(())
            })
        }
    })
    .then(
        "first Dars row is inprogress on P1",
        Duration::from_secs(30),
        |f, _| {
            let db_path = f.db_path(1);
            Box::pin(async move {
                let n = db::count_workflow_runs_inprogress(&db_path, "Dars", "Coordinator")
                    .await
                    .ok()?;
                (n >= 1).then_some(Ok(()))
            })
        },
    )
    .when("P1 posts second /dars/distribute — expect 202 (concurrent)", {
        let dar_b64 = dar_b64.clone();
        move |f, _| {
            let dar_b64 = dar_b64.clone();
            Box::pin(async move {
                // Dars instance names are `dars-distribute-<timestamp>` at
                // *second* granularity, so the two posts must straddle a second
                // boundary to get distinct instances; otherwise they collide
                // (409). Wait >1s so the second post is deterministically a
                // distinct, concurrently-allowed run.
                tokio::time::sleep(Duration::from_millis(1100)).await;
                let req = json!({
                    "dar_files": [{
                        "filename": "governance-core-v0-rc3-0.1.0.dar",
                        "data": dar_b64,
                    }],
                    "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                });
                let (status, body) = f
                    .post_expect_status(f.p1.http, "/dars/distribute", &req)
                    .await?;
                anyhow::ensure!(
                    status.as_u16() == 202,
                    "expected 202 for a distinct concurrent /dars/distribute, got {status}: {body}"
                );
                info!("[G10] distinct concurrent /dars/distribute accepted (202)");
                Ok(())
            })
        }
    })
    .when("cancel + decline + dismiss leftovers", |f, _| {
        Box::pin(async move {
            // Drain every in-flight run of each kind: the per-kind cancel
            // endpoint cancels one registered run at a time, and this phase
            // started multiple concurrent onboarding + dars runs. Loop until a
            // cancel reports nothing left (409 "No ... workflow in progress")
            // or we hit a safety cap. Failures are ignored — runs may have
            // already settled.
            for path in ["/onboarding/cancel", "/dars/cancel"] {
                for _ in 0..5 {
                    match f.post_expect_status(f.p1.http, path, &json!({})).await {
                        Ok((status, _)) if status.as_u16() == 409 => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }

            // Decline pending invitations on both peers.
            for port in [f.p2.http, f.p3.http] {
                if let Ok(r) = f
                    .get_json::<crate::common::types::PendingInvitationsResponse>(
                        port,
                        "/invitations",
                    )
                    .await
                {
                    for inv in r.invitations {
                        let _ = f
                            .post_expect_status(
                                port,
                                "/invitations/decline",
                                &json!({"id": inv.id}),
                            )
                            .await;
                    }
                }
            }

            // Allow workflows to settle into terminal state.
            tokio::time::sleep(Duration::from_secs(3)).await;

            // Dismiss any leftover cancelled/failed rows on P1 for these kinds.
            let db_path = f.db_path(1);
            let leftover = db::list_undismissed_terminal_runs(
                &db_path,
                &["Onboarding", "Dars"],
                "Coordinator",
            )
            .await
            .unwrap_or_default();
            for inst in leftover {
                let path = format!("/workflows/{inst}/dismiss");
                let _ = f.post_expect_status(f.p1.http, &path, &json!({})).await;
            }

            Ok(())
        })
    })
    .run(f)
    .await
}
