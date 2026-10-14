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
        AuditRow {
            event_type: params.event_type,
            party_id: &params.party_id,
            member_party_id: &params.member_party_id,
            governance_type: &gov_type,
            action_summary: &params.action_summary,
            details: &params.details,
            status: params.status,
            error_message: params.error_message.as_deref(),
        },
    )
    .await;
}

/// The `governance_type` of a row that no governance package produced.
const NO_GOVERNANCE_TYPE: &str = "none";

/// A step of a purge of a former party's leftover contracts. The end carries
/// `(purged, kept)` on success and the error otherwise.
#[derive(Clone, Debug)]
pub enum PurgeAuditEvent {
    Start,
    End(Result<(usize, usize), String>),
}

/// Write the row for one step of a purge. The party is both the subject and
/// the "member": no other party takes part. `actor` is `None` when the step
/// is written by a restart that found the purge cut off, so no request
/// carried the admin user.
///
/// Waits for the write, unlike [`spawn_audit_log`], so the start row exists
/// before the purge disconnects the participant.
pub async fn log_purge_audit(
    pool: &SqlitePool,
    event: PurgeAuditEvent,
    party_id: &CantonId,
    actor: Option<&AuditActor>,
) {
    let mut details = serde_json::json!({
        "admin_sub": actor.map(|a| a.sub.as_str()),
        "admin_email": actor.and_then(|a| a.email.as_deref()),
    });
    let (event_type, status, error_message) = match &event {
        PurgeAuditEvent::Start => (AuditEvent::PurgeStart, "started", None),
        PurgeAuditEvent::End(Ok((purged, kept))) => {
            details["purged_count"] = (*purged).into();
            details["kept_count"] = (*kept).into();
            (AuditEvent::PurgeEnd, "success", None)
        }
        PurgeAuditEvent::End(Err(e)) => (AuditEvent::PurgeEnd, "failed", Some(e.as_str())),
    };
    insert_audit_row(
        pool,
        AuditRow {
            event_type,
            party_id,
            member_party_id: party_id,
            governance_type: NO_GOVERNANCE_TYPE,
            action_summary: "purge_former_party",
            details: &details.to_string(),
            status,
            error_message,
        },
    )
    .await;
}

/// One `governance_audit` row as it is bound.
struct AuditRow<'a> {
    event_type: AuditEvent,
    party_id: &'a CantonId,
    member_party_id: &'a CantonId,
    governance_type: &'a str,
    action_summary: &'a str,
    details: &'a str,
    status: &'a str,
    error_message: Option<&'a str>,
}

async fn insert_audit_row(pool: &SqlitePool, row: AuditRow<'_>) {
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
    .bind(row.event_type.as_str())
    .bind(row.party_id.to_string())
    .bind(row.member_party_id.to_string())
    .bind(row.governance_type)
    .bind(row.action_summary)
    .bind(row.details)
    .bind(row.status)
    .bind(row.error_message)
    .bind(now)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::error!("Failed to write governance audit log: {e}");
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
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::{MIGRATOR, schema::SchemaRead};

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
        let actor = actor();
        log_purge_audit(&pool, PurgeAuditEvent::Start, &party, Some(&actor)).await;
        log_purge_audit(
            &pool,
            PurgeAuditEvent::End(Ok((3, 2))),
            &party,
            Some(&actor),
        )
        .await;
        log_purge_audit(
            &pool,
            PurgeAuditEvent::End(Err("disconnect failed".to_string())),
            &party,
            None,
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
        assert!(details["admin_sub"].is_null());
        assert!(details["admin_email"].is_null());
        assert!(details.get("purged_count").is_none());
    }
}
