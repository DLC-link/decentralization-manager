use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use common::types::{
    AuditLogEntry, AuthConfigResponse, ConnectionStatus, DecentralizedParty, ParticipantStatus,
    ParticipantsStatusResponse, PeerPackageComparison, PendingInvitation, VettedPackageInfo,
    WorkflowInfo, WorkflowRun,
};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// User credentials and optional IdP overrides for the OAuth2 password grant.
///
/// The TUI authenticates exactly like the web frontend: a human's username and
/// password. The token endpoint and client id are discovered from the API's
/// public `/auth-config` unless explicitly overridden here.
#[derive(Clone, Debug)]
pub struct AuthSettings {
    pub username: String,
    pub password: String,
    /// Token endpoint override; discovered from `/auth-config` when `None`.
    pub token_url: Option<String>,
    /// Client id override; discovered from `/auth-config` when `None`.
    pub client_id: Option<String>,
    /// Client secret, for confidential clients (public SPA clients omit it).
    pub client_secret: Option<String>,
    /// Audience, required by some IdPs (e.g. Auth0).
    pub audience: Option<String>,
    /// Optional space-separated scopes.
    pub scope: Option<String>,
}

/// Human-readable party name: the prefix of the Canton party id (the segment
/// before `::`), which the [`CantonId`] type already splits out.
pub fn party_name(party: &DecentralizedParty) -> &str {
    party.party_id.prefix.as_str()
}

/// Sort vetted packages by name, then version — matching the web frontend.
fn sort_vetted(packages: &mut [VettedPackageInfo]) {
    packages.sort_by(|a, b| {
        a.package_name
            .cmp(&b.package_name)
            .then_with(|| a.package_version.cmp(&b.package_version))
    });
}

/// A peer merged from `/network-config` and live `/participants-status`, ready
/// for display — the same information the web frontend's network panel shows.
#[derive(Clone, Debug)]
pub struct PeerView {
    pub participant_id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    /// Live connectivity status, or `None` when no status was reported for this
    /// peer (rendered as "Unknown").
    pub status: Option<ConnectionStatus>,
    pub latency_ms: Option<u64>,
    pub version: Option<String>,
    pub workflow: Option<String>,
    pub is_self: bool,
}

/// `/network-config` response: the configured peer list.
#[derive(Debug, Deserialize)]
struct NetworkConfigResponse {
    #[serde(default)]
    peers: Vec<PeerConfig>,
}

/// A configured peer (name, address) from `/network-config`.
#[derive(Debug, Deserialize)]
struct PeerConfig {
    participant_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    port: u16,
}

/// `/node-config` response: this node's own identity and version. The backend
/// flattens its `NodeConfig`, so `node` and `version` are top-level fields.
#[derive(Debug, Deserialize)]
struct NodeConfigResponse {
    node: NodeIdentity,
    #[serde(default)]
    version: Option<String>,
}

/// This node's identity, as reported by `/node-config`.
#[derive(Debug, Deserialize)]
struct NodeIdentity {
    participant_id: String,
    #[serde(default)]
    public_address: Option<String>,
    #[serde(default)]
    listen_address: String,
    #[serde(default)]
    port: u16,
}

/// Display name for a peer: its configured name, else the id prefix.
fn display_name(name: &str, participant_id: &str) -> String {
    if name.is_empty() {
        participant_id
            .split("::")
            .next()
            .unwrap_or(participant_id)
            .to_owned()
    } else {
        name.to_owned()
    }
}

/// Format a peer's active workflow as `Kind (step)` (or just `Kind`).
fn format_workflow(workflow: &WorkflowInfo) -> String {
    if workflow.step.is_empty() {
        workflow.kind.to_string()
    } else {
        format!(
            "{kind} ({step})",
            kind = workflow.kind,
            step = workflow.step
        )
    }
}

/// Merge this node and the configured peers with their live statuses, putting
/// this node first. If this node is not in the peer list, it is synthesized
/// from `/node-config` so it is always shown — exactly like the web frontend.
fn merge_peers(
    node: &NodeConfigResponse,
    peers: &[PeerConfig],
    statuses: &[ParticipantStatus],
) -> Vec<PeerView> {
    let by_id: HashMap<&str, &ParticipantStatus> =
        statuses.iter().map(|s| (s.id.as_str(), s)).collect();
    let self_id = node.node.participant_id.as_str();
    let mut self_seen = false;

    let mut views: Vec<PeerView> = peers
        .iter()
        .map(|peer| {
            let live = by_id.get(peer.participant_id.as_str());
            let is_self = peer.participant_id == self_id;
            self_seen |= is_self;
            let status = if is_self {
                Some(ConnectionStatus::CurrentNode)
            } else {
                live.map(|s| s.status)
            };
            PeerView {
                participant_id: peer.participant_id.clone(),
                name: display_name(&peer.name, &peer.participant_id),
                address: peer.address.clone(),
                port: peer.port,
                status,
                latency_ms: live.and_then(|s| s.latency_ms),
                version: live
                    .and_then(|s| s.version.clone())
                    .or_else(|| if is_self { node.version.clone() } else { None }),
                workflow: live.and_then(|s| s.workflow.as_ref()).map(format_workflow),
                is_self,
            }
        })
        .collect();

    if !self_seen {
        let address = node
            .node
            .public_address
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| node.node.listen_address.clone());
        views.push(PeerView {
            participant_id: self_id.to_owned(),
            name: display_name("", self_id),
            address,
            port: node.node.port,
            status: Some(ConnectionStatus::CurrentNode),
            latency_ms: None,
            version: node.version.clone(),
            workflow: None,
            is_self: true,
        });
    }

    // This node first, then the rest in configured order (stable sort).
    views.sort_by_key(|peer| !peer.is_self);
    views
}

/// Human-readable name for a workflow run: the target party prefix when known,
/// otherwise the instance name.
pub fn run_name(run: &WorkflowRun) -> &str {
    match run.prefix.as_deref() {
        Some(prefix) if !prefix.is_empty() => prefix,
        _ => run.instance_name.as_str(),
    }
}

/// `/workflows` response envelope.
#[derive(Debug, Deserialize)]
struct WorkflowRunsResponse {
    #[serde(default)]
    runs: Vec<WorkflowRun>,
}

/// Human-readable name for a pending invitation: the target party prefix, else
/// the coordinator.
pub fn invitation_name(invitation: &PendingInvitation) -> String {
    match invitation.prefix.as_deref() {
        Some(prefix) if !prefix.is_empty() => prefix.to_owned(),
        _ => invitation
            .coordinator_name
            .clone()
            .map(|name| format!("from {name}"))
            .unwrap_or_else(|| "from peer".to_owned()),
    }
}

/// `/invitations` response envelope.
#[derive(Debug, Deserialize)]
struct InvitationsResponse {
    #[serde(default)]
    invitations: Vec<PendingInvitation>,
}

/// An item in the Workflows feed: an incoming invitation or a workflow run.
#[derive(Clone, Debug)]
pub enum FeedItem {
    Invitation(PendingInvitation),
    Run(WorkflowRun),
}

/// Human action label for an audit entry: the summary when present, else the
/// event type.
pub fn audit_action(entry: &AuditLogEntry) -> &str {
    if entry.action_summary.is_empty() {
        &entry.event_type
    } else {
        &entry.action_summary
    }
}

/// `/governance/audit` response envelope.
#[derive(Debug, Deserialize)]
struct AuditLogResponse {
    #[serde(default)]
    entries: Vec<AuditLogEntry>,
}

/// A token holding for a party, from `/holdings`. Decimal amounts are strings.
#[derive(Clone, Debug, Deserialize)]
pub struct Holding {
    #[serde(default)]
    pub instrument_admin: String,
    #[serde(default)]
    pub instrument_id: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub locked_amount: String,
    #[serde(default)]
    pub preapproval_set_up: bool,
}

/// `/holdings` response envelope.
#[derive(Debug, Deserialize)]
struct HoldingsResponse {
    #[serde(default)]
    holdings: Vec<Holding>,
}

/// Resolved OAuth2 target after discovery / overrides are applied.
struct AuthTarget {
    token_url: String,
    client_id: String,
    audience: Option<String>,
}

/// Token endpoint response; only `access_token` is consumed.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// `GET /decentralized-parties` response envelope.
#[derive(Debug, Deserialize)]
struct PartiesResponse {
    #[serde(default)]
    parties: Vec<DecentralizedParty>,
}

/// Build a Keycloak token endpoint from its host and realm.
fn keycloak_token_url(host: &str, realm: &str) -> String {
    format!(
        "{host}/realms/{realm}/protocol/openid-connect/token",
        host = host.trim_end_matches('/'),
    )
}

/// Blocking client for the decman HTTP API, authenticating via the OAuth2
/// password grant (the same username/password login the web frontend uses).
pub struct DecmanClient {
    base_url: String,
    http: Client,
    auth: AuthSettings,
    token: Option<String>,
    authenticated: bool,
}

impl DecmanClient {
    /// Build a client for `base_url`, logging in with `auth`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new(base_url: String, auth: AuthSettings) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http,
            auth,
            token: None,
            authenticated: false,
        })
    }

    /// Fetch the decentralized parties this node is a member of.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_parties(&mut self) -> Result<Vec<DecentralizedParty>> {
        let parsed: PartiesResponse = self.get_json("/decentralized-parties")?;
        Ok(parsed.parties)
    }

    /// Fetch the network peers with their live connectivity status — the same
    /// data the web frontend's network panel shows.
    ///
    /// # Errors
    ///
    /// Returns an error if login or either request fails, the API returns a
    /// non-success status, or a response cannot be parsed.
    pub fn fetch_peers(&mut self) -> Result<Vec<PeerView>> {
        let node: NodeConfigResponse = self.get_json("/node-config")?;
        let config: NetworkConfigResponse = self.get_json("/network-config")?;
        let statuses: ParticipantsStatusResponse = self.get_json("/participants-status")?;
        Ok(merge_peers(&node, &config.peers, &statuses.statuses))
    }

    /// Fetch the DAML packages (DARs) vetted on this node, sorted like the
    /// frontend (by name, then version).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_dars(&mut self) -> Result<Vec<VettedPackageInfo>> {
        let mut packages: Vec<VettedPackageInfo> = self.get_json("/packages/vetted")?;
        sort_vetted(&mut packages);
        Ok(packages)
    }

    /// Fetch the Workflows feed: pending invitations followed by workflow runs
    /// — the same data the frontend's notifications panel shows.
    ///
    /// # Errors
    ///
    /// Returns an error if login or a request fails, the API returns a
    /// non-success status, or a response cannot be parsed.
    pub fn fetch_feed(&mut self) -> Result<Vec<FeedItem>> {
        let invitations: InvitationsResponse = self.get_json("/invitations")?;
        let runs: WorkflowRunsResponse = self.get_json("/workflows")?;

        let mut feed: Vec<FeedItem> = invitations
            .invitations
            .into_iter()
            .map(FeedItem::Invitation)
            .collect();
        feed.extend(runs.runs.into_iter().map(FeedItem::Run));
        Ok(feed)
    }

    /// Compare the local vetted packages against each peer's (the "checker").
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn compare_packages(&mut self) -> Result<PeerPackageComparison> {
        self.get_json("/packages/compare-peers")
    }

    /// Accept a pending invitation by id.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn accept_invitation(&mut self, id: &str) -> Result<()> {
        self.post("/invitations/accept", Some(json!({ "id": id })))
    }

    /// Decline a pending invitation by id.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn decline_invitation(&mut self, id: &str) -> Result<()> {
        self.post("/invitations/decline", Some(json!({ "id": id })))
    }

    /// Dismiss a finished workflow run by instance name.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it
    /// (e.g. the run is still in progress).
    pub fn dismiss_workflow(&mut self, instance_name: &str) -> Result<()> {
        self.post(&format!("/workflows/{instance_name}/dismiss"), None)
    }

    /// Upload a DAR to this node only (no distribution).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn upload_dar(&mut self, filename: &str, data: &str) -> Result<()> {
        self.post(
            "/dars/upload",
            Some(json!({ "dar_files": [{ "filename": filename, "data": data }], "peer_ids": [] })),
        )
    }

    /// Distribute a DAR to the given peers (by participant id).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it
    /// (e.g. no peers, or a workflow already in progress).
    pub fn distribute_dar(
        &mut self,
        filename: &str,
        data: &str,
        peer_ids: &[String],
    ) -> Result<()> {
        self.post(
            "/dars/distribute",
            Some(json!({
                "dar_files": [{ "filename": filename, "data": data }],
                "peer_ids": peer_ids,
            })),
        )
    }

    /// Fetch the governance audit log for a party (newest entries).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_audit(&mut self, party_id: &str) -> Result<Vec<AuditLogEntry>> {
        let response: AuditLogResponse =
            self.get_json_query("/governance/audit", &[("party_id", party_id)])?;
        Ok(response.entries)
    }

    /// Fetch the token holdings for a party.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_holdings(&mut self, party_id: &str) -> Result<Vec<Holding>> {
        let response: HoldingsResponse =
            self.get_json_query("/holdings", &[("party_id", party_id)])?;
        Ok(response.holdings)
    }

    /// GET `path` as JSON (no query params).
    fn get_json<T: DeserializeOwned>(&mut self, path: &str) -> Result<T> {
        self.get_json_query(path, &[])
    }

    /// GET `path` with query params as JSON, authenticating on first use and
    /// re-authenticating once on `401 Unauthorized` (i.e. an expired token).
    fn get_json_query<T: DeserializeOwned>(
        &mut self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        if !self.authenticated {
            self.authenticate()?;
        }

        let mut response = self.get(path, query)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.authenticated = false;
            self.token = None;
            self.authenticate()?;
            response = self.get(path, query)?;
        }

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("decman API {path} returned {status}: {body}");
        }

        response
            .json()
            .with_context(|| format!("failed to parse {path} response"))
    }

    /// Resolve the IdP target and, when authentication is required, mint a token.
    fn authenticate(&mut self) -> Result<()> {
        self.token = match self.resolve_auth_target()? {
            Some(target) => Some(self.password_grant(&target)?),
            None => None,
        };
        self.authenticated = true;
        Ok(())
    }

    /// Resolve the token endpoint, client id, and audience.
    ///
    /// Explicit `token_url` + `client_id` overrides skip the network call;
    /// otherwise `/auth-config` is queried (returning `None` when the node has
    /// auth disabled, e.g. test mode).
    fn resolve_auth_target(&self) -> Result<Option<AuthTarget>> {
        if let (Some(token_url), Some(client_id)) = (&self.auth.token_url, &self.auth.client_id) {
            return Ok(Some(AuthTarget {
                token_url: token_url.clone(),
                client_id: client_id.clone(),
                audience: self.auth.audience.clone(),
            }));
        }

        let config = self.fetch_auth_config()?;
        if !config.auth_required {
            return Ok(None);
        }

        // Auth0 takes precedence over Keycloak, mirroring the backend.
        if let Some(domain) = config.auth0_domain {
            let client_id = self
                .auth
                .client_id
                .clone()
                .or(config.auth0_client_id)
                .context("Auth0 client id missing from /auth-config")?;
            return Ok(Some(AuthTarget {
                token_url: self
                    .auth
                    .token_url
                    .clone()
                    .unwrap_or_else(|| format!("https://{domain}/oauth/token")),
                client_id,
                audience: self.auth.audience.clone().or(config.auth0_audience),
            }));
        }

        let client_id = self
            .auth
            .client_id
            .clone()
            .or(config.keycloak_client_id)
            .context("Keycloak client id missing from /auth-config")?;
        let token_url = match &self.auth.token_url {
            Some(url) => url.clone(),
            None => {
                let host = config
                    .keycloak_host
                    .context("Keycloak host missing from /auth-config")?;
                let realm = config
                    .keycloak_realm
                    .context("Keycloak realm missing from /auth-config")?;
                keycloak_token_url(&host, &realm)
            }
        };

        Ok(Some(AuthTarget {
            token_url,
            client_id,
            audience: self.auth.audience.clone(),
        }))
    }

    /// Fetch the public `/auth-config` to discover IdP settings.
    fn fetch_auth_config(&self) -> Result<AuthConfigResponse> {
        let url = format!("{base}/auth-config", base = self.base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("request to {url} failed"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("/auth-config returned {status}: {body}");
        }

        response
            .json()
            .context("failed to parse /auth-config response")
    }

    /// Mint an access token via the OAuth2 password grant.
    fn password_grant(&self, target: &AuthTarget) -> Result<String> {
        let mut form = vec![
            ("grant_type", "password"),
            ("client_id", target.client_id.as_str()),
            ("username", self.auth.username.as_str()),
            ("password", self.auth.password.as_str()),
        ];
        if let Some(secret) = &self.auth.client_secret {
            form.push(("client_secret", secret.as_str()));
        }
        if let Some(audience) = &target.audience {
            form.push(("audience", audience.as_str()));
        }
        if let Some(scope) = &self.auth.scope {
            form.push(("scope", scope.as_str()));
        }

        let response = self
            .http
            .post(&target.token_url)
            .form(&form)
            .send()
            .with_context(|| format!("token request to {url} failed", url = target.token_url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            // The login (password / direct-access) grant needs the OAuth client
            // to permit it — surface an actionable hint for this common case.
            if body.contains("unauthorized_client") || body.contains("direct access grants") {
                bail!(
                    "token endpoint returned {status}: {body} — this client does not allow the \
                     password grant. Enable \"Direct Access Grants\" on the client in Keycloak, or \
                     set oauth_client_id for this profile to a client that allows it."
                );
            }
            bail!("token endpoint returned {status}: {body}");
        }

        let token: TokenResponse = response.json().context("failed to parse token response")?;
        Ok(token.access_token)
    }

    /// Send a `GET {base_url}{path}` with query params, attaching the bearer
    /// token if present.
    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Response> {
        let url = format!("{base}{path}", base = self.base_url);
        let mut request = self.http.get(&url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        request
            .send()
            .with_context(|| format!("request to {url} failed"))
    }

    /// POST `path` with an optional JSON body, discarding the success body.
    /// Authenticates on first use and re-authenticates once on `401`.
    fn post(&mut self, path: &str, body: Option<Value>) -> Result<()> {
        if !self.authenticated {
            self.authenticate()?;
        }

        let mut response = self.send_post(path, body.as_ref())?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.authenticated = false;
            self.token = None;
            self.authenticate()?;
            response = self.send_post(path, body.as_ref())?;
        }

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("decman API {path} returned {status}: {body}");
        }
        Ok(())
    }

    /// Send a `POST {base_url}{path}`, attaching the bearer token and JSON body
    /// if present.
    fn send_post(&self, path: &str, body: Option<&Value>) -> Result<Response> {
        let url = format!("{base}{path}", base = self.base_url);
        let mut request = self.http.post(&url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        request
            .send()
            .with_context(|| format!("request to {url} failed"))
    }
}

#[cfg(test)]
mod tests {
    use common::canton_id::CantonId;

    use super::*;

    /// A valid 34-byte (68 hex char) namespace for building realistic Canton ids.
    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn party(prefix: &str) -> DecentralizedParty {
        DecentralizedParty {
            party_id: CantonId::parse(&format!("{prefix}::{NS}")).unwrap(),
            threshold: 1,
            owners: Vec::new(),
            my_owner_key: None,
            participants: Vec::new(),
            contracts: Vec::new(),
            local_metadata: None,
        }
    }

    fn auth_settings() -> AuthSettings {
        AuthSettings {
            username: "alice".to_owned(),
            password: "secret".to_owned(),
            token_url: None,
            client_id: None,
            client_secret: None,
            audience: None,
            scope: None,
        }
    }

    #[test]
    fn party_name_is_the_canton_id_prefix() {
        assert_eq!(party_name(&party("cbtc-network")), "cbtc-network");
    }

    #[test]
    fn keycloak_token_url_trims_trailing_slash() {
        assert_eq!(
            keycloak_token_url("https://kc.example/", "my-realm"),
            "https://kc.example/realms/my-realm/protocol/openid-connect/token"
        );
    }

    #[test]
    fn explicit_overrides_skip_discovery() {
        // Arrange — both overrides present, so no /auth-config call is made.
        let auth = AuthSettings {
            token_url: Some("https://idp/token".to_owned()),
            client_id: Some("decman-web".to_owned()),
            ..auth_settings()
        };
        let client = DecmanClient::new("http://api.invalid".to_owned(), auth).unwrap();

        // Act
        let target = client.resolve_auth_target().unwrap().unwrap();

        // Assert
        assert_eq!(target.token_url, "https://idp/token");
        assert_eq!(target.client_id, "decman-web");
    }

    fn node(participant_id: &str) -> NodeConfigResponse {
        NodeConfigResponse {
            node: NodeIdentity {
                participant_id: participant_id.to_owned(),
                public_address: Some("node.local".to_owned()),
                listen_address: "0.0.0.0".to_owned(),
                port: 9000,
            },
            version: Some("0.9.1".to_owned()),
        }
    }

    fn status(id: &str, workflow: Option<WorkflowInfo>) -> ParticipantStatus {
        ParticipantStatus {
            id: id.to_owned(),
            status: ConnectionStatus::Connected,
            latency_ms: Some(12),
            version: Some("1.2.3".to_owned()),
            workflow,
        }
    }

    #[test]
    fn merge_peers_puts_self_first_and_maps_status() {
        // Arrange — self is present in the peer list.
        let peers = [
            PeerConfig {
                participant_id: "peer1::1220".to_owned(),
                name: "alpha".to_owned(),
                address: "10.0.0.1".to_owned(),
                port: 9001,
            },
            PeerConfig {
                participant_id: "self::1220".to_owned(),
                name: "me".to_owned(),
                address: "127.0.0.1".to_owned(),
                port: 9000,
            },
        ];
        let statuses = [status(
            "peer1::1220",
            Some(WorkflowInfo {
                kind: common::types::WorkflowKind::Onboarding,
                role: common::types::WorkflowRole::Peer,
                step: "Sign".to_owned(),
                step_index: 0,
                step_total: 0,
            }),
        )];

        // Act
        let views = merge_peers(&node("self::1220"), &peers, &statuses);

        // Assert — self first, peer status/latency/workflow mapped.
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name, "me");
        assert!(views[0].is_self);
        assert_eq!(views[0].status, Some(ConnectionStatus::CurrentNode));
        assert_eq!(views[1].name, "alpha");
        assert_eq!(views[1].status, Some(ConnectionStatus::Connected));
        assert_eq!(views[1].latency_ms, Some(12));
        assert_eq!(views[1].workflow.as_deref(), Some("Onboarding (Sign)"));
    }

    #[test]
    fn merge_peers_synthesizes_self_when_absent() {
        // Arrange — this node is NOT in the peer list, and a peer has no status.
        let peers = [PeerConfig {
            participant_id: "ghost::1220".to_owned(),
            name: "ghost".to_owned(),
            address: "10.0.0.9".to_owned(),
            port: 9009,
        }];

        // Act
        let views = merge_peers(&node("self::1220"), &peers, &[]);

        // Assert — self synthesized and listed first; missing status → None (Unknown).
        assert_eq!(views.len(), 2);
        assert!(views[0].is_self);
        assert_eq!(views[0].status, Some(ConnectionStatus::CurrentNode));
        assert_eq!(views[0].version.as_deref(), Some("0.9.1"));
        assert!(!views[1].is_self);
        assert_eq!(views[1].name, "ghost");
        assert_eq!(views[1].status, None);
    }

    #[test]
    fn sort_vetted_orders_by_name_then_version() {
        // Arrange
        let mut packages = vec![
            VettedPackageInfo {
                package_id: "c".to_owned(),
                package_name: "splice".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            VettedPackageInfo {
                package_id: "a".to_owned(),
                package_name: "daml-stdlib".to_owned(),
                package_version: "2.0.0".to_owned(),
            },
            VettedPackageInfo {
                package_id: "b".to_owned(),
                package_name: "daml-stdlib".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
        ];

        // Act
        sort_vetted(&mut packages);

        // Assert
        let order: Vec<&str> = packages.iter().map(|p| p.package_id.as_str()).collect();
        assert_eq!(order, ["b", "a", "c"]);
    }

    #[test]
    fn parties_response_ignores_unknown_fields() {
        // Arrange — the real envelope also carries `source` and `refreshing`.
        let json = format!(
            r#"{{"parties":[{{"party_id":"p::{NS}","threshold":2,"owners":["a"],
            "participants":[{{"participant_uid":"par::{NS}","permission":"submission"}}]}}],
            "source":"cache","refreshing":true}}"#
        );

        // Act
        let parsed: PartiesResponse = serde_json::from_str(&json).unwrap();

        // Assert
        assert_eq!(parsed.parties.len(), 1);
        assert_eq!(parsed.parties[0].threshold, 2);
        assert_eq!(parsed.parties[0].participants.len(), 1);
    }
}
