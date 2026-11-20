use actix_web::{App, http::StatusCode, test};
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    db::MIGRATOR,
    server::{middleware::AuthMiddleware, tenant_call_audit},
    workflow::{
        external_party::add_hosts::{import_window_owner, replication_target},
        party_replication::pipe::PipeBlock,
    },
};

use super::*;

fn state(db: SqlitePool) -> web::Data<AppState> {
    let mut state = std::sync::Arc::try_unwrap(AppState::for_test_on(None, db).into_inner())
        .unwrap_or_else(|_| panic!("the test state has no other owner"));
    state.test_mode = false;
    state.config.tenant_api_keys = vec!["relay-test-key".into()];
    state.config.tenant_rate_limit_per_minute = 0;
    state.config.node.participant_id = Some(participant());
    state.config.canton.admin_api_host = "127.0.0.1".into();
    state.config.canton.admin_api_port = 1;
    web::Data::new(state)
}

fn participant() -> CantonId {
    format!("joiner::1220{}", "bb".repeat(32))
        .parse()
        .expect("a valid participant id")
}

fn request(path: &str, authorized: bool) -> test::TestRequest {
    let request = test::TestRequest::post().uri(path);
    let request = if authorized {
        request.insert_header(("Authorization", "Bearer relay-test-key"))
    } else {
        request
    };
    request.set_json(json!({
        "party_id": format!("observed::1220{}", "aa".repeat(32)),
        "new_hosts": [participant()],
        "base_serial": 7,
        "signed_topology_transactions": ["not-base64!"],
    }))
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn both_relay_routes_require_a_tenant_key_before_any_audit_or_canton_call(
    db: SqlitePool,
) -> anyhow::Result<()> {
    let state = state(db.clone());
    let app = test::init_service(
        App::new()
            .app_data(state.clone())
            .wrap(AuthMiddleware)
            .service(tenant_add_hosts_stage)
            .service(tenant_add_hosts_publish),
    )
    .await;
    for path in ["/v0/tenant/add-hosts/stage", "/v0/tenant/add-hosts/publish"] {
        let response = test::call_service(&app, request(path, false).to_request()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    assert!(tenant_call_audit::list(&db, None, 10).await?.is_empty());
    assert!(
        import_window_owner(&db, state.config.participant_id())
            .await?
            .is_none()
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn relay_routes_audit_authenticated_refusals_without_disclosing_keys_or_signatures(
    db: SqlitePool,
) -> anyhow::Result<()> {
    let state = state(db.clone());
    let app = test::init_service(
        App::new()
            .app_data(state.clone())
            .wrap(AuthMiddleware)
            .service(tenant_add_hosts_stage)
            .service(tenant_add_hosts_publish),
    )
    .await;
    for (path, expected) in [
        (
            "/v0/tenant/add-hosts/stage",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        ("/v0/tenant/add-hosts/publish", StatusCode::BAD_REQUEST),
    ] {
        let response = test::call_service(&app, request(path, true).to_request()).await;
        assert_eq!(response.status(), expected, "{path}");
        let response: serde_json::Value = test::read_body_json(response).await;
        assert!(response.get("signed_topology_transactions").is_none());
        let rows = tenant_call_audit::list(&db, None, 10).await?;
        let row = rows.first().expect("the refusal is recorded");
        assert_eq!(row.endpoint, path);
        assert_eq!(row.http_status, Some(i64::from(expected.as_u16())));
        assert_eq!(row.key_index, Some(0));
        assert!(row.key_id.is_some());
        let error = row
            .error
            .as_deref()
            .expect("the audit records the refusal reason");
        assert!(!error.contains("relay-test-key"));
        assert!(!error.contains("not-base64!"));
    }
    assert_eq!(tenant_call_audit::list(&db, None, 10).await?.len(), 2);
    assert!(
        import_window_owner(&db, state.config.participant_id())
            .await?
            .is_none()
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn joiner_prepare_refuses_an_open_actual_relay_before_calling_canton(
    db: SqlitePool,
) -> anyhow::Result<()> {
    let state = state(db.clone());
    let party = format!("observed::1220{}", "aa".repeat(32));
    let target = replication_target(&party, state.config.participant_id(), 7)?;
    state
        .relay_sessions
        .accept_block(
            &state.config,
            &state.db,
            &target,
            &[],
            PipeBlock::Data {
                seq: 1,
                bytes: b"a relay block awaiting completion".to_vec(),
            },
            None,
        )
        .await?;
    assert!(state.relay_sessions.any_session(|_| true).await);
    let app = test::init_service(
        App::new()
            .app_data(state.clone())
            .wrap(AuthMiddleware)
            .service(tenant_add_hosts_prepare),
    )
    .await;
    let response = test::call_service(
        &app,
        request("/v0/tenant/add-hosts/prepare", true).to_request(),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "the open-relay guard must refuse preparation before the unreachable Canton call"
    );
    let body: serde_json::Value = test::read_body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .expect("a refusal reason")
            .contains("relay is still open")
    );
    assert!(
        import_window_owner(&db, state.config.participant_id())
            .await?
            .is_none()
    );
    state.relay_sessions.discard(&target.instance_name).await;
    Ok(())
}
