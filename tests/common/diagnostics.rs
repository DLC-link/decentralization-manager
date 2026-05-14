//! Diagnostic harness for the owner_key resolution issue tracked in #149.
//!
//! Enabled by setting the env var `DPM_IT_OWNER_KEY_SNAPSHOTS=1`. Both
//! `devnet.env.sh` and `env.sh` (localnet) export this so the same run
//! produces directly-comparable output on both targets — that side-by-side
//! diff was how the #149 diagnosis was pinned down. Localnet runs do emit a
//! file with this flag set; remove the export from `env.sh` if you'd prefer
//! localnet runs stay quiet.
//!
//! Each call records:
//!
//! - A one-line `tracing::info!` summary keyed on the calling phase, showing
//!   whether each of P1/P2/P3 has resolved every peer's owner_key in its
//!   `/decentralized-parties` view of the current dec party.
//! - A JSONL entry appended to
//!   `/tmp/owner-key-snapshots-{target}-{run_id}.jsonl` (not `$DEV_DIR` —
//!   `env.sh`'s cleanup() rm -rf's that on localnet at end of run, which
//!   would wipe the file before inspection).
//!
//! Best-effort: errors are logged at WARN and swallowed so this never fails
//! a test. The caller does `let _ = snapshot_owner_keys(...).await;`.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::common::{Fixture, types::DecentralizedPartiesResponse};

fn enabled() -> bool {
    std::env::var("DPM_IT_OWNER_KEY_SNAPSHOTS")
        .ok()
        .is_some_and(|v| !v.is_empty() && v != "0")
}

pub async fn snapshot_owner_keys(f: &Fixture, phase: &str) {
    if !enabled() {
        return;
    }
    if let Err(e) = inner(f, phase).await {
        warn!("owner_key snapshot failed for phase={phase}: {e:#}");
    }
}

async fn inner(f: &Fixture, phase: &str) -> Result<()> {
    let Some(prefix) = f.party_prefix.as_deref() else {
        return Ok(());
    };
    let path = format!("/decentralized-parties?prefix={prefix}");

    let nodes = [("P1", f.p1.http), ("P2", f.p2.http), ("P3", f.p3.http)];
    let p3_uid = f.p3.participant_id.as_str();

    let mut observations: Vec<Value> = Vec::with_capacity(3);
    let mut summary: Vec<String> = Vec::with_capacity(3);

    for (label, port) in nodes {
        let observation = match f.get_json::<DecentralizedPartiesResponse>(port, &path).await {
            Ok(r) => observation_for_party(label, prefix, p3_uid, r, &mut summary),
            Err(e) => {
                summary.push(format!("{label}=err"));
                json!({"from_node": label, "error": format!("{e:#}")})
            }
        };
        observations.push(observation);
    }

    info!(
        "[owner-key snapshot] phase={phase} (P3-owner_key seen by each node) {}",
        summary.join(" ")
    );

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = json!({
        "phase": phase,
        "ts_unix": ts,
        "party_id": f.party_id.as_deref().unwrap_or(""),
        "p3_uid": p3_uid,
        "observations": observations,
    });

    // Write to /tmp (not DEV_DIR) because env.sh's cleanup() rm -rf's the
    // localnet DEV_DIR at end-of-run, wiping the file before we can inspect.
    // The path encodes target+run_id so concurrent localnet/devnet runs don't
    // clobber each other.
    let target = std::env::var("DPM_IT_TARGET").unwrap_or_else(|_| "localnet".into());
    let path = std::path::PathBuf::from(format!(
        "/tmp/owner-key-snapshots-{target}-{run_id}.jsonl",
        run_id = f.run_id
    ));
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    let line = format!("{}\n", serde_json::to_string(&entry)?);
    file.write_all(line.as_bytes()).await?;
    Ok(())
}

fn observation_for_party(
    label: &str,
    prefix: &str,
    p3_uid: &str,
    r: DecentralizedPartiesResponse,
    summary: &mut Vec<String>,
) -> Value {
    let refreshing = r.refreshing;
    let Some(party) = r.parties.into_iter().find(|p| p.party_id.starts_with(prefix)) else {
        summary.push(format!("{label}=none"));
        return json!({"from_node": label, "party_found": false, "refreshing": refreshing});
    };
    let participants: Vec<Value> = party
        .participants
        .iter()
        .map(|pi| {
            json!({
                "uid": pi.participant_uid,
                "owner_key_set": pi.owner_key.is_some(),
            })
        })
        .collect();
    let p3_has = party
        .participants
        .iter()
        .find(|pi| pi.participant_uid == p3_uid)
        .map(|pi| pi.owner_key.is_some());
    summary.push(match p3_has {
        Some(true) => format!("{label}=Y"),
        Some(false) => format!("{label}=N"),
        None => format!("{label}=missing"),
    });
    json!({
        "from_node": label,
        "party_found": true,
        "refreshing": refreshing,
        "participants": participants,
    })
}
