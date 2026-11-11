//! The tenant call audit around each write.

use std::panic::AssertUnwindSafe;

use actix_web::{
    HttpResponse,
    body::MessageBody,
    http::{StatusCode, header::HeaderMap},
    web,
};
use futures::FutureExt;

use crate::{
    server::{
        AppState,
        middleware::TenantKey,
        tenant_call_audit::{self, NewTenantCall, TenantEndpoint},
        types::ErrorResponse,
    },
    workflow::party_replication::relay::{ImportEnd, OnImportEnd},
};

/// How the audit names a tenant caller: by the index of its key in
/// `DECPM_TENANT_API_KEYS`, never by the key.
pub(super) fn tenant_requester(key_index: Option<usize>) -> String {
    match key_index {
        Some(index) => format!("tenant-key-{index}"),
        // Insecure/test mode checks no key.
        None => "tenant-key-unchecked".to_string(),
    }
}

/// Run one tenant write and record it in the audit, in a task of its own, and
/// return the write's answer unchanged.
///
/// A client that goes away drops its request, and actix drops the handler with
/// it. The task does not depend on the handler, so the write still finishes
/// and its row is still written. The task also catches a write that panics:
/// it answers 500 and writes the row itself.
pub(super) async fn audited_write<W, F>(
    data: &web::Data<AppState>,
    key: Option<TenantKey>,
    endpoint: TenantEndpoint,
    party_id: String,
    write: W,
) -> HttpResponse
where
    W: FnOnce(web::Data<AppState>) -> F + Send + 'static,
    F: Future<Output = HttpResponse> + Send + 'static,
{
    let task = tokio::spawn({
        let data = data.clone();
        let party_id = party_id.clone();
        async move {
            let written = {
                let data = data.clone();
                AssertUnwindSafe(async move { write(data).await })
                    .catch_unwind()
                    .await
            };
            let response = written.unwrap_or_else(|_| {
                tracing::error!(
                    endpoint = endpoint.as_str(),
                    %party_id,
                    "a tenant write panicked"
                );
                write_failed()
            });
            let answer = Answer::of(response);
            record_answer(&data.db, key.as_ref(), endpoint, &party_id, &answer).await;
            answer
        }
    });
    match task.await {
        Ok(answer) => answer.into_response(),
        // The task caught the write's own panic and wrote its row, so this is a
        // failure of the audit insert itself. Another row could repeat that one.
        Err(e) => {
            tracing::error!(
                endpoint = endpoint.as_str(),
                %party_id,
                "a tenant write task did not complete: {e}"
            );
            write_failed()
        }
    }
}

/// The answer to a write that did not complete.
fn write_failed() -> HttpResponse {
    HttpResponse::InternalServerError().json(ErrorResponse {
        error: "The call failed on this host; see the host's logs".to_string(),
    })
}

/// A write's answer as plain data, so it can leave the task the write ran in.
struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: web::Bytes,
}

impl Answer {
    fn of(response: HttpResponse) -> Self {
        let (head, body) = response.into_parts();
        let body = body.try_into_bytes().unwrap_or_else(|_| {
            // The handlers here answer with JSON, which is never streamed.
            tracing::error!("a tenant write answered with a streamed body, which is dropped");
            web::Bytes::new()
        });
        Self {
            status: head.status(),
            headers: head.headers().clone(),
            body,
        }
    }

    fn into_response(self) -> HttpResponse {
        let mut response = HttpResponse::with_body(self.status, self.body).map_into_boxed_body();
        *response.headers_mut() = self.headers;
        response
    }
}

/// Insert the audit row for a write's answer. The error text is kept only for
/// a refusal (4xx/5xx).
async fn record_answer(
    db: &sqlx::SqlitePool,
    key: Option<&TenantKey>,
    endpoint: TenantEndpoint,
    party_id: &str,
    answer: &Answer,
) {
    let error = (answer.status.is_client_error() || answer.status.is_server_error())
        .then(|| error_text(&answer.body))
        .flatten();
    let call = NewTenantCall {
        key_index: key.map(|k| k.index),
        key_id: key.map(|k| k.id.as_str()),
        endpoint,
        party_id: Some(party_id),
        http_status: Some(answer.status.as_u16()),
        error: error.as_deref(),
    };
    record(db, &call).await;
}

/// What an import that block 1 opens calls when it is retired: it records how
/// the copy ended against the key that opened it. No call answers at that
/// point, so the row has no HTTP status.
pub(super) fn record_import_end(
    data: &AppState,
    key: Option<TenantKey>,
    party_id: &str,
) -> OnImportEnd {
    let db = data.db.clone();
    let party_id = party_id.to_string();
    Box::new(move |end| {
        Box::pin(async move {
            let (endpoint, error) = match &end {
                ImportEnd::Completed => (TenantEndpoint::AcsImportCompleted, None),
                ImportEnd::Abandoned(why) => {
                    (TenantEndpoint::AcsImportAbandoned, Some(why.as_str()))
                }
            };
            let call = NewTenantCall {
                key_index: key.as_ref().map(|k| k.index),
                key_id: key.as_ref().map(|k| k.id.as_str()),
                endpoint,
                party_id: Some(&party_id),
                http_status: None,
                error,
            };
            record(&db, &call).await;
        })
    })
}

/// Insert an audit row. A failure is logged and goes no further, because the
/// Canton write may already have happened and the caller must still see its
/// outcome.
async fn record(db: &sqlx::SqlitePool, call: &NewTenantCall<'_>) {
    if let Err(e) = tenant_call_audit::insert_call(db, call).await {
        tracing::error!(
            endpoint = call.endpoint.as_str(),
            http_status = call.http_status,
            "failed to record a tenant call in the audit: {e:#}"
        );
    }
}

/// The text of a refusal: the `error` field of an `ErrorResponse`, or the raw
/// body when it is not one.
fn error_text(body: &[u8]) -> Option<String> {
    let text = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|json| json.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    (!text.is_empty()).then_some(text)
}

/// Every request here is refused at validation, or reaches an import the test
/// opened without Canton, so the tests need no participant.
#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use actix_web::{App, http::StatusCode, test as actix_test};
    use base64::{Engine, engine::general_purpose::STANDARD};
    use common::api::TenantCallAuditEntry;
    use serde_json::json;
    use sqlx::SqlitePool;
    use tokio::sync::oneshot;

    use super::{
        super::{
            acs::tenant_acs_import,
            add_hosts::{tenant_add_hosts_execute, tenant_add_hosts_execute_on_this_host},
            onboard::tenant_execute,
        },
        *,
    };
    use crate::{
        canton_id::CantonId,
        db::MIGRATOR,
        server::{
            middleware::{AuthMiddleware, tenant_key_id},
            tenant_call_audit::{PARTY_ID_CAP, TenantCallAuditRow},
            types::TenantAddHostsExecuteRequest,
        },
        workflow::external_party::add_hosts::replication_target,
    };

    /// The audit names the key by its position, and must never carry the key.
    #[test]
    fn a_tenant_caller_is_recorded_by_key_index() {
        assert_eq!(tenant_requester(Some(2)), "tenant-key-2");
        assert_eq!(tenant_requester(None), "tenant-key-unchecked");
    }

    const PARTY: &str = "alice::1220aa";

    /// A party id that `replication_target` accepts.
    fn valid_party() -> String {
        format!("alice::1220{}", "aa".repeat(32))
    }

    /// The instance name of the copy of `party` onto this node at serial 1.
    fn instance_of(state: &AppState, party: &str) -> String {
        match replication_target(party, state.config.participant_id(), 1) {
            Ok(target) => target.instance_name,
            Err(e) => panic!("the test party must name a replication: {e}"),
        }
    }

    /// A state on `db` that checks the tenant keys `keys` against a budget of
    /// `limit` calls per minute, as a production node does.
    fn keyed_state(db: SqlitePool, keys: &[&str], limit: u32) -> web::Data<AppState> {
        let Ok(mut state) = Arc::try_unwrap(AppState::for_test_on(None, db).into_inner()) else {
            panic!("the freshly built state has no other owner");
        };
        state.test_mode = false;
        state.config.tenant_api_keys = keys.iter().map(ToString::to_string).collect();
        state.config.tenant_rate_limit_per_minute = limit;
        state.config.node.participant_id =
            CantonId::parse(&format!("participant-3::1220{}", "bb".repeat(32))).ok();
        // Nothing listens here, so a Canton call fails at once.
        state.config.canton.admin_api_host = "127.0.0.1".to_string();
        state.config.canton.admin_api_port = 1;
        web::Data::new(state)
    }

    async fn audit_rows(db: &SqlitePool) -> anyhow::Result<Vec<TenantCallAuditEntry>> {
        Ok(tenant_call_audit::list(db, None, 100)
            .await?
            .into_iter()
            .map(TenantCallAuditRow::into_entry)
            .collect())
    }

    /// One signature for two transactions, so the host refuses it with a 400.
    fn mismatched_add_hosts() -> TenantAddHostsExecuteRequest {
        TenantAddHostsExecuteRequest {
            party_id: PARTY.to_string(),
            base_serial: 4,
            topology_transactions: vec!["AQID".to_string(), "BAU=".to_string()],
            signatures: vec!["BAU=".to_string()],
            signed_by: "1220aa".to_string(),
        }
    }

    fn add_hosts_execute(key: &str) -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/execute")
            .insert_header(("Authorization", format!("Bearer {key}")))
            .set_json(mismatched_add_hosts())
    }

    fn import(party_id: &str, seq: u64, chunk: &str, end: bool) -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(json!({
                "party_id": party_id,
                "base_serial": 1,
                "seq": seq,
                "chunk": chunk,
                "end": end,
            }))
    }

    fn the_key() -> TenantKey {
        TenantKey {
            index: 0,
            id: tenant_key_id("key"),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_refused_add_hosts_execute_is_recorded_with_its_error(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .wrap(AuthMiddleware)
                .service(tenant_add_hosts_execute),
        )
        .await;

        let response = actix_test::call_service(&app, add_hosts_execute("key").to_request()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let content_type = response.headers().get("content-type").cloned();
        let body = actix_test::read_body(response).await;

        // The audit must hand the caller the handler's answer untouched.
        let direct = tenant_add_hosts_execute_on_this_host(&state, &mismatched_add_hosts()).await;
        assert_eq!(direct.headers().get("content-type").cloned(), content_type);
        let Ok(direct_body) = actix_web::body::to_bytes(direct.into_body()).await else {
            panic!("the handler's body must be readable");
        };
        assert_eq!(body, direct_body);

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row.endpoint, "/v0/tenant/add-hosts/execute");
        assert_eq!(row.party_id.as_deref(), Some(PARTY));
        assert_eq!(row.http_status, Some(400));
        assert_eq!(row.key_index, Some(0));
        let body: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(row.error.as_deref(), body["error"].as_str());
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains("one signature per topology transaction")),
            "{:?}",
            row.error
        );
        Ok(())
    }

    /// A 401 returns before the handler body runs, so it leaves no row.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unauthenticated_call_leaves_no_row(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_add_hosts_execute),
        )
        .await;

        let response =
            actix_test::call_service(&app, add_hosts_execute("not-a-key").to_request()).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(audit_rows(&db).await?.is_empty());
        Ok(())
    }

    /// The index changes when the key list is reordered, so the row also keeps
    /// the key's id, which does not. Neither is the key.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_key_is_named_by_index_and_id_and_not_stored(db: SqlitePool) -> anyhow::Result<()> {
        for keys in [["first", "second"], ["second", "first"]] {
            let app = actix_test::init_service(
                App::new()
                    .app_data(keyed_state(db.clone(), &keys, 0))
                    .wrap(AuthMiddleware)
                    .service(tenant_add_hosts_execute),
            )
            .await;
            actix_test::call_service(&app, add_hosts_execute("second").to_request()).await;
        }

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 2, "{rows:?}");
        // Newest first: the reordered list put the key at index 0.
        assert_eq!(rows[0].key_index, Some(0));
        assert_eq!(rows[1].key_index, Some(1));
        let id = tenant_key_id("second");
        assert_eq!(rows[0].key_id.as_deref(), Some(id.as_str()));
        assert_eq!(rows[1].key_id, rows[0].key_id);
        for row in &rows {
            let stored = serde_json::to_string(row)?;
            assert!(!stored.contains("second"), "{stored}");
        }
        Ok(())
    }

    /// Block 1 opens an import, so it is recorded. A final block that no open
    /// import takes is refused, and is recorded too. The blocks between only
    /// carry data.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_import_records_its_start_and_a_refused_end(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let party = valid_party();

        let data_block =
            actix_test::call_service(&app, import(&party, 2, "", false).to_request()).await;
        assert_eq!(data_block.status(), StatusCode::CONFLICT);
        assert!(audit_rows(&db).await?.is_empty());

        // A chunk that is not base64 stops block 1 before any Canton call.
        let start =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(start.status(), StatusCode::BAD_REQUEST);
        let end = actix_test::call_service(&app, import(&party, 3, "", true).to_request()).await;
        assert_eq!(end.status(), StatusCode::CONFLICT);

        let rows = audit_rows(&db).await?;
        let seen: Vec<(&str, Option<u16>)> = rows
            .iter()
            .map(|r| (r.endpoint.as_str(), r.http_status))
            .collect();
        assert_eq!(
            seen,
            [
                ("/v0/tenant/add-hosts/import (end)", Some(409)),
                ("/v0/tenant/add-hosts/import (start)", Some(400)),
            ]
        );
        assert!(
            rows.iter()
                .all(|r| r.party_id.as_deref() == Some(party.as_str()))
        );
        Ok(())
    }

    /// A block that no open copy takes spends the budget. Past it, the call
    /// gets a 429 and writes nothing.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_block_no_open_copy_takes_spends_the_budget(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 1))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        let first = actix_test::call_service(&app, import(PARTY, 0, "", true).to_request()).await;
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);
        let second = actix_test::call_service(&app, import(PARTY, 0, "", true).to_request()).await;
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        let stray =
            actix_test::call_service(&app, import(&valid_party(), 3, "", true).to_request()).await;
        assert_eq!(stray.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(audit_rows(&db).await?.len(), 1);
        Ok(())
    }

    /// A copy refused partway leaves the participant disconnected until the
    /// reaper ends it. So once block 1 is admitted, the final block must pass
    /// even when block 1 spent the last of the budget. The copy's teardown
    /// records how it ended.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_final_block_of_an_open_copy_spends_no_budget(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        // A real block 1 needs Canton to open the copy. A refused one spends
        // the budget the same way, and the test then opens the copy itself.
        let start =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(start.status(), StatusCode::BAD_REQUEST);
        let spent =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(spent.status(), StatusCode::TOO_MANY_REQUESTS);
        let instance = match replication_target(&party, state.config.participant_id(), 1) {
            Ok(target) => target.instance_name,
            Err(e) => panic!("the test party must name a replication: {e}"),
        };
        state
            .relay_sessions
            .open_test_import(
                &instance,
                Some(record_import_end(&state, Some(the_key()), &party)),
                Ok(()),
            )
            .await;

        let end = actix_test::call_service(&app, import(&party, 2, "", true).to_request()).await;

        assert_ne!(end.status(), StatusCode::TOO_MANY_REQUESTS);
        // The import finished. Without Canton the marker clear after it fails.
        let body: serde_json::Value = actix_test::read_body_json(end).await;
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.starts_with("Imported the ACS")),
            "{body}"
        );
        let rows = audit_rows(&db).await?;
        let seen: Vec<(&str, Option<u16>)> = rows
            .iter()
            .map(|r| (r.endpoint.as_str(), r.http_status))
            .collect();
        assert_eq!(
            seen,
            [
                ("/v0/tenant/add-hosts/import (completed)", None),
                ("/v0/tenant/add-hosts/import (start)", Some(400)),
            ]
        );
        Ok(())
    }

    /// A block of an open copy that is not the next one is refused, so it is
    /// not part of the copy. It spends budget, and a refused final block gets
    /// its own row.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_wrong_block_of_an_open_copy_spends_budget_and_records_its_end(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        let wrong_end =
            actix_test::call_service(&app, import(&party, 5, "", true).to_request()).await;
        assert_eq!(wrong_end.status(), StatusCode::CONFLICT);
        let wrong_data =
            actix_test::call_service(&app, import(&party, 6, "", false).to_request()).await;
        assert_eq!(wrong_data.status(), StatusCode::TOO_MANY_REQUESTS);

        let rows = audit_rows(&db).await?;
        let seen: Vec<(&str, Option<u16>)> = rows
            .iter()
            .map(|r| (r.endpoint.as_str(), r.http_status))
            .collect();
        assert_eq!(seen, [("/v0/tenant/add-hosts/import (end)", Some(409))]);
        Ok(())
    }

    /// The next block of an open copy passes with the budget spent, so a copy
    /// is never refused partway.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_next_block_of_an_open_copy_passes_with_the_budget_spent(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let chunk = STANDARD.encode(b"block");

        let wrong =
            actix_test::call_service(&app, import(&party, 7, &chunk, false).to_request()).await;
        assert_eq!(wrong.status(), StatusCode::CONFLICT);
        let spent =
            actix_test::call_service(&app, import(&party, 7, &chunk, false).to_request()).await;
        assert_eq!(spent.status(), StatusCode::TOO_MANY_REQUESTS);

        for seq in 2..=3 {
            let next =
                actix_test::call_service(&app, import(&party, seq, &chunk, false).to_request())
                    .await;
            assert_eq!(next.status(), StatusCode::OK, "block {seq}");
        }
        Ok(())
    }

    /// Block 1 spends budget before its chunk is checked. With the budget
    /// spent, a block 1 with a bad chunk gets a 429 and writes no row. Whether
    /// the chunk was decoded is not visible from outside, so this test does not
    /// check it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_spent_key_gets_429_for_block_1_and_no_row(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 1))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let party = valid_party();

        let first =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            audit_rows(&db).await?.len(),
            1,
            "block 1 gets its (start) row"
        );

        let spent =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(spent.status(), StatusCode::TOO_MANY_REQUESTS);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "a 429 writes no row: {rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/add-hosts/import (start)");
        Ok(())
    }

    /// A next block that fails the checks cannot be imported, so it is not
    /// part of the copy: it spends budget, and a failed final block gets its
    /// own row. A valid next block still passes with the budget spent.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_next_block_that_fails_its_checks_spends_budget(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        let bad_end =
            actix_test::call_service(&app, import(&party, 2, "not base64!", true).to_request())
                .await;
        assert_eq!(bad_end.status(), StatusCode::BAD_REQUEST);
        let bad_data =
            actix_test::call_service(&app, import(&party, 2, "not base64!", false).to_request())
                .await;
        assert_eq!(bad_data.status(), StatusCode::TOO_MANY_REQUESTS);
        let good = actix_test::call_service(
            &app,
            import(&party, 2, &STANDARD.encode(b"block"), false).to_request(),
        )
        .await;
        assert_eq!(good.status(), StatusCode::OK);

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/add-hosts/import (end)");
        assert_eq!(rows[0].http_status, Some(400));
        assert!(
            rows[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("not valid base64")),
            "{:?}",
            rows[0].error
        );
        Ok(())
    }

    /// The write runs in its own task. When it panics after its caller went
    /// away, the task still writes its row, once.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_write_that_panics_after_its_caller_went_away_is_recorded_once(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);
        let (finish, finished) = oneshot::channel::<()>();
        let call = audited_write(
            &state,
            Some(the_key()),
            TenantEndpoint::ThresholdExecute,
            PARTY.to_string(),
            move |_| async move {
                let _ = finished.await;
                a_broken_write()
            },
        );

        let gone = tokio::time::timeout(Duration::from_millis(50), call).await;
        assert!(
            gone.is_err(),
            "the write must still run when the caller goes"
        );
        let _ = finish.send(());

        for _ in 0..200 {
            if !audit_rows(&db).await?.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // A second row would come at once, from the same task.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].http_status, Some(500));
        Ok(())
    }

    /// A client that goes away drops the handler. The write runs on in its own
    /// task, so its row is still written.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_write_whose_caller_went_away_is_still_recorded(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);
        let (finish, finished) = oneshot::channel::<()>();
        let call = audited_write(
            &state,
            Some(the_key()),
            TenantEndpoint::AddHostsExecute,
            PARTY.to_string(),
            move |_| async move {
                // Stands for a Canton write that is still running.
                let _ = finished.await;
                HttpResponse::Conflict().json(ErrorResponse {
                    error: "the pinned serial moved".to_string(),
                })
            },
        );

        let gone = tokio::time::timeout(Duration::from_millis(50), call).await;
        assert!(
            gone.is_err(),
            "the write must still run when the caller goes"
        );
        assert!(audit_rows(&db).await?.is_empty());
        let _ = finish.send(());

        let mut rows = Vec::new();
        for _ in 0..200 {
            rows = audit_rows(&db).await?;
            if !rows.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/add-hosts/execute");
        assert_eq!(rows[0].http_status, Some(409));
        assert_eq!(rows[0].error.as_deref(), Some("the pinned serial moved"));
        assert_eq!(rows[0].key_id, Some(the_key().id));
        Ok(())
    }

    fn a_broken_write() -> HttpResponse {
        panic!("the write broke")
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_write_that_panics_answers_500_and_is_recorded(db: SqlitePool) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);

        let response = audited_write(
            &state,
            Some(the_key()),
            TenantEndpoint::ThresholdExecute,
            PARTY.to_string(),
            move |_| async move { a_broken_write() },
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/threshold/execute");
        assert_eq!(rows[0].http_status, Some(500));
        assert!(
            rows[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("failed on this host")),
            "{:?}",
            rows[0].error
        );
        Ok(())
    }

    /// The JSON limit is 100 MiB, and the caller chooses the party id.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_over_long_party_id_is_stored_capped(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let party = "p".repeat(100_000);

        let response =
            actix_test::call_service(&app, import(&party, 0, "", true).to_request()).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let Some(stored) = rows[0].party_id.as_deref() else {
            panic!("the row must keep the capped party id");
        };
        assert!(stored.starts_with(&"p".repeat(PARTY_ID_CAP)), "{stored}");
        assert!(stored.ends_with("[truncated]"), "{stored}");
        assert!(stored.chars().count() < PARTY_ID_CAP + 20, "{stored}");
        Ok(())
    }

    /// The end of an import that block 1 opened is recorded against the key
    /// that opened it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_end_of_an_import_is_recorded_against_its_key(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);

        record_import_end(&state, Some(the_key()), PARTY)(ImportEnd::Completed).await;
        record_import_end(&state, Some(the_key()), PARTY)(ImportEnd::Abandoned(
            "the reaper ended the idle copy: the wallet stopped relaying".to_string(),
        ))
        .await;

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 2, "{rows:?}");
        let (abandoned, completed) = (&rows[0], &rows[1]);
        assert_eq!(
            abandoned.endpoint,
            "/v0/tenant/add-hosts/import (abandoned)"
        );
        assert!(
            abandoned
                .error
                .as_deref()
                .is_some_and(|e| e.contains("reaper")),
            "{:?}",
            abandoned.error
        );
        assert_eq!(
            completed.endpoint,
            "/v0/tenant/add-hosts/import (completed)"
        );
        assert_eq!(completed.error, None);
        for row in &rows {
            assert_eq!(row.party_id.as_deref(), Some(PARTY));
            assert_eq!(row.http_status, None, "no call answers an import's end");
            assert_eq!(row.key_index, Some(0));
            assert_eq!(row.key_id, Some(the_key().id));
        }
        Ok(())
    }

    /// An execute names no party id. The row holds the one the call would
    /// create, so the audit can be read by party.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn execute_records_the_party_the_call_named(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_execute),
        )
        .await;
        // `signed_by` is not the fingerprint of the key, so the host refuses it.
        let request = actix_test::TestRequest::post()
            .uri("/v0/tenant/execute")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(json!({
                "party_hint": "alice",
                "public_key": STANDARD.encode([7u8; 32]),
                "topology_transactions": [],
                "signatures": [],
                "signed_by": "1220ff",
            }));

        let response = actix_test::call_service(&app, request.to_request()).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/execute");
        assert_eq!(rows[0].party_id.as_deref(), Some("alice::1220ff"));
        Ok(())
    }

    /// The Canton write may already have happened, so a broken audit must not
    /// change what the caller sees.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_audit_write_does_not_fail_the_call(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_add_hosts_execute),
        )
        .await;
        let recorded = actix_test::call_service(&app, add_hosts_execute("key").to_request()).await;
        assert_eq!(recorded.status(), StatusCode::BAD_REQUEST);
        let recorded = actix_test::read_body(recorded).await;

        sqlx::query("DROP TABLE tenant_call_audit")
            .execute(&db)
            .await?;
        let unrecorded =
            actix_test::call_service(&app, add_hosts_execute("key").to_request()).await;

        assert_eq!(unrecorded.status(), StatusCode::BAD_REQUEST);
        assert_eq!(actix_test::read_body(unrecorded).await, recorded);
        Ok(())
    }
}
