//! Shared scaffolding for chaos integration tests (kill-and-restart phases).
//!
//! All chaos phases follow the same shape: drive a workflow up to a chosen
//! mid-flight state, kill or pause one or more nodes, restart them (or not,
//! for "leave offline" tests), poll the DB-backed workflow_runs row for the
//! expected terminal status, and dismiss the leftover row before yielding to
//! the next phase. The helpers below make it cheap for each individual
//! chaos phase to focus on the part that's unique to it.

use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use serde_json::{Value, json};
use tokio::time::sleep;
use tracing::info;

use super::{Fixture, invitations::probe_pending_invitation};

/// Generate a stable, monotonically increasing prefix for a chaos phase. The
/// suffix is epoch-millis so two phases never collide even when they run
/// back-to-back inside the same e2e.
pub fn fresh_prefix(label: &str) -> String {
    let suffix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("{label}-{suffix}")
}

/// POST /onboarding on P1, expecting the row to be persisted asynchronously.
/// Used by chaos phases that drive a fresh onboarding and don't care about
/// the response shape beyond success.
///
/// The handler runs a peer-mesh pre-flight that can transiently 422 right
/// after a chaos phase has killed/restarted peers — the new Noise sessions
/// take a few seconds to re-converge across all three nodes. We retry a
/// handful of times before giving up, since this is a precondition rather
/// than the actual property under test.
pub async fn post_onboarding(f: &Fixture, prefix: &str) -> anyhow::Result<()> {
    let req = json!({
        "party_id_prefix": prefix,
        "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
    });
    let max_attempts = 12;
    for attempt in 1..=max_attempts {
        let (status, body) = f
            .post_expect_status(f.p1.http, "/onboarding", &req)
            .await
            .context("POST /onboarding (chaos)")?;
        if status.is_success() {
            // Parse the body to validate JSON shape, mirroring post_json.
            let _: Value = serde_json::from_str(&body)
                .with_context(|| format!("deserialize POST /onboarding response: {body}"))?;
            return Ok(());
        }
        // Two retryable 4xx classes:
        //   422 = peer-mesh pre-flight not yet converged after a chaos
        //         restart; Noise sessions need a few seconds to settle.
        //   409 = previous chaos phase's workflow_runs row is still
        //         "active" from the server's perspective. Cleanup is
        //         async (Disconnect commands to peers must ACK before
        //         the coordinator marks its run completed). On localnet
        //         this is sub-second; on devnet the per-peer Noise round
        //         trip plus Canton submission acknowledgments push it
        //         into the seconds range.
        // Everything else is treated as fatal.
        let s = status.as_u16();
        if (s == 422 || s == 409) && attempt < max_attempts {
            let reason = if s == 409 {
                "409 previous workflow still cleaning up"
            } else {
                "422 peer-mesh pre-flight not yet converged"
            };
            info!("post_onboarding attempt {attempt}/{max_attempts}: {reason}, retrying");
            sleep(Duration::from_secs(3)).await;
            continue;
        }
        anyhow::bail!("POST /onboarding returned {status}: {body}");
    }
    unreachable!("loop exits via return or bail")
}

/// Best-effort dismiss of a workflow_runs row on P1. Failures are swallowed
/// because chaos cleanup is non-essential — the next phase generates a fresh
/// prefix anyway.
pub async fn dismiss_p1(f: &Fixture, instance_name: &str) {
    let path = format!("/workflows/{instance_name}/dismiss");
    let _ = f.post_expect_status(f.p1.http, &path, &json!({})).await;
}

/// Log a chaos-phase milestone using the same `[Gx]` prefix the bash tests
/// used, so a CI log archive lines up between the two formats during the
/// transitional period.
pub fn say(label: &str, msg: &str) {
    info!("[{label}] {msg}");
}

/// Verify all three nodes' HTTP + Noise ports are reachable; if any are
/// not, respawn that node via `processes::spawn_only`. Used at the start of
/// chaos phases to repair any state left by an earlier phase (or by an
/// in-process race between cancel/abort and a CancelInvite delivery that
/// leaves a Noise listener in a bad state).
pub async fn ensure_nodes_healthy(f: &mut Fixture) -> anyhow::Result<()> {
    use tokio::net::TcpStream;
    let probes = [
        (1u8, f.p1.http, f.p1.noise),
        (2, f.p2.http, f.p2.noise),
        (3, f.p3.http, f.p3.noise),
    ];
    for (idx, http_port, noise_port) in probes {
        // Only repair nodes the fixture believes should be alive. A
        // `current_pids[idx] = None` slot means a chaos phase intentionally
        // killed this node and is expecting it to stay dead — respawning
        // it here would defeat the test's own premise (e.g. G3/G4/P2 kill
        // peers to force a coordinator failure within the bounded
        // wait). Crashes leave the slot as `Some(stale_pid)`, so the
        // self-heal path still kicks in for the case it's meant to cover.
        let has_pid = f.current_pids[(idx as usize) - 1].is_some();
        if !has_pid {
            continue;
        }
        let http_ok = TcpStream::connect(("127.0.0.1", http_port)).await.is_ok();
        let noise_ok = TcpStream::connect(("127.0.0.1", noise_port)).await.is_ok();
        if http_ok && noise_ok {
            continue;
        }
        tracing::warn!(
            "ensure_nodes_healthy: P{idx} unreachable (http={http_ok}, noise={noise_ok}); \
             respawning"
        );
        crate::common::processes::restart_node(f, idx).await?;
    }
    Ok(())
}

/// Wait for a pending invitation of the given type to appear on `port`,
/// returning its id. Bails after the deadline.
pub async fn wait_for_invite(
    f: &Fixture,
    port: u16,
    invitation_type: &str,
    deadline: Duration,
) -> anyhow::Result<String> {
    let start = Instant::now();
    loop {
        if let Some(id) = probe_pending_invitation(f, port, invitation_type).await {
            return Ok(id);
        }
        if start.elapsed() >= deadline {
            anyhow::bail!(
                "{invitation_type} invitation not visible on port {port} within {deadline:?}"
            );
        }
        sleep(Duration::from_millis(500)).await;
    }
}

/// Poll `probe` every second until it returns `true` or the deadline passes.
/// Probe errors propagate.
pub async fn poll_until<F, Fut>(deadline: Duration, mut probe: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let start = Instant::now();
    loop {
        if probe().await? {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            anyhow::bail!("poll_until exhausted deadline of {deadline:?}");
        }
        sleep(Duration::from_secs(1)).await;
    }
}
