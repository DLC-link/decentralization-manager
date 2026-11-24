//! This test sends SIGTERM to its own process. It lives in its own test binary,
//! because in a shared binary the signal would also reach every other test.

use std::{net::TcpListener, process::Command, time::Duration};

use anyhow::{Context, ensure};
use dec_party_manager::server::metrics_server;
use reqwest::StatusCode;
use tokio::signal::unix::{SignalKind, signal};

/// In production, the API server handles SIGTERM and the process exit ends the
/// metrics server. The metrics server must not stop on the signal itself: its
/// supervisor would log that stop at error, and every restart would fire an alert.
#[tokio::test]
async fn the_metrics_server_keeps_serving_after_sigterm() -> anyhow::Result<()> {
    // Stands in for the API server's handler. Without one, SIGTERM kills the test.
    let mut sigterm = signal(SignalKind::terminate())?;
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?.port()
    };
    let mut running = tokio::spawn(metrics_server("127.0.0.1", port)?);
    // actix installs its own signal handler when the server starts, so a server
    // that answers has already had its chance to listen for SIGTERM.
    assert_eq!(get_metrics(port).await?, StatusCode::OK);

    let kill = Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()?;
    ensure!(kill.success(), "kill -TERM failed: {kill}");
    tokio::time::timeout(Duration::from_secs(5), sigterm.recv())
        .await
        .context("SIGTERM never arrived")?;

    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut running)
            .await
            .is_err(),
        "the metrics server stopped on SIGTERM"
    );
    assert_eq!(get_metrics(port).await?, StatusCode::OK);
    Ok(())
}

/// Opens a fresh connection for each request. A pooled keep-alive connection
/// would hold a stopping server open for its 30-second grace period.
async fn get_metrics(port: u16) -> anyhow::Result<StatusCode> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(5))
        .build()?;
    Ok(client
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .await?
        .status())
}
