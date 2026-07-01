use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
    types::DecentralizedPartiesResponse,
};

/// Re-add P3 to the party the kick phase removed it from. Runs directly
/// after `kick`, so the party is known to have exactly P1 + P2 (threshold 2)
/// and P3 holds no membership — the inverse precondition kick verified.
///
/// Covers, in one scenario:
/// - the full add flow (key generation on P3, threshold signatures from
///   P1+P2+P3, topology growth, ACS sync, onboarding-flag clearing);
/// - the same-party in-flight guard (second /add-party → 409);
/// - the already-a-member guard (/add-party for P2 → 409);
/// - invitations surfacing on BOTH the existing member (P2) and the new
///   member (P3), and both being acceptable;
/// - the resulting topology: 3 participants, configured threshold.
pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: add_party");

    Scenario::with_ctx("add participant-3 back", InvitationIds::default())
        .given("party present without P3", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                f.party_prefix()?;
                Ok(())
            })
        })
        .when("P1 posts /add-party", |f, _| {
            Box::pin(async move {
                let party_id = f.party_id()?.to_string();
                let p3_uid = f.p3.participant_id.clone();

                // Post-add the party has 3 members again; keep threshold 2 so
                // the later topology assertion distinguishes "restored" from
                // "stale cache" via the participant count. (Threshold == owner
                // count is intentionally NOT used here: the P2P add does not
                // become effective at full threshold — see ADD_PARTY_REDEVELOPMENT
                // §"open items". The coordinator's flag-clear co-sign still runs
                // on this path; it just isn't load-bearing below full threshold.)
                let req = json!({
                    "decentralized_party_id": party_id,
                    "new_participant_id": p3_uid,
                    "new_threshold": 2_i64,
                    "previous_threshold": 2_i64,
                });
                let _: Value = f
                    .post_json(f.p1.http, "/add-party", &req)
                    .await
                    .context("POST /add-party")?;
                Ok(())
            })
        })
        .when(
            "second /add-party for the SAME party — expect 409 (already-member guard)",
            |f, _| {
                Box::pin(async move {
                    // Adding P2 — already a member — must be rejected before
                    // the same-party guard even gets a say.
                    let party_id = f.party_id()?.to_string();
                    let req = json!({
                        "decentralized_party_id": party_id,
                        "new_participant_id": f.p2.participant_id.clone(),
                        "new_threshold": 2_i64,
                        "previous_threshold": 2_i64,
                    });
                    let (status, body) =
                        f.post_expect_status(f.p1.http, "/add-party", &req).await?;
                    anyhow::ensure!(
                        status.as_u16() == 409,
                        "expected 409 for adding an existing member, got {status}: {body}"
                    );
                    anyhow::ensure!(
                        body.contains("already a member"),
                        "409 body should say the participant is already a member: {body}"
                    );
                    Ok(())
                })
            },
        )
        .when(
            "third /add-party while one is in flight — expect 409 (same-party guard)",
            |f, _| {
                Box::pin(async move {
                    // P3 again: passes the member checks, must then hit the
                    // same-party in-flight guard (the first run's row is
                    // persisted InProgress before its 202 returned).
                    let party_id = f.party_id()?.to_string();
                    let req = json!({
                        "decentralized_party_id": party_id,
                        "new_participant_id": f.p3.participant_id.clone(),
                        "new_threshold": 2_i64,
                        "previous_threshold": 2_i64,
                    });
                    let (status, body) =
                        f.post_expect_status(f.p1.http, "/add-party", &req).await?;
                    anyhow::ensure!(
                        status.as_u16() == 409,
                        "expected 409 from the same-party guard, got {status}: {body}"
                    );
                    anyhow::ensure!(
                        body.contains("already has a"),
                        "409 body should name the conflicting in-flight workflow: {body}"
                    );
                    Ok(())
                })
            },
        )
        .then(
            "AddParty invitation visible on P2 (existing member)",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id = probe_pending_invitation(f, f.p2.http, "AddParty").await?;
                    ctx.p2 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .then(
            "AddParty invitation visible on P3 (new member)",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    let id = probe_pending_invitation(f, f.p3.http, "AddParty").await?;
                    ctx.p3 = Some(id);
                    Some(Ok(()))
                })
            },
        )
        .when("P2 accepts AddParty invitation", |f, ctx| {
            Box::pin(async move {
                let id = ctx
                    .p2
                    .as_deref()
                    .context("P2 invitation id not set")?
                    .to_string();
                post_accept_invitation(f, f.p2.http, &id)
                    .await
                    .context("accept AddParty on P2")
            })
        })
        .when("P3 accepts AddParty invitation", |f, ctx| {
            Box::pin(async move {
                let id = ctx
                    .p3
                    .as_deref()
                    .context("P3 invitation id not set")?
                    .to_string();
                post_accept_invitation(f, f.p3.http, &id)
                    .await
                    .context("accept AddParty on P3")
            })
        })
        .then(
            "add-party workflow reaches completed",
            // Longer than kick's budget: this flow has two topology rounds
            // (add + flag clearing), the ACS export/import, and Canton's
            // safe-time wait before the onboarding flag may be cleared.
            Duration::from_secs(420),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_status(&*f, f.p1.http, "/add-party/status", "add-party").await
                })
            },
        )
        .then(
            "AddParty completed run visible in /workflows on P1 (Coordinator)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p1.http, "AddParty", "Coordinator", "completed")
                        .await
                })
            },
        )
        .then(
            "AddParty completed run visible in /workflows on P2 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p2.http, "AddParty", "Peer", "completed").await
                })
            },
        )
        .then(
            "AddParty completed run visible in /workflows on P3 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(f, f.p3.http, "AddParty", "Peer", "completed").await
                })
            },
        )
        .then(
            "P3 is back in the party with the configured threshold",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let prefix = f.party_prefix().ok()?.to_string();
                    let p3_uid = f.p3.participant_id.clone();
                    // `refresh=true` forces a fresh Canton fetch so we assert
                    // the real topology, not the stale cache without P3.
                    let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, &path).await.ok()?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.starts_with(&prefix))?;
                    let p3_present = party
                        .participants
                        .iter()
                        .any(|p| p.participant_uid == p3_uid);
                    if !p3_present || party.participants.len() != 3 || party.threshold != 2 {
                        return None;
                    }
                    Some(Ok(()))
                })
            },
        )
        .then(
            // Topology lists P3, but that alone doesn't prove the offline ACS
            // replication delivered the party's contracts. Assert P3's OWN
            // ledger view of the party covers the contract set an existing
            // member (P1) sees — the actual point of the export/import.
            "P3 sees the party's contracts replicated via the ACS import",
            Duration::from_secs(120),
            |f, _| {
                Box::pin(async move {
                    let prefix = f.party_prefix().ok()?.to_string();
                    // refresh=true makes each side answer from a fresh Canton
                    // query of its own participant, not a cache.
                    let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");

                    let contract_ids = |r: DecentralizedPartiesResponse| {
                        r.parties
                            .into_iter()
                            .find(|p| p.party_id.starts_with(&prefix))
                            .map(|p| {
                                p.contracts
                                    .into_iter()
                                    .map(|c| c.contract_id)
                                    .collect::<std::collections::HashSet<_>>()
                            })
                    };

                    let p1 = contract_ids(f.get_json(f.p1.http, &path).await.ok()?)?;
                    let p3 = contract_ids(f.get_json(f.p3.http, &path).await.ok()?)?;

                    // Non-empty baseline that P3 fully covers == import worked.
                    // Retry until P3's freshly-imported view catches up to P1's.
                    if p1.is_empty() || !p1.is_subset(&p3) {
                        return None;
                    }
                    Some(Ok(()))
                })
            },
        )
        .run(f)
        .await
}
