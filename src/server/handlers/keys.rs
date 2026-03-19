use actix_web::{HttpResponse, Responder, get, web};

use crate::{
    noise::NoiseKeypair,
    server::{AppState, types::KeyStatusResponse},
};

/// Check if Noise keys exist for this node
#[utoipa::path(
    tag = "Keys",
    responses(
        (status = 200, description = "Key status", body = KeyStatusResponse)
    )
)]
#[get("/keys/status")]
pub async fn get_key_status(data: web::Data<AppState>) -> impl Responder {
    let key_file = data.config.key_file_path();

    match NoiseKeypair::from_file(&key_file).await {
        Ok(keypair) => HttpResponse::Ok().json(KeyStatusResponse {
            has_keys: true,
            public_key: Some(keypair.public_key_hex()),
        }),
        Err(_) => HttpResponse::Ok().json(KeyStatusResponse {
            has_keys: false,
            public_key: None,
        }),
    }
}
