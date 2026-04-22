use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use super::types::GovernanceType;

/// Event types for the governance audit trail
#[derive(Clone, Copy, Debug)]
pub enum AuditEvent {
    Propose,
    Confirm,
    Execute,
    Expire,
    Cancel,
}

impl AuditEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditEvent::Propose => "propose",
            AuditEvent::Confirm => "confirm",
            AuditEvent::Execute => "execute",
            AuditEvent::Expire => "expire",
            AuditEvent::Cancel => "cancel",
        }
    }
}

/// Parameters for an audit log entry
pub struct AuditParams {
    pub event_type: AuditEvent,
    pub party_id: String,
    pub member_party_id: String,
    pub governance_type: GovernanceType,
    pub action_summary: String,
    pub details: String,
    pub status: &'static str,
    pub error_message: Option<String>,
}

/// Insert a governance audit row
async fn log_governance_audit(pool: &SqlitePool, params: AuditParams) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let gov_type = serde_json::to_value(params.governance_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "vault".to_string());

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
    .bind(params.event_type.as_str())
    .bind(&params.party_id)
    .bind(&params.member_party_id)
    .bind(&gov_type)
    .bind(&params.action_summary)
    .bind(&params.details)
    .bind(params.status)
    .bind(&params.error_message)
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
