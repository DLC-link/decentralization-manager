//! Rotate P1's Daml key on the shared party, then deploy with the new key.
//!
//! The rotation swaps exactly one key in the party's P2P mapping. The other
//! members then record P1's new key, so a later kick or rotation attributes
//! it to P1. P1 coordinates the deploy that follows, and a coordinator always
//! signs, so the deploy only completes if P1 signs with the new key.
//!
//! Localnet only: its participants keep keys in Canton's software vault, which
//! only a `test-mode` build accepts for a new party key.

use std::{collections::BTreeSet, time::Duration};

use anyhow::Context;
use common::{
    canton_id::CantonId,
    types::{InvitationType, WorkflowKind, WorkflowProgress, WorkflowRole},
};
use dec_party_manager::{config::NodeConfig, utils, workflow::topology};
use serde_json::{Value, json};
use tracing::info;

use super::{deploy_gov_core::post_governance_rules, legacy_key_retirement::configs};
use crate::common::{
    Fixture, TestTarget, db,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: rotate_key");
    if !matches!(f.target, TestTarget::Localnet) {
        info!("rotate_key runs on localnet only; skipping");
        return Ok(());
    }

    let config = configs(f)?
        .into_iter()
        .next()
        .context("no node config for P1")?;
    let party: CantonId = f.party_id()?.parse()?;
    let before = party_key_fingerprints(&config, &party).await?;

    rotation().run(f).await?;

    let after = party_key_fingerprints(&config, &party).await?;
    let removed: Vec<&String> = before.difference(&after).collect();
    let added: Vec<&String> = after.difference(&before).collect();
    anyhow::ensure!(
        removed.len() == 1 && added.len() == 1,
        "the rotation must swap exactly one key, but it removed {removed:?} and added {added:?}"
    );
    let new_key = added[0].clone();

    for participant in [2u8, 3] {
        let recorded =
            db::cached_signing_key(&f.db_path(participant), f.party_id()?, &f.p1.participant_id)
                .await?;
        anyhow::ensure!(
            recorded.as_deref() == Some(new_key.as_str()),
            "P{participant} has {recorded:?} on record for P1, not the new key {new_key}"
        );
    }

    deploy_with_the_new_key().run(f).await?;

    let stored = db::stored_party_key_fingerprints(&f.db_path(1), f.party_id()?).await?;
    anyhow::ensure!(
        stored.contains(&new_key),
        "P1's key store holds {stored:?} for the party, not the new key {new_key}"
    );
    Ok(())
}

async fn party_key_fingerprints(
    config: &NodeConfig,
    party: &CantonId,
) -> anyhow::Result<BTreeSet<String>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    Ok(topology::fetch_p2p_mapping(config, &synchronizer_id, party)
        .await?
        .party_signing_keys
        .map(|keys| keys.keys)
        .unwrap_or_default()
        .iter()
        .map(|key| utils::compute_fingerprint(key).into())
        .collect())
}

fn rotation() -> Scenario<InvitationIds> {
    Scenario::with_ctx("rotate P1's Daml key", InvitationIds::default())
        .given("party present with all three members", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                Ok(())
            })
        })
        .when("P1 posts /rotate-key", |f, _| {
            Box::pin(async move {
                let req = json!({ "decentralized_party_id": f.party_id()? });
                let _: Value = f
                    .post_json(f.p1.http, "/rotate-key", &req)
                    .await
                    .context("POST /rotate-key")?;
                Ok(())
            })
        })
        .then(
            "RotateKey invitations visible on P2 and P3",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    if ctx.p2.is_none() {
                        ctx.p2 =
                            probe_pending_invitation(f, f.p2.http, InvitationType::RotateKey).await;
                    }
                    if ctx.p3.is_none() {
                        ctx.p3 =
                            probe_pending_invitation(f, f.p3.http, InvitationType::RotateKey).await;
                    }
                    (ctx.p2.is_some() && ctx.p3.is_some()).then_some(Ok(()))
                })
            },
        )
        .when("P2 and P3 accept", |f, ctx| {
            Box::pin(async move {
                let p2 = ctx.p2.clone().context("P2 invitation id not set")?;
                let p3 = ctx.p3.clone().context("P3 invitation id not set")?;
                let (r2, r3) = tokio::join!(
                    post_accept_invitation(f, f.p2.http, &p2),
                    post_accept_invitation(f, f.p3.http, &p3)
                );
                r2.context("accept RotateKey on P2")?;
                r3.context("accept RotateKey on P3")
            })
        })
        .then(
            "rotate-key workflow reaches completed",
            Duration::from_secs(240),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_status(&*f, f.p1.http, "/rotate-key/status", "rotate-key").await
                })
            },
        )
        .then(
            "RotateKey completed run visible in /workflows on P2 (Peer)",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_run_visible(
                        f,
                        f.p2.http,
                        WorkflowKind::RotateKey,
                        WorkflowRole::Peer,
                        WorkflowProgress::Completed,
                    )
                    .await
                })
            },
        )
}

fn deploy_with_the_new_key() -> Scenario<InvitationIds> {
    Scenario::with_ctx("deploy with the rotated key", InvitationIds::default())
        .when("P1 posts a GovernanceRules contract", |f, _| {
            Box::pin(async move {
                let party_id = f.party_id()?.to_string();
                post_governance_rules(&*f, &party_id, "governance-rules-rotated").await
            })
        })
        .then(
            "Contracts invitations visible on P2 and P3",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    if ctx.p2.is_none() {
                        ctx.p2 =
                            probe_pending_invitation(f, f.p2.http, InvitationType::Contracts).await;
                    }
                    if ctx.p3.is_none() {
                        ctx.p3 =
                            probe_pending_invitation(f, f.p3.http, InvitationType::Contracts).await;
                    }
                    (ctx.p2.is_some() && ctx.p3.is_some()).then_some(Ok(()))
                })
            },
        )
        .when("P2 and P3 accept", |f, ctx| {
            Box::pin(async move {
                let p2 = ctx.p2.clone().context("P2 invitation id not set")?;
                let p3 = ctx.p3.clone().context("P3 invitation id not set")?;
                let (r2, r3) = tokio::join!(
                    post_accept_invitation(f, f.p2.http, &p2),
                    post_accept_invitation(f, f.p3.http, &p3)
                );
                r2.context("accept Contracts on P2")?;
                r3.context("accept Contracts on P3")
            })
        })
        .then(
            "contracts workflow reaches completed",
            Duration::from_secs(240),
            |f, _| {
                Box::pin(async move {
                    probe_workflow_status(&*f, f.p1.http, "/contracts/status", "contracts").await
                })
            },
        )
}
