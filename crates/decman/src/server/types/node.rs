//! Node liveness, participant permissions and workflow response aliases.

use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use serde::Serialize;

use common::api::WorkflowResponse;
use common::types::{Permission, WorkflowProgress};

/// Liveness response for the `/healthz` ping endpoint. The body is
/// intentionally tiny: the frontend uses it to time its own round-trip to
/// this node, so the handler does no work beyond returning this. Named
/// `Liveness*` to avoid clashing with [`super::health::HealthResponse`], the
/// Noise health-probe payload.
///
/// Not generated into the frontend types: the frontend pings `/healthz` only to
/// time the round-trip and never reads the body — the latency is measured
/// client-side (`pingLatency`), not carried by this response.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LivenessResponse {
    pub status: String,
}

/// Map a Canton proto `ParticipantPermission` discriminant to the wire
/// [`Permission`] DTO.
///
/// This conversion lives in the backend (not in `common` alongside the enum)
/// because it depends on the proto-generated `ParticipantPermission`, which is
/// a server-only dependency; the `Permission` enum itself is shared with the
/// `decman-cli` client and so must stay free of proto deps. Replaces the former
/// `impl From<i32> for Permission`, which the orphan rule no longer permits now
/// that `Permission` is a foreign type.
pub fn permission_from_proto(value: i32) -> Permission {
    match value {
        x if x == ParticipantPermission::Submission as i32 => Permission::Submission,
        x if x == ParticipantPermission::Confirmation as i32 => Permission::Confirmation,
        x if x == ParticipantPermission::Observation as i32 => Permission::Observation,
        _ => Permission::Unknown,
    }
}

/// Type aliases for backwards compatibility
pub type KickStatus = WorkflowProgress;
pub type OnboardingStatus = WorkflowProgress;

/// Type aliases for backwards compatibility
pub type KickResponse = WorkflowResponse;
pub type OnboardingResponse = WorkflowResponse;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canton_id::CantonId;
    use crate::error::Result;
    use common::types::{
        AcsTransferDirection, AcsTransferProgress, WorkflowKind, WorkflowRole, WorkflowRun,
    };
    use serde_json::Value;

    /// The card reads these field names and the lowercase direction straight
    /// off the wire, and `acs_progress` must vanish rather than appear as
    /// `null` when nothing is transferring, or every idle run renders a meter.
    #[test]
    fn acs_transfer_progress_wire_shape_is_stable() -> Result {
        let progress = AcsTransferProgress {
            direction: AcsTransferDirection::Import,
            bytes: 1_369_579_398,
            block: 1306,
            started_at_ms: 1_788_968_788_000,
            updated_at_ms: 1_788_969_093_000,
        };
        let json = serde_json::to_value(&progress)?;

        assert_eq!(
            json.get("direction").and_then(Value::as_str),
            Some("import")
        );
        assert_eq!(
            json.get("bytes").and_then(Value::as_i64),
            Some(1_369_579_398)
        );
        assert_eq!(json.get("block").and_then(Value::as_i64), Some(1306));
        assert_eq!(
            json.get("started_at_ms").and_then(Value::as_i64),
            Some(1_788_968_788_000)
        );
        assert_eq!(
            json.get("updated_at_ms").and_then(Value::as_i64),
            Some(1_788_969_093_000)
        );

        let exported = serde_json::to_value(AcsTransferProgress {
            direction: AcsTransferDirection::Export,
            ..progress
        })?;
        assert_eq!(
            exported.get("direction").and_then(Value::as_str),
            Some("export")
        );
        Ok(())
    }

    /// P3: locks the wire shape of `WorkflowRun` so the `String → CantonId`
    /// typing change for participant-id fields cannot silently switch from
    /// plain strings to nested objects on the JSON the frontend consumes.
    #[test]
    fn workflow_run_serializes_canton_ids_as_plain_strings() {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let participant_id_str = format!("participant::{ns}");
        let dec_party_id_str = format!("test-network-1::{ns}");

        let peer_a = CantonId::parse(&format!("participant::{ns}")).unwrap();
        let peer_b = CantonId::parse(&format!(
            "participant::1220{0}{0}",
            "abcdefabcdefabcdefabcdefabcdef00"
        ))
        .unwrap();

        let run = WorkflowRun {
            instance_name: "test-network-1-creation".to_string(),
            kind: WorkflowKind::Onboarding,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "WaitingForPeers".to_string(),
            step_index: 0,
            step_total: 7,
            config_json: r#"{"prefix":"test-network-1"}"#.to_string(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: vec![peer_a.clone(), peer_b.clone()],
            completed_peers: vec![peer_a],
            connected_peers: vec![peer_b],
            acs_progress: None,
            dec_party_id: Some(CantonId::parse(&dec_party_id_str).unwrap()),
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            kicked_owner_key: None,
            added_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_001,
        };

        let json = serde_json::to_value(&run).expect("serialize WorkflowRun");

        // expected_peers and completed_peers must be JSON arrays of
        // plain strings — never objects with prefix/namespace fields.
        let expected = json
            .get("expected_peers")
            .and_then(Value::as_array)
            .expect("expected_peers must be a JSON array");
        assert_eq!(expected.len(), 2);
        for v in expected {
            assert!(
                v.is_string(),
                "expected_peers entry must be a string, got {v}"
            );
        }
        assert_eq!(expected[0].as_str().unwrap(), participant_id_str);

        let completed = json
            .get("completed_peers")
            .and_then(Value::as_array)
            .expect("completed_peers must be a JSON array");
        assert_eq!(completed.len(), 1);
        assert!(completed[0].is_string());

        let connected = json
            .get("connected_peers")
            .and_then(Value::as_array)
            .expect("connected_peers must be a JSON array");
        assert_eq!(connected.len(), 1);
        assert!(
            connected[0].is_string(),
            "connected_peers entry must be a string, got {}",
            connected[0]
        );

        // dec_party_id (Option<CantonId>) must serialize as a plain string,
        // not as a nested object with prefix/namespace fields.
        let dec_party = json.get("dec_party_id").expect("dec_party_id key present");
        assert!(
            dec_party.is_string(),
            "dec_party_id must be a JSON string when set, got {dec_party}"
        );
        assert_eq!(dec_party.as_str().unwrap(), dec_party_id_str);
    }
}
