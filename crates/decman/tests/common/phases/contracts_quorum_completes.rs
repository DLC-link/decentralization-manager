//! Threshold/quorum regression: a contracts workflow must COMPLETE when only a
//! quorum of peers accept, instead of looping forever at the signing step.
//!
//! Reproduces the production hang: on a 2-of-3 party the coordinator + one peer
//! are a signing quorum (M = 2). We invite both P2 and P3 but have only P2
//! accept; P3 stays up but never accepts (the "absent owner" of the bug report).
//! The coordinator's start gate advances on the connected quorum, and — with the
//! fix — the signing gate advances once every *connected* peer (just P2) has
//! signed, so the run reaches `completed`. Before the fix it hung at
//! `SignSubmissions` waiting for P3 forever.
//!
//! This also empirically answers whether an M-of-N interactive submission
//! finalizes on a quorum of signatures (coordinator + P2): if it didn't, the
//! coordinator would stall in ExecuteSubmissions and this phase would time out.
//!
//! Depends on `deploy_gov_core` having run (reuses the member parties it set on
//! the fixture). Runs in the chaos block so the stale, never-accepted P3 invite
//! can't interfere with the happy-path contracts phases.

use std::time::{Duration, Instant};

use anyhow::Context;
use serde_json::{Value, json};
use tokio::time::sleep;
use tracing::info;

use crate::common::{
    Fixture, TestTarget, chaos,
    http::probe_workflow_status,
    invitations::{post_accept_invitation, probe_pending_invitation},
    types::DecentralizedPartiesResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: contracts_quorum_completes");
    chaos::ensure_nodes_healthy(f).await?;

    let party_id = f.party_id()?.to_string();
    let p1m = f
        .p1_member_party
        .clone()
        .context("p1 member party missing (deploy_gov_core must run first)")?;
    let p2m = f
        .p2_member_party
        .clone()
        .context("p2 member party missing")?;
    let p3m = f
        .p3_member_party
        .clone()
        .context("p3 member party missing")?;

    // Re-fetch the party's participant uids (same as deploy_gov_core).
    let parties: DecentralizedPartiesResponse =
        f.get_json(f.p1.http, "/decentralized-parties").await?;
    let party = parties
        .parties
        .into_iter()
        .find(|p| p.party_id == party_id)
        .with_context(|| format!("party {party_id} not found"))?;
    let uids: Vec<String> = party
        .participants
        .iter()
        .map(|p| p.participant_uid.clone())
        .collect();
    anyhow::ensure!(
        uids.len() == 3,
        "expected 3 participants, got {}",
        uids.len()
    );

    let operator_party = match f.target {
        TestTarget::Localnet => p1m.clone(),
        TestTarget::Devnet => f
            .operator_party
            .clone()
            .context("operator_party not set on devnet")?,
    };

    // Deploy a SECOND GovernanceRules (no contract key → additive, no conflict
    // with the one deploy_gov_core created), inviting both P2 and P3.
    let req = json!({
        "decentralized_party_id": party_id,
        "participant_ids": uids,
        "participant_parties": [&p1m, &p2m, &p3m],
        "operator_party": operator_party,
        "contracts": [{
            "id": "governance-rules-quorum",
            "name": "GovernanceRules",
            "package_id": "#governance-core-v1",
            "module_name": "Governance.Rules",
            "entity_name": "GovernanceRules",
            "fields": [
                {"type": "decentralized_party"},
                {"type": "party_set", "parties": [&p1m, &p2m, &p3m]},
                {"type": "int64", "value": 2},
                {"type": "rel_time", "microseconds": 1800000000_i64},
                {"type": "none"},
            ],
        }],
    });
    let _: Value = f
        .post_json(f.p1.http, "/contracts", &req)
        .await
        .context("POST /contracts")?;

    // Only P2 accepts. P3 stays up but never accepts — the absent owner.
    let p2_inv = chaos::wait_for_invite(f, f.p2.http, "Contracts", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv)
        .await
        .context("accept Contracts on P2")?;
    info!("P2 accepted; P3 deliberately left un-accepted (the absent owner)");

    // The coordinator must reach `completed` on the quorum (coordinator + P2),
    // without P3. Poll /contracts/status until terminal or 240s.
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        if let Some(res) =
            probe_workflow_status(&*f, f.p1.http, "/contracts/status", "contracts").await
        {
            res?; // Ok(()) => completed; Err => failed (surfaces the message)
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "contracts workflow did not complete at quorum within 240s (P3 absent) — \
             the signing gate likely still waits for all peers, or the quorum submission \
             did not finalize"
        );
        sleep(Duration::from_secs(2)).await;
    }
    info!("contracts workflow completed at quorum with P3 absent");

    // Best-effort: clear P3's stale, never-accepted invite so it doesn't linger.
    if let Some(id) = probe_pending_invitation(f, f.p3.http, "Contracts").await {
        let _ = f
            .post_expect_status(f.p3.http, "/invitations/decline", &json!({ "id": id }))
            .await;
    }
    Ok(())
}
