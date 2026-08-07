//! Serves the demo wallet's embedded UI bundle.

use actix_web::{HttpRequest, HttpResponse, get};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "frontend/dist"]
struct Assets;

#[get("/{path:.*}")]
pub async fn serve_frontend(req: HttpRequest) -> HttpResponse {
    let path = req.match_info().query("path");
    let file_path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(file_path) {
        Some(content) => {
            let mime = mime_guess::from_path(file_path).first_or_octet_stream();
            HttpResponse::Ok()
                .content_type(mime.as_ref())
                .body(content.data.into_owned())
        }
        // Single-page app: unknown paths fall back to the entry point.
        None => match Assets::get("index.html") {
            Some(content) => HttpResponse::Ok()
                .content_type("text/html")
                .body(content.data.into_owned()),
            None => HttpResponse::NotFound().body("404 Not Found"),
        },
    }
}
