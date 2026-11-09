//! Deploy with Canton's key download off (#608).
//!
//! Canton 3.7 (Splice 0.10) exports a vault key only while the participant
//! sets `enable-key-download`, and Canton's configuration reference says not
//! to set it in production. The localnet turns it on, so a party onboarded
//! there can make its first deploy. This phase turns it off, restarts Canton,
//! and deploys once more for the shared party. Both remaining members hold
//! their keys in DecMan's key store by now, so the deploy must succeed with no
//! export.
//!
//! It runs last because it restarts Canton and leaves key download off. By
//! then `add_party_missing_dar` has kicked P3, so the party is P1 and P2, and
//! both signed that phase's deploy.

use std::{path::Path, time::Duration};

use anyhow::Context;
use common::{canton_id::CantonId, types::InvitationType};
use dec_party_manager::{
    config::NodeConfig,
    signing::{
        SigningKeyContext,
        vault_export::{export_private_key, export_protocol_version, key_download_disabled},
    },
    utils,
    workflow::{signing_keys::vault_holds, topology},
};
use serde_json::{Value, json};
use tracing::info;

use super::legacy_key_retirement::configs;
use crate::common::{
    Fixture, TestTarget, chaos, db,
    http::probe_workflow_status,
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

/// What `VaultExportSigner` logs when Canton refuses an export, followed by
/// the key's fingerprint.
const EXPORT_ATTEMPT: &str = "Attempted fingerprint: ";

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: key_download_off");
    if !matches!(f.target, TestTarget::Localnet) || !exports_need_key_download() {
        info!("key_download_off needs a Splice 0.10 localnet; skipping");
        return Ok(());
    }

    let party_id = f.party_id()?.to_string();
    let mut stored = Vec::new();
    for participant in [1u8, 2] {
        let keys = db::stored_party_key_fingerprints(&f.db_path(participant), &party_id).await?;
        anyhow::ensure!(
            !keys.is_empty(),
            "P{participant} holds no key for {party_id} in its key store, so this phase \
             cannot show a deploy that needs no export"
        );
        stored.extend(keys);
    }

    turn_key_download_off().await?;
    chaos::ensure_nodes_healthy(f).await?;
    let config = configs(f)?
        .into_iter()
        .next()
        .context("no node config for P1")?;
    let party: CantonId = party_id.parse()?;
    chaos::poll_until(Duration::from_secs(180), || {
        export_is_refused(&config, &party)
    })
    .await
    .context("Canton still exports P1's key after the restart")?;

    let offsets = [log_len(f, 1).await?, log_len(f, 2).await?];
    deploy().run(f).await?;

    for (index, offset) in offsets.into_iter().enumerate() {
        let participant = index + 1;
        let log = log_since(f, participant, offset).await?;
        if let Some(fingerprint) = stored
            .iter()
            .find(|fingerprint| log.contains(&format!("{EXPORT_ATTEMPT}{fingerprint}")))
        {
            anyhow::bail!(
                "P{participant} tried to export {fingerprint} for {party_id} during the deploy"
            );
        }
    }
    Ok(())
}

/// Splice 0.9 runs Canton 3.6, which exports keys without the setting.
fn exports_need_key_download() -> bool {
    let version = std::env::var("LOCALNET_VERSION").unwrap_or_else(|_| "0.10.0".to_string());
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    (major, minor) >= (0, 10)
}

async fn turn_key_download_off() -> anyhow::Result<()> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../integration-tests/localnet-key-download-off.sh");
    let status = tokio::process::Command::new("bash")
        .arg(&script)
        .status()
        .await
        .with_context(|| format!("running {}", script.display()))?;
    anyhow::ensure!(status.success(), "{} failed: {status}", script.display());
    Ok(())
}

/// Whether Canton now refuses to export P1's Daml key for `party`. Errors
/// other than that refusal, such as a participant still starting, count as
/// not yet.
async fn export_is_refused(config: &NodeConfig, party: &CantonId) -> anyhow::Result<bool> {
    let attempt = async {
        let synchronizer_id = utils::get_synchronizer_id(config).await?;
        let keys = topology::fetch_p2p_mapping(config, &synchronizer_id, party)
            .await?
            .party_signing_keys
            .map(|keys| keys.keys)
            .unwrap_or_default();
        for public_key in keys {
            let fingerprint = utils::compute_fingerprint(&public_key);
            if !vault_holds(config, &fingerprint).await? {
                continue;
            }
            let key = SigningKeyContext {
                fingerprint,
                public_key,
                kms_key_id: None,
            };
            let protocol_version = export_protocol_version(config).await?;
            let channel = config.admin_channel().await?;
            return Ok::<_, anyhow::Error>(Some(
                export_private_key(&channel, &key, protocol_version).await,
            ));
        }
        Ok(None)
    };
    match attempt.await {
        Ok(Some(Err(e))) if key_download_disabled(&e) => Ok(true),
        Ok(Some(Ok(_))) => {
            info!("Canton still exported P1's key; waiting for the restart to apply");
            Ok(false)
        }
        Ok(None) => anyhow::bail!("P1's vault holds none of {party}'s signing keys"),
        Ok(Some(Err(e))) => {
            info!("Export check not ready yet: {e}");
            Ok(false)
        }
        Err(e) => {
            info!("Export check not ready yet: {e:#}");
            Ok(false)
        }
    }
}

fn log_path(f: &Fixture, participant: usize) -> std::path::PathBuf {
    f.dev_dir
        .join(format!("participant-{participant}"))
        .join("stderr.log")
}

async fn log_len(f: &Fixture, participant: usize) -> anyhow::Result<u64> {
    Ok(tokio::fs::metadata(log_path(f, participant)).await?.len())
}

async fn log_since(f: &Fixture, participant: usize, offset: u64) -> anyhow::Result<String> {
    let bytes = tokio::fs::read(log_path(f, participant)).await?;
    let start = usize::try_from(offset)?.min(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

/// One more contract for the party, signed by P1 and P2.
fn deploy() -> Scenario<InvitationIds> {
    Scenario::with_ctx("deploy with key download off", InvitationIds::default())
        .when("P1 posts one OrphanMarker contract", |f, _| {
            Box::pin(async move {
                let p1m = f.p1_member_party.clone().context("p1 member party")?;
                let p2m = f.p2_member_party.clone().context("p2 member party")?;
                let req = json!({
                    "decentralized_party_id": f.party_id()?,
                    "participant_ids": [f.p1.participant_id.clone(), f.p2.participant_id.clone()],
                    "participant_parties": [&p1m, &p2m],
                    "operator_party": &p1m,
                    "contracts": [{
                        "id": "orphan-key-download-off",
                        "name": "OrphanMarker",
                        "package_id": "#orphan-marker",
                        "module_name": "OrphanMarker",
                        "entity_name": "OrphanMarker",
                        "fields": [
                            {"type": "decentralized_party"},
                            {"type": "int64", "value": 608},
                        ],
                    }],
                });
                let _: Value = f
                    .post_json(f.p1.http, "/contracts", &req)
                    .await
                    .context("POST /contracts")?;
                Ok(())
            })
        })
        .then(
            "Contracts invitation visible on P2",
            Duration::from_secs(60),
            |f, ctx| {
                Box::pin(async move {
                    ctx.p2 = Some(
                        probe_pending_invitation(f, f.p2.http, InvitationType::Contracts).await?,
                    );
                    Some(Ok(()))
                })
            },
        )
        .when("P2 accepts", |f, ctx| {
            Box::pin(async move {
                let id = ctx.p2.clone().context("P2 invitation id not set")?;
                post_accept_invitation(f, f.p2.http, &id)
                    .await
                    .context("accept Contracts on P2")
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
