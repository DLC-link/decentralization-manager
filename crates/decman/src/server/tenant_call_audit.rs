//! The `tenant_call_audit` table: one row per tenant API call that writes
//! topology or state on this node.
//!
//! The tenant handlers insert the row after the call has its answer, so the row
//! holds the status the caller saw. A row names the key by its index and never
//! holds the key, a signature or topology bytes.

use common::api::TenantCallAuditEntry;
use sqlx::SqlitePool;

use crate::error::Result;

/// The tenant calls that write. The final block of an ACS import is the one
/// that finishes the import and clears the onboarding marker, so only that
/// block counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TenantEndpoint {
    Execute,
    AddHostsAuthorize,
    AddHostsExecute,
    AcsImportComplete,
    ThresholdAuthorize,
    ThresholdExecute,
    LocalPartyAdoptExecute,
}

impl TenantEndpoint {
    /// The route, as the row stores it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "/v0/tenant/execute",
            Self::AddHostsAuthorize => "/v0/tenant/add-hosts/authorize",
            Self::AddHostsExecute => "/v0/tenant/add-hosts/execute",
            Self::AcsImportComplete => "/v0/tenant/add-hosts/import",
            Self::ThresholdAuthorize => "/v0/tenant/threshold/authorize",
            Self::ThresholdExecute => "/v0/tenant/threshold/execute",
            Self::LocalPartyAdoptExecute => "/v0/tenant/local-party/adopt-key/execute",
        }
    }
}

/// What a new row holds.
pub struct NewTenantCall<'a> {
    /// The index of the key in `DECPM_TENANT_API_KEYS`. `None` in insecure
    /// mode, where no key is checked.
    pub key_index: Option<usize>,
    pub endpoint: TenantEndpoint,
    pub party_id: Option<&'a str>,
    pub http_status: u16,
    pub error: Option<&'a str>,
}

/// One row, as stored.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct TenantCallAuditRow {
    pub id: i64,
    pub called_at: i64,
    pub key_index: Option<i64>,
    pub endpoint: String,
    pub party_id: Option<String>,
    pub http_status: i64,
    pub error: Option<String>,
}

impl TenantCallAuditRow {
    /// The wire shape.
    pub fn into_entry(self) -> TenantCallAuditEntry {
        TenantCallAuditEntry {
            id: self.id,
            called_at: self.called_at,
            key_index: self.key_index.and_then(|i| u32::try_from(i).ok()),
            endpoint: self.endpoint,
            party_id: self.party_id,
            http_status: u16::try_from(self.http_status).unwrap_or_default(),
            error: self.error,
        }
    }
}

/// Insert the row for one call and return its id.
///
/// # Errors
/// Returns an error if the insert fails.
pub async fn insert_call(db: &SqlitePool, call: &NewTenantCall<'_>) -> Result<i64> {
    let id = sqlx::query(
        "INSERT INTO tenant_call_audit \
         (called_at, key_index, endpoint, party_id, http_status, error) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(chrono::Utc::now().timestamp())
    .bind(call.key_index.and_then(|i| i64::try_from(i).ok()))
    .bind(call.endpoint.as_str())
    .bind(call.party_id)
    .bind(i64::from(call.http_status))
    .bind(call.error)
    .execute(db)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// The newest `limit` calls, for one party or for all, newest first.
///
/// # Errors
/// Returns an error if the read fails.
pub async fn list(
    db: &SqlitePool,
    party_id: Option<&str>,
    limit: i64,
) -> Result<Vec<TenantCallAuditRow>> {
    let rows = match party_id {
        Some(party_id) => {
            sqlx::query_as::<_, TenantCallAuditRow>(
                "SELECT * FROM tenant_call_audit WHERE party_id = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(party_id)
            .bind(limit)
            .fetch_all(db)
            .await?
        }
        None => {
            sqlx::query_as::<_, TenantCallAuditRow>(
                "SELECT * FROM tenant_call_audit ORDER BY id DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(db)
            .await?
        }
    };
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MIGRATOR;

    const PARTY: &str = "alice::1220aa";

    fn call(endpoint: TenantEndpoint) -> NewTenantCall<'static> {
        NewTenantCall {
            key_index: Some(2),
            endpoint,
            party_id: Some(PARTY),
            http_status: 202,
            error: None,
        }
    }

    async fn only_entry(db: &SqlitePool) -> Result<TenantCallAuditEntry> {
        let mut rows = list(db, None, 10).await?;
        assert_eq!(rows.len(), 1, "expected exactly one row");
        Ok(rows.remove(0).into_entry())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_call_keeps_every_field(db: SqlitePool) -> Result {
        let before = chrono::Utc::now().timestamp();
        let id = insert_call(&db, &call(TenantEndpoint::ThresholdExecute)).await?;

        let entry = only_entry(&db).await?;
        assert_eq!(entry.id, id);
        assert!(entry.called_at >= before, "{}", entry.called_at);
        assert_eq!(entry.key_index, Some(2));
        assert_eq!(entry.endpoint, "/v0/tenant/threshold/execute");
        assert_eq!(entry.party_id.as_deref(), Some(PARTY));
        assert_eq!(entry.http_status, 202);
        assert_eq!(entry.error, None);
        Ok(())
    }

    /// Insecure mode checks no key, so the row has no key index.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_nullable_fields_stay_empty(db: SqlitePool) -> Result {
        insert_call(
            &db,
            &NewTenantCall {
                key_index: None,
                party_id: None,
                ..call(TenantEndpoint::Execute)
            },
        )
        .await?;

        let entry = only_entry(&db).await?;
        assert_eq!(entry.key_index, None);
        assert_eq!(entry.party_id, None);
        assert_eq!(entry.error, None);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_refused_call_keeps_its_error(db: SqlitePool) -> Result {
        insert_call(
            &db,
            &NewTenantCall {
                http_status: 409,
                error: Some("the pinned serial 4 has moved to 5"),
                ..call(TenantEndpoint::AddHostsExecute)
            },
        )
        .await?;

        let entry = only_entry(&db).await?;
        assert_eq!(entry.http_status, 409);
        assert_eq!(
            entry.error.as_deref(),
            Some("the pinned serial 4 has moved to 5")
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_audit_lists_newest_first_and_filters_by_party(db: SqlitePool) -> Result {
        let first = insert_call(&db, &call(TenantEndpoint::AddHostsAuthorize)).await?;
        let other = insert_call(
            &db,
            &NewTenantCall {
                party_id: Some("bob::1220dd"),
                ..call(TenantEndpoint::Execute)
            },
        )
        .await?;
        let last = insert_call(&db, &call(TenantEndpoint::AcsImportComplete)).await?;

        let ids =
            |rows: Vec<TenantCallAuditRow>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(list(&db, None, 10).await?), vec![last, other, first]);
        assert_eq!(ids(list(&db, Some(PARTY), 10).await?), vec![last, first]);
        assert!(list(&db, Some("carol::1220ee"), 10).await?.is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_limit_bounds_the_read(db: SqlitePool) -> Result {
        for _ in 0..3 {
            insert_call(&db, &call(TenantEndpoint::Execute)).await?;
        }
        let last = insert_call(&db, &call(TenantEndpoint::Execute)).await?;

        let rows = list(&db, None, 2).await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, last);
        assert_eq!(list(&db, Some(PARTY), 1).await?.len(), 1);
        Ok(())
    }
}
