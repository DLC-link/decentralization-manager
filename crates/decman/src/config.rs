use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};

use crate::{
    canton_id::CantonId,
    consts::{DARS_DIR, DATA_DIR, DB_FILENAME},
    error::Result,
};

/// Network configuration - list of peers in the network
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NetworkConfig {
    /// List of peers in the network
    pub peers: Vec<Peer>,
}

/// A peer in the network (design D2). Operators exchange one string per
/// peer, `participant_id,node_party_id,name`; nothing else is needed because
/// nodes coordinate only through Canton.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Peer {
    /// Canton participant UID (e.g., "participant1::1220...")
    pub participant_id: CantonId,
    /// Human-readable name
    pub name: String,
    /// The peer's node party (design D1). A peer without one cannot be
    /// invited to a workflow.
    #[serde(default)]
    pub party: Option<CantonId>,
}

impl NetworkConfig {
    /// Construct a NetworkConfig from a list of peers (e.g., loaded from DB)
    pub fn from_peers(peers: Vec<Peer>) -> Self {
        Self { peers }
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
    /// Extra space-separated scopes the SPA requests on top of the default
    /// `openid profile email`. Auth0 RBAC returns a permission in `scope` only
    /// when the client asked for it, so an admin role granted as a
    /// resource-server scope needs naming here to reach the token.
    #[serde(default)]
    pub scope: Option<String>,
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

/// What a `party_credentials` row stands for.
///
/// A `Decparty` row maps one decentralized party to the member party that
/// acts for it on this node. A `Node` row is this node's own identity (design
/// D1): `dec_party_id` and `member_party_id` both hold the node party, so
/// `AuthRegistry::get(node_party)` returns its token manager unchanged.
/// Decparty views filter on `Decparty`; the inbound JWT trust set skips `Node`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CredentialKind {
    #[default]
    Decparty,
    Node,
}

impl CredentialKind {
    /// The stored column value; matches the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decparty => "decparty",
            Self::Node => "node",
        }
    }
}

impl std::fmt::Display for CredentialKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CredentialKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "decparty" => Ok(Self::Decparty),
            "node" => Ok(Self::Node),
            other => Err(anyhow::anyhow!("unknown credential kind: {other}")),
        }
    }
}

/// Credentials for a specific decentralized party
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PartyCredentials {
    /// Whether this row is a decparty mapping or the node identity.
    #[serde(default)]
    pub kind: CredentialKind,
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
    /// Tick interval (seconds) for the CIP-104 Mode A reward-assignment
    /// automation loop. Enablement is on-ledger (presence of a
    /// `CouponReassignmentDelegation`), so this only controls cadence. Default 300s.
    pub reward_automation_interval_secs: u64,
    /// How often (seconds) to re-read the backlog purely to refresh the expiry
    /// gauge, when no sweep is due. A sweep reads the ledger anyway, so the gauge
    /// refreshes at whichever of the two intervals is shorter. Separating them lets
    /// the sweep interval stay long enough to fill a `Delegation_Assign` chunk
    /// without making the expiry signal that stale. Default 3600s.
    pub reward_expiry_read_interval_secs: u64,
    /// Ceiling on an ACS snapshot the wallet relays over the tenant API, in
    /// bytes.
    ///
    /// The decparty add-party path has no equivalent ceiling: its operator
    /// streams the snapshot through a file, so nothing assembles it in memory.
    ///
    /// Still bounded: the snapshot is assembled in memory on both ends, so this
    /// is a real memory commitment on the exporting and importing nodes. Raise
    /// it deliberately.
    /// Output contracts one `Delegation_Assign` may create, which bounds the
    /// coupons per transaction (`/ beneficiary_count`). The ledger's real
    /// ceiling for this transaction shape is unmeasured — configurable so it can
    /// be raised stepwise against a live ledger without a rebuild. Set too high,
    /// the ledger rejects each chunk and the tick ends having assigned nothing,
    /// so lower it if assigns start failing. Default 100.
    pub reward_max_creates: usize,
    /// How much time (seconds) a coupon must have left before expiry to be
    /// assigned. This guards against a coupon vanishing between the ACS read
    /// and the commit, which would fail its whole chunk — it is NOT a reserve
    /// of minting time for the beneficiary. Withholding a coupon guarantees it
    /// is never minted, whereas assigning it late still lets the beneficiary
    /// try, so this should be a small submission-latency allowance rather than
    /// a generous window. Default 120s.
    pub reward_min_expiry_margin_secs: u64,
    /// Port serving Prometheus metrics at `/metrics`, on its own listener rather
    /// than the API port. 0 serves no metrics. Default 9464.
    pub metrics_port: u16,
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
            reward_automation_interval_secs: 300,
            reward_expiry_read_interval_secs: 3600,
            reward_max_creates: 100,
            reward_min_expiry_margin_secs: 120,
            metrics_port: 9464,
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
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NodeInfo {
    /// Canton participant ID for this node (e.g., "participant1::1220...").
    /// Always resolved before serving, so it is non-null on the wire.
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub participant_id: Option<CantonId>,
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
        governance_rewards: Some("#governance-rewards-automation-v1".to_string()),
        governance_token_custody: Some("#governance-token-custody-v1".to_string()),
        governance_utility_credential: Some("#governance-utility-credential-v1".to_string()),
        governance_utility_onboarding: Some("#governance-utility-onboarding-v1".to_string()),
        utility_credential: Some("#utility-credential-v0".to_string()),
        utility_credential_app: Some("#utility-credential-app-v0".to_string()),
        utility_registry: Some("#utility-registry-app-v0".to_string()),
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
    pub fn has_top_level_idp(&self) -> bool {
        self.keycloak.is_some() || self.auth0.is_some()
    }

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

/// How long a gRPC channel may take to establish before it is a failure.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the reachability probe in [`connect_advice`] waits.
const TCP_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a plain TCP connect to `uri` succeeds.
///
/// tonic reports a refused connection and a TLS mismatch through the same
/// opaque transport error, and reading its source chain only works while its
/// internals keep exposing one. A TCP connect is direct evidence instead: if
/// even that fails, nothing is listening and TLS cannot be the problem.
async fn tcp_reachable(uri: &tonic::transport::Uri) -> bool {
    let Some(host) = uri.host() else {
        return true;
    };
    // `Uri::host` keeps the brackets on an IPv6 literal, and `TcpStream`
    // resolves `[::1]` as a hostname and fails. Without this the advice claims
    // nothing is listening whenever the endpoint is addressed by IPv6.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
        Some("https") => 443,
        _ => 80,
    });

    matches!(
        tokio::time::timeout(
            TCP_PROBE_TIMEOUT,
            tokio::net::TcpStream::connect((host, port)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// What to tell the operator about a failed connect.
///
/// A TLS mismatch and an unreachable endpoint need opposite fixes, so the two
/// have to stay apart in the message. #260 was a real mismatch, but the same
/// hint on a refused connection points at the wrong knob: when nothing is
/// listening, no TLS setting changes the outcome.
fn connect_advice(reachable: bool, tls_enabled: bool, label: &str) -> String {
    if !reachable {
        return format!(
            "nothing accepts TCP connections there. The {label} API is down, \
             still starting up, or the host and port are wrong. TLS settings \
             play no part in this failure"
        );
    }

    let upper = label.to_uppercase();
    if tls_enabled {
        format!(
            "TLS is enabled for the {label} API. If the endpoint actually serves \
             plaintext h2c, unset DECPM_CANTON_{upper}_TLS; if it serves TLS under a \
             private CA, point DECPM_CANTON_{upper}_TLS_CA_CERT at that CA"
        )
    } else {
        format!(
            "the {label} API channel is plaintext. If the endpoint serves TLS it closes \
             the connection on the first bytes, which looks exactly like this; set \
             DECPM_CANTON_{upper}_TLS=true"
        )
    }
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

    // Bound connection establishment. Without this a black-holed address hangs
    // on the OS TCP timeout, which outlives any deadline a caller puts around
    // the call it is trying to make.
    endpoint = endpoint.connect_timeout(CONNECT_TIMEOUT);

    if tls.enabled {
        endpoint = endpoint
            .tls_config(client_tls_config(tls, label).await?)
            .with_context(|| format!("applying TLS settings to the {label} API channel"))?;
    }

    let error = match endpoint.connect().await {
        Ok(channel) => return Ok(channel),
        Err(error) => error,
    };

    let hint = connect_advice(tcp_reachable(endpoint.uri()).await, tls.enabled, label);
    Err(anyhow::Error::new(error).context(format!("connecting to {url}: {hint}")))
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
    /// fails with a bare transport error naming nothing. The advice must point
    /// at the knob that fixes it. The server accepts the TCP connection and
    /// then drops it on the h2 preface, so the failure arrives as a reset or a
    /// broken pipe rather than as a connect error.
    #[test]
    fn a_plaintext_failure_points_at_the_tls_flag() {
        let advice = connect_advice(true, false, "admin");

        assert!(
            advice.contains("DECPM_CANTON_ADMIN_TLS=true"),
            "unhelpful advice: {advice}"
        );
    }

    /// And the mirror case, so an operator who turned TLS on against a
    /// plaintext endpoint is not left guessing either.
    #[test]
    fn a_tls_failure_points_back_at_plaintext_and_the_ca() {
        let advice = connect_advice(true, true, "ledger");

        assert!(
            advice.contains("unset DECPM_CANTON_LEDGER_TLS"),
            "unhelpful advice: {advice}"
        );
        assert!(
            advice.contains("DECPM_CANTON_LEDGER_TLS_CA_CERT"),
            "unhelpful advice: {advice}"
        );
    }

    /// An IPv6 endpoint that is listening must read as reachable. `Uri::host`
    /// keeps the brackets, and a bracketed literal handed to `TcpStream` is
    /// resolved as a hostname and fails, which would blame TLS for a healthy
    /// endpoint.
    #[tokio::test]
    async fn a_listening_ipv6_endpoint_is_reachable() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("[::1]:0").await?;
        let port = listener.local_addr()?.port();
        let uri: tonic::transport::Uri = format!("http://[::1]:{port}").parse()?;

        assert!(
            tcp_reachable(&uri).await,
            "a listening IPv6 endpoint must not read as unreachable"
        );
        Ok(())
    }

    /// The regression this pairs with: an unreachable endpoint has nothing to
    /// do with TLS, so saying otherwise points at the wrong knob. A refused
    /// connection must not name the flag in either direction.
    #[tokio::test]
    async fn a_refused_connection_does_not_blame_tls() {
        for (tls, label, flag) in [
            (
                CantonTlsConfig::default(),
                "admin",
                "DECPM_CANTON_ADMIN_TLS",
            ),
            (
                CantonTlsConfig {
                    enabled: true,
                    ..Default::default()
                },
                "ledger",
                "DECPM_CANTON_LEDGER_TLS",
            ),
        ] {
            let url = if tls.enabled {
                "https://127.0.0.1:1"
            } else {
                CLOSED
            };

            let message = match connect_channel(url, &tls, label).await {
                Ok(_) => panic!("nothing listens on port 1"),
                Err(e) => format!("{e:#}"),
            };
            assert!(
                !message.contains(flag),
                "a refused connection blamed TLS: {message}"
            );
            assert!(
                message.contains("nothing accepts TCP connections there"),
                "unhelpful error: {message}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer(index: u8) -> Peer {
        let namespace = format!("1220{:0>64}", format!("{index:02x}"));
        Peer {
            participant_id: CantonId::parse(&format!("node{index}::{namespace}")).unwrap(),
            name: format!("Node {index}"),
            party: CantonId::parse(&format!("node{index}-party::{namespace}")).ok(),
        }
    }

    /// The peer JSON is what the UI and the CLI post to `/network-config`, so
    /// the field set is a wire contract: exactly `participant_id`, `name`,
    /// and `party`, with `party` optional on the way in.
    #[test]
    fn peer_wire_shape_is_participant_name_party() -> anyhow::Result<()> {
        let peer = test_peer(1);
        let json = serde_json::to_value(&peer)?;
        let keys: Vec<&str> = json
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(keys, ["name", "participant_id", "party"]);

        let without_party: Peer = serde_json::from_value(serde_json::json!({
            "participant_id": peer.participant_id.to_string(),
            "name": "Node 1",
        }))?;
        assert!(without_party.party.is_none());

        let network = NetworkConfig::from_peers(vec![test_peer(1), test_peer(2)]);
        assert_eq!(network.peers.len(), 2);
        Ok(())
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

    fn auth0_config() -> Auth0Config {
        Auth0Config {
            domain: "tenant.eu.auth0.com".to_string(),
            client_id: "spa-client-id".to_string(),
            audience: Some("https://decman-api.example".to_string()),
            scope: None,
        }
    }

    #[test]
    fn has_top_level_idp_is_false_when_neither_provider_is_set() {
        assert!(!NodeConfig::default().has_top_level_idp());
    }

    #[test]
    fn has_top_level_idp_accepts_a_keycloak_only_node() {
        let config = NodeConfig {
            keycloak: Some(KeycloakConfig::default()),
            auth0: None,
            ..NodeConfig::default()
        };

        assert!(config.has_top_level_idp());
    }

    #[test]
    fn has_top_level_idp_accepts_an_auth0_only_node() {
        let config = NodeConfig {
            keycloak: None,
            auth0: Some(auth0_config()),
            ..NodeConfig::default()
        };

        assert!(config.has_top_level_idp());
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
            Some("#governance-rewards-automation-v1"),
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
    }
}
