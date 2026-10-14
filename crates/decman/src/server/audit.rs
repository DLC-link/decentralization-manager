use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use crate::canton_id::CantonId;

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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let gov_type = serde_json::to_value(params.governance_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "core_self".to_string());

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
    .bind(params.party_id.to_string())
    .bind(params.member_party_id.to_string())
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

/// Add the created proposal's contract id to a propose row's details.
///
/// The propose request has no contract id yet, while the confirm, execute and
/// cancel-proposal rows of the same proposal carry one as `proposal_cid`. With
/// it here too, a reader can group every row this node logged for a proposal.
/// Details that are not a JSON object are returned unchanged.
pub fn with_proposal_cid(details: &str, proposal_cid: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(details) {
        Ok(serde_json::Value::Object(mut fields)) => {
            fields.insert("proposal_cid".to_string(), proposal_cid.into());
            serde_json::Value::Object(fields).to_string()
        }
        _ => details.to_string(),
    }
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
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn propose_details_gain_the_proposal_cid_and_keep_the_request() -> anyhow::Result<()> {
        let request = json!({
            "party_id": "dec::1220ab",
            "rules_contract_id": "00rules",
            "proposal": { "type": "generic_vote", "description": "hi" },
        });

        let details: Value =
            serde_json::from_str(&with_proposal_cid(&request.to_string(), "00proposal"))?;

        let mut expected = request;
        expected["proposal_cid"] = json!("00proposal");
        assert_eq!(details, expected);
        Ok(())
    }

    #[test]
    fn details_that_are_not_an_object_are_left_alone() {
        assert_eq!(with_proposal_cid("", "00proposal"), "");
        assert_eq!(with_proposal_cid("[1,2]", "00proposal"), "[1,2]");
    }
}
