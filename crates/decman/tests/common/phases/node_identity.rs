//! A Canton-native node identity: each node allocates
//! `decman-node-<participant prefix>::<participant namespace>` through
//! `POST /node-identity`, and checks its peers' parties through
//! `GET /node-identity`. The run's log is the evidence, so every observed
//! value is logged.
//!
//! - 1a: AllocateParty returns exactly the derived id.
//! - 1b: the user passed to AllocateParty can act as the new party, with no
//!   GrantUserRights call.
//! - 2: the rights of the harness's admin user (logged only).
//! - 3: the prefix scan returns every node party and the decoy, while the
//!   per-peer check matches the exact id.
//! - 4: whether a user with act-as on the participant admin party can sign a
//!   create as it (logged only, never fails the suite).

use std::{collections::BTreeSet, time::Duration};

use anyhow::{Context, ensure};
use chrono::Utc;
use common::{
    api::{NodeIdentityResponse, NodeIdentityStatusResponse},
    types::Permission,
};
use dec_party_manager::config::Peer;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, TestTarget,
    ledger_api::{
        P1_JSON_API, REWARD_COUPON_V2_TEMPLATE, SeedCoupon, archive_command,
        reward_coupon_create_command,
    },
    phases::deploy_gov_core::{allocate_party, grant_rights},
    scenario::Scenario,
};

#[derive(Deserialize)]
struct NetworkPeers {
    peers: Vec<Peer>,
}

#[derive(Default)]
struct Ctx {
    /// The first `POST /node-identity` response of P1, P2 and P3, in order.
    first: Vec<NodeIdentityResponse>,
    repeat: Option<NodeIdentityResponse>,
    decoy: Option<String>,
}

/// Name, HTTP port and participant id of P1, P2 and P3.
fn nodes(f: &Fixture) -> [(&'static str, u16, String); 3] {
    [
        ("P1", f.p1.http, f.p1.participant_id.clone()),
        ("P2", f.p2.http, f.p2.participant_id.clone()),
        ("P3", f.p3.http, f.p3.participant_id.clone()),
    ]
}

/// The node party of a participant, derived here rather than by decman.
fn expected_node_party(participant_id: &str) -> anyhow::Result<String> {
    let (prefix, namespace) = participant_id
        .split_once("::")
        .with_context(|| format!("participant id {participant_id} has no '::'"))?;
    Ok(format!("decman-node-{prefix}::{namespace}"))
}

fn check_first_allocations(f: &Fixture, ctx: &Ctx) -> anyhow::Result<()> {
    ensure!(
        ctx.first.len() == 3,
        "expected 3 responses, got {}",
        ctx.first.len()
    );
    for ((name, _, participant_id), response) in nodes(f).iter().zip(&ctx.first) {
        let expected = expected_node_party(participant_id)?;
        ensure!(
            response.node_party_id.to_string() == expected,
            "{name}: AllocateParty returned {}, expected {expected}",
            response.node_party_id
        );
        ensure!(
            response.allocated,
            "{name}: the node party was not allocated"
        );
        ensure!(
            response.act_as,
            "{name}: {} cannot act as {expected} after AllocateParty",
            response.user_id
        );
    }
    Ok(())
}

fn check_repeat(ctx: &Ctx) -> anyhow::Result<()> {
    let first = ctx.first.first().context("no first P1 response")?;
    let repeat = ctx.repeat.as_ref().context("no repeat P1 response")?;
    ensure!(!repeat.allocated, "the second call allocated again");
    ensure!(
        repeat.node_party_id == first.node_party_id,
        "the second call returned {}, the first {}",
        repeat.node_party_id,
        first.node_party_id
    );
    Ok(())
}

/// Fact 3 on one node: the scan holds every node party and the decoy, and
/// the peer rows are exactly the other nodes, each verified by exact id.
fn check_status(
    status: &NodeIdentityStatusResponse,
    own_id: &str,
    peer_ids: &[String],
    node_parties: &[String],
    decoy: &str,
) -> anyhow::Result<()> {
    for party in node_parties.iter().map(String::as_str).chain([decoy]) {
        ensure!(
            status.scan.iter().any(|p| p == party),
            "scan lacks {party}: {:?}",
            status.scan
        );
    }
    let seen: BTreeSet<String> = status
        .peers
        .iter()
        .map(|p| p.participant_id.to_string())
        .collect();
    let want: BTreeSet<String> = peer_ids.iter().cloned().collect();
    ensure!(seen == want, "peer rows {seen:?}, expected {want:?}");
    for peer in &status.peers {
        let expected = expected_node_party(&peer.participant_id.to_string())?;
        ensure!(
            peer.node_party_id.to_string() == expected,
            "peer row names {} instead of {expected}",
            peer.node_party_id
        );
        ensure!(peer.verified, "peer not verified: {peer:?}");
    }
    ensure!(
        status.self_.participant_id.to_string() == own_id,
        "self row is {}, expected {own_id}",
        status.self_.participant_id
    );
    ensure!(
        matches!(
            status.self_.hosting_permission,
            Some(Permission::Submission)
        ),
        "own node party hosted with {:?}",
        status.self_.hosting_permission
    );
    Ok(())
}

async fn probe_status(f: &Fixture, ctx: &Ctx, index: usize) -> Option<anyhow::Result<()>> {
    let all = nodes(f);
    let (name, port, own_id) = all[index].clone();
    let peer_ids: Vec<String> = all
        .into_iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(_, (_, _, id))| id)
        .collect();
    let node_parties: Vec<String> = ctx
        .first
        .iter()
        .map(|r| r.node_party_id.to_string())
        .collect();
    let Some(decoy) = ctx.decoy.as_deref() else {
        return Some(Err(anyhow::anyhow!("no decoy party recorded")));
    };

    let status: NodeIdentityStatusResponse = f.probe_get_json(port, "/node-identity").await?;
    f.probe_diag.ok(
        &format!("{name} GET /node-identity"),
        check_status(&status, &own_id, &peer_ids, &node_parties, decoy),
    )?;
    info!(
        "{name} scan with filter {:?}: {:?}",
        status.scan_filter, status.scan
    );
    info!("{name} self: {:?}", status.self_);
    for peer in &status.peers {
        info!("{name} peer: {peer:?}");
    }
    Some(Ok(()))
}

/// Fact 4: grant P1's harness user act-as on P1's participant admin party,
/// create a `RewardCouponV2` with it as signatory, then archive the coupon.
async fn create_as_admin_party(f: &Fixture) -> anyhow::Result<()> {
    let admin_party = f.p1.participant_id.clone();
    grant_rights(f, P1_JSON_API, &admin_party, "participant-1").await?;
    let coupon = SeedCoupon {
        dso: admin_party.clone(),
        provider: admin_party.clone(),
        amount: "1.0".to_string(),
        expires_at: (Utc::now() + chrono::Duration::hours(1))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
        round: 0,
    };
    let command_id = format!("node-identity-admin-party-{}", f.run_id);
    let created = f
        .submit_create(
            P1_JSON_API,
            &reward_coupon_create_command(std::slice::from_ref(&coupon), &command_id),
        )
        .await?;
    info!("Fact 4: the create as {admin_party} returned {created}");
    let coupons = f.active_reward_coupons(P1_JSON_API, &admin_party).await?;
    for (i, coupon) in coupons.iter().filter(|c| c.dso == admin_party).enumerate() {
        let archive = archive_command(
            REWARD_COUPON_V2_TEMPLATE,
            &coupon.contract_id,
            &admin_party,
            &format!("{command_id}-archive-{i}"),
        );
        f.submit_create(P1_JSON_API, &archive)
            .await
            .context("archive the coupon")?;
    }
    Ok(())
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    if f.target != TestTarget::Localnet {
        info!("node_identity: skipped (localnet-only)");
        return Ok(());
    }
    info!("Phase: node_identity (localnet)");

    Scenario::with_ctx("node identity: allocate, verify, scan", Ctx::default())
        .given("3 peers configured on P1", |f, _| {
            Box::pin(async move {
                let config: NetworkPeers = f
                    .get_json(f.p1.http, "/network-config")
                    .await
                    .context("GET /network-config on P1")?;
                for peer in &config.peers {
                    info!("P1 peer {}: {}", peer.name, peer.participant_id);
                }
                ensure!(
                    config.peers.len() == 3,
                    "P1 has {} peers, expected 3",
                    config.peers.len()
                );
                Ok(())
            })
        })
        .given("the harness admin user's rights on P1 (fact 2)", |f, _| {
            Box::pin(async move {
                let rights: Value = f
                    .get_json(P1_JSON_API, "/v2/users/ledger-api-user/rights")
                    .await
                    .context("GET ledger-api-user rights on P1")?;
                info!("Fact 2: ledger-api-user rights on P1: {rights}");
                Ok(())
            })
        })
        .when("each node posts /node-identity", |f, ctx| {
            Box::pin(async move {
                for (name, port, _) in nodes(f) {
                    let response: NodeIdentityResponse = f
                        .post_json(port, "/node-identity", &json!({}))
                        .await
                        .with_context(|| format!("POST /node-identity on {name}"))?;
                    info!("{name} POST /node-identity: {response:?}");
                    ctx.first.push(response);
                }
                Ok(())
            })
        })
        .then(
            "each node got the derived id, a new party, and act-as (facts 1a, 1b)",
            Duration::from_secs(5),
            |f, ctx| Box::pin(async move { Some(check_first_allocations(f, ctx)) }),
        )
        .when("P1 posts /node-identity again", |f, ctx| {
            Box::pin(async move {
                let response: NodeIdentityResponse = f
                    .post_json(f.p1.http, "/node-identity", &json!({}))
                    .await
                    .context("second POST /node-identity on P1")?;
                info!("P1 second POST /node-identity: {response:?}");
                ctx.repeat = Some(response);
                Ok(())
            })
        })
        .then(
            "the second call allocates nothing and returns the same id",
            Duration::from_secs(5),
            |_, ctx| Box::pin(async move { Some(check_repeat(ctx)) }),
        )
        .when("a decoy party shares P1's node party prefix", |f, ctx| {
            Box::pin(async move {
                let (prefix, _) =
                    f.p1.participant_id
                        .split_once("::")
                        .context("P1 participant id has no '::'")?;
                let hint = format!("decman-node-{prefix}-decoy");
                let decoy = allocate_party(&*f, P1_JSON_API, &hint, "participant-1").await?;
                info!("Decoy party on P1: {decoy}");
                ctx.decoy = Some(decoy);
                Ok(())
            })
        })
        .then(
            "P1 scans every node party and the decoy, and verifies P2 and P3 (fact 3)",
            Duration::from_secs(60),
            |f, ctx| Box::pin(async move { probe_status(f, ctx, 0).await }),
        )
        .then("P2 sees the same", Duration::from_secs(60), |f, ctx| {
            Box::pin(async move { probe_status(f, ctx, 1).await })
        })
        .then("P3 sees the same", Duration::from_secs(60), |f, ctx| {
            Box::pin(async move { probe_status(f, ctx, 2).await })
        })
        .when(
            "P1's user creates a contract as the participant admin party (fact 4)",
            |f, _| {
                Box::pin(async move {
                    match create_as_admin_party(f).await {
                        Ok(()) => info!(
                            "Fact 4: ledger-api-user created and archived a contract as P1's \
                             participant admin party"
                        ),
                        Err(e) => {
                            info!("Fact 4: acting as P1's participant admin party failed: {e:#}")
                        }
                    }
                    Ok(())
                })
            },
        )
        .run(f)
        .await
}
