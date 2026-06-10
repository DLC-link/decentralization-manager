//! G10: Start handler rejects a duplicate `instance_name`.
//!
//! Concurrent multi-instance workflows removed the global one-at-a-time gate:
//! the start handler now rejects only a run whose `instance_name` is already
//! registered (409). (That distinct instances run concurrently is covered by
//! the `WorkflowRegistry` unit tests + the DB migration test; exercising it
//! here would leave extra in-flight runs that pollute later restart/resume
//! phases.)
//!
//! Drive the coordinator into an inprogress state by posting /onboarding
//! without accepting the invites on either peer — the coordinator stalls at
//! `WaitingForPeers`. A second POST with the SAME prefix (→ same
//! `{prefix}-creation` instance) must return 409. Cleanup cancels, declines,
//! and dismisses the leftover run so subsequent phases start clean.
//!
//! Stalling is achieved by deferring `accept_invitation` rather than by
//! pausing peer processes — the start handler pre-flight peer-meshes over
//! Noise, so peers must remain responsive.

use std::time::Duration;

use serde_json::{Value, json};
use tracing::info;

use crate::common::{Fixture, db, scenario::Scenario};

#[derive(Default)]
struct Ctx;

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: start_handler_conflict_409");

    let prefix = format!(
        "conflict-409-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    );

    Scenario::with_ctx(format!("start handler 409 ({prefix})"), Ctx)
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
                    // Same prefix → same `{prefix}-creation` instance_name, which
                    // is already registered → duplicate-instance rejection.
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
        .when("cancel + decline + dismiss leftover", |f, _| {
            Box::pin(async move {
                // Cancel the in-flight onboarding; ignore failures (it may have
                // already settled).
                let _ = f
                    .post_expect_status(f.p1.http, "/onboarding/cancel", &json!({}))
                    .await;

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

                // Allow the cancel to settle into terminal state.
                tokio::time::sleep(Duration::from_secs(3)).await;

                // Dismiss any leftover cancelled/failed onboarding rows on P1 so
                // they aren't resumed by a later restart phase.
                let db_path = f.db_path(1);
                let leftover =
                    db::list_undismissed_terminal_runs(&db_path, &["Onboarding"], "Coordinator")
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
