use actix_web::{
    HttpRequest, HttpResponse, get,
    http::header::{CACHE_CONTROL, HeaderValue},
};
use rust_embed::Embed;

const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const REVALIDATE_CACHE_CONTROL: &str = "no-cache";

#[derive(Embed)]
#[folder = "frontend/dist"]
struct Assets;

fn cache_control(file_path: &str) -> HeaderValue {
    // Vite fingerprints everything emitted under /assets, so those URLs can
    // safely be cached forever. Entry points and other public files keep stable
    // URLs and must be revalidated so a deployment is picked up immediately.
    HeaderValue::from_static(if file_path.starts_with("assets/") {
        IMMUTABLE_CACHE_CONTROL
    } else {
        REVALIDATE_CACHE_CONTROL
    })
}

fn embedded_response(file_path: &str, content: rust_embed::EmbeddedFile) -> HttpResponse {
    let mime = mime_guess::from_path(file_path).first_or_octet_stream();
    HttpResponse::Ok()
        .content_type(mime.as_ref())
        .insert_header((CACHE_CONTROL, cache_control(file_path)))
        .body(content.data.into_owned())
}

#[get("/{path:.*}")]
pub async fn serve_frontend(req: HttpRequest) -> HttpResponse {
    let path = req.match_info().query("path");

    let file_path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(file_path) {
        Some(content) => embedded_response(file_path, content),
        None => {
            // For SPA routing, serve index.html for non-asset paths
            match Assets::get("index.html") {
                Some(content) => embedded_response("index.html", content),
                None => HttpResponse::NotFound().body("404 Not Found"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, http::header::CONTENT_TYPE, test as actix_test};

    #[test]
    fn vite_assets_are_cached_immutably() {
        assert_eq!(
            cache_control("assets/index-C4G4w4u6.js"),
            IMMUTABLE_CACHE_CONTROL
        );
        assert_eq!(
            cache_control("assets/space-grotesk-DiSf0yqz.woff2"),
            IMMUTABLE_CACHE_CONTROL
        );
    }

    #[test]
    fn stable_urls_are_revalidated() {
        assert_eq!(cache_control("index.html"), REVALIDATE_CACHE_CONTROL);
        assert_eq!(cache_control("favicon.svg"), REVALIDATE_CACHE_CONTROL);
    }

    #[actix_web::test]
    async fn entrypoint_and_spa_fallback_are_revalidated() {
        let app = actix_test::init_service(App::new().service(serve_frontend)).await;

        for uri in ["/", "/notifications"] {
            let request = actix_test::TestRequest::get().uri(uri).to_request();
            let response = actix_test::call_service(&app, request).await;

            assert!(response.status().is_success());
            assert_eq!(
                response.headers().get(CACHE_CONTROL).unwrap(),
                REVALIDATE_CACHE_CONTROL
            );
            assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "text/html");
        }
    }
}
