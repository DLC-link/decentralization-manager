use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    db::schema::SchemaRead,
    server::{WorkflowKind, WorkflowRole},
};

/// The workflow a node is currently participating in, if any.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkflowInfo {
    pub kind: WorkflowKind,
    pub role: WorkflowRole,
    pub step: String,
    pub step_index: i64,
    pub step_total: i64,
}

/// Health report a node returns in response to a `Health` Noise message.
///
/// Reported to peers that probe this node's liveness; it lets them see, without
/// a separate channel, whether this node is mid-workflow (and which one) even
/// while it is busy participating.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct HealthResponse {
    pub participant_id: String,
    pub in_workflow: bool,
    pub workflow: Option<WorkflowInfo>,
    pub version: String,
}

impl HealthResponse {
    /// Serialize to the JSON bytes carried in a `HealthResponse` Noise message.
    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Parse from a `HealthResponse` Noise message payload. Returns `None` if
    /// the bytes aren't a valid `HealthResponse` (e.g. a peer on older code).
    pub fn from_payload(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Build this node's health report from the DB's in-progress workflow runs.
///
/// A node runs at most one workflow at a time (the global in-flight mutex), so
/// we report the first in-progress run if present.
pub async fn build_health_response(db: &SqlitePool, participant_id: &str) -> HealthResponse {
    let workflow = db
        .get_in_progress_workflow_runs()
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
        .map(|r| WorkflowInfo {
            kind: r.kind,
            role: r.role,
            step: r.current_step,
            step_index: r.step_index,
            step_total: r.step_total,
        });

    HealthResponse {
        participant_id: participant_id.to_string(),
        in_workflow: workflow.is_some(),
        workflow,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::db::MIGRATOR;

    use super::*;

    #[test]
    fn health_response_payload_round_trips() {
        let h = HealthResponse {
            participant_id: "p1::1220ab".into(),
            in_workflow: true,
            workflow: Some(WorkflowInfo {
                kind: WorkflowKind::Onboarding,
                role: WorkflowRole::Peer,
                step: "SignDns".into(),
                step_index: 3,
                step_total: 8,
            }),
            version: "0.1.0".into(),
        };
        let bytes = h.to_payload();
        let back = HealthResponse::from_payload(&bytes).unwrap();
        assert!(back.in_workflow);
        assert_eq!(back.workflow.unwrap().step, "SignDns");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn build_health_response_idle_when_no_runs(pool: SqlitePool) {
        let h = build_health_response(&pool, "p1::1220ab").await;
        assert!(!h.in_workflow);
        assert!(h.workflow.is_none());
        assert_eq!(h.participant_id, "p1::1220ab");
    }
}
