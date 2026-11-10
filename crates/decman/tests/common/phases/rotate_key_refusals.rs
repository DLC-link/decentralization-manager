//! The ways a key rotation stops without changing the party.
//!
//! 1. A node without KMS and without `DECPM_ALLOW_SOFTWARE_PARTY_KEYS` refuses
//!    to start a rotation.
//! 2. The coordinator cancels a run before the members accept.
//! 3. The coordinator names another member's key as its own, through the
//!    test-mode fault file. Every peer refuses to sign, because its records
//!    name that key as the other member's.
//!
//! The party's keys must be the same after each case. Localnet only: the
//! fault file needs a `test-mode` build, and P2 must be a software-vault node.

use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use anyhow::Context;
use common::canton_id::CantonId;
use dec_party_manager::config::NodeConfig;
use serde_json::{Value, json};
use tracing::info;

use super::{legacy_key_retirement::configs, rotate_key::party_key_fingerprints};
use crate::common::{
    Fixture, TestTarget, chaos, db,
    invitations::{post_accept_invitation, probe_run_invitation},
    processes,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: rotate_key_refusals");
    if !matches!(f.target, TestTarget::Localnet) {
        info!("rotate_key_refusals needs the localnet's test-mode build; skipping");
        return Ok(());
    }
    chaos::ensure_nodes_healthy(f).await?;
    let config = configs(f)?
        .into_iter()
        .next()
        .context("no node config for P1")?;
    let party: CantonId = f.party_id()?.parse()?;
    let keys = party_key_fingerprints(&config, &party).await?;

    the_strict_rule_refuses_to_start(f).await?;

    cancel_before_the_members_accept(f).await?;
    ensure_unchanged(&config, &party, &keys, "the cancelled rotation").await?;

    the_peers_refuse_another_members_key(f).await?;
    ensure_unchanged(&config, &party, &keys, "the refused rotation").await
}

/// P2 keeps its keys in the software vault. Without the opt-out, DecMan must
/// refuse the rotation before it invites anyone.
async fn the_strict_rule_refuses_to_start(f: &mut Fixture) -> anyhow::Result<()> {
    let mut strict = f.node_spawn(2)?;
    strict.extra_env = vec![(
        "DECPM_ALLOW_SOFTWARE_PARTY_KEYS".to_string(),
        "false".to_string(),
    )];
    let current = f.current_pids[1].context("no tracked pid for P2")?;
    f.current_pids[1] = Some(processes::restart_node_explicit(&strict, current, f).await?);

    let outcome = async {
        let (status, body) = f
            .post_expect_status(
                f.p2.http,
                "/rotate-key",
                &json!({ "decentralized_party_id": f.party_id()? }),
            )
            .await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::BAD_REQUEST
                && body.contains("software vault")
                && body.contains("DECPM_ALLOW_SOFTWARE_PARTY_KEYS"),
            "P2 without KMS or the opt-out must refuse a rotation with 400, but returned \
             {status}: {body}"
        );
        info!("P2 refused the rotation under the strict rule: {body}");
        Ok(())
    }
    .await;

    // Restore P2's opt-out whether or not the check held, because the later
    // phases create keys on it.
    let restored = processes::restart_node(f, 2).await;
    outcome?;
    restored?;
    chaos::ensure_nodes_healthy(f).await
}

/// P1 starts a rotation and cancels it while P2 and P3 still hold their
/// invitations. The run ends cancelled, and the peers decline.
async fn cancel_before_the_members_accept(f: &mut Fixture) -> anyhow::Result<()> {
    let run = start_rotation_on_p1(f).await?;
    let (status, body) = f
        .post_expect_status(f.p1.http, "/rotate-key/cancel", &json!({}))
        .await?;
    anyhow::ensure!(
        status.is_success(),
        "/rotate-key/cancel returned {status}: {body}"
    );
    let p1_db = f.db_path(1);
    chaos::poll_until(Duration::from_secs(30), || async {
        Ok(
            db::workflow_run_status(&p1_db, &run.instance, "Coordinator")
                .await?
                .as_deref()
                == Some("cancelled"),
        )
    })
    .await
    .context("the cancelled rotation never reached cancelled")?;
    decline(f, f.p2.http, &run.p2).await?;
    decline(f, f.p3.http, &run.p3).await?;
    info!("P1 cancelled rotation {instance}", instance = run.instance);
    Ok(())
}

/// P1's invitation names P2's key as the key P1 replaces. P2 and P3 each have
/// that key on record as P2's, so neither signs, and the run cannot finish.
async fn the_peers_refuse_another_members_key(f: &mut Fixture) -> anyhow::Result<()> {
    let p2_key = db::cached_signing_key(&f.db_path(1), f.party_id()?, &f.p2.participant_id)
        .await?
        .context("P1 has no Daml key on record for P2")?;
    let fault = fault_file(f, 1);
    tokio::fs::write(&fault, &p2_key)
        .await
        .with_context(|| format!("writing {}", fault.display()))?;
    let started = start_rotation_on_p1(f).await;
    // Clear the fault before anything else can fail, so later rotations name
    // P1's own key again.
    tokio::fs::remove_file(&fault).await?;
    let run = started?;

    post_accept_invitation(f, f.p2.http, &run.p2)
        .await
        .context("accept the refused rotation on P2")?;
    post_accept_invitation(f, f.p3.http, &run.p3)
        .await
        .context("accept the refused rotation on P3")?;

    let p3_db = f.db_path(3);
    chaos::poll_until(Duration::from_secs(120), || async {
        Ok(
            db::peer_run_status_by_coordinator_instance(&p3_db, &run.instance)
                .await?
                .as_deref()
                == Some("failed"),
        )
    })
    .await
    .context("P3 never gave up on the rotation that replaces P2's key")?;
    let error = db::peer_run_error_by_coordinator_instance(&p3_db, &run.instance)
        .await?
        .unwrap_or_default();
    anyhow::ensure!(
        error.contains(&format!("The rotation replaces {p2_key}")),
        "P3 must refuse because it has {p2_key} on record as P2's key, but it recorded: {error}"
    );
    info!("P3 refused to sign: {error}");

    // The coordinator may still wait for signatures. Cancel it, so no later
    // phase finds a run in flight for the party.
    let (status, body) = f
        .post_expect_status(f.p1.http, "/rotate-key/cancel", &json!({}))
        .await?;
    anyhow::ensure!(
        status.is_success() || status == reqwest::StatusCode::CONFLICT,
        "/rotate-key/cancel returned {status}: {body}"
    );
    let p1_db = f.db_path(1);
    chaos::poll_until(Duration::from_secs(30), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &run.instance, "Coordinator")
                .await?
                .as_deref(),
            Some("cancelled" | "failed")
        ))
    })
    .await
    .context("the refused rotation is still in flight on P1")
}

/// A rotation that P1 started, with each peer's invitation id.
struct StartedRotation {
    instance: String,
    p2: String,
    p3: String,
}

/// Post /rotate-key on P1 and wait until both peers hold the new run's
/// invitation.
async fn start_rotation_on_p1(f: &Fixture) -> anyhow::Result<StartedRotation> {
    let p1_db = f.db_path(1);
    let before = db::latest_coordinator_instance(&p1_db, "RotateKey").await?;
    let _: Value = f
        .post_json(
            f.p1.http,
            "/rotate-key",
            &json!({ "decentralized_party_id": f.party_id()? }),
        )
        .await
        .context("POST /rotate-key on P1")?;

    chaos::poll_for(Duration::from_secs(60), || {
        let (p1_db, before) = (&p1_db, &before);
        async move {
            let Some(instance) = db::latest_coordinator_instance(p1_db, "RotateKey").await? else {
                return Ok(None);
            };
            if Some(&instance) == before.as_ref() {
                return Ok(None);
            }
            let p2 = probe_run_invitation(f, f.p2.http, &instance).await;
            let p3 = probe_run_invitation(f, f.p3.http, &instance).await;
            Ok(match (p2, p3) {
                (Some(p2), Some(p3)) => Some(StartedRotation { instance, p2, p3 }),
                _ => None,
            })
        }
    })
    .await
    .context("the rotation's invitations never reached P2 and P3")
}

async fn decline(f: &Fixture, port: u16, id: &str) -> anyhow::Result<()> {
    let (status, body) = f
        .post_expect_status(port, "/invitations/decline", &json!({ "id": id }))
        .await?;
    anyhow::ensure!(
        status.is_success() || status == reqwest::StatusCode::NOT_FOUND,
        "declining invitation {id} returned {status}: {body}"
    );
    Ok(())
}

async fn ensure_unchanged(
    config: &NodeConfig,
    party: &CantonId,
    before: &BTreeSet<String>,
    case: &str,
) -> anyhow::Result<()> {
    let after = party_key_fingerprints(config, party).await?;
    anyhow::ensure!(
        &after == before,
        "{case} changed the party's keys from {before:?} to {after:?}"
    );
    Ok(())
}

/// The file that `DECPM_TEST_ROTATE_KEY_REPLACE_FILE` names for a node.
fn fault_file(f: &Fixture, participant: u8) -> PathBuf {
    f.dev_dir
        .join(format!("participant-{participant}"))
        .join("rotate-key-replace")
}
