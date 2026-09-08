//! Daml `Value` construction toolkit.
//!
//! The low-level constructors (`make_party`, `make_record`, ...), the
//! `extraArgs`/choice-context builders, and `TransferValidity`. Integrators
//! implementing `DamlProtoEncode` are built from these.

use canton_common::{
    decimal::DamlDecimal,
    transfer_factory::{Context as ChoiceContext, ContextValue},
};
use canton_proto_rs::com::daml::ledger::api::v2::{
    List, Optional, Record, RecordField, TextMap, Value, Variant, text_map, value,
};
use common::api::{Claim, InstrumentId, InstrumentIdentifier, PartyCredentialRequirement};

use crate::error::Error;

pub fn make_party(p: impl std::fmt::Display) -> Value {
    Value {
        sum: Some(value::Sum::Party(p.to_string())),
    }
}

pub fn make_text(t: &str) -> Value {
    Value {
        sum: Some(value::Sum::Text(t.to_string())),
    }
}

pub fn make_int64(n: i64) -> Value {
    Value {
        sum: Some(value::Sum::Int64(n)),
    }
}

pub fn make_numeric(d: &str) -> Value {
    Value {
        sum: Some(value::Sum::Numeric(d.to_string())),
    }
}

pub fn make_bool(b: bool) -> Value {
    Value {
        sum: Some(value::Sum::Bool(b)),
    }
}

pub fn make_contract_id(c: &str) -> Value {
    Value {
        sum: Some(value::Sum::ContractId(c.to_string())),
    }
}

pub fn field(label: &str, value: Value) -> RecordField {
    RecordField {
        label: label.to_string(),
        value: Some(value),
    }
}

pub fn make_record(fields: Vec<RecordField>) -> Value {
    Value {
        sum: Some(value::Sum::Record(Record {
            record_id: None,
            fields,
        })),
    }
}

pub fn make_variant(constructor: &str, value: Value) -> Value {
    Value {
        sum: Some(value::Sum::Variant(Box::new(Variant {
            variant_id: None,
            constructor: constructor.to_string(),
            value: Some(Box::new(value)),
        }))),
    }
}

pub fn make_list(values: Vec<Value>) -> Value {
    Value {
        sum: Some(value::Sum::List(List { elements: values })),
    }
}

pub fn make_empty_text_map() -> Value {
    make_text_map(vec![])
}

pub fn make_text_map(entries: Vec<(String, Value)>) -> Value {
    Value {
        sum: Some(value::Sum::TextMap(TextMap {
            entries: entries
                .into_iter()
                .map(|(k, v)| text_map::Entry {
                    key: k,
                    value: Some(v),
                })
                .collect(),
        })),
    }
}

// Splice's `Metadata.values` is typed `TextMap Text` and `ChoiceContext.values`
// is typed `TextMap AnyValue` (see `Splice.Api.Token.MetadataV1`). Both must be
// sent as a `TextMap` value — an empty `GenMap` is rejected by Canton's command
// preprocessor with `mismatching type: TextMap ... and value: ValueGenMap()`.
pub fn make_empty_metadata() -> Value {
    make_record(vec![field("values", make_empty_text_map())])
}

pub fn make_empty_extra_args() -> Value {
    make_extra_args(make_empty_text_map())
}

/// Fallback timestamps for serializing a `Transfer` record when no explicit
/// validity window is supplied (tests only — the propose handler always passes
/// a real, bounded window). `0` is epoch and `i64::MAX / 1000` is the maximum
/// Daml `Time` value.
///
/// These were the *production* values once, but an effectively-infinite
/// `executeBefore` meant a two-step transfer offer the receiver never accepted
/// locked the sender's holdings forever. Production now bounds the window via
/// [`TransferValidity`]; see [`TRANSFER_VALIDITY_WINDOW_MICROS`].
pub const TRANSFER_REQUESTED_AT_MICROS: i64 = 0;
pub const TRANSFER_EXECUTE_BEFORE_MICROS: i64 = i64::MAX / 1000;

/// How long a `Transfer` proposal (and, for two-step transfers, the resulting
/// offer) stays executable/acceptable after creation. Bounding this means an
/// unaccepted offer expires and its escrowed holdings can be reclaimed, rather
/// than locking funds indefinitely. 24h matches the daml test fixtures and the
/// governance action timeout.
pub const TRANSFER_VALIDITY_WINDOW_MICROS: i64 = 24 * 60 * 60 * 1_000_000;

/// The `requestedAt` / `executeBefore` pair stamped onto a `Transfer`. The same
/// instance must be used for both the registry choice-context fetch and the
/// on-chain `TransferProposal` create args — the registrar resolves the context
/// for these exact values, so any drift fails interpretation at execute time.
#[derive(Clone, Copy, Debug)]
pub struct TransferValidity {
    pub requested_at_micros: i64,
    pub execute_before_micros: i64,
}

impl TransferValidity {
    /// A window starting at `now_micros` and lasting the default
    /// [`TRANSFER_VALIDITY_WINDOW_MICROS`].
    pub fn from_now(now_micros: i64) -> Self {
        Self::from_now_with_window(now_micros, TRANSFER_VALIDITY_WINDOW_MICROS)
    }

    /// A window starting at `now_micros` and lasting `window_micros`. Used when
    /// the caller (e.g. the propose handler) lets the user override the default
    /// expiry. `now_micros` is captured once so the registry and on-chain
    /// payloads agree byte-for-byte.
    ///
    /// `executeBefore` is clamped to [`TRANSFER_EXECUTE_BEFORE_MICROS`] (the
    /// module's max Daml `Time`) so a large `now_micros`/`window_micros` can
    /// never serialize an out-of-range timestamp.
    pub fn from_now_with_window(now_micros: i64, window_micros: i64) -> Self {
        Self {
            requested_at_micros: now_micros,
            execute_before_micros: now_micros
                .saturating_add(window_micros)
                .min(TRANSFER_EXECUTE_BEFORE_MICROS),
        }
    }
}

pub fn make_extra_args(context_values: Value) -> Value {
    make_record(vec![
        field(
            "context",
            make_record(vec![field("values", context_values)]),
        ),
        field("meta", make_empty_metadata()),
    ])
}

/// Serialize a `Splice.Api.Token.MetadataV1.AnyValue` constructor as a Daml
/// `Variant` Value suitable for the Ledger API.
pub fn make_any_value(v: &ContextValue) -> Result<Value, Error> {
    let (ctor, inner) = match v {
        ContextValue::Text(s) => ("AV_Text", make_text(s)),
        ContextValue::Int(n) => ("AV_Int", make_int64(*n)),
        ContextValue::Decimal(d) => ("AV_Decimal", make_numeric(&d.to_string())),
        ContextValue::Bool(b) => ("AV_Bool", make_bool(*b)),
        ContextValue::Party(p) => ("AV_Party", make_party(p)),
        ContextValue::ContractId(cid) => ("AV_ContractId", make_contract_id(cid)),
        ContextValue::List(items) => {
            let elements: Result<Vec<Value>, Error> = items.iter().map(make_any_value).collect();
            ("AV_List", make_list(elements?))
        }
        ContextValue::Map(m) => {
            let mut entries: Vec<(String, Value)> = m
                .iter()
                .map(|(k, v)| make_any_value(v).map(|av| (k.clone(), av)))
                .collect::<Result<_, Error>>()?;
            // Stable order so wire bytes are deterministic.
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            ("AV_Map", make_text_map(entries))
        }
        ContextValue::Date(_) | ContextValue::Time(_) | ContextValue::RelTime(_) => {
            return Err(Error::Encode(format!(
                "ContextValue::{v:?} not supported in choice context: only Text, Int, Decimal, \
                 Bool, Party, ContractId, List, and Map are translated to the Ledger API today",
            )));
        }
    };
    Ok(make_variant(ctor, inner))
}

/// Build the `extraArgs` record with the choice-context values populated from
/// a registry response (e.g. `registry::accept_context::get`).
pub fn make_extra_args_from_context(ctx: &ChoiceContext) -> Result<Value, Error> {
    let mut entries: Vec<(String, Value)> = ctx
        .values
        .iter()
        .map(|(k, v)| make_any_value(v).map(|av| (k.clone(), av)))
        .collect::<Result<_, Error>>()?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(make_extra_args(make_text_map(entries)))
}

pub fn serialize_instrument_id(id: &InstrumentId) -> Value {
    make_record(vec![
        field("admin", make_party(&id.admin)),
        field("id", make_text(&id.id)),
    ])
}

pub fn make_optional_list(values: Vec<Value>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: if values.is_empty() {
                None
            } else {
                Some(Box::new(make_list(values)))
            },
        }))),
    }
}

pub fn make_optional_numeric(opt: &Option<DamlDecimal>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: opt.as_ref().map(|n| Box::new(make_numeric(&n.to_string()))),
        }))),
    }
}

pub fn make_optional_contract_id(opt: &Option<String>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: opt.as_ref().map(|c| Box::new(make_contract_id(c))),
        }))),
    }
}

pub fn make_optional_bool(opt: &Option<bool>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: opt.as_ref().map(|b| Box::new(make_bool(*b))),
        }))),
    }
}

pub fn serialize_claim(claim: &Claim) -> Value {
    make_record(vec![
        field("subject", make_text(&claim.subject)),
        field("property", make_text(&claim.property)),
        field("value", make_text(&claim.value)),
    ])
}

pub fn serialize_instrument_identifier(i: &InstrumentIdentifier) -> Value {
    make_record(vec![
        field("source", make_party(&i.source)),
        field("id", make_text(&i.id)),
        field("scheme", make_text(&i.scheme)),
    ])
}

/// Serialize RelTime (microseconds wrapped in a record)
pub fn serialize_reltime(microseconds: i64) -> Value {
    make_record(vec![field("microseconds", make_int64(microseconds))])
}

/// Serialize a `[PartyCredentialRequirement]` field. Field order matches the
/// Daml record: `issuer`, then `requiredClaims`. Each required claim is a
/// `DA.Types:Tuple2 Text Text`, which the Ledger API encodes as a record
/// with fields `_1` (the property) and `_2` (the value).
pub fn serialize_party_credential_requirements(
    requirements: &[PartyCredentialRequirement],
) -> Value {
    make_list(
        requirements
            .iter()
            .map(|requirement| {
                make_record(vec![
                    field("issuer", make_party(&requirement.issuer)),
                    field(
                        "requiredClaims",
                        make_list(
                            requirement
                                .required_claims
                                .iter()
                                .map(|claim| {
                                    make_record(vec![
                                        field("_1", make_text(&claim.property)),
                                        field("_2", make_text(&claim.value)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    // Locks the AV_* constructor strings against the on-ledger
    // `Splice.Api.Token.MetadataV1.AnyValue` definition. A typo here would
    // surface as a runtime interpretation error far from the source, so the
    // mapping for every supported `ContextValue` variant is asserted explicitly.
    #[test]
    fn make_any_value_maps_each_variant_to_expected_ctor() {
        let cases: Vec<(ContextValue, &str)> = vec![
            (ContextValue::Text("hi".to_string()), "AV_Text"),
            (ContextValue::Int(42), "AV_Int"),
            (
                ContextValue::Decimal(DamlDecimal::parse("1.5").expect("valid decimal")),
                "AV_Decimal",
            ),
            (ContextValue::Bool(true), "AV_Bool"),
            (ContextValue::Party("alice::pid".to_string()), "AV_Party"),
            (
                ContextValue::ContractId("cid-1".to_string()),
                "AV_ContractId",
            ),
            (ContextValue::List(vec![ContextValue::Int(1)]), "AV_List"),
            (
                ContextValue::Map(HashMap::from([(
                    "k".to_string(),
                    ContextValue::Text("v".to_string()),
                )])),
                "AV_Map",
            ),
        ];

        for (input, expected_ctor) in cases {
            let value = make_any_value(&input).expect("make_any_value succeeded");
            match value.sum {
                Some(value::Sum::Variant(v)) => assert_eq!(
                    v.constructor, expected_ctor,
                    "wrong constructor for {input:?}",
                ),
                other => panic!("expected Variant for {input:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn make_any_value_recurses_into_nested_map() {
        let nested = ContextValue::Map(HashMap::from([
            ("text".to_string(), ContextValue::Text("hi".to_string())),
            (
                "cid".to_string(),
                ContextValue::ContractId("cid-1".to_string()),
            ),
            (
                "nested".to_string(),
                ContextValue::Map(HashMap::from([("n".to_string(), ContextValue::Int(7))])),
            ),
        ]));

        let value = make_any_value(&nested).expect("nested map serializes");
        let Some(value::Sum::Variant(v)) = value.sum else {
            panic!("expected Variant");
        };
        assert_eq!(v.constructor, "AV_Map");
    }

    #[test]
    fn transfer_validity_from_now_bounds_the_window() {
        let now = 1_700_000_000_000_000;
        let v = TransferValidity::from_now(now);
        assert_eq!(v.requested_at_micros, now);
        assert_eq!(
            v.execute_before_micros,
            now + TRANSFER_VALIDITY_WINDOW_MICROS
        );
        // The window is finite (24h), not the old effectively-infinite deadline.
        assert!(v.execute_before_micros < TRANSFER_EXECUTE_BEFORE_MICROS);
    }

    #[test]
    fn transfer_validity_from_now_clamps_to_max_daml_time() {
        // A near-max `now` must neither panic on overflow nor serialize past the
        // module's max Daml `Time`; it clamps to TRANSFER_EXECUTE_BEFORE_MICROS.
        let v = TransferValidity::from_now(i64::MAX - 5);
        assert_eq!(v.execute_before_micros, TRANSFER_EXECUTE_BEFORE_MICROS);
    }

    #[test]
    fn make_any_value_rejects_unsupported_time_variants() {
        for unsupported in [
            ContextValue::Date("2026-05-19".to_string()),
            ContextValue::Time("2026-05-19T00:00:00Z".to_string()),
            ContextValue::RelTime("PT1H".to_string()),
        ] {
            let err = make_any_value(&unsupported).expect_err("must reject");
            assert!(
                err.to_string().contains("ContextValue::"),
                "error should reference the Rust type, got: {err}",
            );
        }
    }
}
