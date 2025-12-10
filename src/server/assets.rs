use actix_web::{HttpRequest, HttpResponse, get};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "frontend/dist"]
struct Assets;

#[get("/{path:.*}")]
pub async fn serve_frontend(req: HttpRequest) -> HttpResponse {
    let path = req.match_info().query("path");

    // Try the exact path first
    let file_path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(file_path) {
        Some(content) => {
            let mime = mime_guess::from_path(file_path).first_or_octet_stream();
            HttpResponse::Ok()
                .content_type(mime.as_ref())
                .body(content.data.into_owned())
        }
        None => {
            // For SPA routing, serve index.html for non-asset paths
            match Assets::get("index.html") {
                Some(content) => HttpResponse::Ok()
                    .content_type("text/html")
                    .body(content.data.into_owned()),
                None => HttpResponse::NotFound().body("404 Not Found"),
            }
        }
    }
}
