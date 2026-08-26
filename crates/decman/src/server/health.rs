use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    db::schema::SchemaRead,
    noise::{Message, MessageType},
    server::ConnectionStatus,
};

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
    /// The oldest in-progress run (deterministic; see `build_health_response`).
    pub workflow: Option<WorkflowInfo>,
    /// Total in-progress runs on this node — with concurrent workflows a node
    /// can hold several; `workflow` alone shows only one of them. `default`
    /// so payloads from nodes that predate this field still parse.
    #[serde(default)]
    pub workflow_count: usize,
    /// Cargo semver — the compatibility version peers gate on
    /// (`MIN_PEER_VERSION`).
    pub version: String,
    /// Display build identity (image tag / short SHA / `<semver>-dev`). Shown
    /// in the peers table; not used for compatibility gating. `default` so
    /// payloads from peers that predate this field still parse.
    #[serde(default)]
    pub build_version: String,
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
/// With concurrent workflows a node can hold any number of in-progress runs;
/// `workflow` reports the oldest one (`get_in_progress_workflow_runs` orders
/// by `created_at ASC`, so repeated probes don't flip between runs) and
/// `workflow_count` carries the total.
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
    let workflow_count = runs.len();
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
        workflow_count,
        version: crate::build_info::SEMVER.to_string(),
        build_version: crate::build_info::build_version().to_string(),
    }
}

/// Outcome of classifying a peer's reply to a `Health` probe. A reachable peer
/// on older code fills only `status` (the rest are `None`/`Connected`), since
/// its reply isn't a parseable [`HealthResponse`].
pub(crate) struct HealthReply {
    pub status: ConnectionStatus,
    pub workflow: Option<WorkflowInfo>,
    /// Semver the peer reported — used for `MIN_PEER_VERSION` gating.
    pub version: Option<String>,
    /// Display build identity the peer reported. `None` for peers on code that
    /// predates the field (they send an empty string, normalized to `None`).
    pub build_version: Option<String>,
}

/// Classify a successful Noise reply to a `Health` probe. Any reply that isn't a
/// parseable `HealthResponse` (a peer on older code, a `Pong`, an empty body)
/// still means the peer is reachable — we just don't learn its workflow state or
/// version.
pub(crate) fn classify_health_reply(reply: &[u8]) -> HealthReply {
    if let Ok(msg) = Message::from_bytes(reply)
        && msg.msg_type == MessageType::HealthResponse
        && let Some(h) = HealthResponse::from_payload(&msg.payload)
    {
        // A blank build_version means the peer predates the field; normalize to
        // None so the UI shows "—" rather than an empty cell.
        let build_version = Some(h.build_version).filter(|v| !v.is_empty());
        return HealthReply {
            status: ConnectionStatus::Connected,
            workflow: h.workflow,
            version: Some(h.version),
            build_version,
        };
    }
    HealthReply {
        status: ConnectionStatus::Connected,
        workflow: None,
        version: None,
        build_version: None,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use crate::{
        db::MIGRATOR,
        error::Result,
        server::{WorkflowKind, WorkflowRole},
    };

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
            workflow_count: 1,
            version: "0.1.0".into(),
            build_version: "v0.1.0".into(),
        };
        let back =
            HealthResponse::from_payload(&h.to_payload()).context("payload should round-trip")?;
        assert!(back.in_workflow);
        assert_eq!(back.build_version, "v0.1.0");
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
            workflow_count: 1,
            version: "0.1.0".into(),
            build_version: "v0.1.0".into(),
        };
        let reply = Message::new(MessageType::HealthResponse, hr.to_payload()).to_bytes();
        let r = classify_health_reply(&reply);
        assert_eq!(r.status, ConnectionStatus::Connected);
        assert_eq!(
            r.workflow.context("workflow should be parsed")?.kind,
            WorkflowKind::Onboarding
        );
        assert_eq!(r.version.as_deref(), Some("0.1.0"));
        assert_eq!(r.build_version.as_deref(), Some("v0.1.0"));

        // Peer that predates build_version: sends an empty string → normalized
        // to None, while version still parses.
        let hr_old = HealthResponse {
            participant_id: "p3::1220".into(),
            in_workflow: false,
            workflow: None,
            workflow_count: 0,
            version: "0.1.9".into(),
            build_version: String::new(),
        };
        let reply = Message::new(MessageType::HealthResponse, hr_old.to_payload()).to_bytes();
        let r = classify_health_reply(&reply);
        assert_eq!(r.version.as_deref(), Some("0.1.9"));
        assert!(r.build_version.is_none());

        // Old peer: replies Pong (not HealthResponse) → reachable, no workflow,
        // no version.
        let pong = Message::new_empty(MessageType::Pong).to_bytes();
        let r = classify_health_reply(&pong);
        assert_eq!(r.status, ConnectionStatus::Connected);
        assert!(r.workflow.is_none());
        assert!(r.version.is_none());
        assert!(r.build_version.is_none());

        // Empty body (e.g. an old listener's fall-through) → still reachable.
        let r = classify_health_reply(&[]);
        assert_eq!(r.status, ConnectionStatus::Connected);
        assert!(r.workflow.is_none());
        assert!(r.version.is_none());
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
