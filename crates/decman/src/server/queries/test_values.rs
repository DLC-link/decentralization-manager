//! Ledger value builders shared by the query tests.

use canton_proto_rs::com::daml::ledger::api::v2::{Record, value};

use canton_proto_rs::com::daml::ledger::api::v2::Value;

use canton_proto_rs::com::daml::ledger::api::v2::{List, Optional, RecordField, Variant};

pub fn field(label: &str, value: Value) -> RecordField {
    RecordField {
        label: label.to_string(),
        value: Some(value),
    }
}

pub fn text_value(s: &str) -> Value {
    Value {
        sum: Some(value::Sum::Text(s.to_string())),
    }
}

pub fn party_value(p: &str) -> Value {
    Value {
        sum: Some(value::Sum::Party(p.to_string())),
    }
}

pub fn numeric_value(n: &str) -> Value {
    Value {
        sum: Some(value::Sum::Numeric(n.to_string())),
    }
}

pub fn timestamp_value(micros: i64) -> Value {
    Value {
        sum: Some(value::Sum::Timestamp(micros)),
    }
}

pub fn variant_value(constructor: &str, inner: Value) -> Value {
    Value {
        sum: Some(value::Sum::Variant(Box::new(Variant {
            variant_id: None,
            constructor: constructor.to_string(),
            value: Some(Box::new(inner)),
        }))),
    }
}

pub fn record_value(fields: Vec<RecordField>) -> Value {
    Value {
        sum: Some(value::Sum::Record(Record {
            record_id: None,
            fields,
        })),
    }
}

pub fn unit_value() -> Value {
    record_value(vec![])
}

// `<prefix>::<34-byte-multihash-hex>`; CantonId::parse rejects other shapes.
pub const SR_FP: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

pub fn optional_value(inner: Option<Value>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: inner.map(Box::new),
        }))),
    }
}

pub fn list_value(elements: Vec<Value>) -> Value {
    Value {
        sum: Some(value::Sum::List(List { elements })),
    }
}
