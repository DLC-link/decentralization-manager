use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    db::schema::SchemaRead,
    noise::{Message, MessageType},
    server::{ConnectionStatus, WorkflowKind},
};

// `WorkflowInfo` now lives in the shared `common` crate (consumed by both this
// server and the `decman-cli` client). Re-exported here so the existing
// `crate::server::health::WorkflowInfo` path keeps working.
pub use common::types::WorkflowInfo;

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
        serde_json::to_vec(self).unwrap_or_else(|e| {
            // HealthResponse is always serializable; if this ever fails, surface
            // it instead of silently emitting an unparseable empty payload.
            tracing::error!("health: failed to serialize HealthResponse: {e}");
            Vec::new()
        })
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
    let runs = match db.get_in_progress_workflow_runs().await {
        Ok(runs) => runs,
        Err(e) => {
            // Don't silently report not-in-workflow on a DB error — log it so a
            // degraded health response can be diagnosed.
            tracing::warn!(
                "health: failed to read in-progress workflow runs, reporting not-in-workflow: {e}"
            );
            Vec::new()
        }
    };
    let workflow = runs.into_iter().next().map(|r| WorkflowInfo {
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

/// Classify a successful Noise reply to a `Health` probe. Any reply that isn't a
/// parseable `HealthResponse` (a peer on older code, a `Pong`, an empty body)
/// still means the peer is reachable — we just don't learn its workflow state or
/// version. Returns `(status, workflow, version)`.
pub(crate) fn classify_health_reply(
    reply: &[u8],
) -> (ConnectionStatus, Option<WorkflowInfo>, Option<String>) {
    if let Ok(msg) = Message::from_bytes(reply)
        && msg.msg_type == MessageType::HealthResponse
        && let Some(h) = HealthResponse::from_payload(&msg.payload)
    {
        return (ConnectionStatus::Connected, h.workflow, Some(h.version));
    }
    (ConnectionStatus::Connected, None, None)
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use crate::{db::MIGRATOR, error::Result, server::WorkflowRole};

    use super::*;

    #[test]
    fn health_response_payload_round_trips() -> Result {
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
        let back =
            HealthResponse::from_payload(&h.to_payload()).context("payload should round-trip")?;
        assert!(back.in_workflow);
        let workflow = back.workflow.context("workflow should be present")?;
        assert_eq!(workflow.step, "SignDns");
        Ok(())
    }

    #[test]
    fn classify_health_reply_parses_workflow_and_falls_back() -> Result {
        // New peer: HealthResponse with workflow → Connected + workflow.
        let hr = HealthResponse {
            participant_id: "p2::1220".into(),
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
        let reply = Message::new(MessageType::HealthResponse, hr.to_payload()).to_bytes();
        let (status, workflow, version) = classify_health_reply(&reply);
        assert_eq!(status, ConnectionStatus::Connected);
        assert_eq!(
            workflow.context("workflow should be parsed")?.kind,
            WorkflowKind::Onboarding
        );
        assert_eq!(version.as_deref(), Some("0.1.0"));

        // Old peer: replies Pong (not HealthResponse) → reachable, no workflow,
        // no version.
        let pong = Message::new_empty(MessageType::Pong).to_bytes();
        let (status, workflow, version) = classify_health_reply(&pong);
        assert_eq!(status, ConnectionStatus::Connected);
        assert!(workflow.is_none());
        assert!(version.is_none());

        // Empty body (e.g. an old listener's fall-through) → still reachable.
        let (status, workflow, version) = classify_health_reply(&[]);
        assert_eq!(status, ConnectionStatus::Connected);
        assert!(workflow.is_none());
        assert!(version.is_none());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn build_health_response_idle_when_no_runs(pool: SqlitePool) {
        let h = build_health_response(&pool, "p1::1220ab").await;
        assert!(!h.in_workflow);
        assert!(h.workflow.is_none());
        assert_eq!(h.participant_id, "p1::1220ab");
    }
}
