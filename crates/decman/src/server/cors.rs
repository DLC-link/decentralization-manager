//! CORS for the HTTP API, chosen per route.
//!
//! Every route keeps one rule: same origin only, or one more origin with
//! credentials when the operator sets `DECPM_ALLOWED_ORIGIN`. The two routes a
//! decentralized-party dApp calls are the exception. They accept any origin,
//! without credentials. They authenticate with a bearer key rather than a
//! cookie, and the dApp may be served from any host.

use std::{
    rc::Rc,
    task::{Context, Poll},
};

use actix_cors::{Cors, CorsMiddleware};
use actix_web::{
    Error,
    body::{EitherBody, MessageBody},
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    http::{Method, header},
};
use futures::future::LocalBoxFuture;

use super::middleware::is_dapp_path;

/// Actix `Transform` that applies the dApp CORS rule to the dApp routes and
/// the operator's rule to every other route.
pub struct RouteCors {
    dapp: Cors,
    other: Cors,
}

impl RouteCors {
    /// `allowed_origin` is `DECPM_ALLOWED_ORIGIN`. It applies to every route
    /// except the dApp routes.
    pub fn new(allowed_origin: Option<&str>) -> Self {
        let other = match allowed_origin {
            Some(origin) => Cors::default()
                .allowed_origin(origin)
                .allow_any_method()
                .allow_any_header()
                .supports_credentials(),
            None => Cors::default(),
        };
        // `Retry-After` is exposed so a browser can honor a 429.
        let dapp = Cors::default()
            .allow_any_origin()
            .send_wildcard()
            .allowed_methods([Method::GET, Method::POST])
            .allowed_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
            .expose_headers([header::RETRY_AFTER]);
        Self { dapp, other }
    }
}

impl<S, B> Transform<S, ServiceRequest> for RouteCors
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = RouteCorsMiddleware<S>;
    type Future = LocalBoxFuture<'static, Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        let service = Rc::new(service);
        let dapp = self.dapp.new_transform(Shared(Rc::clone(&service)));
        let other = self.other.new_transform(Shared(service));
        Box::pin(async move {
            Ok(RouteCorsMiddleware {
                dapp: dapp.await?,
                other: other.await?,
            })
        })
    }
}

/// The service [`RouteCors`] builds. Both rules wrap the same inner service.
pub struct RouteCorsMiddleware<S> {
    dapp: CorsMiddleware<Shared<S>>,
    other: CorsMiddleware<Shared<S>>,
}

impl<S, B> Service<ServiceRequest> for RouteCorsMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = <CorsMiddleware<Shared<S>> as Service<ServiceRequest>>::Future;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Both rules forward to the same inner service.
        self.other.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        if is_dapp_path(req.path()) {
            self.dapp.call(req)
        } else {
            self.other.call(req)
        }
    }
}

/// One inner service that both CORS rules call.
pub struct Shared<S>(Rc<S>);

impl<S> Service<ServiceRequest> for Shared<S>
where
    S: Service<ServiceRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        self.0.call(req)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use actix_web::{
        App, HttpResponse, Responder, get,
        http::{StatusCode, header::HeaderMap},
        post,
        test::{self, TestRequest},
        web::Data,
    };

    use super::*;
    use crate::server::{AppState, middleware::AuthMiddleware};

    const DAPP_ORIGIN: &str = "https://dapp.example";
    const UI_ORIGIN: &str = "https://ui.example";
    const APPROVE: &str = "/v0/tenant/decentralized-party/approve-hosting";
    const STATUS: &str = "/v0/tenant/alice::1220aa/status";
    const PREPARE: &str = "/v0/tenant/prepare";

    #[post("/v0/tenant/decentralized-party/approve-hosting")]
    async fn stub_approve() -> impl Responder {
        HttpResponse::Accepted().finish()
    }

    #[get("/v0/tenant/{party}/status")]
    async fn stub_status() -> impl Responder {
        HttpResponse::Ok().finish()
    }

    #[post("/v0/tenant/prepare")]
    async fn stub_prepare() -> impl Responder {
        HttpResponse::Ok().finish()
    }

    async fn call(allowed_origin: Option<&str>, request: TestRequest) -> (StatusCode, HeaderMap) {
        let app = test::init_service(
            App::new()
                .wrap(RouteCors::new(allowed_origin))
                .service(stub_approve)
                .service(stub_status)
                .service(stub_prepare),
        )
        .await;
        let response = test::call_service(&app, request.to_request()).await;
        (response.status(), response.headers().clone())
    }

    fn from(origin: &str, request: TestRequest) -> TestRequest {
        request.insert_header((header::ORIGIN, origin))
    }

    fn preflight(origin: &str, path: &str, method: &str) -> TestRequest {
        TestRequest::default()
            .method(Method::OPTIONS)
            .uri(path)
            .insert_header((header::ORIGIN, origin))
            .insert_header((header::ACCESS_CONTROL_REQUEST_METHOD, method))
            .insert_header((
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization, content-type",
            ))
    }

    fn allow_origin(headers: &HeaderMap) -> Option<&str> {
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok())
    }

    fn allows_credentials(headers: &HeaderMap) -> bool {
        headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
    }

    /// Any origin may call the two dApp routes, with or without an operator
    /// origin set, and never with credentials.
    #[actix_web::test]
    async fn the_dapp_routes_accept_any_origin_without_credentials() {
        for allowed_origin in [None, Some(UI_ORIGIN)] {
            for request in [
                TestRequest::post().uri(APPROVE),
                TestRequest::get().uri(STATUS),
            ] {
                let (status, headers) = call(allowed_origin, from(DAPP_ORIGIN, request)).await;
                assert!(status.is_success(), "{allowed_origin:?}: {status}");
                assert_eq!(allow_origin(&headers), Some("*"), "{allowed_origin:?}");
                assert!(!allows_credentials(&headers), "{allowed_origin:?}");
            }
            for (path, method) in [(APPROVE, "POST"), (STATUS, "GET")] {
                let (status, headers) =
                    call(allowed_origin, preflight(DAPP_ORIGIN, path, method)).await;
                assert_eq!(status, StatusCode::OK, "{allowed_origin:?} {path}");
                assert_eq!(allow_origin(&headers), Some("*"));
                assert!(!allows_credentials(&headers));
            }
        }
    }

    /// A browser reads `Retry-After` from a cross-origin 429 only when it is
    /// exposed.
    #[actix_web::test]
    async fn the_dapp_routes_expose_retry_after() {
        let (_, headers) = call(None, from(DAPP_ORIGIN, TestRequest::post().uri(APPROVE))).await;
        let exposed = headers
            .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(exposed.contains("retry-after"), "{exposed}");
    }

    /// The key check answers before any handler runs. A browser must still be
    /// able to read that answer. The server wraps CORS outside the key check,
    /// as here.
    #[actix_web::test]
    async fn a_refused_dapp_call_carries_the_cors_headers() {
        let Ok(state) = AppState::for_test(None).await else {
            panic!("the test state must build");
        };
        let Ok(mut state) = Arc::try_unwrap(state.into_inner()) else {
            panic!("the freshly built state has no other owner");
        };
        state.test_mode = false;
        state.config.dapp_api_keys = vec!["dapp".to_string()];
        let app = test::init_service(
            App::new()
                .app_data(Data::new(state))
                .wrap(AuthMiddleware)
                .wrap(RouteCors::new(None))
                .service(stub_approve),
        )
        .await;

        let request = from(DAPP_ORIGIN, TestRequest::post().uri(APPROVE))
            .insert_header((header::AUTHORIZATION, "Bearer guess"));
        let response = test::call_service(&app, request.to_request()).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(allow_origin(response.headers()), Some("*"));
    }

    /// Every other route keeps today's rule: no cross-origin access by default.
    #[actix_web::test]
    async fn other_routes_stay_same_origin_by_default() {
        let (status, headers) =
            call(None, from(DAPP_ORIGIN, TestRequest::post().uri(PREPARE))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(allow_origin(&headers), None);

        let (status, headers) = call(None, preflight(DAPP_ORIGIN, PREPARE, "POST")).await;
        assert_ne!(status, StatusCode::OK);
        assert_eq!(allow_origin(&headers), None);
    }

    /// With an operator origin set, other routes allow that origin alone, with
    /// credentials, as before.
    #[actix_web::test]
    async fn other_routes_keep_the_operator_origin_with_credentials() {
        let (_, headers) = call(
            Some(UI_ORIGIN),
            from(UI_ORIGIN, TestRequest::post().uri(PREPARE)),
        )
        .await;
        assert_eq!(allow_origin(&headers), Some(UI_ORIGIN));
        assert!(allows_credentials(&headers));

        let (_, headers) = call(
            Some(UI_ORIGIN),
            from(DAPP_ORIGIN, TestRequest::post().uri(PREPARE)),
        )
        .await;
        assert_eq!(allow_origin(&headers), None);
    }
}
