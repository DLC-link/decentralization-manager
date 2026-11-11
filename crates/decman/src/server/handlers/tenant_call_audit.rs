//! Operator read of the tenant call audit: the tenant API calls that wrote
//! topology or state on this node.

use actix_web::{HttpRequest, HttpResponse, Responder, get, web};
use serde::Deserialize;

use crate::server::{
    AppState,
    middleware::require_admin,
    tenant_call_audit,
    types::{ErrorResponse, TenantCallAuditResponse},
};

/// How many rows a read returns when the caller names no limit.
const DEFAULT_LIMIT: i64 = 100;
/// The most rows one read returns.
const MAX_LIMIT: i64 = 500;

/// `?party_id=&limit=` on the tenant call audit.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct TenantCallAuditQuery {
    /// Only the calls that named this party.
    pub party_id: Option<String>,
    /// How many rows to return, newest first. Default 100, at most 500.
    pub limit: Option<i64>,
}

/// The tenant API calls that wrote topology or state on this node, newest
/// first. Calls refused after the key check are included.
#[utoipa::path(
    tag = "Tenant",
    params(TenantCallAuditQuery),
    responses(
        (status = 200, description = "The tenant calls, newest first", body = TenantCallAuditResponse),
        (status = 401, description = "Not authenticated", body = ErrorResponse),
        (status = 403, description = "Not an admin", body = ErrorResponse),
        (status = 500, description = "The audit could not be read", body = ErrorResponse)
    )
)]
#[get("/tenant-call-audit")]
pub async fn get_tenant_call_audit(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    query: web::Query<TenantCallAuditQuery>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    match tenant_call_audit::list(&data.db, query.party_id.as_deref(), limit).await {
        Ok(rows) => HttpResponse::Ok().json(TenantCallAuditResponse {
            entries: rows.into_iter().map(|row| row.into_entry()).collect(),
        }),
        Err(e) => {
            tracing::error!("failed to read the tenant call audit: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the tenant call audit; see the host's logs".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use actix_web::{App, http::StatusCode, test as actix_test};
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        db::MIGRATOR,
        server::{
            middleware::AuthMiddleware,
            tenant_call_audit::{NewTenantCall, TenantEndpoint, insert_call},
        },
    };

    fn call(party_id: &'static str) -> NewTenantCall<'static> {
        NewTenantCall {
            key_index: Some(0),
            key_id: Some("0123456789abcdef"),
            endpoint: TenantEndpoint::AddHostsExecute,
            party_id: Some(party_id),
            http_status: Some(202),
            error: None,
        }
    }

    /// Read the audit as an admin, through the auth middleware.
    async fn read(db: &SqlitePool, query: &str) -> TenantCallAuditResponse {
        let Ok(mut state) = Arc::try_unwrap(AppState::for_test_on(None, db.clone()).into_inner())
        else {
            panic!("the freshly built state has no other owner");
        };
        state.admin_role = Some("decman-admin".to_string());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .wrap(AuthMiddleware)
                .service(get_tenant_call_audit),
        )
        .await;
        let request = actix_test::TestRequest::get()
            .uri(&format!("/tenant-call-audit{query}"))
            .insert_header(("Authorization", "Bearer admin.token"))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        actix_test::read_body_json(response).await
    }

    fn ids(response: &TenantCallAuditResponse) -> Vec<i64> {
        response.entries.iter().map(|e| e.id).collect()
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_audit_reads_newest_first(db: SqlitePool) -> anyhow::Result<()> {
        let first = insert_call(&db, &call("alice::1220aa")).await?;
        let second = insert_call(&db, &call("bob::1220bb")).await?;
        let third = insert_call(&db, &call("alice::1220aa")).await?;

        let response = read(&db, "").await;

        assert_eq!(ids(&response), vec![third, second, first]);
        assert_eq!(response.entries[0].endpoint, "/v0/tenant/add-hosts/execute");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_party_filter_and_the_limit_bound_the_read(db: SqlitePool) -> anyhow::Result<()> {
        let alice = insert_call(&db, &call("alice::1220aa")).await?;
        insert_call(&db, &call("bob::1220bb")).await?;
        // 501 more rows for alice, one past the most a read returns.
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 501) \
             INSERT INTO tenant_call_audit (called_at, endpoint, party_id, http_status) \
             SELECT 0, '/v0/tenant/execute', 'alice::1220aa', 202 FROM n",
        )
        .execute(&db)
        .await?;

        let bob = read(&db, "?party_id=bob::1220bb").await;
        assert_eq!(bob.entries.len(), 1);
        assert_eq!(bob.entries[0].party_id.as_deref(), Some("bob::1220bb"));

        assert_eq!(read(&db, "").await.entries.len(), DEFAULT_LIMIT as usize);
        assert_eq!(read(&db, "?limit=0").await.entries.len(), 1);
        assert_eq!(read(&db, "?limit=-3").await.entries.len(), 1);
        assert_eq!(
            read(&db, "?limit=100000").await.entries.len(),
            MAX_LIMIT as usize
        );
        let alice_rows = read(&db, "?party_id=alice::1220aa&limit=500").await;
        assert_eq!(alice_rows.entries.len(), 500);
        assert!(
            alice_rows
                .entries
                .iter()
                .all(|e| e.party_id.as_deref() == Some("alice::1220aa"))
        );
        assert!(
            alice_rows.entries.iter().all(|e| e.id != alice),
            "the oldest row is past the limit"
        );
        Ok(())
    }

    /// The audit names which key wrote what, so it is for admins only.
    #[actix_web::test]
    async fn a_non_admin_cannot_read_the_audit() -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(AppState::for_test_as_non_admin().await?)
                .wrap(AuthMiddleware)
                .service(get_tenant_call_audit),
        )
        .await;
        let request = actix_test::TestRequest::get()
            .uri("/tenant-call-audit")
            .insert_header(("Authorization", "Bearer viewer.token"))
            .to_request();

        let response = actix_test::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    /// The frontend types are generated from these DTOs, so the wire names
    /// must not drift.
    #[test]
    fn the_audit_response_keeps_its_wire_shape() {
        let json = serde_json::json!({
            "entries": [{
                "id": 7,
                "called_at": 1_700_000_000,
                "key_index": 1,
                "key_id": "0123456789abcdef",
                "endpoint": "/v0/tenant/threshold/execute",
                "party_id": "alice::1220aa",
                "http_status": 409,
                "error": "stale serial",
            }, {
                "id": 6,
                "called_at": 1_700_000_000,
                "key_index": null,
                "key_id": null,
                "endpoint": "/v0/tenant/add-hosts/import (abandoned)",
                "party_id": "alice::1220aa",
                "http_status": null,
                "error": "the reaper ended the idle copy",
            }],
        });
        let Ok(response) = serde_json::from_value::<TenantCallAuditResponse>(json) else {
            panic!("the documented audit response shape must deserialize");
        };
        let entry = &response.entries[0];
        assert_eq!(entry.id, 7);
        assert_eq!(entry.key_index, Some(1));
        assert_eq!(entry.key_id.as_deref(), Some("0123456789abcdef"));
        assert_eq!(entry.http_status, Some(409));
        assert_eq!(entry.error.as_deref(), Some("stale serial"));
        let abandoned = &response.entries[1];
        assert_eq!(abandoned.key_id, None);
        assert_eq!(abandoned.http_status, None);
    }
}
