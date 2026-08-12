use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};

use crate::{
    canton_id::CantonId,
    consts::{DARS_DIR, DATA_DIR, DB_FILENAME, NOISE_KEY_FILENAME},
    error::Result,
};

/// Network configuration - list of peers in the network
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NetworkConfig {
    /// List of peers in the network
    pub peers: Vec<Peer>,
}

/// A peer in the network
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Peer {
    /// Canton participant UID (e.g., "participant1::1220...")
    pub participant_id: CantonId,
    /// Human-readable name
    pub name: String,
    /// Network address (hostname or IP)
    pub address: String,
    /// Port for Noise protocol communication
    pub port: u16,
    /// Hex-encoded Noise static public key
    pub public_key: String,
    /// Canton party ID (optional, can be allocated dynamically if not provided)
    #[serde(default)]
    pub party: Option<String>,
}

impl NetworkConfig {
    /// Construct a NetworkConfig from a list of peers (e.g., loaded from DB)
    pub fn from_peers(peers: Vec<Peer>) -> Self {
        Self { peers }
    }

    /// Get the governance threshold for multi-sig operations
    /// Returns majority threshold: (n/2 + 1)
    pub fn governance_threshold(&self) -> u32 {
        ((self.peers.len() / 2) + 1) as u32
    }
}

/// Keycloak authentication configuration
///
/// Supports two authentication methods:
/// 1. Client credentials (M2M): Set `client_id` and `client_secret`
/// 2. Password flow: Set `client_id`, `username`, and `password`
#[derive(Clone, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KeycloakConfig {
    /// Keycloak server URL (e.g., "https://keycloak.example.com"). This is the
    /// URL the browser logs in against and is what appears as the token `iss`,
    /// so it anchors issuer matching during validation.
    pub url: String,
    /// Internal/backchannel base URL the *server* uses to reach this Keycloak
    /// for OIDC discovery, JWKS, and introspection.
    ///
    /// Set this when the server cannot reach `url` directly — e.g. `url` is a
    /// tailnet (`.ts.net`) address the browser can reach but the in-cluster pod
    /// cannot, so the server fetches metadata via the cluster Service instead.
    /// In OIDC terms `url` is the frontchannel (browser-facing) URL and this is
    /// the backchannel (server-to-server) URL.
    ///
    /// Falls back to `url` when unset or empty, so existing single-URL configs
    /// behave exactly as before. Does not affect issuer matching — tokens still
    /// carry `url` as `iss`.
    ///
    /// `skip_serializing`: server-only address, must never reach the browser
    /// via `GET /node-config`.
    #[serde(default, skip_serializing)]
    pub internal_url: Option<String>,
    /// Keycloak realm name
    pub realm: String,
    /// OAuth2 client ID
    pub client_id: String,
    /// Client secret for M2M (client_credentials) flow.
    ///
    /// `skip_serializing`: a secret that must never reach the browser via
    /// `GET /node-config`. The frontend never reads it; keeping it off the wire
    /// also keeps it out of the generated TypeScript type.
    #[serde(default, skip_serializing)]
    pub client_secret: Option<String>,
    /// Username for password flow
    #[serde(default)]
    pub username: Option<String>,
    /// Password for password flow.
    ///
    /// `skip_serializing`: a secret that must never reach the browser via
    /// `GET /node-config`. The frontend never reads it; keeping it off the wire
    /// also keeps it out of the generated TypeScript type.
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
}

/// Auth0 authentication configuration for frontend website gating.
///
/// Mutually exclusive with [`KeycloakConfig`] at the top level — each node
/// operator picks one or the other via environment variables at deploy time.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Auth0Config {
    /// Auth0 tenant domain (e.g., "tenant.us.auth0.com")
    pub domain: String,
    /// Auth0 SPA client ID
    pub client_id: String,
    /// API audience identifier. Required for `getAccessTokenSilently()` to
    /// return a backend-validatable JWT rather than a userinfo-scoped token.
    #[serde(default)]
    pub audience: Option<String>,
}

/// Per-party Auth0 M2M credentials. Used to mint outbound access tokens the
/// backend sends to Canton when acting as the decentralized party.
///
/// Sibling of [`KeycloakConfig`] on [`PartyCredentials`]: when present, this
/// provider is used in place of Keycloak.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct Auth0M2MConfig {
    /// Auth0 tenant domain (e.g., "tenant.us.auth0.com")
    pub domain: String,
    /// Auth0 API audience (the API identifier the access token targets)
    pub audience: String,
    /// Auth0 M2M application client ID
    pub client_id: String,
    /// Auth0 M2M application client secret
    pub client_secret: String,
}

/// Package identifiers for Daml contracts (configurable per party). Defined in
/// the shared `common::api` crate (the frontend's TypeScript is generated from
/// it); re-exported so `crate::config::PackageConfig` resolves unchanged.
pub use common::api::PackageConfig;

/// Credentials for a specific decentralized party
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PartyCredentials {
    /// The decentralized party ID (shared among all members)
    pub dec_party_id: CantonId,
    /// The member party ID (local to this node, owns the credentials)
    pub member_party_id: CantonId,
    /// Canton/Ledger API user ID (must match JWT 'sub' claim, belongs to member_party)
    pub user_id: String,
    /// Keycloak authentication configuration. Empty/unused when `auth0` is set.
    #[serde(default)]
    pub keycloak: KeycloakConfig,
    /// Auth0 M2M authentication. When `Some`, used in preference to `keycloak`.
    #[serde(default)]
    pub auth0: Option<Auth0M2MConfig>,
    /// Package identifiers for deployed Daml contracts
    #[serde(default)]
    pub packages: PackageConfig,
}

/// Timeout configuration
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Timeouts {
    pub handshake_timeout_secs: u64,
    pub message_timeout_secs: u64,
    pub connection_retry_attempts: u32,
    pub connection_retry_delay_secs: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake_timeout_secs: 30,
            message_timeout_secs: 120,
            connection_retry_attempts: 3,
            connection_retry_delay_secs: 5,
        }
    }
}

/// Configuration for the bounded retry wrapper around peer Noise calls
/// (`send_noise_message_with_retry`). Defaults match the spec working
/// hypothesis: 5s × 2 attempts, 250ms backoff between attempts.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NoiseRetryConfig {
    /// Per-attempt timeout in seconds (applied independently to TCP connect
    /// and to the Noise/HTTP request budget).
    pub per_attempt_timeout_secs: u64,
    /// Total attempts (initial + retries). 2 means "1 retry."
    pub max_attempts: usize,
    /// Fixed backoff between attempts in milliseconds.
    pub backoff_ms: u64,
}

impl Default for NoiseRetryConfig {
    fn default() -> Self {
        Self {
            per_attempt_timeout_secs: 5,
            max_attempts: 2,
            backoff_ms: 250,
        }
    }
}

impl NoiseRetryConfig {
    pub fn per_attempt_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.per_attempt_timeout_secs)
    }

    pub fn backoff(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.backoff_ms)
    }
}

/// Settings for the unsafe HS256 ("HMAC") token decman presents to Canton
/// when running in [`NodeConfig::insecure`] mode. Point Canton's unsafe auth
/// service at the same `secret`/`audience` to have it accept the token.
///
/// Never serialized (the `secret` must not leak); populated only from CLI/env.
#[derive(Clone, Debug)]
pub struct InsecureAuthConfig {
    /// HS256 signing secret. Canton's conventional dev secret is `unsafe`.
    pub secret: String,
    /// `aud` claim.
    pub audience: String,
    /// `sub` claim, doubling as the ledger-api user id.
    pub subject: String,
}

impl Default for InsecureAuthConfig {
    fn default() -> Self {
        Self {
            secret: "unsafe".to_string(),
            audience: "https://canton.network.global".to_string(),
            subject: "ledger-api-user".to_string(),
        }
    }
}

/// Individual node configuration
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NodeConfig {
    pub node: NodeInfo,
    pub canton: CantonConfig,
    pub timeouts: Timeouts,
    pub noise_retry: NoiseRetryConfig,
    /// Top-level Keycloak config for frontend website gating
    pub keycloak: Option<KeycloakConfig>,
    /// Top-level Auth0 config for frontend website gating (mutually exclusive
    /// with `keycloak` — operator picks one via env vars at deploy time).
    pub auth0: Option<Auth0Config>,
    /// Run without an IdP: accept any inbound token and mint an unsafe HS256
    /// token for Canton. Never enable in production. Not serialized.
    #[serde(skip)]
    pub insecure: bool,
    /// Unsafe token settings, used only when `insecure` is set. Not serialized
    /// (carries the signing secret).
    #[serde(skip)]
    pub insecure_auth: InsecureAuthConfig,
    /// Bearer API keys a wallet provider presents to the `/v0/tenant/*`
    /// endpoints, authenticated separately from the Keycloak UI token. Empty
    /// disables the tenant API (except in insecure/test mode). Not serialized.
    #[serde(skip)]
    pub tenant_api_keys: std::collections::HashSet<String>,
    /// Root directory containing data/ subdirectory
    #[serde(skip)]
    root_dir: PathBuf,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            node: NodeInfo::default(),
            canton: CantonConfig::default(),
            timeouts: Timeouts::default(),
            noise_retry: NoiseRetryConfig::default(),
            keycloak: None,
            auth0: None,
            insecure: false,
            insecure_auth: InsecureAuthConfig::default(),
            tenant_api_keys: std::collections::HashSet::new(),
            root_dir: PathBuf::new(),
        }
    }
}

/// Node-specific information
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NodeInfo {
    /// Canton participant ID for this node (e.g., "participant1::1220...").
    /// Always resolved before serving, so it is non-null on the wire.
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub participant_id: Option<CantonId>,
    /// Address to listen on for Noise protocol connections
    pub listen_address: String,
    /// Port to listen on for Noise protocol connections
    pub port: u16,
    /// Public address that other peers should use to connect to this node
    pub public_address: Option<String>,
}

impl NodeInfo {
    /// Get the public address for this node (for sharing with peers).
    /// Falls back to listen_address if public_address is not set.
    pub fn public_address(&self) -> &str {
        self.public_address
            .as_deref()
            .unwrap_or(&self.listen_address)
    }
}

impl Default for NodeInfo {
    fn default() -> Self {
        Self {
            participant_id: None,
            listen_address: "0.0.0.0".to_string(),
            port: 9000,
            public_address: None,
        }
    }
}

/// Default Keycloak configuration values for a network
pub struct KeycloakDefaults {
    /// Keycloak server URL
    pub url: String,
    /// Keycloak realm name
    pub realm: String,
}

/// Canton Network environment
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema, clap::ValueEnum,
)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Devnet,
    Testnet,
    Mainnet,
}

impl Network {
    /// Get the DSO API base URL for this network
    pub fn dso_url(&self) -> &str {
        match self {
            Network::Devnet => "https://docs.dev.global.canton.network.sync.global/dso",
            Network::Testnet => "https://docs.test.global.canton.network.sync.global/dso",
            Network::Mainnet => "https://docs.global.canton.network.sync.global/dso",
        }
    }

    /// Get the DA Utility operator API URL for this network
    pub fn operator_url(&self) -> &str {
        match self {
            Network::Devnet => {
                "https://api.utilities.digitalasset-dev.com/api/utilities/v0/operator"
            }
            Network::Testnet => {
                "https://api.utilities.digitalasset-staging.com/api/utilities/v0/operator"
            }
            Network::Mainnet => "https://api.utilities.digitalasset.com/api/utilities/v0/operator",
        }
    }

    /// Get the DA token-standard registry base URL for this network.
    ///
    /// Used to fetch choice contexts and disclosed contracts for token-standard
    /// transfer flows (e.g. `AcceptTransfer` requires the `transfer-rule`
    /// context entry, which the registry resolves per `TransferInstruction`).
    pub fn registry_url(&self) -> &str {
        match self {
            Network::Devnet => "https://api.utilities.digitalasset-dev.com",
            Network::Testnet => "https://api.utilities.digitalasset-staging.com",
            Network::Mainnet => "https://api.utilities.digitalasset.com",
        }
    }

    /// Get default Keycloak configuration for this network
    pub fn keycloak_defaults(&self) -> KeycloakDefaults {
        match self {
            Network::Devnet => KeycloakDefaults {
                url: "https://keycloak.dev.canton.ibtc.network".to_string(),
                realm: "ibtc-catalyst-devnet".to_string(),
            },
            Network::Testnet => KeycloakDefaults {
                url: String::new(),
                realm: String::new(),
            },
            Network::Mainnet => KeycloakDefaults {
                url: String::new(),
                realm: String::new(),
            },
        }
    }
}

/// Default package identifiers used for new party configurations
pub fn default_package_config() -> PackageConfig {
    PackageConfig {
        governance_action: Some("#governance-action-v1".to_string()),
        governance_core: Some("#governance-core-v1".to_string()),
        governance_rewards: Some("#governance-rewards-v1".to_string()),
        governance_token_custody: Some("#governance-token-custody-v1".to_string()),
        governance_utility_credential: Some("#governance-utility-credential-v1".to_string()),
        governance_utility_onboarding: Some("#governance-utility-onboarding-v1".to_string()),
        utility_credential: Some("#utility-credential-v0".to_string()),
        utility_credential_app: Some("#utility-credential-app-v0".to_string()),
        utility_registry: Some("#utility-registry-app-v0".to_string()),
        vault: Some("#bitsafe-vault-v0-rc8".to_string()),
        vault_governance: Some("#bitsafe-vault-governance-v0-rc8".to_string()),
    }
}

/// TLS settings for one Canton gRPC endpoint.
///
/// Off by default: a participant reachable only over a trusted private
/// network (loopback, a Docker network, a pod) serves plaintext h2c, which is
/// what every deployment did before this existed.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CantonTlsConfig {
    /// Speak TLS to this endpoint.
    pub enabled: bool,
    /// PEM file holding the CA that issued the endpoint's certificate. Needed
    /// for the usual case of a participant behind a private CA; when unset,
    /// the platform trust store is used.
    pub ca_cert: Option<String>,
    /// PEM client certificate, for endpoints that require mTLS. Must be set
    /// together with `client_key`.
    pub client_cert: Option<String>,
    /// PEM private key matching `client_cert`.
    pub client_key: Option<String>,
    /// Name to validate the server certificate against, when it differs from
    /// the configured host — the common case being a certificate issued for a
    /// service DNS name while DecMan connects by IP.
    pub domain: Option<String>,
}

/// Canton participant configuration
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CantonConfig {
    pub admin_api_host: String,
    pub admin_api_port: u16,
    pub ledger_api_host: String,
    pub ledger_api_port: u16,
    pub synchronizer: String,
    /// Canton Network environment (devnet, testnet, mainnet)
    pub network: Network,
    /// TLS for the Admin API channel.
    #[serde(default)]
    pub admin_api_tls: CantonTlsConfig,
    /// TLS for the Ledger API channel.
    #[serde(default)]
    pub ledger_api_tls: CantonTlsConfig,
}

impl Default for CantonConfig {
    fn default() -> Self {
        Self {
            admin_api_host: "127.0.0.1".to_string(),
            admin_api_port: 5002,
            ledger_api_host: "127.0.0.1".to_string(),
            ledger_api_port: 5001,
            synchronizer: "global".to_string(),
            network: Network::Devnet,
            admin_api_tls: CantonTlsConfig::default(),
            ledger_api_tls: CantonTlsConfig::default(),
        }
    }
}

impl NodeConfig {
    /// Create a NodeConfig with the given root directory
    pub fn with_root_dir<P: AsRef<Path>>(mut self, root_dir: P) -> Self {
        self.root_dir = root_dir.as_ref().to_path_buf();
        self
    }

    /// Get the full Admin API URL
    pub fn admin_api_url(&self) -> String {
        format!(
            "{scheme}://{host}:{port}",
            scheme = scheme(&self.canton.admin_api_tls),
            host = self.canton.admin_api_host,
            port = self.canton.admin_api_port
        )
    }

    /// Get the full Ledger API URL
    pub fn ledger_api_url(&self) -> String {
        format!(
            "{scheme}://{host}:{port}",
            scheme = scheme(&self.canton.ledger_api_tls),
            host = self.canton.ledger_api_host,
            port = self.canton.ledger_api_port
        )
    }

    /// Connect a gRPC channel to the participant's Admin API, applying the
    /// configured TLS settings.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS material cannot be read or the endpoint
    /// cannot be reached. A plaintext/TLS mismatch — the failure mode that
    /// otherwise shows up as a bare `transport error` — is annotated with
    /// what to change.
    pub async fn admin_channel(&self) -> Result<Channel> {
        connect_channel(&self.admin_api_url(), &self.canton.admin_api_tls, "admin").await
    }

    /// Connect a gRPC channel to the participant's Ledger API, applying the
    /// configured TLS settings.
    ///
    /// # Errors
    ///
    /// As [`NodeConfig::admin_channel`].
    pub async fn ledger_channel(&self) -> Result<Channel> {
        connect_channel(
            &self.ledger_api_url(),
            &self.canton.ledger_api_tls,
            "ledger",
        )
        .await
    }

    /// Get the synchronizer name
    pub fn synchronizer(&self) -> &str {
        &self.canton.synchronizer
    }

    /// Get the data directory
    pub fn data_dir(&self) -> PathBuf {
        self.root_dir.join(DATA_DIR)
    }

    /// Get the path to the noise key file
    pub fn key_file_path(&self) -> PathBuf {
        self.data_dir().join(NOISE_KEY_FILENAME)
    }

    /// Get the dars directory
    pub fn dars_dir(&self) -> PathBuf {
        self.data_dir().join(DARS_DIR)
    }

    /// Get the path to the SQLite database file
    pub fn db_path(&self) -> PathBuf {
        self.data_dir().join(DB_FILENAME)
    }

    /// Get the root directory
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Get the participant ID, panicking if not resolved
    ///
    /// Call `resolve_participant_id` before using this method.
    pub fn participant_id(&self) -> &CantonId {
        self.node
            .participant_id
            .as_ref()
            .expect("participant_id not resolved - call resolve_participant_id first")
    }

    /// Check if participant_id is already set
    pub fn has_participant_id(&self) -> bool {
        self.node.participant_id.is_some()
    }
}

fn scheme(tls: &CantonTlsConfig) -> &'static str {
    if tls.enabled { "https" } else { "http" }
}

/// Build the tonic TLS settings for an endpoint.
///
/// With no `ca_cert` the platform trust store is used, which covers a
/// publicly-issued certificate; a participant behind a private CA needs the
/// CA PEM. `client_cert` + `client_key` are only for endpoints demanding
/// mTLS.
async fn client_tls_config(tls: &CantonTlsConfig, label: &str) -> Result<ClientTlsConfig> {
    let mut config = match &tls.ca_cert {
        Some(path) => {
            let pem = tokio::fs::read(path).await.with_context(|| {
                format!("reading the {label} API TLS CA certificate from {path}")
            })?;
            ClientTlsConfig::new().ca_certificate(Certificate::from_pem(pem))
        }
        None => ClientTlsConfig::new().with_enabled_roots(),
    };

    match (&tls.client_cert, &tls.client_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert = tokio::fs::read(cert_path).await.with_context(|| {
                format!("reading the {label} API TLS client certificate from {cert_path}")
            })?;
            let key = tokio::fs::read(key_path).await.with_context(|| {
                format!("reading the {label} API TLS client key from {key_path}")
            })?;
            config = config.identity(Identity::from_pem(cert, key));
        }
        (None, None) => {}
        (cert, _) => {
            let (set, missing) = if cert.is_some() {
                ("certificate", "key")
            } else {
                ("key", "certificate")
            };
            anyhow::bail!(
                "{label} API mTLS is half-configured: the client {set} is set but the \
                 client {missing} is not. Set both or neither."
            );
        }
    }

    if let Some(domain) = &tls.domain {
        config = config.domain_name(domain);
    }

    Ok(config)
}

/// Connect a channel to `url`, applying `tls`.
///
/// The error path is the point of this function as much as the happy path:
/// a plaintext client against a TLS listener (and the reverse) fails deep in
/// the transport with nothing that names TLS, which is exactly how #260 was
/// reported — a permanent `transport error` / BrokenPipe on every call.
async fn connect_channel(url: &str, tls: &CantonTlsConfig, label: &str) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .with_context(|| format!("{url} is not a valid {label} API endpoint"))?;

    if tls.enabled {
        endpoint = endpoint
            .tls_config(client_tls_config(tls, label).await?)
            .with_context(|| format!("applying TLS settings to the {label} API channel"))?;
    }

    endpoint.connect().await.map_err(|e| {
        let hint = if tls.enabled {
            format!(
                "TLS is enabled for the {label} API. If the endpoint actually serves \
                 plaintext h2c, unset DECPM_CANTON_{upper}_TLS; if it serves TLS under a \
                 private CA, point DECPM_CANTON_{upper}_TLS_CA_CERT at that CA",
                upper = label.to_uppercase()
            )
        } else {
            format!(
                "the {label} API channel is plaintext. If the endpoint serves TLS it closes \
                 the connection on the first bytes, which looks exactly like this; set \
                 DECPM_CANTON_{upper}_TLS=true",
                upper = label.to_uppercase()
            )
        };
        anyhow::Error::new(e).context(format!("connecting to {url}: {hint}"))
    })
}

#[cfg(test)]
mod tls_tests {
    use super::*;

    /// A port nothing listens on, so `connect` fails immediately and the test
    /// only exercises how that failure is reported.
    const CLOSED: &str = "http://127.0.0.1:1";

    #[test]
    fn urls_carry_the_scheme_the_tls_flag_implies() {
        let mut config = NodeConfig::default();
        assert_eq!(config.admin_api_url(), "http://127.0.0.1:5002");
        assert_eq!(config.ledger_api_url(), "http://127.0.0.1:5001");

        config.canton.admin_api_tls.enabled = true;
        assert_eq!(config.admin_api_url(), "https://127.0.0.1:5002");
        // Each endpoint is configured on its own: a TLS admin API says
        // nothing about the ledger API.
        assert_eq!(config.ledger_api_url(), "http://127.0.0.1:5001");

        config.canton.ledger_api_tls.enabled = true;
        assert_eq!(config.ledger_api_url(), "https://127.0.0.1:5001");
    }

    #[tokio::test]
    async fn half_configured_mtls_is_rejected() {
        let tls = CantonTlsConfig {
            enabled: true,
            client_cert: Some("/tmp/client.pem".to_string()),
            ..Default::default()
        };

        let message = match client_tls_config(&tls, "admin").await {
            Ok(_) => panic!("a client certificate without a key must be rejected"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            message.contains("half-configured"),
            "unhelpful error: {message}"
        );
    }

    #[tokio::test]
    async fn a_missing_ca_file_names_the_path() {
        let tls = CantonTlsConfig {
            enabled: true,
            ca_cert: Some("/nonexistent/ca.pem".to_string()),
            ..Default::default()
        };

        let message = match client_tls_config(&tls, "ledger").await {
            Ok(_) => panic!("an unreadable CA certificate must be rejected"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            message.contains("/nonexistent/ca.pem"),
            "unhelpful error: {message}"
        );
    }

    /// #260's reported symptom: a plaintext client against a TLS endpoint
    /// fails with a bare transport error naming nothing. The connect error
    /// must point at the knob that fixes it.
    #[tokio::test]
    async fn a_plaintext_failure_points_at_the_tls_flag() {
        let tls = CantonTlsConfig::default();

        let message = match connect_channel(CLOSED, &tls, "admin").await {
            Ok(_) => panic!("nothing listens on port 1"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            message.contains("DECPM_CANTON_ADMIN_TLS=true"),
            "unhelpful error: {message}"
        );
    }

    /// And the mirror case, so an operator who turned TLS on against a
    /// plaintext endpoint is not left guessing either.
    #[tokio::test]
    async fn a_tls_failure_points_back_at_plaintext_and_the_ca() {
        let tls = CantonTlsConfig {
            enabled: true,
            ..Default::default()
        };

        let message = match connect_channel("https://127.0.0.1:1", &tls, "ledger").await {
            Ok(_) => panic!("nothing listens on port 1"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            message.contains("unset DECPM_CANTON_LEDGER_TLS"),
            "unhelpful error: {message}"
        );
        assert!(
            message.contains("DECPM_CANTON_LEDGER_TLS_CA_CERT"),
            "unhelpful error: {message}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canton_id::{NAMESPACE_LENGTH, Namespace};

    fn test_peer(index: u8, pub_key: &str) -> Peer {
        let namespace = format!("1220{:0>64}", format!("{index:02x}"));
        Peer {
            participant_id: CantonId::parse(&format!("node{index}::{namespace}")).unwrap(),
            name: format!("Node {index}"),
            address: format!("10.0.1.{index}"),
            port: 9000,
            public_key: pub_key.to_string(),
            party: None,
        }
    }

    #[test]
    fn test_governance_threshold() {
        let network = NetworkConfig {
            peers: vec![
                test_peer(1, "abc123"),
                test_peer(2, "def456"),
                test_peer(3, "ghi789"),
            ],
        };

        assert_eq!(network.governance_threshold(), 2);
    }

    fn dummy_peer() -> Peer {
        Peer {
            participant_id: CantonId::new(
                "node".to_string(),
                Namespace::new([0u8; NAMESPACE_LENGTH]),
            ),
            name: "n".to_string(),
            address: "127.0.0.1".to_string(),
            port: 9000,
            public_key: "deadbeef".to_string(),
            party: None,
        }
    }

    #[test]
    fn governance_threshold_is_strict_majority_across_sizes() {
        // (n/2 + 1): a strict majority for both odd and even member counts.
        // The even cases (2->2, 4->3, 6->4) require strictly more than half,
        // which is exactly where off-by-one majority bugs hide. n=0 yields 1
        // by the formula — documented here as the degenerate-empty behavior.
        for (n, expected) in [(0u32, 1u32), (1, 1), (2, 2), (3, 2), (4, 3), (5, 3), (6, 4)] {
            let network = NetworkConfig::from_peers((0..n).map(|_| dummy_peer()).collect());
            assert_eq!(
                network.governance_threshold(),
                expected,
                "threshold for {n} peers"
            );
        }
    }

    #[test]
    fn test_registry_url_per_network() {
        assert_eq!(
            Network::Devnet.registry_url(),
            "https://api.utilities.digitalasset-dev.com",
        );
        assert_eq!(
            Network::Testnet.registry_url(),
            "https://api.utilities.digitalasset-staging.com",
        );
        assert_eq!(
            Network::Mainnet.registry_url(),
            "https://api.utilities.digitalasset.com",
        );
    }

    #[test]
    fn test_keycloak_defaults_devnet() {
        let defaults = Network::Devnet.keycloak_defaults();
        assert_eq!(defaults.url, "https://keycloak.dev.canton.ibtc.network");
        assert_eq!(defaults.realm, "ibtc-catalyst-devnet");
    }

    #[test]
    fn test_keycloak_defaults_testnet_mainnet_empty() {
        for network in [Network::Testnet, Network::Mainnet] {
            let defaults = network.keycloak_defaults();
            assert!(defaults.url.is_empty());
            assert!(defaults.realm.is_empty());
        }
    }

    #[test]
    fn test_default_package_config() {
        let packages = default_package_config();
        assert_eq!(
            packages.governance_action.as_deref(),
            Some("#governance-action-v1"),
        );
        assert_eq!(
            packages.governance_core.as_deref(),
            Some("#governance-core-v1"),
        );
        assert_eq!(
            packages.governance_rewards.as_deref(),
            Some("#governance-rewards-v1"),
        );
        assert_eq!(
            packages.governance_token_custody.as_deref(),
            Some("#governance-token-custody-v1"),
        );
        assert_eq!(
            packages.governance_utility_credential.as_deref(),
            Some("#governance-utility-credential-v1"),
        );
        assert_eq!(
            packages.governance_utility_onboarding.as_deref(),
            Some("#governance-utility-onboarding-v1"),
        );
        assert_eq!(
            packages.utility_credential.as_deref(),
            Some("#utility-credential-v0"),
        );
        assert_eq!(
            packages.utility_credential_app.as_deref(),
            Some("#utility-credential-app-v0"),
        );
        assert_eq!(
            packages.utility_registry.as_deref(),
            Some("#utility-registry-app-v0"),
        );
        assert_eq!(packages.vault.as_deref(), Some("#bitsafe-vault-v0-rc8"));
        assert_eq!(
            packages.vault_governance.as_deref(),
            Some("#bitsafe-vault-governance-v0-rc8"),
        );
    }
}
