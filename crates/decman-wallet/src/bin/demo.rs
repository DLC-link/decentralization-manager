//! The DecMan demo wallet.
//!
//! Stands in for a wallet provider's own app: it generates a party's Ed25519 key,
//! onboards that party across several DecMan hosts as a co-validated party, and
//! transacts as it — all through the same [`decman_wallet`] library a provider
//! would embed. The key is created in this process and never leaves it.
//!
//! ```text
//! decman-wallet-demo \
//!   --host http://localhost:8080=participant::1220aa… \
//!   --host http://localhost:8081=participant::1220bb… \
//!   --host http://localhost:8082=participant::1220cc… \
//!   --api-key $DECMAN_TENANT_API_KEY
//! ```

use std::path::PathBuf;

use clap::Parser;
use common::canton_id::CantonId;
use decman_wallet::demo::{DemoConfig, DemoState, HostConfig, run};
use tracing_subscriber::{filter::EnvFilter, prelude::*};

#[derive(Debug, Parser)]
#[command(
    name = "decman-wallet-demo",
    about = "Demo wallet for DecMan co-validated parties (the /v0/tenant/* API)"
)]
struct Cli {
    /// A participant that will host the party, as `<decman-base-url>=<participant-id>`.
    /// Repeat for each host — co-validation needs at least two. The first is the
    /// one asked to build the topology.
    #[arg(
        long = "host",
        value_name = "URL=PARTICIPANT_ID",
        value_parser = parse_host,
        required = true,
        num_args = 1..,
    )]
    hosts: Vec<HostConfig>,

    /// The provider-issued tenant API key the hosts accept. Nodes running
    /// `--insecure` accept any value.
    #[arg(long, env = "DECMAN_TENANT_API_KEY", default_value = "")]
    api_key: String,

    /// How many hosts must confirm a transaction. Omit to let DecMan default to
    /// `N-1`, which is what keeps a host able to leave later.
    #[arg(long)]
    confirmation_threshold: Option<u32>,

    /// Address to serve the demo UI on. Loopback by default — this process holds
    /// the party's private key and the tenant API key.
    #[arg(long, default_value = "127.0.0.1:7878")]
    bind: String,

    /// Persist the party's key here (mode 0600) so a restart keeps the same
    /// party. Omit to keep it in memory only, so each run is a fresh demo.
    #[arg(long)]
    state_file: Option<PathBuf>,
}

/// Parse `<base-url>=<participant-id>`.
fn parse_host(raw: &str) -> Result<HostConfig, String> {
    let (base_url, participant_id) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected <base-url>=<participant-id>, got '{raw}'"))?;
    if base_url.trim().is_empty() {
        return Err(format!("missing base url in '{raw}'"));
    }
    let participant_id = CantonId::parse(participant_id.trim())
        .map_err(|e| format!("invalid participant id in '{raw}': {e}"))?;
    Ok(HostConfig {
        base_url: base_url.trim().to_string(),
        participant_id,
    })
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .init();

    let cli = Cli::parse();

    if cli.hosts.len() < 2 {
        return Err(std::io::Error::other(format!(
            "co-validation needs at least two hosts, got {count} — pass --host more than once",
            count = cli.hosts.len()
        )));
    }
    for host in &cli.hosts {
        tracing::info!(
            base_url = %host.base_url,
            participant_id = %host.participant_id,
            "host configured"
        );
    }

    let state = DemoState::new(DemoConfig {
        hosts: cli.hosts,
        api_key: cli.api_key,
        confirmation_threshold: cli.confirmation_threshold,
        state_file: cli.state_file,
    })
    .map_err(|e| std::io::Error::other(e.to_string()))?;

    run(state, &cli.bind).await
}
