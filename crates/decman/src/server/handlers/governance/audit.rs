//! Governance audit endpoints: the node audit log and the chain audit.

use actix_web::{HttpResponse, Responder, get, web};

use crate::{
    canton_id::CantonId,
    db::{rows::ChainAuditCacheRow, schema::SchemaRead},
    server::{
        AppState, chain_audit,
        types::{
            AuditLogEntry, AuditLogQuery, AuditLogResponse, AuditScope, ChainAuditEntry,
            ChainAuditQuery, ChainAuditResponse, ErrorResponse, MAX_RECENT_AUDIT_LIMIT,
            chain_audit_entry_from_row,
        },
    },
};

use super::credentials::{get_party_token, packages};

/// Get paginated governance audit trail
///
/// Scoped to `party_id` when given. Without it, returns this node's newest
/// entries across every party, which is how the notification feed fills its
/// Completed section without one ledger read per party.
#[utoipa::path(
    tag = "Governance",
    params(AuditLogQuery),
    responses(
        (status = 200, description = "Governance audit entries", body = AuditLogResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/audit")]
pub async fn get_governance_audit(
    data: web::Data<AppState>,
    query: web::Query<AuditLogQuery>,
) -> impl Responder {
    let rows = match &query.party_id {
        Some(party_id) => {
            data.db
                .get_governance_audit(party_id, query.limit, query.offset)
                .await
        }
        None => {
            data.db
                .get_recent_governance_audit(
                    query.limit.clamp(1, MAX_RECENT_AUDIT_LIMIT),
                    query.offset.max(0),
                )
                .await
        }
    };
    match rows {
        Ok(rows) => {
            let entries: Vec<AuditLogEntry> = rows
                .into_iter()
                .filter_map(|row| {
                    // Skip entries with malformed party ids — they're DB-level
                    // corruption, not something the API should propagate.
                    let party_id = CantonId::parse(&row.party_id)
                        .map_err(|e| {
                            tracing::warn!(
                                "Skipping audit row {id}: bad party_id '{p}': {e}",
                                id = row.id,
                                p = row.party_id
                            );
                        })
                        .ok()?;
                    let member_party_id = CantonId::parse(&row.member_party_id)
                        .map_err(|e| {
                            tracing::warn!(
                                "Skipping audit row {id}: bad member_party_id '{m}': {e}",
                                id = row.id,
                                m = row.member_party_id
                            );
                        })
                        .ok()?;
                    Some(AuditLogEntry {
                        id: row.id,
                        timestamp: row.timestamp,
                        event_type: row.event_type,
                        party_id,
                        member_party_id,
                        governance_type: row.governance_type,
                        action_summary: row.action_summary,
                        details: serde_json::from_str(&row.details)
                            .unwrap_or(serde_json::Value::String(row.details)),
                        status: row.status,
                        error_message: row.error_message,
                        created_at: row.created_at,
                    })
                })
                .collect();
            let total_returned = entries.len();
            HttpResponse::Ok().json(AuditLogResponse {
                entries,
                total_returned,
            })
        }
        Err(e) => {
            tracing::error!("Failed to fetch governance audit: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch governance audit: {e}"),
            })
        }
    }
}

/// Wrap a page of audit entries, deriving the cursor for the next (older) page.
///
/// `has_more` is passed in rather than inferred from the row count: a page can
/// hold more than `limit` rows (it is extended to the end of an offset group),
/// so counting rows would hand out a cursor to a page that turns out empty.
fn chain_audit_response(entries: Vec<ChainAuditEntry>, has_more: bool) -> ChainAuditResponse {
    let total_returned = entries.len();
    let next_before_offset = has_more.then(|| entries.last().map(|e| e.offset)).flatten();

    ChainAuditResponse {
        entries,
        total_returned,
        next_before_offset,
    }
}

/// The cached answer for a page request, or `None` when the cache cannot answer
/// it and the read has to reach Canton.
///
/// An empty result is a miss rather than an answer: the cache only holds the
/// pages fetched so far, so paging past its tail (or a cold start) must not be
/// reported as an exhausted trail.
fn cached_chain_audit_page(
    rows: Vec<ChainAuditCacheRow>,
    limit: usize,
) -> Option<ChainAuditResponse> {
    if rows.is_empty() {
        return None;
    }

    // The cache cannot prove the trail is exhausted either, so a full page is
    // reported as "more" and the next request falls through to Canton — better
    // a spare empty page than hiding entries that do exist.
    let has_more = rows.len() >= limit;
    let entries: Vec<ChainAuditEntry> = rows.into_iter().map(chain_audit_entry_from_row).collect();

    Some(chain_audit_response(entries, has_more))
}

/// Get on-chain audit entries for a party.
///
/// Defaults to the governance scope, served from cache; pass `refresh=true` to
/// fetch from Canton and update the cache. `scope=all` returns every ledger
/// event the party witnesses and is always read live — the cache holds
/// governance-scoped pages, and mixing the two under one party key would let
/// an `all` read answer a governance request.
#[utoipa::path(
    tag = "Governance",
    params(ChainAuditQuery),
    responses(
        (status = 200, description = "On-chain audit entries for the requested scope", body = ChainAuditResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/chain-audit")]
pub async fn get_governance_chain_audit(
    data: web::Data<AppState>,
    query: web::Query<ChainAuditQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    // `limit` is caller-supplied, so it is bounded here rather than trusted:
    // one page must not be able to ask for the whole retained ledger.
    let limit = query.clamped_limit();

    // A zero-row page needs no ledger reads.
    if limit == 0 {
        return HttpResponse::Ok().json(chain_audit_response(Vec::new(), false));
    }

    let cacheable = query.scope == AuditScope::Governance;

    if !query.refresh && cacheable {
        match data
            .db
            .get_chain_audit_cache(party_id, limit as i64, query.before_offset)
            .await
        {
            Ok(rows) => {
                if let Some(response) = cached_chain_audit_page(rows, limit) {
                    return HttpResponse::Ok().json(response);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to read chain audit cache: {e}");
                // Fall through to live query
            }
        }
    }

    // Fetch from Canton
    let token = get_party_token(&data, party_id).await;
    let pkgs = packages();

    match chain_audit::get_chain_audit(
        &data.config,
        party_id,
        token,
        &pkgs,
        query.scope,
        limit,
        query.before_offset,
    )
    .await
    {
        Ok(page) => {
            if cacheable {
                // Save to cache in background
                let pool = data.db.clone();
                let pid = party_id.clone();
                let cached = page.entries.clone();
                tokio::spawn(async move {
                    chain_audit::save_chain_audit_cache(&pool, &pid, &cached).await;
                });
            }

            HttpResponse::Ok().json(chain_audit_response(page.entries, page.has_more))
        }
        Err(e) => {
            tracing::error!("Failed to fetch chain audit for {party_id}: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to query chain audit: {e}"),
            })
        }
    }
}

#[cfg(test)]
mod governance_audit_endpoint_tests {
    use actix_web::{App, http::StatusCode, test as actix_test};
    use serde_json::Value;
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::MIGRATOR;
    use crate::error::Result;

    fn party(prefix: &str) -> Result<CantonId> {
        CantonId::parse(&format!("{prefix}::1220{}", "ab".repeat(32)))
    }

    async fn log_row(
        pool: &SqlitePool,
        party_id: &CantonId,
        event_type: &str,
        created_at: i64,
    ) -> Result {
        sqlx::query(
            r"
            INSERT INTO governance_audit (
                timestamp, event_type, party_id, member_party_id,
                governance_type, action_summary, details, status,
                error_message, created_at
            ) VALUES (?, ?, ?, ?, 'core_domain', 'transfer', '{}', 'success', NULL, ?)
            ",
        )
        .bind(created_at)
        .bind(event_type)
        .bind(party_id.to_string())
        .bind(party("member")?.to_string())
        .bind(created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// `(party_id, created_at)` of every entry the endpoint returns for `uri`.
    async fn fetch(pool: &SqlitePool, uri: &str) -> Result<Vec<(String, i64)>> {
        let app = actix_test::init_service(
            App::new()
                .app_data(AppState::for_test_on(None, pool.clone()))
                .service(get_governance_audit),
        )
        .await;
        let response =
            actix_test::call_service(&app, actix_test::TestRequest::get().uri(uri).to_request())
                .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = actix_test::read_body_json(response).await;
        let entries = body["entries"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("no entries array in {body}"))?;
        assert_eq!(body["total_returned"].as_u64(), Some(entries.len() as u64));
        entries
            .iter()
            .map(|e| {
                let party_id = e["party_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("entry without party_id: {e}"))?;
                let created_at = e["created_at"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("entry without created_at: {e}"))?;
                Ok((party_id.to_string(), created_at))
            })
            .collect()
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn without_a_party_it_lists_every_party_newest_first(pool: SqlitePool) -> Result {
        let (a, b) = (party("party-a")?, party("party-b")?);
        log_row(&pool, &a, "propose", 1000).await?;
        log_row(&pool, &b, "confirm", 2000).await?;
        log_row(&pool, &a, "execute", 3000).await?;

        let entries = fetch(&pool, "/governance/audit").await?;

        assert_eq!(
            entries,
            vec![
                (a.to_string(), 3000),
                (b.to_string(), 2000),
                (a.to_string(), 1000),
            ]
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn with_a_party_it_lists_only_that_party(pool: SqlitePool) -> Result {
        let (a, b) = (party("party-a")?, party("party-b")?);
        log_row(&pool, &a, "propose", 1000).await?;
        log_row(&pool, &b, "confirm", 2000).await?;
        log_row(&pool, &a, "execute", 3000).await?;

        let uri = format!("/governance/audit?party_id={a}&limit=1&offset=1");
        let entries = fetch(&pool, &uri).await?;

        assert_eq!(entries, vec![(a.to_string(), 1000)]);
        Ok(())
    }

    /// SQLite reads a negative LIMIT as "no limit", so an unscoped read must
    /// clamp the value rather than pass it through.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn without_a_party_the_limit_is_bounded(pool: SqlitePool) -> Result {
        let a = party("party-a")?;
        for created_at in 0..=MAX_RECENT_AUDIT_LIMIT {
            log_row(&pool, &a, "confirm", created_at).await?;
        }

        let huge = fetch(&pool, "/governance/audit?limit=100000").await?;
        assert_eq!(huge.len() as i64, MAX_RECENT_AUDIT_LIMIT);
        assert_eq!(huge[0].1, MAX_RECENT_AUDIT_LIMIT);

        let negative = fetch(&pool, "/governance/audit?limit=-1&offset=-5").await?;
        assert_eq!(negative, vec![(a.to_string(), MAX_RECENT_AUDIT_LIMIT)]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_at(offset: i64) -> ChainAuditEntry {
        ChainAuditEntry {
            offset,
            timestamp: 0,
            event_type: "propose".to_string(),
            contract_id: String::new(),
            template_id: String::new(),
            package_id: String::new(),
            governance_type: "core_domain".to_string(),
            action_summary: String::new(),
            choice: None,
            acting_parties: Vec::new(),
            update_id: String::new(),
            details: serde_json::Value::Null,
        }
    }

    fn cache_row_at(offset: i64) -> ChainAuditCacheRow {
        ChainAuditCacheRow {
            party_id: "dec::1220ff".to_string(),
            offset,
            timestamp: 0,
            event_type: "propose".to_string(),
            contract_id: String::new(),
            template_id: String::new(),
            package_id: String::new(),
            governance_type: "core_domain".to_string(),
            action_summary: String::new(),
            choice: None,
            acting_parties: "[]".to_string(),
            update_id: String::new(),
            details: "null".to_string(),
        }
    }

    /// The cursor is the oldest offset on the page, since the next request asks
    /// for `offset < cursor`.
    #[test]
    fn cursor_is_the_oldest_offset_when_more_exist() {
        let response = chain_audit_response(vec![entry_at(30), entry_at(20), entry_at(10)], true);
        assert_eq!(response.next_before_offset, Some(10));
        assert_eq!(response.total_returned, 3);
    }

    #[test]
    fn no_cursor_when_nothing_older_exists() {
        let response = chain_audit_response(vec![entry_at(30), entry_at(20)], false);
        assert_eq!(response.next_before_offset, None);
        assert_eq!(response.total_returned, 2);
    }

    /// A `has_more` with no rows to derive a cursor from must not hand out one:
    /// there is no offset to page before, and inventing one would skip entries.
    #[test]
    fn no_cursor_from_an_empty_page() {
        let response = chain_audit_response(Vec::new(), true);
        assert_eq!(response.next_before_offset, None);
        assert_eq!(response.total_returned, 0);
    }

    /// An empty cache read is a miss, not "the trail ends here" — the caller
    /// has to fall through to Canton.
    #[test]
    fn empty_cache_is_a_miss() {
        assert!(cached_chain_audit_page(Vec::new(), 2).is_none());
    }

    /// A cache page that fills `limit` is reported as having more, so the next
    /// request reaches Canton rather than stopping at the cache's tail.
    #[test]
    fn full_cache_page_reports_more() {
        let response = cached_chain_audit_page(vec![cache_row_at(30), cache_row_at(20)], 2)
            .expect("non-empty rows are a hit");
        assert_eq!(response.next_before_offset, Some(20));
    }

    #[test]
    fn short_cache_page_reports_no_more() {
        let response =
            cached_chain_audit_page(vec![cache_row_at(30)], 2).expect("non-empty rows are a hit");
        assert_eq!(response.next_before_offset, None);
        assert_eq!(response.total_returned, 1);
    }

    /// An over-`limit` cache page (a whole offset group kept together) still
    /// reports more, and its cursor is the oldest offset returned.
    #[test]
    fn oversized_cache_page_reports_more() {
        let rows = vec![cache_row_at(30), cache_row_at(20), cache_row_at(20)];
        let response = cached_chain_audit_page(rows, 2).expect("non-empty rows are a hit");
        assert_eq!(response.total_returned, 3);
        assert_eq!(response.next_before_offset, Some(20));
    }
}
