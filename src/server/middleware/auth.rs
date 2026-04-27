//! Bearer-token auth middleware.
//!
//! Runs in front of every request. Public paths (SPA assets, `/auth-config`,
//! swagger) pass through. `PUT /party-config` is special-cased for first-run
//! bootstrap: if the `party_credentials` table is empty we let the call
//! through unauthenticated so a fresh node can be configured; after the first
//! row lands, normal auth applies and the handler enforces admin role.
//!
//! Everything else requires a valid `Authorization: Bearer <token>` that the
//! configured `TokenValidator` accepts. The resolved `Principal` is attached
//! to the request extensions for handlers to consume.

use std::{
    future::{Ready, ready},
    rc::Rc,
};

use actix_web::{
    Error, HttpMessage, HttpRequest, HttpResponse,
    body::{EitherBody, MessageBody},
    dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready},
    http::header::AUTHORIZATION,
    web,
};
use futures::future::LocalBoxFuture;
use serde_json::json;

use crate::{auth::Principal, server::AppState};

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
                    tracing::warn!(
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
/// check it carries the admin role.
///
/// # Errors
///
/// Returns an `HttpResponse` ready to return: 401 if no principal was
/// attached to the request, 403 if the principal lacks `admin_role`.
pub fn require_admin(req: &HttpRequest, admin_role: &str) -> Result<Principal, HttpResponse> {
    let principal = {
        let extensions = req.extensions();
        extensions.get::<Principal>().cloned()
    };
    let Some(principal) = principal else {
        return Err(HttpResponse::Unauthorized().json(json!({"error": "authentication required"})));
    };
    principal
        .require_admin(admin_role)
        .map_err(|e| HttpResponse::Forbidden().json(json!({"error": e.to_string()})))?;
    Ok(principal)
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
/// assets, swagger (only mounted in `--test`), and `/auth-config` which the
/// login page fetches before the user has a token.
///
/// Deliberate non-goal: being a precise route allowlist. Any handler not
/// matched here will require auth, even unknown paths that happen to fall
/// through to the catch-all SPA handler. A 401 on an unknown path is fine.
fn is_always_public(path: &str) -> bool {
    matches!(
        path,
        "" | "/" | "/index.html" | "/favicon.ico" | "/favicon.svg" | "/auth-config"
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
}
