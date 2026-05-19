//! Process control for chaos integration tests.
//!
//! Reads per-participant PIDs and the spawn parameters from the env (set by
//! `integration-tests/run.sh`) so tests can SIGKILL a running
//! dec-party-manager and respawn it with the same data dir / Canton ports.
//!
//! Restarted PIDs are appended to `$DEV_DIR/restarted-pids` so the bash
//! `cleanup()` trap kills them even if the cargo test panics.

use std::{
    fs::OpenOptions as StdOpenOptions,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, net::TcpStream, process::Command, time::sleep};

use super::Fixture;

/// Per-node spawn parameters captured from env vars at test boot.
#[derive(Debug, Clone)]
pub struct NodeSpawn {
    pub participant: u8,
    pub binary: PathBuf,
    pub data_dir: PathBuf,
    pub http_port: u16,
    pub noise_port: u16,
    pub canton_admin_port: u16,
    pub canton_ledger_port: u16,
    pub initial_pid: u32,
}

fn read_env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("env var {key} not set"))
}

fn read_port(key: &str) -> Result<u16> {
    read_env(key)?
        .parse::<u16>()
        .with_context(|| format!("env var {key} is not a u16"))
}

fn read_pid(key: &str) -> Result<u32> {
    read_env(key)?
        .parse::<u32>()
        .with_context(|| format!("env var {key} is not a u32"))
}

impl Fixture {
    pub fn node_spawn(&self, participant: u8) -> Result<NodeSpawn> {
        let (http, noise, ledger, admin, pid_var) = match participant {
            1 => (
                "P1_HTTP",
                "P1_NOISE",
                "P1_CANTON_LEDGER",
                "P1_CANTON_ADMIN",
                "P1_PID",
            ),
            2 => (
                "P2_HTTP",
                "P2_NOISE",
                "P2_CANTON_LEDGER",
                "P2_CANTON_ADMIN",
                "P2_PID",
            ),
            3 => (
                "P3_HTTP",
                "P3_NOISE",
                "P3_CANTON_LEDGER",
                "P3_CANTON_ADMIN",
                "P3_PID",
            ),
            _ => anyhow::bail!("invalid participant index {participant}"),
        };
        let binary = PathBuf::from(read_env("BINARY")?);
        let data_dir = self.dev_dir.join(format!("participant-{participant}"));
        Ok(NodeSpawn {
            participant,
            binary,
            data_dir,
            http_port: read_port(http)?,
            noise_port: read_port(noise)?,
            canton_admin_port: read_port(admin)?,
            canton_ledger_port: read_port(ledger)?,
            initial_pid: read_pid(pid_var)?,
        })
    }
}

/// SIGKILL the given pid. Returns Ok even if the process is already gone.
pub async fn kill_pid(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .with_context(|| format!("spawn `kill -9 {pid}`"))?;
    // status.success() is fine; non-success means the pid was already gone.
    let _ = status;
    // Wait for the kernel to reap the port; a quick poll loop is cheaper
    // than a fixed sleep in the common case.
    Ok(())
}

/// Block (with deadline) until `pid` is no longer alive — i.e., `kill -0`
/// returns non-zero. Used immediately after `kill_pid` to make sure the
/// HTTP/Noise ports are released before respawning.
pub async fn wait_for_exit(pid: u32, deadline: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        let status = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .with_context(|| format!("spawn `kill -0 {pid}`"))?;
        if !status.success() {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            anyhow::bail!("pid {pid} still alive after {deadline:?}");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Block (with deadline) until the HTTP listener port is accepting TCP
/// connections.
///
/// HTTP-only by design — the Noise *invite* listener's bound state is not
/// a reliable signal that DPM is healthy after a chaos restart. DPM's
/// restart-resume path (`src/server/mod.rs`) detects in-progress
/// `workflow_runs` rows and resumes them as coordinators, which pauses the
/// Noise invite listener and drops its TCP socket — workflow-specific Noise
/// servers take exclusive control of port 9000 instead. On devnet, where
/// each workflow step makes a Canton round trip taking 10-30s, the invite
/// listener can stay paused well past any plausible deadline. Polling for
/// the Noise port in this state used to false-fail with
/// "ports not bound: http=true noise=false" on G1 (restart_coordinator_resume).
///
/// HTTP listener coming up is the right "DPM completed bootstrap" signal:
/// it's bound exactly once in src/server/mod.rs after Canton's participant
/// ID lookup, DB migrations, and the auth/workflow-state init pass. From
/// that point forward, DPM is reachable on the HTTP plane; the Noise plane
/// might or might not be bound depending on what workflow_runs were
/// recovered from disk.
pub async fn wait_for_server(http_port: u16, deadline: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", http_port)).await.is_ok() {
            // Settle delay so a freshly-respawned DPM has time to finish
            // any in-flight workflow-resume work before the next test
            // step starts pounding it. Bash harness used 5s here; chaos
            // phases can restart multiple nodes back-to-back, so we use
            // a longer settle to keep the next phase's peer-mesh
            // pre-flight green.
            sleep(Duration::from_secs(8)).await;
            return Ok(());
        }
        if start.elapsed() >= deadline {
            anyhow::bail!("http port {http_port} not bound after {deadline:?}");
        }
        sleep(Duration::from_millis(200)).await;
    }
}

/// Spawn a fresh dec-party-manager with the same args env.sh used at boot.
/// Returns the new PID; also appends it to `$DEV_DIR/restarted-pids` so the
/// bash cleanup trap can SIGKILL it if cargo test exits abnormally.
pub async fn spawn_node(spawn: &NodeSpawn, restarted_pids_file: &PathBuf) -> Result<u32> {
    let rust_log = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "dec_party_manager=info,tokio_noise=error,hyper_noise=error".into());
    // Same per-participant log file as bash bringup (integration-tests/common.sh::start_nodes).
    // Open in append mode so chaos-phase respawns accumulate into one timeline
    // per participant, matching the bash side's `>> "$log_file" 2>&1`.
    let log_path = spawn.data_dir.join("stderr.log");
    let log_file = StdOpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {} for write", log_path.display()))?;
    let log_clone = log_file
        .try_clone()
        .with_context(|| format!("cloning {} for stderr after stdout", log_path.display()))?;
    let child = Command::new(&spawn.binary)
        .arg("-d")
        .arg(&spawn.data_dir)
        .arg("serve")
        .args(["--host", "0.0.0.0"])
        .arg("--port")
        .arg(spawn.http_port.to_string())
        .env("RUST_LOG", rust_log)
        .env("DECPM_CANTON_ADMIN_HOST", "127.0.0.1")
        .env(
            "DECPM_CANTON_ADMIN_PORT",
            spawn.canton_admin_port.to_string(),
        )
        .env("DECPM_CANTON_LEDGER_HOST", "127.0.0.1")
        .env(
            "DECPM_CANTON_LEDGER_PORT",
            spawn.canton_ledger_port.to_string(),
        )
        .env("DECPM_CANTON_NETWORK", "devnet")
        .env("DECPM_NOISE_PORT", spawn.noise_port.to_string())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_clone))
        .kill_on_drop(false)
        .spawn()
        .with_context(|| {
            format!(
                "spawning {} for participant-{}",
                spawn.binary.display(),
                spawn.participant
            )
        })?;
    let pid = child.id().with_context(|| {
        format!(
            "freshly-spawned participant-{} has no pid",
            spawn.participant
        )
    })?;
    // Detach the child handle so the kernel doesn't clean it up when we drop
    // it — bash cleanup will reap it via the restarted-pids file.
    drop(child);

    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(restarted_pids_file)
        .await
        .with_context(|| format!("opening {} for write", restarted_pids_file.display()))?;
    f.write_all(format!("{pid}\n").as_bytes()).await?;
    f.flush().await?;
    Ok(pid)
}

/// Convenience wrapper: kill `current_pid`, respawn with `spawn` parameters,
/// wait for both ports to come back up, and return the new PID. Tests use
/// this for "hard-kill mid-flight then auto-resume" scenarios.
pub async fn restart_node_explicit(
    spawn: &NodeSpawn,
    current_pid: u32,
    fixture: &Fixture,
) -> Result<u32> {
    kill_pid(current_pid).await?;
    wait_for_exit(current_pid, Duration::from_secs(15)).await?;
    let restarted_file = fixture.dev_dir.join("restarted-pids");
    let new_pid = spawn_node(spawn, &restarted_file).await?;
    wait_for_server(spawn.http_port, Duration::from_secs(60)).await?;
    Ok(new_pid)
}

/// Restart `participant` (1-based) using the PID tracked by the fixture, and
/// update `fixture.current_pids` in place so later chaos tests target the new
/// process. Returns the new PID.
pub async fn restart_node(fixture: &mut Fixture, participant: u8) -> Result<u32> {
    let idx = (participant as usize)
        .checked_sub(1)
        .context("participant index must be 1-based")?;
    let current_pid = fixture
        .current_pids
        .get(idx)
        .copied()
        .flatten()
        .with_context(|| format!("no tracked pid for participant-{participant}"))?;
    let spawn = fixture.node_spawn(participant)?;
    let new_pid = restart_node_explicit(&spawn, current_pid, fixture).await?;
    fixture.current_pids[idx] = Some(new_pid);
    Ok(new_pid)
}

/// Kill `participant`'s tracked process and clear the slot. The caller is
/// responsible for respawning later (used by tests that intentionally leave
/// a node down for part of the run).
pub async fn kill_node(fixture: &mut Fixture, participant: u8) -> Result<()> {
    let idx = (participant as usize)
        .checked_sub(1)
        .context("participant index must be 1-based")?;
    if let Some(pid) = fixture.current_pids[idx].take() {
        kill_pid(pid).await?;
        wait_for_exit(pid, Duration::from_secs(15)).await?;
    }
    Ok(())
}

/// Spawn a fresh process for `participant` (no kill first). Used to restart
/// a node that was previously killed via `kill_node`.
pub async fn spawn_only(fixture: &mut Fixture, participant: u8) -> Result<u32> {
    let idx = (participant as usize)
        .checked_sub(1)
        .context("participant index must be 1-based")?;
    let spawn = fixture.node_spawn(participant)?;
    let restarted_file = fixture.dev_dir.join("restarted-pids");
    let new_pid = spawn_node(&spawn, &restarted_file).await?;
    wait_for_server(spawn.http_port, Duration::from_secs(60)).await?;
    fixture.current_pids[idx] = Some(new_pid);
    Ok(new_pid)
}
