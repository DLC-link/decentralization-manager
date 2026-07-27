//! Regression (#260): DecMan against a Canton admin API that speaks TLS.
//!
//! Every gRPC channel is built as plaintext h2c (`http://host:port`, no
//! `tls_config` anywhere), and there is no config knob to change that. A
//! participant with TLS enabled on its admin API closes the connection on the
//! first bytes, which a mainnet operator sees as a permanent `transport
//! error` / BrokenPipe on every `/decentralized-parties` fetch, with no hint
//! that TLS is the reason.
//!
//! The localnet stack serves plaintext, so the suite has no TLS coverage at
//! all. This phase puts a TLS terminator (`common::tls_proxy`) in front of
//! P3's Canton admin port, restarts P3 pointed at it with TLS enabled, and
//! requires the node to come up and keep serving admin-API-backed responses.
//!
//! Pre-fix P3 cannot boot at all: `resolve_participant_id` is a startup step
//! and its plaintext client cannot talk to the TLS listener, so the HTTP
//! listener is never bound. Post-fix P3 boots over TLS and answers.
//!
//! P3 is put back on the plaintext port before the phase returns, pass or
//! fail, so later phases see the node they expect.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::info;

use crate::common::{
    Fixture,
    processes::{self, NodeSpawn},
    tls_proxy::TlsProxy,
    types::DecentralizedPartiesResponse,
};

#[derive(Debug, Deserialize)]
struct NodeSection {
    participant_id: String,
}

#[derive(Debug, Deserialize)]
struct NodeConfigResponse {
    node: NodeSection,
}

pub async fn run(f: &mut Fixture) -> Result<()> {
    info!("Phase: canton_admin_tls");

    let plaintext_spawn = f.node_spawn(3)?;
    let before: NodeConfigResponse = f
        .get_json(f.p3.http, "/node-config")
        .await
        .context("GET /node-config on P3 before switching to TLS")?;

    let proxy = TlsProxy::start(plaintext_spawn.canton_admin_port, &plaintext_spawn.data_dir)
        .await
        .context("starting the TLS terminator in front of P3's Canton admin API")?;

    let mut tls_spawn = plaintext_spawn.clone();
    tls_spawn.canton_admin_port = proxy.port;
    tls_spawn.extra_env = vec![
        ("DECPM_CANTON_ADMIN_TLS".to_string(), "true".to_string()),
        (
            "DECPM_CANTON_ADMIN_TLS_CA_CERT".to_string(),
            proxy.ca_cert_path.display().to_string(),
        ),
    ];

    let outcome = serves_over_tls(f, &tls_spawn, &before.node.participant_id).await;

    // Restore before propagating: the rest of the suite needs P3 on its
    // normal plaintext wiring whether or not the assertions held.
    let restored = restore_plaintext(f, &plaintext_spawn).await;
    outcome?;
    restored
}

/// Restart P3 against the TLS proxy and require it to serve admin-API-backed
/// responses.
async fn serves_over_tls(
    f: &mut Fixture,
    tls_spawn: &NodeSpawn,
    participant_id: &str,
) -> Result<()> {
    let current = current_pid(f)?;
    info!(
        "restarting P3 against the TLS proxy on 127.0.0.1:{port}",
        port = tls_spawn.canton_admin_port
    );
    let pid = processes::restart_node_explicit(tls_spawn, current, f)
        .await
        .context(
            "P3 did not come up against the TLS admin API — a plaintext h2c client cannot \
             talk to a TLS listener, so the startup participant-id lookup fails and the \
             HTTP listener is never bound",
        )?;
    f.current_pids[2] = Some(pid);

    // The participant id is read from Canton's admin API at startup, so
    // getting the same one back proves the TLS round trip actually happened
    // rather than a cached or defaulted value being served.
    let after: NodeConfigResponse = f
        .get_json(f.p3.http, "/node-config")
        .await
        .context("GET /node-config on P3 over TLS")?;
    anyhow::ensure!(
        after.node.participant_id == participant_id,
        "participant id changed over TLS: expected {participant_id}, got {got}",
        got = after.node.participant_id
    );

    // The operator-visible symptom in the field report: this endpoint issues
    // live topology reads over the admin channel.
    let parties: DecentralizedPartiesResponse = f
        .get_json(f.p3.http, "/decentralized-parties")
        .await
        .context("GET /decentralized-parties on P3 over TLS")?;
    info!(
        "P3 served {count} party/parties over the TLS admin channel",
        count = parties.parties.len()
    );

    Ok(())
}

async fn restore_plaintext(f: &mut Fixture, plaintext_spawn: &NodeSpawn) -> Result<()> {
    let current = current_pid(f)?;
    let pid = processes::restart_node_explicit(plaintext_spawn, current, f)
        .await
        .context("restoring P3 on the plaintext Canton admin port")?;
    f.current_pids[2] = Some(pid);
    tokio::time::sleep(Duration::from_secs(2)).await;
    info!("P3 restored on the plaintext Canton admin port");
    Ok(())
}

fn current_pid(f: &Fixture) -> Result<u32> {
    f.current_pids[2].context("no tracked pid for participant-3")
}
