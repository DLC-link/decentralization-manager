//! P3's party keys live in AWS KMS, and P3 signs through `AwsKmsSigner`.
//!
//! On localnet the SV participant, which P3 drives, uses Canton's AWS KMS
//! crypto provider against a mock KMS (`integration-tests/localnet-kms.yaml`).
//! This phase checks the whole KMS path on the shared party:
//!
//! 1. Every key in P3's vault carries a `kms_key_id`, and so does P3's Daml
//!    key for the party. DecMan's key store on P3 holds no copy of it.
//! 2. P1 deploys a contract and only P3 accepts. At threshold 2 the
//!    submission needs P3's signature, and P3 can only make it with the KMS
//!    `Sign` API. The mock logs a `Sign` call for the key's KMS id.
//! 3. P3 rotates its Daml key. The new key is a KMS key too, and the other
//!    members record it. A second deploy that only P3 accepts signs with it.
//!
//! Localnet only, because only localnet runs the mock. It needs the party with
//! all three members, so it runs before `kick_owner_only` and the chaos block.

use std::time::Duration;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::crypto::admin::v30::{
    ListKeysFilters, ListMyKeysRequest, PrivateKeyMetadata,
    private_key_metadata::PublicKeyWithName, vault_service_client::VaultServiceClient,
};
use common::{
    canton_id::CantonId,
    types::{WorkflowKind, WorkflowProgress, WorkflowRole},
};
use dec_party_manager::{
    config::NodeConfig,
    utils,
    workflow::{signing_keys::vault_holds, topology},
};
use serde_json::{Value, json};
use tracing::info;

use super::{deploy_gov_core::post_governance_rules, legacy_key_retirement::configs};
use crate::common::{
    Fixture, TestTarget, chaos, db,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_run_invitation},
    scenario::Scenario,
};

/// The mock KMS container that `integration-tests/localnet-kms.yaml` names.
const KMS_CONTAINER: &str = "kms";

/// P3's Daml key for the party, as its vault reports it.
struct KmsKey {
    fingerprint: String,
    kms_key_id: String,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: kms_party_keys");
    if !matches!(f.target, TestTarget::Localnet) {
        info!("kms_party_keys needs the localnet's mock KMS; skipping");
        return Ok(());
    }
    chaos::ensure_nodes_healthy(f).await?;

    let p3 = configs(f)?
        .into_iter()
        .nth(2)
        .context("no node config for P3")?;
    let party: CantonId = f.party_id()?.parse()?;

    let vault = list_keys(&p3, "").await?;
    let outside_kms: Vec<&str> = vault
        .iter()
        .filter(|key| kms_key_id(key).is_none())
        .map(key_name)
        .collect();
    anyhow::ensure!(
        !vault.is_empty() && outside_kms.is_empty(),
        "P3's participant must keep every key in KMS, but these have no kms_key_id: \
         {outside_kms:?}"
    );

    let key = p3_party_key(&p3, &party).await?;
    info!(
        "P3's Daml key {fingerprint} lives in KMS key {kms_key_id}",
        fingerprint = key.fingerprint,
        kms_key_id = key.kms_key_id
    );
    ensure_not_stored(f, &key).await?;

    let signs = kms_sign_count(&key.kms_key_id).await?;
    deploy_signed_by_p3("governance-rules-kms").run(f).await?;
    ensure_kms_signed(&key, signs).await?;
    ensure_not_stored(f, &key).await?;

    rotation().run(f).await?;
    let rotated = p3_party_key(&p3, &party).await?;
    anyhow::ensure!(
        rotated.fingerprint != key.fingerprint && rotated.kms_key_id != key.kms_key_id,
        "the rotation left P3 with its old key {fingerprint}",
        fingerprint = key.fingerprint
    );
    for participant in [1u8, 2] {
        let recorded =
            db::cached_signing_key(&f.db_path(participant), f.party_id()?, &f.p3.participant_id)
                .await?;
        anyhow::ensure!(
            recorded.as_deref() == Some(rotated.fingerprint.as_str()),
            "P{participant} has {recorded:?} on record for P3, not the rotated key {fingerprint}",
            fingerprint = rotated.fingerprint
        );
    }

    let signs = kms_sign_count(&rotated.kms_key_id).await?;
    deploy_signed_by_p3("governance-rules-kms-rotated")
        .run(f)
        .await?;
    ensure_kms_signed(&rotated, signs).await?;
    ensure_not_stored(f, &rotated).await
}

/// The keys in a participant's vault, all of them when `fingerprint` is empty.
async fn list_keys(
    config: &NodeConfig,
    fingerprint: &str,
) -> anyhow::Result<Vec<PrivateKeyMetadata>> {
    Ok(VaultServiceClient::new(config.admin_channel().await?)
        .list_my_keys(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: fingerprint.to_string(),
                ..Default::default()
            }),
            base_request: None,
        })
        .await?
        .into_inner()
        .private_keys_metadata)
}

fn kms_key_id(key: &PrivateKeyMetadata) -> Option<&str> {
    key.kms_key_id.as_deref().filter(|id| !id.is_empty())
}

fn key_name(key: &PrivateKeyMetadata) -> &str {
    match &key.public_key_with_name {
        Some(PublicKeyWithName::V30(named)) => named.name.as_str(),
        None => "<unnamed>",
    }
}

/// The one key in the party's P2P mapping that P3's vault holds, with the KMS
/// id its vault metadata carries.
async fn p3_party_key(p3: &NodeConfig, party: &CantonId) -> anyhow::Result<KmsKey> {
    let synchronizer_id = utils::get_synchronizer_id(p3).await?;
    let keys = topology::fetch_p2p_mapping(p3, &synchronizer_id, party)
        .await?
        .party_signing_keys
        .map(|keys| keys.keys)
        .unwrap_or_default();
    let mut held = Vec::new();
    for public_key in keys {
        let fingerprint = utils::compute_fingerprint(&public_key);
        if vault_holds(p3, &fingerprint).await? {
            held.push(fingerprint.to_string());
        }
    }
    let [fingerprint] = <[String; 1]>::try_from(held).map_err(|held| {
        anyhow::anyhow!("P3's vault must hold exactly one of {party}'s Daml keys, not {held:?}")
    })?;
    let metadata = list_keys(p3, &fingerprint).await?;
    let kms_key_id = metadata
        .iter()
        .find_map(kms_key_id)
        .with_context(|| format!("P3's Daml key {fingerprint} for {party} has no kms_key_id"))?
        .to_string();
    Ok(KmsKey {
        fingerprint,
        kms_key_id,
    })
}

/// A KMS key never reaches DecMan's key store, because it cannot be exported.
async fn ensure_not_stored(f: &Fixture, key: &KmsKey) -> anyhow::Result<()> {
    let stored = db::stored_party_key_fingerprints(&f.db_path(3), f.party_id()?).await?;
    anyhow::ensure!(
        !stored.contains(&key.fingerprint),
        "P3's key store holds a copy of its KMS key {fingerprint}",
        fingerprint = key.fingerprint
    );
    Ok(())
}

/// How many `Sign` calls the mock KMS has logged for `kms_key_id`.
///
/// local-kms logs each one as "RAW message signed with ECC_NIST_P256, using
/// key arn:aws:kms:<region>:<account>:key/<id>".
async fn kms_sign_count(kms_key_id: &str) -> anyhow::Result<usize> {
    let output = tokio::process::Command::new("docker")
        .args(["logs", KMS_CONTAINER])
        .output()
        .await
        .context("docker logs on the mock KMS")?;
    anyhow::ensure!(
        output.status.success(),
        "docker logs {KMS_CONTAINER} failed: {stderr}",
        stderr = String::from_utf8_lossy(&output.stderr)
    );
    let key = format!("key/{kms_key_id}");
    let logs = [&output.stdout, &output.stderr].map(|stream| String::from_utf8_lossy(stream));
    Ok(logs
        .iter()
        .flat_map(|stream| stream.lines())
        .filter(|line| line.contains("message signed") && line.contains(&key))
        .count())
}

/// The deploy just completed, and only P3's KMS key could have signed for P3.
/// The mock must have logged at least one new `Sign` call for it.
async fn ensure_kms_signed(key: &KmsKey, before: usize) -> anyhow::Result<()> {
    let after = kms_sign_count(&key.kms_key_id).await?;
    anyhow::ensure!(
        after > before,
        "the deploy completed, but the mock KMS logged no Sign call for P3's key {kms_key_id}",
        kms_key_id = key.kms_key_id
    );
    info!(
        "The mock KMS signed {count} hash(es) with P3's key {kms_key_id}",
        count = after - before,
        kms_key_id = key.kms_key_id
    );
    Ok(())
}

/// P1 deploys a GovernanceRules contract, and only P3 accepts. P2 declines
/// once the run completes, so no later phase finds its invitation.
fn deploy_signed_by_p3(contract_id: &'static str) -> Scenario<InvitationIds> {
    Scenario::with_ctx(
        "deploy that P3 signs with its KMS key",
        InvitationIds::default(),
    )
    .when("P1 posts a GovernanceRules contract", move |f, _| {
        Box::pin(async move {
            let party_id = f.party_id()?.to_string();
            post_governance_rules(&*f, &party_id, contract_id).await
        })
    })
    .then(
        "the run's invitations are visible on P2 and P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let run = match db::latest_coordinator_instance(&f.db_path(1), "Contracts").await {
                    Ok(run) => run?,
                    Err(e) => return Some(Err(e)),
                };
                if ctx.p2.is_none() {
                    ctx.p2 = probe_run_invitation(f, f.p2.http, &run).await;
                }
                if ctx.p3.is_none() {
                    ctx.p3 = probe_run_invitation(f, f.p3.http, &run).await;
                }
                (ctx.p2.is_some() && ctx.p3.is_some()).then_some(Ok(()))
            })
        },
    )
    .when("only P3 accepts", |f, ctx| {
        Box::pin(async move {
            let p3 = ctx.p3.clone().context("P3 invitation id not set")?;
            post_accept_invitation(f, f.p3.http, &p3)
                .await
                .context("accept Contracts on P3")
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
    // A 200 removes the invitation, and a 404 means it is already gone.
    .when("P2 declines its invitation", |f, ctx| {
        Box::pin(async move {
            let p2 = ctx.p2.clone().context("P2 invitation id not set")?;
            let (status, body) = f
                .post_expect_status(f.p2.http, "/invitations/decline", &json!({ "id": p2 }))
                .await?;
            anyhow::ensure!(
                status.is_success() || status == reqwest::StatusCode::NOT_FOUND,
                "declining P2's Contracts invitation returned {status}: {body}"
            );
            Ok(())
        })
    })
}

/// The invitations P3's rotation sends to the other two members.
#[derive(Default)]
struct RotationInvitations {
    p1: Option<String>,
    p2: Option<String>,
}

/// P3 rotates its Daml key, and P1 and P2 accept.
fn rotation() -> Scenario<RotationInvitations> {
    Scenario::with_ctx(
        "P3 rotates its KMS Daml key",
        RotationInvitations::default(),
    )
    .when("P3 posts /rotate-key", |f, _| {
        Box::pin(async move {
            let req = json!({ "decentralized_party_id": f.party_id()? });
            let _: Value = f
                .post_json(f.p3.http, "/rotate-key", &req)
                .await
                .context("POST /rotate-key on P3")?;
            Ok(())
        })
    })
    .then(
        "the run's invitations are visible on P1 and P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let run = match db::latest_coordinator_instance(&f.db_path(3), "RotateKey").await {
                    Ok(run) => run?,
                    Err(e) => return Some(Err(e)),
                };
                if ctx.p1.is_none() {
                    ctx.p1 = probe_run_invitation(f, f.p1.http, &run).await;
                }
                if ctx.p2.is_none() {
                    ctx.p2 = probe_run_invitation(f, f.p2.http, &run).await;
                }
                (ctx.p1.is_some() && ctx.p2.is_some()).then_some(Ok(()))
            })
        },
    )
    .when("P1 and P2 accept", |f, ctx| {
        Box::pin(async move {
            let p1 = ctx.p1.clone().context("P1 invitation id not set")?;
            let p2 = ctx.p2.clone().context("P2 invitation id not set")?;
            let (r1, r2) = tokio::join!(
                post_accept_invitation(f, f.p1.http, &p1),
                post_accept_invitation(f, f.p2.http, &p2)
            );
            r1.context("accept RotateKey on P1")?;
            r2.context("accept RotateKey on P2")
        })
    })
    .then(
        "rotate-key workflow reaches completed",
        Duration::from_secs(240),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(&*f, f.p3.http, "/rotate-key/status", "rotate-key").await
            })
        },
    )
    .then(
        "RotateKey completed run visible in /workflows on P1 (Peer)",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(
                    f,
                    f.p1.http,
                    WorkflowKind::RotateKey,
                    WorkflowRole::Peer,
                    WorkflowProgress::Completed,
                )
                .await
            })
        },
    )
}
