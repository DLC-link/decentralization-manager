//! Wires the demo wallet's API and UI onto an actix server.

use actix_web::{App, HttpServer, web};

use crate::demo::{DemoState, api, assets};

/// Serve the demo wallet on `bind_address` until interrupted.
///
/// Binds to loopback by default (see the binary's `--bind`): this process holds a
/// party's private key and the provider's tenant API key, so it is not something
/// to expose.
///
/// # Errors
/// Fails if the address cannot be bound.
pub async fn run(state: DemoState, bind_address: &str) -> std::io::Result<()> {
    let state = web::Data::new(state);

    tracing::info!("demo wallet listening on http://{bind_address}");

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .service(api::config)
            .service(api::create_party)
            .service(api::party_status)
            .service(api::party_acs)
            .service(api::create_party_contract)
            .service(api::party)
            .service(api::reset)
            // Registered last: it is a catch-all that would otherwise shadow the
            // API routes above.
            .service(assets::serve_frontend)
    })
    .bind(bind_address)?
    .run()
    .await
}
