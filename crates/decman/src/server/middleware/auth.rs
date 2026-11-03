//! Bearer-token auth middleware.
//!
//! Runs in front of every request. Public paths (SPA assets, `/auth-config`,
//! swagger) pass through. `PUT /party-config` is special-cased for first-run
//! bootstrap: if the `party_credentials` table is empty we let the call
//! through unauthenticated so a fresh node can be configured; after the first
//! row lands, normal auth applies and the handler enforces admin role.
//!
//! The tenant API (`/v0/tenant/*`) authenticates with a provider-issued API key
//! instead of a JWT. The middleware checks that key and spends the key's
//! per-minute budget before the handler reads the body, so a refused call
//! costs no body parsing. It attaches the matched key as a [`TenantCaller`].
//!
//! Everything else requires a valid `Authorization: Bearer <token>` that the
//! configured `TokenValidator` accepts. The resolved `Principal` is attached
//! to the request extensions for handlers to consume.

use std::{
    future::{Ready, ready},
    rc::Rc,
    time::Instant,
};

use actix_web::{
    Error, HttpMessage, HttpRequest, HttpResponse,
    body::{EitherBody, MessageBody},
    dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready},
    http::{
        Method,
        header::{AUTHORIZATION, HeaderMap, RETRY_AFTER},
    },
    web,
};
use futures::future::LocalBoxFuture;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    auth::Principal,
    server::{
        AppState,
        tenant_rate_limit::{Verdict, retry_after_secs},
        types::ErrorResponse,
    },
};

/// Actix `Transform` that enforces inbound token auth. Wrap the app with
/// `.wrap(AuthMiddleware)` to engage it.
pub struct AuthMiddleware;

impl<S, B> Transform<S, ServiceRequest> for AuthMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthMiddlewareService<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddlewareService {
            service: Rc::new(service),
        }))
    }
}

pub struct AuthMiddlewareService<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for AuthMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let service = self.service.clone();
        let method = req.method().as_str().to_string();
        let path = req.path().to_string();

        Box::pin(async move {
            if is_always_public(&path) {
                let res = service.call(req).await?;
                return Ok(res.map_into_left_body());
            }

            let Some(app_state) = req.app_data::<web::Data<AppState>>().cloned() else {
                let response = HttpResponse::InternalServerError()
                    .json(json!({"error": "application state missing"}));
                return Ok(req.into_response(response).map_into_right_body());
            };

            if path.starts_with(TENANT_PREFIX) {
                return match admit_tenant_call(req.method(), &path, req.headers(), &app_state) {
                    Ok(caller) => {
                        req.extensions_mut().insert(caller);
                        let res = service.call(req).await?;
                        Ok(res.map_into_left_body())
                    }
                    Err(response) => Ok(req.into_response(response).map_into_right_body()),
                };
            }

            // Bootstrap: first `PUT /party-config` on a fresh node is allowed
            // without a token. The operator typically has not provisioned an
            // admin user in the IdP at this point, so requiring one would be
            // a chicken-and-egg block.
            //
            // Concurrency: two unauthenticated requests racing while the table
            // is empty must not both pass through (otherwise the second
            // overwrites the first without ever authenticating). We serialize
            // the bootstrap window with `bootstrap_mu` held across the entire
            // request: only one bootstrap-shaped call is in flight at a time;
            // any concurrent attempt is rejected with 409. After the holder
            // writes credentials, subsequent calls fall through to normal auth.
            if method == "PUT"
                && path == "/party-config"
                && app_state.party_credentials.read().await.is_empty()
            {
                let Ok(guard) = app_state.bootstrap_mu.clone().try_lock_owned() else {
                    let response = HttpResponse::Conflict()
                        .json(json!({"error": "bootstrap already in progress"}));
                    return Ok(req.into_response(response).map_into_right_body());
                };
                // Recheck under the guard. A previous holder may have just
                // finished writing; if so, drop the lock and require auth.
                if app_state.party_credentials.read().await.is_empty() {
                    tracing::info!(
                        "PUT /party-config bootstrap: unauthenticated call allowed because \
                         party_credentials is empty. Subsequent writes will require admin role."
                    );
                    let res = service.call(req).await?;
                    drop(guard);
                    return Ok(res.map_into_left_body());
                }
                drop(guard);
            }

            let token = bearer_token(&req).unwrap_or_default();
            match app_state.token_validator.validate(&token).await {
                Ok(principal) => {
                    req.extensions_mut().insert(principal);
                    let res = service.call(req).await?;
                    Ok(res.map_into_left_body())
                }
                Err(e) => {
                    tracing::warn!("rejected {method} {path}: {e}");
                    let response =
                        HttpResponse::Unauthorized().json(json!({"error": e.to_string()}));
                    Ok(req.into_response(response).map_into_right_body())
                }
            }
        })
    }
}

/// Handler helper: pull the `Principal` the middleware attached, then
/// check it carries the admin role. `None` disables the role check —
/// any authenticated caller passes.
///
/// # Errors
///
/// Returns an `HttpResponse` ready to return: 401 if no principal was
/// attached to the request, 403 if `admin_role` is `Some` and the
/// principal does not carry it.
#[allow(clippy::result_large_err)]
pub fn require_admin(
    req: &HttpRequest,
    admin_role: Option<&str>,
) -> Result<Principal, HttpResponse> {
    let principal = {
        let extensions = req.extensions();
        extensions.get::<Principal>().cloned()
    };
    let Some(principal) = principal else {
        return Err(HttpResponse::Unauthorized().json(json!({"error": "authentication required"})));
    };
    if let Some(role) = admin_role {
        principal
            .require_admin(role)
            .map_err(|e| HttpResponse::Forbidden().json(json!({"error": e.to_string()})))?;
    }
    Ok(principal)
}

const TENANT_PREFIX: &str = "/v0/tenant/";

/// A tenant API key that matched, named without the key itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantKey {
    /// The key's position in `DECPM_TENANT_API_KEYS`.
    pub index: usize,
    /// See [`tenant_key_id`].
    pub id: String,
}

/// The tenant key that the middleware accepted for a `/v0/tenant/*` request.
/// `None` in insecure/test mode, where no key is checked.
#[derive(Clone, Debug)]
pub struct TenantCaller(pub Option<TenantKey>);

/// The first 16 hex characters of the SHA-256 of a tenant key. It names the
/// key across a reordered or shortened `DECPM_TENANT_API_KEYS`, which the
/// index does not, and it cannot be turned back into the key.
pub fn tenant_key_id(key: &str) -> String {
    let mut id = hex::encode(Sha256::digest(key.as_bytes()));
    id.truncate(16);
    id
}

/// Check the key of a `/v0/tenant/*` request and spend one call of its budget,
/// before any handler reads the body. The two ACS relay calls are checked but
/// not metered here, because one copy sends a call per block. The import
/// handler spends the budget itself for the blocks that start or end a copy.
///
/// Insecure/test mode checks no key and spends no budget, matching how the
/// JWT path relaxes under `--insecure`.
///
/// # Errors
/// A 401 when the bearer is missing or is not a configured key, and a 429
/// with `Retry-After` when the key has spent its budget.
#[allow(clippy::result_large_err)]
fn admit_tenant_call(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    state: &AppState,
) -> Result<TenantCaller, HttpResponse> {
    let key = match_tenant_api_key(headers, state)?;
    if !is_acs_relay_call(method, path) {
        charge_tenant_key(state, key.as_ref(), path)?;
    }
    Ok(TenantCaller(key))
}

/// `GET /v0/tenant/{party}/acs/{target}` and `POST /v0/tenant/add-hosts/import`.
fn is_acs_relay_call(method: &Method, path: &str) -> bool {
    let Some(route) = path.strip_prefix(TENANT_PREFIX) else {
        return false;
    };
    if *method == Method::POST {
        return route == "add-hosts/import";
    }
    *method == Method::GET && matches!(route.split('/').collect::<Vec<_>>()[..], [_, "acs", _])
}

/// Spend one call of `key`'s per-minute budget. `None` (insecure/test mode)
/// and a limit of 0 spend nothing.
///
/// # Errors
/// A 429 with `Retry-After` when the key has spent its budget.
#[allow(clippy::result_large_err)]
pub fn charge_tenant_key(
    state: &AppState,
    key: Option<&TenantKey>,
    path: &str,
) -> Result<(), HttpResponse> {
    let limit = state.config.tenant_rate_limit_per_minute;
    let Some(key) = key.filter(|_| limit > 0) else {
        return Ok(());
    };
    match state
        .tenant_rate_limiter
        .check(key.index, limit, Instant::now())
    {
        Verdict::Allowed => Ok(()),
        Verdict::Limited { retry_after } => {
            let secs = retry_after_secs(retry_after);
            tracing::warn!(
                key_index = key.index,
                key_id = %key.id,
                path,
                retry_after_secs = secs,
                "tenant API key over its rate limit of {limit} requests per minute"
            );
            Err(HttpResponse::TooManyRequests()
                .insert_header((RETRY_AFTER, secs.to_string()))
                .json(ErrorResponse {
                    error: format!(
                        "rate limit of {limit} requests per minute for this tenant API key \
                         exceeded; retry after {secs} seconds"
                    ),
                }))
        }
    }
}

/// The tenant key that the middleware accepted for this request.
///
/// # Errors
/// A 401 when the request did not pass through the middleware, so no key was
/// checked.
#[allow(clippy::result_large_err)]
pub fn tenant_caller(req: &HttpRequest) -> Result<Option<TenantKey>, HttpResponse> {
    req.extensions()
        .get::<TenantCaller>()
        .map(|caller| caller.0.clone())
        .ok_or_else(|| {
            HttpResponse::Unauthorized().json(json!({"error": "invalid tenant API key"}))
        })
}

#[allow(clippy::result_large_err)]
fn match_tenant_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<Option<TenantKey>, HttpResponse> {
    if state.test_mode {
        return Ok(None);
    }
    let presented = headers
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(parse_bearer)
        .unwrap_or_default();
    let matched = if presented.is_empty() {
        None
    } else {
        state
            .config
            .tenant_api_keys
            .iter()
            .position(|k| constant_time_eq(k.as_bytes(), presented.as_bytes()))
    };
    match matched {
        Some(index) => Ok(Some(TenantKey {
            index,
            id: tenant_key_id(presented),
        })),
        None => Err(HttpResponse::Unauthorized().json(json!({"error": "invalid tenant API key"}))),
    }
}

/// Constant-time byte-slice equality, so a tenant API-key check doesn't leak the
/// matched prefix via response timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Extract the bearer token from the request.
fn bearer_token(req: &ServiceRequest) -> Option<String> {
    let header = req.headers().get(AUTHORIZATION)?.to_str().ok()?;
    parse_bearer(header).map(str::to_string)
}

/// Parse a bearer token out of an `Authorization` header value.
///
/// RFC 7235 §2.1 specifies the auth scheme name as case-insensitive, so
/// `Bearer`, `bearer`, and `BEARER` are all valid; we also trim surrounding
/// whitespace and tolerate multiple spaces between the scheme and token.
fn parse_bearer(header: &str) -> Option<&str> {
    let header = header.trim();
    let (scheme, token) = header.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() { None } else { Some(token) }
}

/// Paths that bypass auth entirely. Covers the SPA entry point, static
/// assets, swagger (only mounted in insecure mode, `--insecure`),
/// `/auth-config` which the login page fetches before the user has a
/// token, and `/healthz` — a no-I/O liveness probe the frontend pings to
/// time its own round-trip to this node (kept auth-free so the measurement
/// reflects transport overhead only, and so it works as a container probe).
/// The `/.well-known/`
/// prefix is also exempt — RFC 8615 reserves it for unauthenticated probes
/// (Chrome DevTools, OIDC discovery, ACME challenges, etc.). If you ever
/// add an app handler at `/.well-known/<something>`, it will be reachable
/// without auth; mount it elsewhere or add an explicit re-check in the
/// handler.
///
/// Deliberate non-goal: being a precise route allowlist. Any handler not
/// matched here will require auth, even unknown paths that happen to fall
/// through to the catch-all SPA handler. A 401 on an unknown path is fine.
fn is_always_public(path: &str) -> bool {
    matches!(
        path,
        "" | "/" | "/index.html" | "/favicon.ico" | "/favicon.svg" | "/auth-config" | "/healthz"
    ) || path.starts_with("/assets/")
        || path.starts_with("/swagger-ui/")
        || path.starts_with("/api-docs/")
        || path.starts_with("/.well-known/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spa_entry_is_public() {
        assert!(is_always_public("/"));
        assert!(is_always_public("/index.html"));
        assert!(is_always_public("/assets/main.abc123.js"));
        assert!(is_always_public("/favicon.ico"));
        assert!(is_always_public("/favicon.svg"));
    }

    #[test]
    fn auth_config_is_public_but_auth_status_is_not() {
        assert!(is_always_public("/auth-config"));
        assert!(!is_always_public("/auth/status"));
        assert!(!is_always_public("/auth/test"));
    }

    #[test]
    fn healthz_is_public() {
        assert!(is_always_public("/healthz"));
    }

    #[test]
    fn api_endpoints_require_auth() {
        assert!(!is_always_public("/party-config"));
        assert!(!is_always_public("/kick"));
        assert!(!is_always_public("/governance/propose"));
        assert!(!is_always_public("/node-config"));
    }

    #[test]
    fn swagger_is_public() {
        assert!(is_always_public("/swagger-ui/index.html"));
        assert!(is_always_public("/api-docs/openapi.json"));
    }

    #[test]
    fn well_known_paths_are_public() {
        assert!(is_always_public(
            "/.well-known/appspecific/com.chrome.devtools.json"
        ));
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        // RFC 7235 §2.1: scheme name is case-insensitive.
        assert_eq!(parse_bearer("Bearer abc.def.ghi"), Some("abc.def.ghi"));
        assert_eq!(parse_bearer("bearer abc.def.ghi"), Some("abc.def.ghi"));
        assert_eq!(parse_bearer("BEARER abc.def.ghi"), Some("abc.def.ghi"));
        assert_eq!(parse_bearer("bEaReR abc.def.ghi"), Some("abc.def.ghi"));
    }

    #[test]
    fn bearer_tolerates_extra_whitespace() {
        assert_eq!(parse_bearer("  Bearer   abc  "), Some("abc"));
        assert_eq!(parse_bearer("Bearer\tabc"), Some("abc"));
    }

    #[test]
    fn non_bearer_schemes_rejected() {
        assert_eq!(parse_bearer("Basic dXNlcjpwYXNz"), None);
        assert_eq!(parse_bearer("Token abc"), None);
        assert_eq!(parse_bearer(""), None);
        assert_eq!(parse_bearer("Bearer"), None);
        assert_eq!(parse_bearer("Bearer "), None);
    }

    // ------------------------------------------------------------------
    // End-to-end middleware tests via actix_web::test
    //
    // Drive the actual `AuthMiddleware::call` against a test app with a
    // stub handler. Covers the bootstrap path, public-allowlist short
    // circuit, validator success/failure, header-parsing edge cases, and
    // concurrent-bootstrap rejection.
    // ------------------------------------------------------------------

    use std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    };

    use actix_web::{
        App, Responder, get,
        http::StatusCode,
        put,
        test::{self, TestRequest},
        web::Data,
    };
    use sqlx::SqlitePool;
    use tokio::sync::{Mutex, RwLock};

    use crate::{
        auth::{JwtValidator, MockValidator, TokenValidator},
        config::{NodeConfig, PartyCredentials},
        server::AppState,
    };

    /// Stub handler for any path — returns 200 with the matched path so the
    /// test can confirm the middleware passed the request through.
    #[get("/{tail:.*}")]
    async fn stub_get(req: HttpRequest) -> impl Responder {
        HttpResponse::Ok().body(req.path().to_string())
    }

    #[put("/party-config")]
    async fn stub_put_party_config() -> impl Responder {
        HttpResponse::Ok().body("ok")
    }

    async fn build_app_state_with(
        parties: Vec<PartyCredentials>,
        validator: TokenValidator,
    ) -> Data<AppState> {
        let db = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let party_credentials = Arc::new(RwLock::new(parties));
        Data::new(AppState {
            db,
            config: NodeConfig::default(),
            peer_status: Arc::new(RwLock::new(HashMap::new())),
            last_seen: Arc::new(RwLock::new(HashMap::new())),
            peer_job_sender: tokio::sync::mpsc::unbounded_channel().0,
            workflows: crate::server::WorkflowRegistry::new(),
            pending_invitations: Arc::new(RwLock::new(Vec::new())),
            auth: Arc::new(RwLock::new(None)),
            token_validator: validator,
            admin_role: Some("decman-admin".to_string()),
            party_credentials,
            bootstrap_mu: Arc::new(Mutex::new(())),
            relay_sessions: Arc::new(
                crate::workflow::party_replication::relay::RelaySessions::new(),
            ),
            test_mode: true,
            refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
            discovery_permits: Arc::new(tokio::sync::Semaphore::new(
                crate::server::handlers::MAX_CONCURRENT_DISCOVERIES,
            )),
            discovery_generations: Arc::new(RwLock::new(HashMap::new())),
            discovery_completed: Arc::new(RwLock::new(HashMap::new())),
            http_client: reqwest::Client::new(),
            health_cache: crate::server::HealthCache::new(),
            external_parties: Default::default(),
            tenant_rate_limiter: Default::default(),
        })
    }

    async fn build_app_state(parties: Vec<PartyCredentials>) -> Data<AppState> {
        build_app_state_with(
            parties,
            TokenValidator::Mock(Arc::new(MockValidator::new("decman-admin".to_string()))),
        )
        .await
    }

    #[actix_web::test]
    async fn well_known_path_bypasses_auth() {
        let state = build_app_state(Vec::new()).await;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(stub_get),
        )
        .await;

        let req = TestRequest::get()
            .uri("/.well-known/appspecific/com.chrome.devtools.json")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn jwt_validator_rejects_when_no_trusted_issuer() {
        // JwtValidator with no inbound config and no party_credentials
        // trusts no issuer. Empty / arbitrary tokens are rejected — the
        // only way to reach the handler is the public allowlist or the
        // bootstrap exemption (neither of which apply to /node-config).
        let parties = Arc::new(RwLock::new(Vec::new()));
        let validator = TokenValidator::Jwt(Arc::new(JwtValidator::new(
            None,
            None,
            None,
            parties.clone(),
            reqwest::Client::new(),
        )));
        let state = build_app_state_with(Vec::new(), validator).await;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(stub_get),
        )
        .await;

        // No header → empty token → MissingToken → 401.
        let req = TestRequest::get().uri("/node-config").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn bearer_token_passes_with_mock_validator() {
        let state = build_app_state(Vec::new()).await;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(stub_get),
        )
        .await;

        let req = TestRequest::get()
            .uri("/node-config")
            .insert_header(("Authorization", "Bearer any.thing.here"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn lowercase_bearer_scheme_passes() {
        let state = build_app_state(Vec::new()).await;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(stub_get),
        )
        .await;

        let req = TestRequest::get()
            .uri("/node-config")
            .insert_header(("Authorization", "bearer any.thing"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn put_party_config_bootstrap_allowed_when_empty() {
        let state = build_app_state(Vec::new()).await;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(stub_put_party_config),
        )
        .await;

        let req = TestRequest::put().uri("/party-config").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// A state that checks the tenant keys `keys` against a budget of `limit`
    /// calls per minute, as a production node does.
    async fn tenant_state(keys: &[&str], limit: u32) -> Data<AppState> {
        let Ok(mut state) = Arc::try_unwrap(build_app_state(Vec::new()).await.into_inner()) else {
            panic!("the freshly built state has no other owner");
        };
        state.test_mode = false;
        state.config.tenant_api_keys = keys.iter().map(ToString::to_string).collect();
        state.config.tenant_rate_limit_per_minute = limit;
        Data::new(state)
    }

    /// How often the stub write handler ran.
    #[derive(Default)]
    struct Calls(std::sync::atomic::AtomicUsize);

    /// Stands for a tenant write handler. It reads a JSON body, so a malformed
    /// body is a 400 once the request gets this far. It answers with the key
    /// the middleware attached.
    #[actix_web::post("/v0/tenant/prepare")]
    async fn stub_tenant_write(
        req: HttpRequest,
        calls: Data<Calls>,
        _body: web::Json<serde_json::Value>,
    ) -> impl Responder {
        calls.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let key = tenant_caller(&req).ok().flatten();
        HttpResponse::Ok().json(json!({
            "index": key.as_ref().map(|k| k.index),
            "id": key.map(|k| k.id),
        }))
    }

    #[actix_web::post("/v0/tenant/add-hosts/import")]
    async fn stub_tenant_import() -> impl Responder {
        HttpResponse::Ok().finish()
    }

    #[get("/v0/tenant/{party}/acs/{target}")]
    async fn stub_tenant_acs() -> impl Responder {
        HttpResponse::Ok().finish()
    }

    fn tenant_post(uri: &str, key: &str, body: &'static str) -> TestRequest {
        TestRequest::post()
            .uri(uri)
            .insert_header(("Authorization", format!("Bearer {key}")))
            .insert_header(("Content-Type", "application/json"))
            .set_payload(body)
    }

    #[actix_web::test]
    async fn the_middleware_names_the_key_that_matched() {
        for keys in [["first", "second"], ["second", "first"]] {
            let app = test::init_service(
                App::new()
                    .app_data(tenant_state(&keys, 0).await)
                    .app_data(Data::new(Calls::default()))
                    .wrap(AuthMiddleware)
                    .service(stub_tenant_write),
            )
            .await;

            let resp = test::call_service(
                &app,
                tenant_post("/v0/tenant/prepare", "second", "{}").to_request(),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body: serde_json::Value = test::read_body_json(resp).await;
            let index = keys.iter().position(|k| *k == "second");
            assert_eq!(body["index"], json!(index), "{keys:?}");
            // The id is the same whatever the key's position.
            assert_eq!(body["id"], json!(tenant_key_id("second")), "{keys:?}");

            let resp = test::call_service(
                &app,
                tenant_post("/v0/tenant/prepare", "third", "{}").to_request(),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[test]
    fn a_key_id_is_short_hex_that_does_not_contain_the_key() {
        let key = "tenant-key-0123456789abcdef";
        let id = tenant_key_id(key);
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
        assert!(!key.contains(&id) && !id.contains(key), "{id}");
        assert_eq!(id, tenant_key_id(key), "the id must be stable");
        assert_ne!(id, tenant_key_id("another-key"));
    }

    /// A caller without a valid key must not make the node parse its body,
    /// must not spend a real key's budget, and must not learn from a 429 that
    /// the budget is spent.
    #[actix_web::test]
    async fn an_unknown_key_gets_401_before_the_body_is_read() {
        let calls = Data::new(Calls::default());
        let app = test::init_service(
            App::new()
                .app_data(tenant_state(&["first"], 1).await)
                .app_data(calls.clone())
                .wrap(AuthMiddleware)
                .service(stub_tenant_write),
        )
        .await;

        for _ in 0..5 {
            let resp = test::call_service(
                &app,
                tenant_post("/v0/tenant/prepare", "guess", "not json").to_request(),
            )
            .await;
            // A 400 would mean the body was parsed.
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        assert_eq!(calls.0.load(std::sync::atomic::Ordering::SeqCst), 0);

        let resp = test::call_service(
            &app,
            tenant_post("/v0/tenant/prepare", "first", "{}").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "the budget must be unspent");

        let resp = test::call_service(
            &app,
            tenant_post("/v0/tenant/prepare", "guess", "{}").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// The budget is spent before the body is parsed, so a malformed body
    /// cannot get past it.
    #[actix_web::test]
    async fn an_exhausted_key_gets_429_even_for_a_malformed_body() {
        let app = test::init_service(
            App::new()
                .app_data(tenant_state(&["first"], 1).await)
                .app_data(Data::new(Calls::default()))
                .wrap(AuthMiddleware)
                .service(stub_tenant_write),
        )
        .await;

        let resp = test::call_service(
            &app,
            tenant_post("/v0/tenant/prepare", "first", "not json").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let limited = test::call_service(
            &app,
            tenant_post("/v0/tenant/prepare", "first", "not json").to_request(),
        )
        .await;
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = limited
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        assert!(
            retry_after.is_some_and(|secs| (1..=60).contains(&secs)),
            "Retry-After must be whole seconds within the window, got {retry_after:?}"
        );
        let body: serde_json::Value = test::read_body_json(limited).await;
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("rate limit")),
            "{body}"
        );
    }

    /// One ACS copy sends a call per block, so the middleware checks the key
    /// of the two relay calls but spends no budget on them.
    #[actix_web::test]
    async fn the_acs_relay_calls_are_checked_but_not_metered() {
        let app = test::init_service(
            App::new()
                .app_data(tenant_state(&["first"], 1).await)
                .app_data(Data::new(Calls::default()))
                .wrap(AuthMiddleware)
                .service(stub_tenant_write)
                .service(stub_tenant_import)
                .service(stub_tenant_acs),
        )
        .await;

        for i in 0..3 {
            let resp = test::call_service(
                &app,
                tenant_post("/v0/tenant/add-hosts/import", "first", "{}").to_request(),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK, "import {i}");
            let read = TestRequest::get()
                .uri("/v0/tenant/alice::1220aa/acs/participant-3::1220bb?base_serial=1&seq=1")
                .insert_header(("Authorization", "Bearer first"))
                .to_request();
            assert_eq!(
                test::call_service(&app, read).await.status(),
                StatusCode::OK,
                "read {i}"
            );
        }
        let resp = test::call_service(
            &app,
            tenant_post("/v0/tenant/prepare", "first", "{}").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "the budget must be unspent");

        let resp = test::call_service(
            &app,
            tenant_post("/v0/tenant/add-hosts/import", "guess", "{}").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_limit_of_zero_turns_the_budget_off() {
        let app = test::init_service(
            App::new()
                .app_data(tenant_state(&["first"], 0).await)
                .app_data(Data::new(Calls::default()))
                .wrap(AuthMiddleware)
                .service(stub_tenant_write),
        )
        .await;
        for _ in 0..200 {
            let resp = test::call_service(
                &app,
                tenant_post("/v0/tenant/prepare", "first", "{}").to_request(),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
    }

    #[actix_web::test]
    async fn test_mode_checks_no_key_and_spends_no_budget() {
        let Ok(mut state) = Arc::try_unwrap(tenant_state(&[], 1).await.into_inner()) else {
            panic!("the freshly built state has no other owner");
        };
        state.test_mode = true;
        let app = test::init_service(
            App::new()
                .app_data(Data::new(state))
                .app_data(Data::new(Calls::default()))
                .wrap(AuthMiddleware)
                .service(stub_tenant_write),
        )
        .await;
        for _ in 0..5 {
            let req = TestRequest::post()
                .uri("/v0/tenant/prepare")
                .set_json(json!({}))
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body: serde_json::Value = test::read_body_json(resp).await;
            assert_eq!(body["index"], serde_json::Value::Null);
        }
    }

    /// A handler reached without the middleware has no checked key, so it must
    /// refuse the call rather than treat it as insecure mode.
    #[test]
    fn a_request_the_middleware_did_not_see_is_refused() {
        let Err(refused) = tenant_caller(&TestRequest::default().to_http_request()) else {
            panic!("a request without a checked key must be refused");
        };
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    }
}
