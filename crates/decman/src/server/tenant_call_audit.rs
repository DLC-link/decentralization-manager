//! The `tenant_call_audit` table: one row per tenant API call that writes
//! topology or state on this node, and one when an ACS import ends.
//!
//! The tenant handlers insert a call's row after the call has its answer, so
//! the row holds the status the caller saw. A row names the key by its index
//! and its id, and never holds the key, a signature or topology bytes. The
//! text columns are capped, because a caller chooses the party id it sends.

use common::api::TenantCallAuditEntry;
use sqlx::SqlitePool;

use crate::error::Result;

/// The longest party id a row keeps. A valid party id is at most 255
/// characters (a 185-character hint, `::` and a 68-character fingerprint), so
/// the cap only ever shortens an invalid one.
pub const PARTY_ID_CAP: usize = 256;
/// The longest error text a row keeps.
pub const ERROR_CAP: usize = 1024;
/// Ends a text that was cut to its cap.
const TRUNCATED: &str = "…[truncated]";

/// What a row records: a tenant call that writes, or a step in the life of an
/// ACS import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TenantEndpoint {
    Execute,
    AddHostsAuthorize,
    AddHostsExecute,
    /// Block 1, which opens an import and disconnects the participant, or
    /// block 0, which does not exist.
    AcsImportStart,
    /// A final block that no open import took, so it was refused.
    AcsImportEnd,
    /// The import took its final block and finished. Its teardown writes the
    /// row, so the row has no HTTP status.
    AcsImportCompleted,
    /// The import ended without finishing. Its teardown writes the row, with
    /// the reason and no HTTP status.
    AcsImportAbandoned,
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
            Self::AcsImportStart => "/v0/tenant/add-hosts/import (start)",
            Self::AcsImportEnd => "/v0/tenant/add-hosts/import (end)",
            Self::AcsImportCompleted => "/v0/tenant/add-hosts/import (completed)",
            Self::AcsImportAbandoned => "/v0/tenant/add-hosts/import (abandoned)",
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
    /// The key's id, which survives a reordered key list. `None` in insecure
    /// mode.
    pub key_id: Option<&'a str>,
    pub endpoint: TenantEndpoint,
    pub party_id: Option<&'a str>,
    /// `None` for an import's end, which no call answers.
    pub http_status: Option<u16>,
    pub error: Option<&'a str>,
}

/// One row, as stored.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct TenantCallAuditRow {
    pub id: i64,
    pub called_at: i64,
    pub key_index: Option<i64>,
    pub key_id: Option<String>,
    pub endpoint: String,
    pub party_id: Option<String>,
    pub http_status: Option<i64>,
    pub error: Option<String>,
}

impl TenantCallAuditRow {
    /// The wire shape.
    pub fn into_entry(self) -> TenantCallAuditEntry {
        TenantCallAuditEntry {
            id: self.id,
            called_at: self.called_at,
            key_index: self.key_index.and_then(|i| u32::try_from(i).ok()),
            key_id: self.key_id,
            endpoint: self.endpoint,
            party_id: self.party_id,
            http_status: self.http_status.and_then(|s| u16::try_from(s).ok()),
            error: self.error,
        }
    }
}

/// `text` cut to `cap` characters, with [`TRUNCATED`] at the end when it was
/// cut.
fn capped(text: &str, cap: usize) -> std::borrow::Cow<'_, str> {
    match text.char_indices().nth(cap) {
        Some((end, _)) => format!("{}{TRUNCATED}", &text[..end]).into(),
        None => text.into(),
    }
}

/// Insert the row for one call and return its id. The party id and the error
/// text are capped at [`PARTY_ID_CAP`] and [`ERROR_CAP`] characters.
///
/// # Errors
/// Returns an error if the insert fails.
pub async fn insert_call(db: &SqlitePool, call: &NewTenantCall<'_>) -> Result<i64> {
    let id = sqlx::query(
        "INSERT INTO tenant_call_audit \
         (called_at, key_index, key_id, endpoint, party_id, http_status, error) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(chrono::Utc::now().timestamp())
    .bind(call.key_index.and_then(|i| i64::try_from(i).ok()))
    .bind(call.key_id)
    .bind(call.endpoint.as_str())
    .bind(call.party_id.map(|p| capped(p, PARTY_ID_CAP)))
    .bind(call.http_status.map(i64::from))
    .bind(call.error.map(|e| capped(e, ERROR_CAP)))
    .execute(db)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// The newest `limit` calls, for one party or for all, newest first. The party
/// filter is capped like the stored party id, so it finds a capped row.
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
            .bind(capped(party_id, PARTY_ID_CAP))
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
            key_id: Some("0123456789abcdef"),
            endpoint,
            party_id: Some(PARTY),
            http_status: Some(202),
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
        assert_eq!(entry.key_id.as_deref(), Some("0123456789abcdef"));
        assert_eq!(entry.endpoint, "/v0/tenant/threshold/execute");
        assert_eq!(entry.party_id.as_deref(), Some(PARTY));
        assert_eq!(entry.http_status, Some(202));
        assert_eq!(entry.error, None);
        Ok(())
    }

    /// Insecure mode checks no key, so the row has no key. An import the node
    /// ended on its own has no status.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_nullable_fields_stay_empty(db: SqlitePool) -> Result {
        insert_call(
            &db,
            &NewTenantCall {
                key_index: None,
                key_id: None,
                party_id: None,
                http_status: None,
                ..call(TenantEndpoint::AcsImportAbandoned)
            },
        )
        .await?;

        let entry = only_entry(&db).await?;
        assert_eq!(entry.key_index, None);
        assert_eq!(entry.key_id, None);
        assert_eq!(entry.party_id, None);
        assert_eq!(entry.http_status, None);
        assert_eq!(entry.error, None);
        Ok(())
    }

    /// A caller chooses the party id it sends, and the JSON limit is 100 MiB.
    /// A row must not grow with it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn long_text_is_capped_and_marked(db: SqlitePool) -> Result {
        let party = "p".repeat(10_000);
        let error = "e".repeat(10_000);
        insert_call(
            &db,
            &NewTenantCall {
                party_id: Some(&party),
                error: Some(&error),
                ..call(TenantEndpoint::AcsImportEnd)
            },
        )
        .await?;

        let entry = only_entry(&db).await?;
        let Some(stored_party) = entry.party_id else {
            panic!("the party id must be kept, capped");
        };
        assert_eq!(
            stored_party,
            format!("{}{TRUNCATED}", "p".repeat(PARTY_ID_CAP))
        );
        let Some(stored_error) = entry.error else {
            panic!("the error must be kept, capped");
        };
        assert_eq!(
            stored_error,
            format!("{}{TRUNCATED}", "e".repeat(ERROR_CAP))
        );
        // The filter is capped the same way, so the row can still be found.
        assert_eq!(list(&db, Some(&party), 10).await?.len(), 1);
        Ok(())
    }

    #[test]
    fn text_at_the_cap_is_kept_whole() {
        let longest_valid_party = format!("{}::1220{}", "a".repeat(185), "b".repeat(64));
        assert_eq!(longest_valid_party.len(), 255);
        assert_eq!(
            capped(&longest_valid_party, PARTY_ID_CAP),
            longest_valid_party
        );
        assert_eq!(capped("ü".repeat(4).as_str(), 4), "üüüü");
        assert_eq!(
            capped("ü".repeat(5).as_str(), 4),
            format!("üüüü{TRUNCATED}")
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_refused_call_keeps_its_error(db: SqlitePool) -> Result {
        insert_call(
            &db,
            &NewTenantCall {
                http_status: Some(409),
                error: Some("the pinned serial 4 has moved to 5"),
                ..call(TenantEndpoint::AddHostsExecute)
            },
        )
        .await?;

        let entry = only_entry(&db).await?;
        assert_eq!(entry.http_status, Some(409));
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
        let last = insert_call(&db, &call(TenantEndpoint::AcsImportEnd)).await?;

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
