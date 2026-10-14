use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use crate::{auth::Principal, canton_id::CantonId};

use super::types::GovernanceType;

/// Event types for the governance audit trail
#[derive(Clone, Copy, Debug)]
pub enum AuditEvent {
    Propose,
    Confirm,
    Execute,
    Expire,
    Cancel,
    CancelProposal,
    PurgeStart,
    PurgeEnd,
}

impl AuditEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditEvent::Propose => "propose",
            AuditEvent::Confirm => "confirm",
            AuditEvent::Execute => "execute",
            AuditEvent::Expire => "expire",
            AuditEvent::Cancel => "cancel",
            AuditEvent::CancelProposal => "cancel_proposal",
            AuditEvent::PurgeStart => "purge_start",
            AuditEvent::PurgeEnd => "purge_end",
        }
    }
}

/// The admin user behind a request, as the audit trail records them.
#[derive(Clone, Debug)]
pub struct AuditActor {
    pub sub: String,
    pub email: Option<String>,
}

impl From<&Principal> for AuditActor {
    fn from(principal: &Principal) -> Self {
        AuditActor {
            sub: principal.sub.clone(),
            email: principal.email.clone(),
        }
    }
}

/// Parameters for an audit log entry
pub struct AuditParams {
    pub event_type: AuditEvent,
    pub party_id: CantonId,
    pub member_party_id: CantonId,
    pub governance_type: GovernanceType,
    pub action_summary: String,
    pub details: String,
    pub status: &'static str,
    pub error_message: Option<String>,
}

/// Insert a governance audit row
async fn log_governance_audit(pool: &SqlitePool, params: AuditParams) {
    let gov_type = serde_json::to_value(params.governance_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "core_self".to_string());
    insert_audit_row(
        pool,
        params.event_type,
        &params.party_id,
        &params.member_party_id,
        &gov_type,
        &params.action_summary,
        &params.details,
        params.status,
        params.error_message.as_deref(),
    )
    .await;
}

/// The `governance_type` of a row that no governance package produced.
const NO_GOVERNANCE_TYPE: &str = "none";

/// Write the row for a purge of a former party's leftover contracts. The party
/// is both the subject and the "member": no other party takes part. `counts`
/// is `(purged, kept)`, known only when the purge ended.
///
/// Waits for the write, unlike [`spawn_audit_log`], so the row exists before
/// the purge disconnects the participant and before its outcome is reported.
pub async fn log_purge_audit(
    pool: &SqlitePool,
    event_type: AuditEvent,
    party_id: &CantonId,
    actor: &AuditActor,
    counts: Option<(usize, usize)>,
    error_message: Option<&str>,
) {
    let status = match (event_type, error_message) {
        (AuditEvent::PurgeStart, _) => "started",
        (_, None) => "success",
        (_, Some(_)) => "failed",
    };
    let mut details = serde_json::json!({
        "admin_sub": actor.sub,
        "admin_email": actor.email,
    });
    if let Some((purged, kept)) = counts {
        details["purged_count"] = purged.into();
        details["kept_count"] = kept.into();
    }
    insert_audit_row(
        pool,
        event_type,
        party_id,
        party_id,
        NO_GOVERNANCE_TYPE,
        "purge_former_party",
        &details.to_string(),
        status,
        error_message,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn insert_audit_row(
    pool: &SqlitePool,
    event_type: AuditEvent,
    party_id: &CantonId,
    member_party_id: &CantonId,
    governance_type: &str,
    action_summary: &str,
    details: &str,
    status: &str,
    error_message: Option<&str>,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let result = sqlx::query(
        r"
        INSERT INTO governance_audit (
            timestamp, event_type, party_id, member_party_id,
            governance_type, action_summary, details, status,
            error_message, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(now)
    .bind(event_type.as_str())
    .bind(party_id.to_string())
    .bind(member_party_id.to_string())
    .bind(governance_type)
    .bind(action_summary)
    .bind(details)
    .bind(status)
    .bind(error_message)
    .bind(now)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::warn!("Failed to write governance audit log: {e}");
    }
}

/// Spawn a fire-and-forget audit log write.
/// This will NOT block the caller and will NOT propagate errors.
pub fn spawn_audit_log(pool: SqlitePool, params: AuditParams) {
    tokio::spawn(async move {
        log_governance_audit(&pool, params).await;
    });
}

/// Derive an action_summary label from an ActionType by extracting the serde "type" tag
pub fn action_summary(action: &super::types::ActionType) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Derive an action_summary label from a ProposalType
pub fn proposal_summary(proposal: &super::types::ProposalType) -> String {
    serde_json::to_value(proposal)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown_proposal".to_string())
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::{MIGRATOR, schema::SchemaRead};

    fn party() -> CantonId {
        CantonId::parse(&format!("old-pool::1220{}", "c".repeat(64))).unwrap()
    }

    fn actor() -> AuditActor {
        AuditActor {
            sub: "admin-1".to_string(),
            email: Some("admin@example.com".to_string()),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn purge_rows_record_actor_counts_and_outcome(pool: SqlitePool) {
        let party = party();
        log_purge_audit(&pool, AuditEvent::PurgeStart, &party, &actor(), None, None).await;
        log_purge_audit(
            &pool,
            AuditEvent::PurgeEnd,
            &party,
            &actor(),
            Some((3, 2)),
            None,
        )
        .await;
        log_purge_audit(
            &pool,
            AuditEvent::PurgeEnd,
            &party,
            &actor(),
            None,
            Some("disconnect failed"),
        )
        .await;

        let rows = pool.get_governance_audit(&party, 50, 0).await.unwrap();
        assert_eq!(rows.len(), 3);
        let by_type = |t: &str, status: &str| {
            rows.iter()
                .find(|r| r.event_type == t && r.status == status)
                .unwrap_or_else(|| panic!("no {t}/{status} row"))
        };

        let start = by_type("purge_start", "started");
        assert_eq!(start.party_id, party.to_string());
        assert_eq!(start.member_party_id, party.to_string());
        assert_eq!(start.governance_type, NO_GOVERNANCE_TYPE);
        assert_eq!(start.action_summary, "purge_former_party");
        let details: serde_json::Value = serde_json::from_str(&start.details).unwrap();
        assert_eq!(details["admin_sub"], "admin-1");
        assert_eq!(details["admin_email"], "admin@example.com");
        assert!(details.get("purged_count").is_none());
        assert!(start.error_message.is_none());

        let ok = by_type("purge_end", "success");
        let details: serde_json::Value = serde_json::from_str(&ok.details).unwrap();
        assert_eq!(details["purged_count"], 3);
        assert_eq!(details["kept_count"], 2);
        assert!(ok.error_message.is_none());

        let failed = by_type("purge_end", "failed");
        assert_eq!(failed.error_message.as_deref(), Some("disconnect failed"));
        let details: serde_json::Value = serde_json::from_str(&failed.details).unwrap();
        assert!(details.get("purged_count").is_none());
    }
}
