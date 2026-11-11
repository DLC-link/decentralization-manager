//! The HTTP client core: settings, errors, request and send.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, Response};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::auth::{AuthTarget, PasswordSource, TokenCache, TokenSet};

/// User credentials and optional IdP overrides for the OAuth2 password grant.
///
/// The TUI authenticates exactly like the web frontend: a human's username and
/// password. The token endpoint and client id are discovered from the API's
/// public `/auth-config` unless explicitly overridden here.
#[derive(Clone, Debug)]
pub struct AuthSettings {
    pub username: String,
    /// `None` works only against a node with auth disabled.
    pub password: Option<PasswordSource>,
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

/// A non-success HTTP response from the decman API. It stays typed so a caller
/// can read the status, for example to format a `422` mesh error.
#[derive(Debug)]
pub struct ApiError {
    pub method: Method,
    pub path: String,
    pub status: StatusCode,
    pub body: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "decman API {method} {path} returned {status}: {body}",
            method = self.method,
            path = self.path,
            status = self.status,
            body = self.body,
        )
    }
}

impl std::error::Error for ApiError {}

/// Blocking client for the decman HTTP API, authenticating via the OAuth2
/// password grant (the same username/password login the web frontend uses).
///
/// The client refreshes an expired access token with the refresh token, and
/// falls back to a new password login only when the refresh fails.
pub struct DecmanClient {
    pub(super) base_url: String,
    pub(super) http: Client,
    pub(super) auth: AuthSettings,
    /// The resolved token endpoint, or `Some(None)` when the node has auth
    /// disabled. `None` until first resolved.
    pub(super) target: Option<Option<AuthTarget>>,
    pub(super) tokens: Option<TokenSet>,
    /// The resolved password, kept so a password command runs at most once.
    pub(super) password: Option<String>,
    pub(super) cache: Option<TokenCache>,
    /// Whether `target` came from the cache rather than from discovery.
    pub(super) target_from_cache: bool,
    /// Timeout for [`Self::request`]; the TUI calls keep the client default.
    request_timeout: Option<Duration>,
    /// When set, [`Self::request`] refuses every non-`GET` with this message.
    writes_blocked: Option<String>,
    /// Print login problems that are not fatal on stderr. The TUI leaves this
    /// off, because stderr output would draw over its screen.
    pub(super) warn: bool,
}

impl DecmanClient {
    /// Build a client for `base_url`, logging in with `auth`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new(base_url: String, auth: AuthSettings) -> Result<Self> {
        ensure_secure_url(&base_url)?;
        // No redirects: a redirect to another host would carry a request body
        // with secrets (grant-rights, party-config) to that host.
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http,
            auth,
            target: None,
            tokens: None,
            password: None,
            cache: None,
            target_from_cache: false,
            request_timeout: None,
            writes_blocked: None,
            warn: false,
        })
    }

    /// Keep tokens in `cache` across invocations, starting from any login it
    /// already holds. A cached login whose token endpoint or client differs
    /// from an explicit override is ignored.
    #[must_use]
    pub fn with_token_cache(mut self, cache: TokenCache) -> Self {
        if let Some(login) = cache
            .load()
            .filter(|login| self.matches_overrides(&login.target))
        {
            self.target = Some(Some(login.target));
            self.target_from_cache = true;
            self.tokens = Some(login.tokens);
        }
        self.cache = Some(cache);
        self
    }

    /// Use `timeout` for each [`Self::request`] call.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Make [`Self::request`] refuse every non-`GET` call with `reason`.
    #[must_use]
    pub fn with_writes_blocked(mut self, reason: String) -> Self {
        self.writes_blocked = Some(reason);
        self
    }

    /// Print login problems that are not fatal, such as a failed refresh or a
    /// cache that cannot be written, on stderr.
    #[must_use]
    pub fn with_warnings(mut self) -> Self {
        self.warn = true;
        self
    }

    /// The message of a write block, if writes are blocked.
    pub fn writes_blocked(&self) -> Option<&str> {
        self.writes_blocked.as_deref()
    }

    /// The base URL this client talks to, without a trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Send any request to the API and return the response body as JSON
    /// (`null` for an empty body, a JSON string for a non-JSON body).
    ///
    /// # Errors
    ///
    /// Returns an error if writes are blocked and `method` is not `GET`, if
    /// login or the request fails, or as an [`ApiError`] if the API returns a
    /// non-success status.
    pub fn request(
        &mut self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        if method != Method::GET
            && let Some(reason) = &self.writes_blocked
        {
            bail!("{reason}");
        }
        let response = self.send_authed(&method, path, query, body)?;

        let status = response.status();
        let is_html = is_html(&response);
        let text = response
            .text()
            .with_context(|| format!("failed to read the {path} response"))?;
        if status.is_success() && is_html {
            bail!(
                "{method} {path} returned the web UI page, not JSON: this server version \
                 probably has no such route"
            );
        }
        if !status.is_success() {
            return Err(ApiError {
                method,
                path: path.to_owned(),
                status,
                body: text,
            }
            .into());
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    /// `GET` a route that answers with raw bytes, such as a DAR download.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or as an [`ApiError`]
    /// if the API returns a non-success status.
    pub fn request_bytes(&mut self, path: &str) -> Result<Vec<u8>> {
        let response = self.send_authed(&Method::GET, path, &[], None)?;
        let response = success_or_api_error(Method::GET, path, response)?;
        if is_html(&response) {
            bail!(
                "GET {path} returned the web UI page, not a file: this server version \
                 probably has no such route"
            );
        }
        Ok(response
            .bytes()
            .with_context(|| format!("failed to read the {path} response"))?
            .to_vec())
    }

    /// Send a request with a valid login, and once more after a fresh login
    /// when the server answers `401`.
    fn send_authed(
        &mut self,
        method: &Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Response> {
        self.authenticate()?;
        let response = self.send_request(method, path, query, body)?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        self.drop_access_token();
        self.authenticate()?;
        self.send_request(method, path, query, body)
    }

    fn send_request(
        &self,
        method: &Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Response> {
        let url = format!("{base}{path}", base = self.base_url);
        let mut request = self.http.request(method.clone(), &url);
        if let Some(token) = self.access_token() {
            request = request.bearer_auth(token);
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(timeout) = self.request_timeout {
            request = request.timeout(timeout);
        }
        request
            .send()
            .with_context(|| format!("request to {url} failed"))
    }

    /// GET `path` as JSON (no query params).
    pub(super) fn get_json<T: DeserializeOwned>(&mut self, path: &str) -> Result<T> {
        self.get_json_query(path, &[])
    }

    /// GET `path` with query params as JSON.
    pub(super) fn get_json_query<T: DeserializeOwned>(
        &mut self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        let response = self.send_authed(&Method::GET, path, &query, None)?;
        let response = success_or_api_error(Method::GET, path, response)?;
        response
            .json()
            .with_context(|| format!("failed to parse {path} response"))
    }

    /// POST `path` with an optional JSON body, returning the raw response,
    /// so the caller can inspect non-success statuses (e.g. a 422 with a typed
    /// body).
    pub(super) fn post_raw(&mut self, path: &str, body: Option<&Value>) -> Result<Response> {
        self.send_authed(&Method::POST, path, &[], body)
    }

    /// POST `path` with an optional JSON body, discarding the success body and
    /// failing on any non-success status.
    pub(super) fn post(&mut self, path: &str, body: Option<Value>) -> Result<()> {
        let response = self.post_raw(path, body.as_ref())?;
        success_or_api_error(Method::POST, path, response).map(drop)
    }

    /// PUT `path` with a JSON body, failing on any non-success status.
    pub(super) fn put(&mut self, path: &str, body: Value) -> Result<()> {
        let response = self.send_authed(&Method::PUT, path, &[], Some(&body))?;
        success_or_api_error(Method::PUT, path, response).map(drop)
    }
}

/// Whether `response` is an HTML page. The server answers unknown routes with
/// the web UI's index.html, so a page means the route does not exist on this
/// server version.
fn is_html(response: &Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"))
}

/// `text` with control characters replaced by `?`, for stderr. Error and
/// progress text can come from a server, an IdP or a peer, and a raw escape
/// sequence could rewrite the terminal.
pub fn for_terminal(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// `response` if it succeeded, else an [`ApiError`] with its body.
fn success_or_api_error(method: Method, path: &str, response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(ApiError {
        method,
        path: path.to_owned(),
        status,
        body: response.text().unwrap_or_default(),
    }
    .into())
}

/// Refuse a plain `http://` URL to anything but this machine. Tokens,
/// passwords and client secrets must not cross the network in clear text.
///
/// # Errors
///
/// Returns an error for a URL that does not parse, or for `http://` to a
/// host other than `localhost`, `127.0.0.1` or `::1`.
pub fn ensure_secure_url(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("{url} is not a valid URL"))?;
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        scheme => bail!("{url}: use https; {scheme} is allowed for localhost only"),
    }
}
