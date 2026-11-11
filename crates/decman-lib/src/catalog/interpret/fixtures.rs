//! Ledger API fixtures shared by the interpret tests.

use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Identifier, InterfaceView};
use canton_proto_rs::com::daml::ledger::api::v2::{Record, RecordField};
use common::canton_id::CantonId;
use prost_types::Timestamp;

/// Valid-shape party ids (a 34-byte SHA-256 multihash namespace, hex
/// encoded) so `CantonId::parse` succeeds.
pub const ALICE: &str =
    "alice::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const BOB: &str = "bob::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const GOV: &str = "gov::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

pub fn cid(prefix: &str) -> CantonId {
    CantonId::parse(prefix).expect("valid test fixture party id")
}

pub fn created_event(contract_id: &str, record: Record, created_at_seconds: i64) -> CreatedEvent {
    CreatedEvent {
        contract_id: contract_id.to_string(),
        create_arguments: Some(record),
        created_at: Some(Timestamp {
            seconds: created_at_seconds,
            nanos: 0,
        }),
        interface_views: vec![],
        ..Default::default()
    }
}

pub fn record_of(fields: Vec<RecordField>) -> Record {
    Record {
        record_id: None,
        fields,
    }
}

/// A `GovernableAction` interface view carrying `view_fields`, shaped the
/// way Canton attaches it when the query asked for it with an
/// `InterfaceFilter`.
pub fn governable_action_interface_view(view_fields: Vec<RecordField>) -> InterfaceView {
    InterfaceView {
        interface_id: Some(Identifier {
            package_id: "#governance-core".to_string(),
            module_name: "Governance.Action".to_string(),
            entity_name: "GovernableAction".to_string(),
        }),
        view_status: None,
        view_value: Some(record_of(view_fields)),
        ..Default::default()
    }
}

/// A created event for a `GovernableAction`-implementing proposal
/// contract, with an optional interface view, create-arguments, and
/// template entity name.
pub fn proposal_created_event(
    contract_id: &str,
    interface_views: Vec<InterfaceView>,
    create_arguments: Option<Record>,
    template_entity_name: Option<&str>,
) -> CreatedEvent {
    CreatedEvent {
        contract_id: contract_id.to_string(),
        template_id: template_entity_name.map(|name| Identifier {
            package_id: "#governance-core".to_string(),
            module_name: "Governance.Action".to_string(),
            entity_name: name.to_string(),
        }),
        create_arguments,
        created_at: Some(Timestamp {
            seconds: 1_700_001_000,
            nanos: 0,
        }),
        interface_views,
        ..Default::default()
    }
}
