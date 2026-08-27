use std::path::PathBuf;

use clap::Subcommand;

pub use clap::Parser;

use dec_party_manager::config::Network;

fn parse_positive_usize(s: &str) -> std::result::Result<usize, String> {
    let v: usize = s
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    if v == 0 {
        Err("must be >= 1".into())
    } else {
        Ok(v)
    }
}

#[derive(Parser)]
#[command(name = "dec-party-manager")]
#[command(about = "Canton decentralized party onboarding workflow automation", long_about = None)]
pub struct Cli {
    /// Path to root directory containing config/ and data/ subdirectories
    #[arg(
        short,
        long,
        value_name = "DIR",
        default_value = ".",
        env = "DECPM_DIR"
    )]
    pub dir: PathBuf,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the HTTP server for querying decentralized parties
    Serve {
        /// Host address to bind to
        #[arg(long, default_value = "0.0.0.0", env = "DECPM_HOST")]
        host: String,

        /// Port to listen on
        #[arg(long, default_value = "8080", env = "DECPM_PORT")]
        port: u16,

        /// Path to SQLite database file (defaults to {dir}/data/decpm.db)
        #[arg(long, env = "DECPM_DB_PATH")]
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

        /// Speak TLS to the Canton Admin API
        #[arg(long, env = "DECPM_CANTON_ADMIN_TLS")]
        canton_admin_tls: Option<bool>,

        /// PEM file of the CA that issued the Admin API certificate
        #[arg(long, env = "DECPM_CANTON_ADMIN_TLS_CA_CERT")]
        canton_admin_tls_ca_cert: Option<String>,

        /// PEM client certificate for Admin API mTLS
        #[arg(long, env = "DECPM_CANTON_ADMIN_TLS_CLIENT_CERT")]
        canton_admin_tls_client_cert: Option<String>,

        /// PEM client key for Admin API mTLS
        #[arg(long, env = "DECPM_CANTON_ADMIN_TLS_CLIENT_KEY")]
        canton_admin_tls_client_key: Option<String>,

        /// Name to validate the Admin API certificate against, when it
        /// differs from the configured host
        #[arg(long, env = "DECPM_CANTON_ADMIN_TLS_DOMAIN")]
        canton_admin_tls_domain: Option<String>,

        /// Speak TLS to the Canton Ledger API
        #[arg(long, env = "DECPM_CANTON_LEDGER_TLS")]
        canton_ledger_tls: Option<bool>,

        /// PEM file of the CA that issued the Ledger API certificate
        #[arg(long, env = "DECPM_CANTON_LEDGER_TLS_CA_CERT")]
        canton_ledger_tls_ca_cert: Option<String>,

        /// PEM client certificate for Ledger API mTLS
        #[arg(long, env = "DECPM_CANTON_LEDGER_TLS_CLIENT_CERT")]
        canton_ledger_tls_client_cert: Option<String>,

        /// PEM client key for Ledger API mTLS
        #[arg(long, env = "DECPM_CANTON_LEDGER_TLS_CLIENT_KEY")]
        canton_ledger_tls_client_key: Option<String>,

        /// Name to validate the Ledger API certificate against, when it
        /// differs from the configured host
        #[arg(long, env = "DECPM_CANTON_LEDGER_TLS_DOMAIN")]
        canton_ledger_tls_domain: Option<String>,

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

        /// Internal/backchannel Keycloak base URL the server uses for OIDC
        /// discovery, JWKS, and introspection when it cannot reach
        /// `DECPM_KEYCLOAK_URL` directly — e.g. that points at a tailnet
        /// (`.ts.net`) host the browser can reach but the in-cluster pod
        /// cannot, so the server uses the cluster Service instead. Defaults to
        /// `DECPM_KEYCLOAK_URL` when unset; does not change the token issuer.
        #[arg(long, env = "DECPM_KEYCLOAK_INTERNAL_URL")]
        keycloak_internal_url: Option<String>,

        // Auth0 (top-level, for frontend gating). Env-only: hidden from
        // --help so operators always configure these via deploy env vars,
        // mirroring `DECPM_KEYCLOAK_*` but with no CLI flag surface.
        /// Auth0 tenant domain for frontend auth
        #[arg(long, env = "DECPM_AUTH0_DOMAIN", hide = true)]
        auth0_domain: Option<String>,

        /// Auth0 SPA client ID for frontend auth
        #[arg(long, env = "DECPM_AUTH0_CLIENT_ID", hide = true)]
        auth0_client_id: Option<String>,

        /// Auth0 API audience for frontend access tokens
        #[arg(long, env = "DECPM_AUTH0_AUDIENCE", hide = true)]
        auth0_audience: Option<String>,

        /// Extra space-separated scopes the SPA requests. Auth0 RBAC returns a
        /// permission in `scope` only if the client asked for it.
        #[arg(long, env = "DECPM_AUTH0_SCOPE", hide = true)]
        auth0_scope: Option<String>,

        /// Optional JWT claim containing an array of role names. Use this for
        /// provider-required namespaced custom claims; standard role carriers
        /// remain enabled when this is unset.
        #[arg(long, env = "DECPM_JWT_ROLE_CLAIM")]
        jwt_role_claim: Option<String>,

        /// Role name that gates sensitive endpoints (PUT /party-config,
        /// POST /kick, etc.). Unset (default) skips the role check —
        /// every authenticated caller is treated as admin. Set this to
        /// require a specific role for shared/multi-user nodes.
        #[arg(long, env = "DECPM_ADMIN_ROLE")]
        admin_role: Option<String>,

        /// Origin permitted by CORS (e.g. `https://decman.example.com`).
        /// Defaults to same-origin only — set this when the SPA is served
        /// from a different host than the API (reverse proxy, dev server).
        #[arg(long, env = "DECPM_ALLOWED_ORIGIN")]
        allowed_origin: Option<String>,

        // Insecure / no-IdP mode (local dev against an unsafe-auth Canton).
        /// Run without an IdP: accept ANY inbound token and present an unsafe
        /// HS256 token to Canton. Disables authentication — NEVER use in
        /// production. Pair with an unsafe-auth Canton (see the HMAC flags).
        #[arg(long, env = "DECPM_INSECURE", default_value_t = false)]
        insecure: bool,

        /// HMAC secret decman signs the unsafe Canton token with (insecure
        /// mode). Must match Canton's unsafe auth-service secret. Default `unsafe`.
        #[arg(long, env = "DECPM_CANTON_HMAC_SECRET")]
        canton_hmac_secret: Option<String>,

        /// `aud` claim for the unsafe Canton token (insecure mode). Must match
        /// Canton's `target-audience`. Default `https://canton.network.global`.
        #[arg(long, env = "DECPM_CANTON_HMAC_AUDIENCE")]
        canton_hmac_audience: Option<String>,

        /// `sub` claim / ledger user for the unsafe Canton token (insecure
        /// mode). Default `ledger-api-user`.
        #[arg(long, env = "DECPM_CANTON_HMAC_SUBJECT")]
        canton_hmac_subject: Option<String>,

        /// Comma-separated bearer API keys for the wallet-facing `/v0/tenant/*`
        /// endpoints (external-party onboarding + interactive submission).
        /// Empty (default) disables the tenant API unless in insecure/test mode.
        #[arg(long, env = "DECPM_TENANT_API_KEYS")]
        tenant_api_keys: Option<String>,

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

        /// Total attempts (initial + retries) for the bounded peer-Noise retry wrapper.
        /// Must be >= 1.
        #[arg(long, env = "DECPM_NOISE_RETRY_MAX_ATTEMPTS", value_parser = parse_positive_usize)]
        noise_retry_max_attempts: Option<usize>,

        /// Backoff between attempts of the bounded peer-Noise retry wrapper, in milliseconds
        #[arg(long, env = "DECPM_NOISE_RETRY_BACKOFF_MS")]
        noise_retry_backoff_ms: Option<u64>,

        /// CIP-104 Mode A reward-automation loop tick interval, in seconds.
        /// Enablement is on-ledger (presence of a CouponReassignmentDelegation);
        /// this only controls cadence. Defaults to 300.
        #[arg(long, env = "DECPM_REWARD_AUTOMATION_INTERVAL_SECS")]
        reward_automation_interval_secs: Option<u64>,
        /// Output contracts one Delegation_Assign may create, bounding the
        /// coupons per transaction. Raise stepwise to find the ledger's real
        /// ceiling; set too high, assigns fail and nothing is assigned.
        /// Defaults to 100.
        #[arg(long, env = "DECPM_REWARD_MAX_CREATES")]
        reward_max_creates: Option<usize>,
        /// Seconds a coupon must have left before expiry to be assigned. Guards
        /// against a coupon vanishing mid-submission, not a minting reserve for
        /// the beneficiary. Defaults to 120.
        #[arg(long, env = "DECPM_REWARD_MIN_EXPIRY_MARGIN_SECS")]
        reward_min_expiry_margin_secs: Option<u64>,
    },
}
