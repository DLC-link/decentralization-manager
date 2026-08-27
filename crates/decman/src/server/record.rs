//! Shared Ledger API `Record` field-reading helpers.
//!
//! Reading one field out of a Ledger API protobuf `Record` is a four-link
//! traversal: linear search by label, unwrap `RecordField.value:
//! Option<Value>`, unwrap `Value.sum: Option<value::Sum>`, then match the
//! variant. [`record_field`] is that traversal, factored out once so a
//! protobuf-shape change (e.g. an extra `Option` layer) needs fixing in one
//! place instead of at every call site. It borrows rather than clones, so
//! callers match on the returned `&value::Sum` without an allocation.
//!
//! The typed accessors below (`field_party_id`, `field_decimal`, `field_time`,
//! `field_optional_is_none`, `field_list_len`, `field_party_list`) originated
//! in `reward_automation.rs`; they moved here unchanged so that module and any
//! other can share them. `field_record` was added later — `queries.rs` reads
//! a nested `Record` payload (`transfer`, `instrumentId`, and similar) often
//! enough that the narrowing to the `Record` variant earned its own accessor,
//! same as the others. Other modules (`queries.rs`, `transfer_context.rs`)
//! keep their own, differently-shaped accessors — some return `Option`, some
//! clone into owned `String`s rather than parsing — only their internal
//! traversal now goes through [`record_field`] rather than repeating it.

use anyhow::{Context, anyhow};
use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{Record, value};
use chrono::{DateTime, Utc};

use crate::canton_id::CantonId;

/// Return the decoded `value::Sum` for `label`, if present.
pub(crate) fn record_field<'a>(rec: &'a Record, label: &str) -> Option<&'a value::Sum> {
    rec.fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| v.sum.as_ref())
}

/// Read a `Party` field and parse it into a [`CantonId`].
pub(crate) fn field_party_id(rec: &Record, label: &str) -> anyhow::Result<CantonId> {
    match record_field(rec, label) {
        Some(value::Sum::Party(p)) => p
            .parse::<CantonId>()
            .with_context(|| format!("field `{label}`: invalid party id `{p}`")),
        _ => Err(anyhow!("field `{label}`: expected a Party value")),
    }
}

/// Read a `Numeric` field and parse it into a [`DamlDecimal`] (exact fixed-point).
pub(crate) fn field_decimal(rec: &Record, label: &str) -> anyhow::Result<DamlDecimal> {
    match record_field(rec, label) {
        Some(value::Sum::Numeric(n)) => DamlDecimal::parse(n)
            .map_err(|e| anyhow!("field `{label}`: invalid decimal `{n}`: {e}")),
        _ => Err(anyhow!("field `{label}`: expected a Numeric value")),
    }
}

/// Read a DAML `Time` field (encoded as microseconds since the epoch) into a
/// UTC timestamp.
pub(crate) fn field_time(rec: &Record, label: &str) -> anyhow::Result<DateTime<Utc>> {
    let micros = match record_field(rec, label) {
        Some(value::Sum::Timestamp(t)) => *t,
        _ => return Err(anyhow!("field `{label}`: expected a Time value")),
    };
    DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| anyhow!("field `{label}`: timestamp {micros} micros is out of range"))
}

/// Return true iff `label` is an `Optional` field carrying `None`.
///
/// A missing field, or a non-optional value, returns false — the caller then
/// treats the contract as *not* unassigned (fail-safe: never assign against a
/// coupon we can't confirm is unassigned).
pub(crate) fn field_optional_is_none(rec: &Record, label: &str) -> bool {
    matches!(record_field(rec, label), Some(value::Sum::Optional(opt)) if opt.value.is_none())
}

/// Return the number of elements in a `List` field.
pub(crate) fn field_list_len(rec: &Record, label: &str) -> anyhow::Result<usize> {
    match record_field(rec, label) {
        Some(value::Sum::List(l)) => Ok(l.elements.len()),
        _ => Err(anyhow!("field `{label}`: expected a List value")),
    }
}

/// Return the field's value if it is a `Record`, e.g. a token-standard
/// nested payload (`transfer`, `instrumentId`, and similar). Unlike the
/// other typed accessors this stays borrowed and untyped rather than parsing
/// into an application type — callers read further fields out of the nested
/// record with their own `record_field`/`field_party`-family calls.
pub(crate) fn field_record<'a>(rec: &'a Record, label: &str) -> Option<&'a Record> {
    match record_field(rec, label) {
        Some(value::Sum::Record(r)) => Some(r),
        _ => None,
    }
}

/// Read a list-of-`Party` field, parsing each element into a [`CantonId`].
/// Mirrors `field_contract_id_list`, decoding each element the same way
/// `field_party_id` decodes a single `Party` value.
pub(crate) fn field_party_list(rec: &Record, label: &str) -> anyhow::Result<Vec<CantonId>> {
    let list = match record_field(rec, label) {
        Some(value::Sum::List(l)) => l,
        _ => return Err(anyhow!("field `{label}`: expected a List value")),
    };
    list.elements
        .iter()
        .map(|elem| match elem.sum.as_ref() {
            Some(value::Sum::Party(p)) => p
                .parse::<CantonId>()
                .with_context(|| format!("field `{label}`: invalid party id `{p}`")),
            _ => Err(anyhow!("field `{label}`: element is not a Party")),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{List, Optional, RecordField, Value};

    use super::*;

    /// Valid-shape party ids (a 34-byte SHA-256 multihash namespace, hex
    /// encoded) so `.parse::<CantonId>()` succeeds.
    const ALICE: &str =
        "alice::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "bob::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn record(fields: Vec<(&str, Option<value::Sum>)>) -> Record {
        Record {
            record_id: None,
            fields: fields
                .into_iter()
                .map(|(label, sum)| RecordField {
                    label: label.to_string(),
                    value: sum.map(|sum| Value { sum: Some(sum) }),
                })
                .collect(),
        }
    }

    /// A field present with `value: None` (the middle `Option` layer empty) —
    /// distinct from the field being absent entirely.
    fn record_with_empty_value(label: &str) -> Record {
        Record {
            record_id: None,
            fields: vec![RecordField {
                label: label.to_string(),
                value: None,
            }],
        }
    }

    /// Assert a `Result` is `Err` and its rendered message contains `needle`.
    /// Test-only: a non-matching error trips a `panic!` (an allowed assertion
    /// macro), keeping the suite free of `.unwrap_err()` / `.expect_err()`.
    fn assert_err_contains<T: std::fmt::Debug>(result: anyhow::Result<T>, needle: &str) {
        match result {
            Ok(v) => panic!("expected Err containing {needle:?}, got Ok({v:?})"),
            Err(e) => assert!(
                e.to_string().contains(needle),
                "error {e:?} did not contain {needle:?}"
            ),
        }
    }

    #[test]
    fn record_field_finds_present_field() {
        let rec = record(vec![(
            "amount",
            Some(value::Sum::Numeric("1.0".to_string())),
        )]);

        let found = record_field(&rec, "amount");

        assert!(matches!(found, Some(value::Sum::Numeric(n)) if n == "1.0"));
    }

    #[test]
    fn record_field_returns_none_for_absent_label() {
        let rec = record(vec![(
            "amount",
            Some(value::Sum::Numeric("1.0".to_string())),
        )]);

        assert!(record_field(&rec, "missing").is_none());
    }

    #[test]
    fn record_field_returns_none_when_value_is_none() {
        let rec = record_with_empty_value("amount");

        assert!(record_field(&rec, "amount").is_none());
    }

    #[test]
    fn field_party_id_parses_valid_party() -> anyhow::Result<()> {
        let rec = record(vec![("owner", Some(value::Sum::Party(ALICE.to_string())))]);

        let parsed = field_party_id(&rec, "owner")?;

        assert_eq!(parsed, ALICE.parse::<CantonId>()?);
        Ok(())
    }

    #[test]
    fn field_party_id_errs_on_absent_field() {
        let rec = record(vec![]);

        assert!(field_party_id(&rec, "owner").is_err());
    }

    #[test]
    fn field_party_id_errs_on_empty_value() {
        let rec = record_with_empty_value("owner");

        assert!(field_party_id(&rec, "owner").is_err());
    }

    #[test]
    fn field_party_id_errs_on_wrong_variant() {
        let rec = record(vec![(
            "owner",
            Some(value::Sum::Text("not-a-party".to_string())),
        )]);

        assert_err_contains(field_party_id(&rec, "owner"), "expected a Party value");
    }

    #[test]
    fn field_decimal_parses_valid_numeric() -> anyhow::Result<()> {
        let rec = record(vec![(
            "weight",
            Some(value::Sum::Numeric("12.5".to_string())),
        )]);

        let parsed = field_decimal(&rec, "weight")?;

        assert_eq!(parsed, DamlDecimal::parse("12.5")?);
        Ok(())
    }

    #[test]
    fn field_decimal_errs_on_absent_field() {
        let rec = record(vec![]);

        assert!(field_decimal(&rec, "weight").is_err());
    }

    #[test]
    fn field_decimal_errs_on_empty_value() {
        let rec = record_with_empty_value("weight");

        assert!(field_decimal(&rec, "weight").is_err());
    }

    #[test]
    fn field_decimal_errs_on_wrong_variant() {
        let rec = record(vec![("weight", Some(value::Sum::Text("12.5".to_string())))]);

        assert_err_contains(field_decimal(&rec, "weight"), "expected a Numeric value");
    }

    #[test]
    fn field_time_parses_valid_timestamp() -> anyhow::Result<()> {
        let micros = 1_700_000_000_000_000;
        let rec = record(vec![("createdAt", Some(value::Sum::Timestamp(micros)))]);

        let parsed = field_time(&rec, "createdAt")?;

        let expected = DateTime::from_timestamp_micros(micros)
            .context("test fixture timestamp is out of range")?;
        assert_eq!(parsed, expected);
        Ok(())
    }

    #[test]
    fn field_time_errs_on_absent_field() {
        let rec = record(vec![]);

        assert!(field_time(&rec, "createdAt").is_err());
    }

    #[test]
    fn field_time_errs_on_empty_value() {
        let rec = record_with_empty_value("createdAt");

        assert!(field_time(&rec, "createdAt").is_err());
    }

    #[test]
    fn field_time_errs_on_wrong_variant() {
        let rec = record(vec![("createdAt", Some(value::Sum::Int64(1)))]);

        assert_err_contains(field_time(&rec, "createdAt"), "expected a Time value");
    }

    #[test]
    fn field_optional_is_none_true_for_optional_none() {
        let rec = record(vec![(
            "beneficiary",
            Some(value::Sum::Optional(Box::new(Optional { value: None }))),
        )]);

        assert!(field_optional_is_none(&rec, "beneficiary"));
    }

    #[test]
    fn field_optional_is_none_false_for_optional_some() {
        let rec = record(vec![(
            "beneficiary",
            Some(value::Sum::Optional(Box::new(Optional {
                value: Some(Box::new(Value {
                    sum: Some(value::Sum::Party(ALICE.to_string())),
                })),
            }))),
        )]);

        assert!(!field_optional_is_none(&rec, "beneficiary"));
    }

    #[test]
    fn field_optional_is_none_false_on_absent_field() {
        // Fail-safe: a missing field must never read as "confirmed absent".
        let rec = record(vec![]);

        assert!(!field_optional_is_none(&rec, "beneficiary"));
    }

    #[test]
    fn field_optional_is_none_false_on_wrong_variant() {
        let rec = record(vec![(
            "beneficiary",
            Some(value::Sum::Text("x".to_string())),
        )]);

        assert!(!field_optional_is_none(&rec, "beneficiary"));
    }

    #[test]
    fn field_record_returns_nested_record() -> anyhow::Result<()> {
        let nested = record(vec![("id", Some(value::Sum::Text("abc".to_string())))]);
        let rec = record(vec![(
            "instrumentId",
            Some(value::Sum::Record(nested.clone())),
        )]);

        let found = field_record(&rec, "instrumentId");

        assert_eq!(found, Some(&nested));
        Ok(())
    }

    #[test]
    fn field_record_none_on_absent_field() {
        let rec = record(vec![]);

        assert!(field_record(&rec, "instrumentId").is_none());
    }

    #[test]
    fn field_record_none_on_empty_value() {
        let rec = record_with_empty_value("instrumentId");

        assert!(field_record(&rec, "instrumentId").is_none());
    }

    #[test]
    fn field_record_none_on_wrong_variant() {
        let rec = record(vec![(
            "instrumentId",
            Some(value::Sum::Text("not-a-record".to_string())),
        )]);

        assert!(field_record(&rec, "instrumentId").is_none());
    }

    #[test]
    fn field_list_len_counts_elements() -> anyhow::Result<()> {
        let rec = record(vec![(
            "split",
            Some(value::Sum::List(List {
                elements: vec![
                    Value {
                        sum: Some(value::Sum::Numeric("1.0".to_string())),
                    },
                    Value {
                        sum: Some(value::Sum::Numeric("2.0".to_string())),
                    },
                ],
            })),
        )]);

        assert_eq!(field_list_len(&rec, "split")?, 2);
        Ok(())
    }

    #[test]
    fn field_list_len_errs_on_absent_field() {
        let rec = record(vec![]);

        assert!(field_list_len(&rec, "split").is_err());
    }

    #[test]
    fn field_list_len_errs_on_empty_value() {
        let rec = record_with_empty_value("split");

        assert!(field_list_len(&rec, "split").is_err());
    }

    #[test]
    fn field_list_len_errs_on_wrong_variant() {
        let rec = record(vec![("split", Some(value::Sum::Text("x".to_string())))]);

        assert_err_contains(field_list_len(&rec, "split"), "expected a List value");
    }

    #[test]
    fn field_party_list_parses_each_element() -> anyhow::Result<()> {
        let rec = record(vec![(
            "assigners",
            Some(value::Sum::List(List {
                elements: vec![
                    Value {
                        sum: Some(value::Sum::Party(ALICE.to_string())),
                    },
                    Value {
                        sum: Some(value::Sum::Party(BOB.to_string())),
                    },
                ],
            })),
        )]);

        let parsed = field_party_list(&rec, "assigners")?;

        assert_eq!(
            parsed,
            vec![ALICE.parse::<CantonId>()?, BOB.parse::<CantonId>()?]
        );
        Ok(())
    }

    #[test]
    fn field_party_list_errs_on_absent_field() {
        let rec = record(vec![]);

        assert!(field_party_list(&rec, "assigners").is_err());
    }

    #[test]
    fn field_party_list_errs_on_empty_value() {
        let rec = record_with_empty_value("assigners");

        assert!(field_party_list(&rec, "assigners").is_err());
    }

    #[test]
    fn field_party_list_errs_on_wrong_variant() {
        let rec = record(vec![("assigners", Some(value::Sum::Text("x".to_string())))]);

        assert_err_contains(field_party_list(&rec, "assigners"), "expected a List value");
    }

    #[test]
    fn field_party_list_errs_on_non_party_element() {
        let rec = record(vec![(
            "assigners",
            Some(value::Sum::List(List {
                elements: vec![Value {
                    sum: Some(value::Sum::Text("not-a-party".to_string())),
                }],
            })),
        )]);

        assert_err_contains(
            field_party_list(&rec, "assigners"),
            "element is not a Party",
        );
    }
}
