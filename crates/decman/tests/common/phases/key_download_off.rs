//! Deploy and rotate with Canton's key download off (#608, #609).
//!
//! Canton 3.7 (Splice 0.10) exports a vault key only while the participant
//! sets `enable-key-download`, and Canton's configuration reference says not
//! to set it in production. This phase turns it off and restarts Canton.
//!
//! P1 and P2 have no KMS and run with `DECPM_ALLOW_SOFTWARE_PARTY_KEYS`, so
//! DecMan generated their Daml keys for the shared party, and Canton's vault
//! never held them. A deploy must therefore succeed with no export.
//!
//! P1 then rotates its key. DecMan generates the new key too, so the rotation
//! completes without an export, and a deploy that P1 coordinates signs with
//! the new key.
//!
//! It runs last because it restarts Canton and leaves key download off. By
//! then `add_party_missing_dar` has kicked P3, so the party is P1 and P2, and
//! both signed that phase's deploy.

use std::{path::Path, time::Duration};

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::crypto::{
    admin::v30::{
        ListMyKeysRequest, private_key_metadata, vault_service_client::VaultServiceClient,
    },
    v30::public_key,
};
use common::{canton_id::CantonId, types::InvitationType};
use dec_party_manager::{
    config::NodeConfig,
    signing::{
        SigningKeyContext,
        vault_export::{export_private_key, export_protocol_version, key_download_disabled},
    },
    utils,
};
use serde_json::{Value, json};
use tracing::info;

use super::legacy_key_retirement::configs;
use crate::common::{
    Fixture, TestTarget, chaos, db,
    http::probe_workflow_status,
    invitations::{
        InvitationIds, post_accept_invitation, probe_pending_invitation, probe_run_invitation,
    },
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
    chaos::poll_until(Duration::from_secs(180), || export_is_refused(&config))
        .await
        .context("Canton still exports P1's keys after the restart")?;
    for (participant, port) in [(1, f.p1.http), (2, f.p2.http)] {
        let f = &*f;
        chaos::poll_until(Duration::from_secs(180), || async move {
            Ok(synchronizer_connected(f, port).await)
        })
        .await
        .with_context(|| format!("P{participant} did not reconnect to the synchronizer"))?;
    }

    let offsets = [log_len(f, 1).await?, log_len(f, 2).await?];
    deploy("orphan-key-download-off").run(f).await?;
    no_export_attempt(f, offsets, &stored, "the deploy").await?;

    let offsets = [log_len(f, 1).await?, log_len(f, 2).await?];
    let new_key = a_rotation_needs_no_export(f, &config, &party).await?;
    deploy("orphan-rotated-key-download-off").run(f).await?;
    stored.push(new_key);
    no_export_attempt(f, offsets, &stored, "the rotation and the deploy after it").await
}

/// Fail when a node logged an export attempt for one of `keys` since
/// `offsets`.
async fn no_export_attempt(
    f: &Fixture,
    offsets: [u64; 2],
    keys: &[String],
    what: &str,
) -> anyhow::Result<()> {
    for (index, offset) in offsets.into_iter().enumerate() {
        let participant = index + 1;
        let log = log_since(f, participant, offset).await?;
        if let Some(fingerprint) = keys
            .iter()
            .find(|fingerprint| log.contains(&format!("{EXPORT_ATTEMPT}{fingerprint}")))
        {
            anyhow::bail!("P{participant} tried to export {fingerprint} during {what}");
        }
    }
    Ok(())
}

/// P1 rotates its key while Canton refuses exports. DecMan generates the new
/// key, so the rotation completes, and P1's key store holds the new key.
/// Returns the new key's fingerprint.
async fn a_rotation_needs_no_export(
    f: &Fixture,
    config: &NodeConfig,
    party: &CantonId,
) -> anyhow::Result<String> {
    let before = super::rotate_key::party_key_fingerprints(config, party).await?;
    let p1_db = f.db_path(1);
    let previous = db::latest_coordinator_instance(&p1_db, "RotateKey").await?;
    let _: Value = f
        .post_json(
            f.p1.http,
            "/rotate-key",
            &json!({ "decentralized_party_id": party.to_string() }),
        )
        .await
        .context("POST /rotate-key on P1")?;

    let invitation = chaos::poll_for(Duration::from_secs(60), || {
        let (p1_db, previous) = (&p1_db, &previous);
        async move {
            let Some(run) = db::latest_coordinator_instance(p1_db, "RotateKey").await? else {
                return Ok(None);
            };
            if Some(&run) == previous.as_ref() {
                return Ok(None);
            }
            Ok(probe_run_invitation(f, f.p2.http, &run).await)
        }
    })
    .await
    .context("the rotation's invitation never reached P2")?;
    post_accept_invitation(f, f.p2.http, &invitation)
        .await
        .context("accept RotateKey on P2")?;

    chaos::poll_for(Duration::from_secs(240), || async move {
        match probe_workflow_status(f, f.p1.http, "/rotate-key/status", "rotate-key").await {
            Some(Err(e)) => anyhow::bail!("the rotation with key download off failed: {e:#}"),
            Some(Ok(())) => Ok(Some(())),
            None => Ok(None),
        }
    })
    .await
    .context("the rotation with key download off never completed")?;

    let after = super::rotate_key::party_key_fingerprints(config, party).await?;
    let added: Vec<&String> = after.difference(&before).collect();
    let removed: Vec<&String> = before.difference(&after).collect();
    let ([new_key], [_]) = (added.as_slice(), removed.as_slice()) else {
        anyhow::bail!(
            "the rotation must swap exactly one key, but it removed {removed:?} and added \
             {added:?}"
        );
    };
    let stored = db::stored_party_key_fingerprints(&f.db_path(1), &party.to_string()).await?;
    anyhow::ensure!(
        stored.contains(*new_key),
        "P1's key store holds {stored:?} for the party, not the new key {new_key}"
    );
    info!("P1 rotated to {new_key} with key download off");
    Ok((*new_key).clone())
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

/// Whether Canton now refuses to export a key from P1's software vault. The
/// party's keys are not in the vault, so any software key there serves.
/// Errors other than that refusal, such as a participant still starting,
/// count as not yet.
async fn export_is_refused(config: &NodeConfig) -> anyhow::Result<bool> {
    let attempt = async {
        let response = VaultServiceClient::new(config.admin_channel().await?)
            .list_my_keys(ListMyKeysRequest {
                filters: None,
                base_request: None,
            })
            .await?
            .into_inner();
        let software_key = response.private_keys_metadata.into_iter().find_map(|meta| {
            if meta.kms_key_id.as_deref().is_some_and(|id| !id.is_empty()) {
                return None;
            }
            let Some(private_key_metadata::PublicKeyWithName::V30(named)) =
                meta.public_key_with_name
            else {
                return None;
            };
            match named.public_key?.key? {
                public_key::Key::SigningPublicKey(key) => Some(key),
                _ => None,
            }
        });
        let Some(public_key) = software_key else {
            return Ok::<_, anyhow::Error>(None);
        };
        let key = SigningKeyContext {
            fingerprint: utils::compute_fingerprint(&public_key),
            public_key,
            kms_key_id: None,
        };
        let protocol_version = export_protocol_version(config).await?;
        let channel = config.admin_channel().await?;
        Ok(Some(
            export_private_key(&channel, &key, protocol_version).await,
        ))
    };
    match attempt.await {
        Ok(Some(Err(e))) if key_download_disabled(&e) => Ok(true),
        Ok(Some(Ok(_))) => {
            info!("Canton still exported a key of P1; waiting for the restart to apply");
            Ok(false)
        }
        Ok(None) => anyhow::bail!("P1's vault holds no software signing key to probe"),
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

/// Whether the node reports a healthy synchronizer connection. The restart
/// drops it, and a deploy before it comes back would fail for that reason.
async fn synchronizer_connected(f: &Fixture, port: u16) -> bool {
    let Some(health) = f.probe_get_json::<Value>(port, "/node-health").await else {
        return false;
    };
    health["synchronizers"]
        .as_array()
        .is_some_and(|all| all.iter().any(|s| s["healthy"] == Value::Bool(true)))
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

/// One more contract for the party, signed by P1 and P2. P1 coordinates, and
/// a coordinator always signs.
fn deploy(id: &'static str) -> Scenario<InvitationIds> {
    Scenario::with_ctx("deploy with key download off", InvitationIds::default())
        .when("P1 posts one OrphanMarker contract", move |f, _| {
            Box::pin(async move {
                let p1m = f.p1_member_party.clone().context("p1 member party")?;
                let p2m = f.p2_member_party.clone().context("p2 member party")?;
                let req = json!({
                    "decentralized_party_id": f.party_id()?,
                    "participant_ids": [f.p1.participant_id.clone(), f.p2.participant_id.clone()],
                    "participant_parties": [&p1m, &p2m],
                    "operator_party": &p1m,
                    "contracts": [{
                        "id": id,
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
