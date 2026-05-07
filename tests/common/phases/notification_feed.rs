//! Notification feed: assert /workflows and /governance/confirmations expose
//! the right state to every node so the frontend feed renders correctly.
//!
//! Three sections, mirroring the bash original:
//!  1. /workflows JSON shape + role/coordinator_name resolution per node.
//!  2. Dismiss filters the row from the feed (DB row stays as dismissed=1).
//!  3. /governance/confirmations is consistent across all 3 nodes for a
//!     freshly-proposed action.
//!
//! Runs after generic_vote and BEFORE kick (kick removes P3 from the
//! dec_party, breaking the multi-node visibility check).

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, db,
    scenario::Scenario,
    types::{GovernanceState, WorkflowRunsResponse},
};

#[derive(Default)]
struct Ctx {
    dismiss_target: Option<String>,
    dismiss_before_count: usize,
    proposal_cid: Option<String>,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: notification_feed");

    Scenario::with_ctx(
        "notification feed: shape + dismiss + multi-node",
        Ctx::default(),
    )
    .given(
        "party + governance rules contract present (notification-feed precondition)",
        |f, _| {
            Box::pin(async move {
                f.party_id()?;
                f.rules_contract_id()?;
                Ok(())
            })
        },
    )
    // ----------------------------------------------------------------
    // Section 1: /workflows shape on every node
    // ----------------------------------------------------------------
    .then(
        "/workflows returns a non-empty runs array on every node",
        Duration::from_secs(15),
        |f, _| {
            Box::pin(async move {
                for port in [f.p1.http, f.p2.http, f.p3.http] {
                    let r: WorkflowRunsResponse = f.get_json(port, "/workflows").await.ok()?;
                    if r.runs.is_empty() {
                        return None;
                    }
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "P1 has Onboarding/Coordinator row with 2 expected peers",
        Duration::from_secs(15),
        |f, _| {
            Box::pin(async move {
                let r: WorkflowRunsResponse = f.get_json(f.p1.http, "/workflows").await.ok()?;
                let row = r
                    .runs
                    .iter()
                    .find(|w| w.kind == "Onboarding" && w.role == "Coordinator")?;
                if row.expected_peers.len() != 2 {
                    return Some(Err(anyhow::anyhow!(
                        "expected 2 peers on P1's Onboarding row, got {}",
                        row.expected_peers.len()
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "P2 has Onboarding/Peer row with coordinator_name resolved",
        Duration::from_secs(15),
        |f, _| {
            Box::pin(async move {
                let r: WorkflowRunsResponse = f.get_json(f.p2.http, "/workflows").await.ok()?;
                let row = r
                    .runs
                    .iter()
                    .find(|w| w.kind == "Onboarding" && w.role == "Peer")?;
                let pubkey = row.coordinator_pubkey.as_deref().unwrap_or("");
                let name = row.coordinator_name.as_deref().unwrap_or("");
                if pubkey.is_empty() {
                    return Some(Err(anyhow::anyhow!(
                        "P2 Onboarding/Peer row has empty coordinator_pubkey"
                    )));
                }
                if name != "Participant 1" {
                    return Some(Err(anyhow::anyhow!(
                        "P2 coordinator_name not resolved: got '{name}'"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    // ----------------------------------------------------------------
    // Section 2: dismiss filters the row out of /workflows
    // ----------------------------------------------------------------
    .given(
        "pick a completed+undismissed row on P1 to dismiss",
        |f, ctx| {
            Box::pin(async move {
                let r: WorkflowRunsResponse = f.get_json(f.p1.http, "/workflows").await?;
                ctx.dismiss_before_count = r.runs.len();
                let target = r
                    .runs
                    .iter()
                    .find(|w| w.status == "completed" && !w.dismissed)
                    .map(|w| w.instance_name.clone())
                    .context("no completed+undismissed row on P1 to dismiss")?;
                ctx.dismiss_target = Some(target);
                Ok(())
            })
        },
    )
    .when("dismiss the chosen row on P1", |f, ctx| {
        let target = ctx.dismiss_target.clone();
        Box::pin(async move {
            let target = target.context("dismiss target not set")?;
            let path = format!("/workflows/{target}/dismiss");
            let (status, body) = f.post_expect_status(f.p1.http, &path, &json!({})).await?;
            anyhow::ensure!(status.as_u16() == 200, "dismiss returned {status}: {body}");
            Ok(())
        })
    })
    .then(
        "dismissed row no longer in /workflows feed",
        Duration::from_secs(10),
        |f, ctx| {
            let target = ctx.dismiss_target.clone();
            Box::pin(async move {
                let target = target?;
                let r: WorkflowRunsResponse = f.get_json(f.p1.http, "/workflows").await.ok()?;
                if r.runs.iter().any(|w| w.instance_name == target) {
                    return None;
                }
                if r.runs.len() >= ctx.dismiss_before_count {
                    return Some(Err(anyhow::anyhow!(
                        "feed count did not decrease ({} → {})",
                        ctx.dismiss_before_count,
                        r.runs.len()
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "DB row preserved with dismissed=1",
        Duration::from_secs(5),
        |f, ctx| {
            let db_path = f.db_path(1);
            let target = ctx.dismiss_target.clone();
            Box::pin(async move {
                let target = target?;
                let dismissed = db::workflow_run_dismissed(&db_path, &target, "Coordinator")
                    .await
                    .ok()
                    .flatten()?;
                dismissed.then_some(Ok(()))
            })
        },
    )
    // ----------------------------------------------------------------
    // Section 3: /governance/confirmations consistent across all 3 nodes
    // ----------------------------------------------------------------
    // The proposal is a GenericVote with a phase-unique description so it
    // never collides with the action token_custody already executed (which
    // is the same SetupCcPreapproval shape and would surface as a slow
    // duplicate-contract creation against Canton).
    .when(
        "P1 proposes GenericVote for multi-node visibility check",
        |f, _| {
            Box::pin(async move {
                let req = json!({
                    "party_id": f.party_id()?,
                    "rules_contract_id": f.rules_contract_id()?,
                    "proposal": {
                        "type": "generic_vote",
                        "description": format!(
                            "notification-feed visibility probe {}",
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis())
                                .unwrap_or_default()
                        ),
                    }
                });
                let _: Value = f.post_json(f.p1.http, "/governance/propose", &req).await?;
                Ok(())
            })
        },
    )
    .then(
        "new proposal_cid surfaced on P1",
        Duration::from_secs(30),
        |f, ctx| {
            Box::pin(async move {
                let party_id = f.party_id().ok()?.to_string();
                let path = format!("/governance/confirmations?party_id={party_id}");
                let s: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                let last = s.domain_actions.last()?;
                ctx.proposal_cid = Some(last.proposal_cid.clone());
                Some(Ok(()))
            })
        },
    )
    .then(
        "proposal_cid visible on P1 + P2 + P3",
        Duration::from_secs(60),
        |f, ctx| {
            let cid = ctx.proposal_cid.clone();
            Box::pin(async move {
                let cid = cid?;
                let party_id = f.party_id().ok()?.to_string();
                let path = format!("/governance/confirmations?party_id={party_id}");
                for port in [f.p1.http, f.p2.http, f.p3.http] {
                    let s: GovernanceState = f.get_json(port, &path).await.ok()?;
                    if !s.domain_actions.iter().any(|a| a.proposal_cid == cid) {
                        return None;
                    }
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "threshold consistent across all 3 nodes",
        Duration::from_secs(15),
        |f, _| {
            Box::pin(async move {
                let party_id = f.party_id().ok()?.to_string();
                let path = format!("/governance/confirmations?party_id={party_id}");
                let s1: GovernanceState = f.get_json(f.p1.http, &path).await.ok()?;
                let s2: GovernanceState = f.get_json(f.p2.http, &path).await.ok()?;
                let s3: GovernanceState = f.get_json(f.p3.http, &path).await.ok()?;
                if s1.threshold != s2.threshold || s2.threshold != s3.threshold {
                    return Some(Err(anyhow::anyhow!(
                        "threshold mismatch: P1={}, P2={}, P3={}",
                        s1.threshold,
                        s2.threshold,
                        s3.threshold
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    // Cleanup: cancel P1's auto-confirmation so the action goes to 0/threshold
    // and stays out of the way of subsequent tests (kick).
    .when(
        "cancel P1's auto-confirmation to clear the feed",
        |f, ctx| {
            let cid = ctx.proposal_cid.clone();
            Box::pin(async move {
                let cid = cid.context("proposal_cid not set")?;
                let party_id = f.party_id()?.to_string();
                let path = format!("/governance/confirmations?party_id={party_id}");
                let s: GovernanceState = f.get_json(f.p1.http, &path).await?;
                if let Some(action) = s.domain_actions.iter().find(|a| a.proposal_cid == cid)
                    && let Some(conf) = action.confirmations.first()
                {
                    let req = json!({
                        "party_id": party_id,
                        "confirmation_cid": conf.contract_id,
                    });
                    let _ = f
                        .post_expect_status(f.p1.http, "/governance/cancel", &req)
                        .await;
                }
                Ok(())
            })
        },
    )
    .run(f)
    .await
}
