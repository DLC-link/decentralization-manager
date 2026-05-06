use std::path::PathBuf;

use clap::Subcommand;

pub use clap::Parser;

use dec_party_manager::config::Network;

#[derive(Parser)]
#[command(name = "dec-party-manager")]
#[command(about = "Canton decentralized party onboarding workflow automation", long_about = None)]
pub struct Cli {
    /// Path to root directory containing config/ and data/ subdirectories
    #[arg(short, long, value_name = "DIR", default_value = ".")]
    pub dir: PathBuf,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the HTTP server for querying decentralized parties
    Serve {
        /// Host address to bind to
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        /// Port to listen on
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Path to SQLite database file (defaults to {dir}/data/decpm.db)
        #[arg(long)]
        db: Option<PathBuf>,

        /// Encryption key for secrets stored in the database
        #[arg(long, env = "DECPM_DB_ENCRYPTION_KEY")]
        db_encryption_key: Option<String>,

        // Node settings
        /// Address to listen on for Noise protocol connections
        #[arg(long, env = "DECPM_LISTEN_ADDRESS")]
        listen_address: Option<String>,

        /// Port to listen on for Noise protocol connections
        #[arg(long, env = "DECPM_NOISE_PORT")]
        noise_port: Option<u16>,

        /// Public address that peers use to connect to this node
        #[arg(long, env = "DECPM_PUBLIC_ADDRESS")]
        public_address: Option<String>,

        // Canton settings
        /// Canton Admin API host
        #[arg(long, env = "DECPM_CANTON_ADMIN_HOST")]
        canton_admin_host: Option<String>,

        /// Canton Admin API port
        #[arg(long, env = "DECPM_CANTON_ADMIN_PORT")]
        canton_admin_port: Option<u16>,

        /// Canton Ledger API host
        #[arg(long, env = "DECPM_CANTON_LEDGER_HOST")]
        canton_ledger_host: Option<String>,

        /// Canton Ledger API port
        #[arg(long, env = "DECPM_CANTON_LEDGER_PORT")]
        canton_ledger_port: Option<u16>,

        /// Canton synchronizer name
        #[arg(long, env = "DECPM_CANTON_SYNCHRONIZER")]
        canton_synchronizer: Option<String>,

        /// Canton network environment (devnet, testnet, mainnet)
        #[arg(long, env = "DECPM_CANTON_NETWORK")]
        canton_network: Option<Network>,

        // Keycloak (top-level, for frontend gating)
        /// Keycloak server URL for frontend auth
        #[arg(long, env = "DECPM_KEYCLOAK_URL")]
        keycloak_url: Option<String>,

        /// Keycloak realm name for frontend auth
        #[arg(long, env = "DECPM_KEYCLOAK_REALM")]
        keycloak_realm: Option<String>,

        /// Keycloak client ID for frontend auth
        #[arg(long, env = "DECPM_KEYCLOAK_CLIENT_ID")]
        keycloak_client_id: Option<String>,

        /// Role name that gates sensitive endpoints (PUT /party-config,
        /// POST /kick, etc.). Unset (default) skips the role check —
        /// every authenticated caller is treated as admin. Set this to
        /// require a specific role for shared/multi-user nodes.
        #[arg(long, env = "DECPM_ADMIN_ROLE")]
        admin_role: Option<String>,

        /// Origin permitted by CORS (e.g. `https://dpm.example.com`).
        /// Defaults to same-origin only — set this when the SPA is served
        /// from a different host than the API (reverse proxy, dev server).
        #[arg(long, env = "DECPM_ALLOWED_ORIGIN")]
        allowed_origin: Option<String>,

        // Timeouts
        /// Noise handshake timeout in seconds
        #[arg(long, env = "DECPM_TIMEOUT_HANDSHAKE")]
        timeout_handshake: Option<u64>,

        /// Noise message timeout in seconds
        #[arg(long, env = "DECPM_TIMEOUT_MESSAGE")]
        timeout_message: Option<u64>,

        /// Connection retry attempts
        #[arg(long, env = "DECPM_TIMEOUT_RETRY_ATTEMPTS")]
        timeout_retry_attempts: Option<u32>,

        /// Connection retry delay in seconds
        #[arg(long, env = "DECPM_TIMEOUT_RETRY_DELAY")]
        timeout_retry_delay: Option<u64>,

        // Noise retry tuning (separate from the legacy Timeouts knobs above)
        /// Per-attempt timeout for the bounded peer-Noise retry wrapper, in seconds
        #[arg(long, env = "DECPM_NOISE_RETRY_TIMEOUT_SEC")]
        noise_retry_timeout_sec: Option<u64>,

        /// Total attempts (initial + retries) for the bounded peer-Noise retry wrapper
        #[arg(long, env = "DECPM_NOISE_RETRY_MAX_ATTEMPTS")]
        noise_retry_max_attempts: Option<usize>,

        /// Backoff between attempts of the bounded peer-Noise retry wrapper, in milliseconds
        #[arg(long, env = "DECPM_NOISE_RETRY_BACKOFF_MS")]
        noise_retry_backoff_ms: Option<u64>,
    },
}
