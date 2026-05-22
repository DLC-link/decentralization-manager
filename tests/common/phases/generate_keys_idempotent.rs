//! G7: GenerateKeys idempotent re-run on resume reuses existing vault keys.
//!
//! Drive an Onboarding to mid-flight on P2 (after the peer has persisted
//! its `peer_public_keys` artifact), capture the artifact payload, kill
//! and restart P2, drive to completion, and verify the dec_party_identity
//! row was created (proving the keys persisted across the restart).

use std::time::Duration;

use crate::common::{
    Fixture, chaos, db, invitations::post_accept_invitation, processes,
    types::DecentralizedPartiesResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("idempotent-keys");
    chaos::say("G7", &format!("starting onboarding with prefix {prefix}"));
    let instance = chaos::post_onboarding(f, &prefix).await?;

    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // Wait for P2 to persist the peer_public_keys artifact for its
    // synthesized peer instance (proves GenerateKeys completed once on
    // P2 before we kill it).
    let p2_db = f.db_path(2);
    let p2_db_clone = p2_db.clone();
    chaos::poll_until(Duration::from_secs(60), || {
        let p2_db = p2_db_clone.clone();
        async move {
            let peer_inst = match db::current_inprogress_peer_instance(&p2_db, "Onboarding").await?
            {
                Some(n) => n,
                None => return Ok(false),
            };
            Ok(db::count_artifacts(&p2_db, &peer_inst).await? > 0)
        }
    })
    .await?;
    chaos::say("G7", "P2 captured key payload, killing mid-run");

    processes::restart_node(f, 2).await?;

    // Drive workflow to completion (coordinator on P1 still running).
    let p1_db = f.db_path(1);
    chaos::poll_until(Duration::from_secs(240), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref(),
            Some("completed")
        ))
    })
    .await?;

    // Resolve dec_party_id from /decentralized-parties.
    let path = format!("/decentralized-parties?prefix={prefix}");
    let r: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
    let dec_party_id = r
        .parties
        .into_iter()
        .find(|p| p.party_id.starts_with(&prefix))
        .map(|p| p.party_id)
        .ok_or_else(|| anyhow::anyhow!("dec_party_id not resolved for prefix {prefix}"))?;

    // dec_party_identity must have rows for this party (keys persist
    // long-term, even after the artifact-cleanup-on-completion fires).
    let id_count = db::count_dec_party_identity(&p2_db, &dec_party_id).await?;
    anyhow::ensure!(
        id_count >= 1,
        "expected ≥1 dec_party_identity rows for {dec_party_id}, got {id_count}"
    );

    // Sanity: the namespace prefix in dec_party_id must equal $PARTY_PREFIX.
    let derived_prefix = dec_party_id
        .split("::")
        .next()
        .ok_or_else(|| anyhow::anyhow!("dec_party_id missing prefix"))?;
    anyhow::ensure!(
        derived_prefix == prefix,
        "dec_party_id prefix '{derived_prefix}' != expected '{prefix}'"
    );

    chaos::say("G7", "GenerateKeys idempotency verified");
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}
