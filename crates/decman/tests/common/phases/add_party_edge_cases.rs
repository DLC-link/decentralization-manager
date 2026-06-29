//! Add-party edge cases. Runs between `kick` and `add_party`: the party has
//! exactly P1 + P2 then, and P3 — the only addable peer — must stay un-added
//! when these scenarios finish, so the happy-path phase can re-add it.
//!
//! Covers:
//! - request validation: bad thresholds, adding self, adding a participant
//!   that isn't a configured peer, a party with no cached membership;
//! - decline cascade: the NEW MEMBER declines → the coordinator run fails
//!   fast and the existing member's pending card is dropped by the
//!   instance-stamped CancelInvite broadcast;
//! - cancel cascade: `/add-party/cancel` aborts the run, drops the
//!   un-accepted card on one peer, and cancels the accepted peer run on the
//!   other.

use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tracing::info;

use crate::common::{
    Fixture, chaos, db, invitations::post_accept_invitation, types::PendingInvitationsResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: add_party_edge_cases");

    wait_for_membership_cache_without_p3(f).await?;
    validation_rejections(f).await?;
    decline_cascade(f).await?;
    cancel_cascade(f).await?;

    chaos::say("ADD", "add-party edge cases verified");
    Ok(())
}

/// Gate on P1's membership CACHE — not the `/decentralized-parties`
/// response — reflecting P3's removal by the kick phase. The start handler's
/// member guards read the cache, and `refresh=true` writes it from a spawned
/// task AFTER serving the fresh response, so the kick phase's final
/// assertion passing does not yet mean the cache has converged (observed as
/// a CI race: the threshold-0 case drew the already-member 409).
async fn wait_for_membership_cache_without_p3(f: &Fixture) -> anyhow::Result<()> {
    let prefix = f.party_prefix()?.to_string();
    let party_id = f.party_id()?.to_string();
    let p3_uid = f.p3.participant_id.clone();
    let p1_db = f.db_path(1);

    // Kick one refresh off (deduped server-side); the poll below converges
    // once its background cache write lands.
    let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
    let _: serde_json::Value = f.get_json(f.p1.http, &path).await?;

    chaos::poll_until(Duration::from_secs(60), || {
        let p1_db = p1_db.clone();
        let party_id = party_id.clone();
        let p3_uid = p3_uid.clone();
        async move { Ok(!db::dec_party_cache_has_participant(&p1_db, &party_id, &p3_uid).await?) }
    })
    .await
    .context("waiting for P1's membership cache to drop P3 after the kick")
}

/// Every malformed `/add-party` request must be rejected up front — no
/// workflow row, no invites.
async fn validation_rejections(f: &Fixture) -> anyhow::Result<()> {
    let party_id = f.party_id()?.to_string();
    let p3_uid = f.p3.participant_id.clone();

    // (request body, expected status, expected error fragment, label)
    let unknown_participant = format!("ghost::{ns}", ns = "1220".to_owned() + &"ab".repeat(32));
    let unknown_party = format!("nosuch::{ns}", ns = "1220".to_owned() + &"cd".repeat(32));
    let cases = [
        (
            json!({
                "decentralized_party_id": party_id,
                "new_participant_id": p3_uid,
                "new_threshold": 0_i64,
                "previous_threshold": 2_i64,
            }),
            400_u16,
            "new_threshold must be between",
            "threshold 0",
        ),
        (
            json!({
                "decentralized_party_id": party_id,
                "new_participant_id": p3_uid,
                // Party has 2 members; post-add max is 3.
                "new_threshold": 4_i64,
                "previous_threshold": 2_i64,
            }),
            400,
            "new_threshold must be between",
            "threshold above member count",
        ),
        (
            json!({
                "decentralized_party_id": party_id,
                "new_participant_id": f.p1.participant_id.clone(),
                "new_threshold": 2_i64,
                "previous_threshold": 2_i64,
            }),
            400,
            "Cannot add yourself",
            "adding self",
        ),
        (
            json!({
                "decentralized_party_id": party_id,
                "new_participant_id": unknown_participant,
                "new_threshold": 2_i64,
                "previous_threshold": 2_i64,
            }),
            400,
            "not a configured peer",
            "unknown participant",
        ),
        (
            json!({
                "decentralized_party_id": unknown_party,
                "new_participant_id": p3_uid,
                "new_threshold": 2_i64,
                "previous_threshold": 2_i64,
            }),
            409,
            "No cached membership",
            "unknown party",
        ),
    ];

    for (req, expected_status, expected_fragment, label) in cases {
        let (status, body) = f.post_expect_status(f.p1.http, "/add-party", &req).await?;
        anyhow::ensure!(
            status.as_u16() == expected_status,
            "{label}: expected {expected_status}, got {status}: {body}"
        );
        anyhow::ensure!(
            body.contains(expected_fragment),
            "{label}: body should contain {expected_fragment:?}: {body}"
        );
    }

    chaos::say("ADD", "validation rejections verified");
    Ok(())
}

/// The new member declines its invitation: the coordinator run must fail
/// fast (no waiting for a timeout) and the existing member's pending card
/// must be dropped by the instance-stamped CancelInvite broadcast.
async fn decline_cascade(f: &Fixture) -> anyhow::Result<()> {
    let instance = start_add_party_p3(f).await?;
    chaos::say("ADD", &format!("decline cascade against run {instance}"));

    let inv_deadline = Duration::from_secs(60);
    let p2_card = chaos::wait_for_invite_for_instance(f, f.p2.http, &instance, inv_deadline)
        .await
        .context("P2 invite for the to-be-declined run")?;
    let p3_card = chaos::wait_for_invite_for_instance(f, f.p3.http, &instance, inv_deadline)
        .await
        .context("P3 invite for the to-be-declined run")?;
    // Silence the unused-variable lint while documenting intent: P2's card is
    // observed (so its later disappearance is meaningful) but never acted on.
    let _ = p2_card;

    let (status, body) = f
        .post_expect_status(f.p3.http, "/invitations/decline", &json!({ "id": p3_card }))
        .await?;
    anyhow::ensure!(status.as_u16() == 200, "decline returned {status}: {body}");

    // Coordinator row fails on P1 with the decline surfaced as the error.
    let p1_db = f.db_path(1);
    let instance_for_poll = instance.clone();
    chaos::poll_until(Duration::from_secs(60), || {
        let p1_db = p1_db.clone();
        let instance = instance_for_poll.clone();
        async move {
            Ok(db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref()
                == Some("failed"))
        }
    })
    .await
    .context("waiting for declined add-party run to fail on P1")?;

    // P2's pending card for THIS run is dropped by the CancelInvite fan-out.
    wait_for_invite_gone(f, f.p2.http, &instance, Duration::from_secs(60))
        .await
        .context("waiting for P2's add-party card to be dropped after decline")?;

    // A declined run's coordinator TASK keeps idling in WaitingForPeers (the
    // decline only fails the row), and the legacy per-kind status/cancel
    // endpoints pick the lowest-instance registered run — so reap it via the
    // unambiguous per-instance cancel (also covering that endpoint for
    // AddParty) before the next scenario starts its own run.
    let (status, body) = f
        .post_expect_status(
            f.p1.http,
            &format!("/workflows/{instance}/cancel"),
            &json!({}),
        )
        .await?;
    anyhow::ensure!(
        status.as_u16() == 200,
        "per-instance cancel of the declined run returned {status}: {body}"
    );

    chaos::dismiss_p1(f, &instance).await;
    chaos::say("ADD", "decline cascade verified");
    Ok(())
}

/// `/add-party/cancel` aborts the coordinator run, drops the un-accepted
/// card on the new member, and cancels the accepted existing member's
/// in-flight peer run.
async fn cancel_cascade(f: &Fixture) -> anyhow::Result<()> {
    let instance = start_add_party_p3(f).await?;
    chaos::say("ADD", &format!("cancel cascade against run {instance}"));

    let inv_deadline = Duration::from_secs(60);
    let p2_card = chaos::wait_for_invite_for_instance(f, f.p2.http, &instance, inv_deadline)
        .await
        .context("P2 invite for the to-be-cancelled run")?;
    let p3_card = chaos::wait_for_invite_for_instance(f, f.p3.http, &instance, inv_deadline)
        .await
        .context("P3 invite for the to-be-cancelled run")?;
    let _ = p3_card; // left pending — the cancel must drop it unaccepted

    // P2 accepts so the cancel exercises the in-flight peer-run teardown.
    post_accept_invitation(f, f.p2.http, &p2_card).await?;
    let p2_db = f.db_path(2);
    let instance_for_poll = instance.clone();
    chaos::poll_until(Duration::from_secs(30), || {
        let p2_db = p2_db.clone();
        let instance = instance_for_poll.clone();
        async move {
            let s = db::peer_run_status_by_coordinator_instance(&p2_db, &instance).await?;
            Ok(s.as_deref() == Some("inprogress"))
        }
    })
    .await
    .context("waiting for P2's accepted peer row before cancelling")?;

    let (status, body) = f
        .post_expect_status(f.p1.http, "/add-party/cancel", &json!({}))
        .await?;
    anyhow::ensure!(status.as_u16() == 200, "cancel returned {status}: {body}");

    // Coordinator row flips to cancelled on P1.
    let p1_db = f.db_path(1);
    let instance_for_poll = instance.clone();
    chaos::poll_until(Duration::from_secs(60), || {
        let p1_db = p1_db.clone();
        let instance = instance_for_poll.clone();
        async move {
            Ok(db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref()
                == Some("cancelled"))
        }
    })
    .await
    .context("waiting for cancelled add-party run on P1")?;

    // P3's un-accepted card is dropped; P2's peer run is cancelled.
    wait_for_invite_gone(f, f.p3.http, &instance, Duration::from_secs(60))
        .await
        .context("waiting for P3's add-party card to be dropped after cancel")?;
    let instance_for_poll = instance.clone();
    chaos::poll_until(Duration::from_secs(60), || {
        let p2_db = p2_db.clone();
        let instance = instance_for_poll.clone();
        async move {
            let s = db::peer_run_status_by_coordinator_instance(&p2_db, &instance).await?;
            Ok(s.as_deref() == Some("cancelled"))
        }
    })
    .await
    .context("waiting for P2's peer run to cancel")?;

    // Cleanup so later phases (and the happy-path add) start from a clean feed.
    chaos::dismiss_p1(f, &instance).await;
    let leftovers = db::list_undismissed_terminal_runs(&p2_db, &["AddParty"], "Peer")
        .await
        .unwrap_or_default();
    for inst in leftovers {
        chaos::dismiss_on(f, f.p2.http, &inst).await;
    }

    chaos::say("ADD", "cancel cascade verified");
    Ok(())
}

/// Start `/add-party` adding P3 to the fixture party, returning the
/// coordinator run's `instance_name`.
async fn start_add_party_p3(f: &Fixture) -> anyhow::Result<String> {
    let req = json!({
        "decentralized_party_id": f.party_id()?.to_string(),
        "new_participant_id": f.p3.participant_id.clone(),
        "new_threshold": 2_i64,
        "previous_threshold": 2_i64,
    });
    chaos::start_workflow_on(f, f.p1.http, "/add-party", &req).await
}

/// Poll until no pending invitation for coordinator run `instance` is
/// visible on `port`. Only meaningful after the card was previously
/// observed — absence before delivery would also pass.
async fn wait_for_invite_gone(
    f: &Fixture,
    port: u16,
    instance: &str,
    deadline: Duration,
) -> anyhow::Result<()> {
    chaos::poll_until(deadline, || async move {
        let r: PendingInvitationsResponse = f.get_json(port, "/invitations").await?;
        Ok(!r
            .invitations
            .iter()
            .any(|i| i.workflow_instance.as_deref() == Some(instance)))
    })
    .await
}
