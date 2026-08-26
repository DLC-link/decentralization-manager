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
//! Two differently-shaped families of accessor build on it:
//!
//! * The **strict** family returns `Result<_, Error>` and is for callers that
//!   want a decode failure to propagate: `field_party_id`, `field_decimal`,
//!   `field_time`, `field_list_len`, and `field_party_list` (formerly
//!   decman's `reward_automation.rs`), plus `extract_party`, `extract_party_id`,
//!   `extract_text`, `extract_int64`, `extract_numeric`, `extract_contract_id`,
//!   `extract_record`, `extract_list`, and `get_field` (formerly decman's
//!   `action_serializer.rs`, which decoded a Ledger API `Value` rather than a
//!   `Record`). Every error is `Error::Decode`, carrying the same message text
//!   the decman `anyhow` versions produced.
//! * The **lenient** family returns `Option` (or, for `field_optional_is_none`,
//!   a fail-safe `bool`) and is for callers that treat a missing or
//!   wrong-shaped field as "not present" rather than an error:
//!   `field_optional_is_none` (formerly decman's `reward_automation.rs`);
//!   `field_party`, `field_text`, `field_numeric`, and `field_timestamp`
//!   (formerly decman's `queries.rs`); and the Set `Party`, `GenMap`, and
//!   `RelTime` extractors `extract_party_set`, `extract_genmap_parties`,
//!   `extract_optional_reltime`, and `extract_reltime` (formerly decman's
//!   `queries.rs`, too).
//!
//! Both families' internal traversal goes through [`record_field`] where the
//! original did; the lenient accessors ported from `queries.rs` keep their own
//! inline traversal unchanged, since they were not written against it.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{List, Record, Value, value};
use chrono::{DateTime, Utc};
use common::canton_id::CantonId;

use crate::error::Error;

/// Return the decoded `value::Sum` for `label`, if present.
pub fn record_field<'a>(rec: &'a Record, label: &str) -> Option<&'a value::Sum> {
    rec.fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| v.sum.as_ref())
}

/// Read a `Party` field and parse it into a [`CantonId`].
pub fn field_party_id(rec: &Record, label: &str) -> Result<CantonId, Error> {
    match record_field(rec, label) {
        Some(value::Sum::Party(p)) => p
            .parse::<CantonId>()
            .map_err(|_| Error::Decode(format!("field `{label}`: invalid party id `{p}`"))),
        _ => Err(Error::Decode(format!(
            "field `{label}`: expected a Party value"
        ))),
    }
}

/// Read a `Numeric` field and parse it into a [`DamlDecimal`] (exact fixed-point).
pub fn field_decimal(rec: &Record, label: &str) -> Result<DamlDecimal, Error> {
    match record_field(rec, label) {
        Some(value::Sum::Numeric(n)) => DamlDecimal::parse(n)
            .map_err(|e| Error::Decode(format!("field `{label}`: invalid decimal `{n}`: {e}"))),
        _ => Err(Error::Decode(format!(
            "field `{label}`: expected a Numeric value"
        ))),
    }
}

/// Read a DAML `Time` field (encoded as microseconds since the epoch) into a
/// UTC timestamp.
pub fn field_time(rec: &Record, label: &str) -> Result<DateTime<Utc>, Error> {
    let micros = match record_field(rec, label) {
        Some(value::Sum::Timestamp(t)) => *t,
        _ => {
            return Err(Error::Decode(format!(
                "field `{label}`: expected a Time value"
            )));
        }
    };
    DateTime::from_timestamp_micros(micros).ok_or_else(|| {
        Error::Decode(format!(
            "field `{label}`: timestamp {micros} micros is out of range"
        ))
    })
}

/// Return true iff `label` is an `Optional` field carrying `None`.
///
/// A missing field, or a non-optional value, returns false — the caller then
/// treats the contract as *not* unassigned (fail-safe: never assign against a
/// coupon we can't confirm is unassigned).
pub fn field_optional_is_none(rec: &Record, label: &str) -> bool {
    matches!(record_field(rec, label), Some(value::Sum::Optional(opt)) if opt.value.is_none())
}

/// Return the number of elements in a `List` field.
pub fn field_list_len(rec: &Record, label: &str) -> Result<usize, Error> {
    match record_field(rec, label) {
        Some(value::Sum::List(l)) => Ok(l.elements.len()),
        _ => Err(Error::Decode(format!(
            "field `{label}`: expected a List value"
        ))),
    }
}

/// Read a list-of-`Party` field, parsing each element into a [`CantonId`].
/// Mirrors `field_contract_id_list`, decoding each element the same way
/// `field_party_id` decodes a single `Party` value.
pub fn field_party_list(rec: &Record, label: &str) -> Result<Vec<CantonId>, Error> {
    let list = match record_field(rec, label) {
        Some(value::Sum::List(l)) => l,
        _ => {
            return Err(Error::Decode(format!(
                "field `{label}`: expected a List value"
            )));
        }
    };
    list.elements
        .iter()
        .map(|elem| match elem.sum.as_ref() {
            Some(value::Sum::Party(p)) => p
                .parse::<CantonId>()
                .map_err(|_| Error::Decode(format!("field `{label}`: invalid party id `{p}`"))),
            _ => Err(Error::Decode(format!(
                "field `{label}`: element is not a Party"
            ))),
        })
        .collect()
}

/// Read a `Party` value.
pub fn extract_party(value: &Value) -> Result<String, Error> {
    match &value.sum {
        Some(value::Sum::Party(p)) => Ok(p.clone()),
        _ => Err(Error::Decode("Expected Party value".to_string())),
    }
}

/// Read a `Party` value and parse it into a [`CantonId`].
pub fn extract_party_id(value: &Value) -> Result<CantonId, Error> {
    let party_str = extract_party(value)?;
    party_str
        .parse()
        .map_err(|_| Error::Decode("Failed to parse party as CantonId".to_string()))
}

/// Read a `Text` value.
pub fn extract_text(value: &Value) -> Result<String, Error> {
    match &value.sum {
        Some(value::Sum::Text(t)) => Ok(t.clone()),
        _ => Err(Error::Decode("Expected Text value".to_string())),
    }
}

/// Read an `Int64` value.
pub fn extract_int64(value: &Value) -> Result<i64, Error> {
    match &value.sum {
        Some(value::Sum::Int64(n)) => Ok(*n),
        _ => Err(Error::Decode("Expected Int64 value".to_string())),
    }
}

/// Read a `Numeric` value.
pub fn extract_numeric(value: &Value) -> Result<String, Error> {
    match &value.sum {
        Some(value::Sum::Numeric(n)) => Ok(n.clone()),
        _ => Err(Error::Decode("Expected Numeric value".to_string())),
    }
}

/// Read a `ContractId` value.
pub fn extract_contract_id(value: &Value) -> Result<String, Error> {
    match &value.sum {
        Some(value::Sum::ContractId(c)) => Ok(c.clone()),
        _ => Err(Error::Decode("Expected ContractId value".to_string())),
    }
}

/// Read a `Record` value.
pub fn extract_record(value: &Value) -> Result<&Record, Error> {
    match &value.sum {
        Some(value::Sum::Record(r)) => Ok(r),
        _ => Err(Error::Decode("Expected Record value".to_string())),
    }
}

/// Read a `List` value.
pub fn extract_list(value: &Value) -> Result<&List, Error> {
    match &value.sum {
        Some(value::Sum::List(l)) => Ok(l),
        _ => Err(Error::Decode("Expected List value".to_string())),
    }
}

/// Look up a field by label on a `Record`, erroring if absent.
pub fn get_field<'a>(record: &'a Record, label: &str) -> Result<&'a Value, Error> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .ok_or_else(|| Error::Decode(format!("Missing field: {label}")))
}

pub fn field_party(record: &Record, label: &str) -> Option<String> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => Some(p.clone()),
            _ => None,
        })
}

pub fn field_text(record: &Record, label: &str) -> Option<String> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Text(t)) => Some(t.clone()),
            _ => None,
        })
}

pub fn field_numeric(record: &Record, label: &str) -> Option<String> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Numeric(n)) => Some(n.clone()),
            _ => None,
        })
}

pub fn field_timestamp(record: &Record, label: &str) -> Option<i64> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Timestamp(t)) => Some(*t),
            _ => None,
        })
}

/// Extract a Set Party (DA.Set.Types:Set) which is stored as Record { map: GenMap<Party, Unit> }
pub fn extract_party_set(value: &Value) -> Option<Vec<String>> {
    // Set Party is represented as a Record containing a GenMap
    match &value.sum {
        Some(value::Sum::Record(record)) => {
            // The record should have a "map" field containing the GenMap
            record
                .fields
                .iter()
                .find(|f| f.label == "map")
                .and_then(|f| f.value.as_ref())
                .and_then(extract_genmap_parties)
        }
        // Fallback: try as GenMap directly
        Some(value::Sum::GenMap(gen_map)) => Some(
            gen_map
                .entries
                .iter()
                .filter_map(|entry| {
                    entry.key.as_ref().and_then(|k| match &k.sum {
                        Some(value::Sum::Party(p)) => Some(p.clone()),
                        _ => None,
                    })
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Extract parties from a GenMap<Party, Unit>
pub fn extract_genmap_parties(value: &Value) -> Option<Vec<String>> {
    match &value.sum {
        Some(value::Sum::GenMap(gen_map)) => Some(
            gen_map
                .entries
                .iter()
                .filter_map(|entry| {
                    entry.key.as_ref().and_then(|k| match &k.sum {
                        Some(value::Sum::Party(p)) => Some(p.clone()),
                        _ => None,
                    })
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Extract Optional RelTime (DA.Time.Types:RelTime is Record { microseconds: Int64 })
pub fn extract_optional_reltime(value: &Value) -> Option<i64> {
    match &value.sum {
        Some(value::Sum::Optional(opt)) => {
            opt.value.as_ref().and_then(|v| extract_reltime(v.as_ref()))
        }
        _ => None,
    }
}

/// Extract RelTime (stored as Record { microseconds: Int64 })
pub fn extract_reltime(value: &Value) -> Option<i64> {
    match &value.sum {
        Some(value::Sum::Record(record)) => record
            .fields
            .iter()
            .find(|f| f.label == "microseconds")
            .and_then(|f| f.value.as_ref())
            .and_then(|v| match &v.sum {
                Some(value::Sum::Int64(i)) => Some(*i),
                _ => None,
            }),
        // Fallback: try as Int64 directly
        Some(value::Sum::Int64(i)) => Some(*i),
        _ => None,
    }
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
    fn assert_err_contains<T: std::fmt::Debug>(result: Result<T, Error>, needle: &str) {
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
    fn field_party_id_parses_valid_party() -> Result<(), Error> {
        let rec = record(vec![("owner", Some(value::Sum::Party(ALICE.to_string())))]);

        let parsed = field_party_id(&rec, "owner")?;

        assert_eq!(parsed, ALICE.parse::<CantonId>().expect("valid party id"));
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
    fn field_decimal_parses_valid_numeric() -> Result<(), Error> {
        let rec = record(vec![(
            "weight",
            Some(value::Sum::Numeric("12.5".to_string())),
        )]);

        let parsed = field_decimal(&rec, "weight")?;

        assert_eq!(parsed, DamlDecimal::parse("12.5").expect("valid decimal"));
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
    fn field_time_parses_valid_timestamp() -> Result<(), Error> {
        let micros = 1_700_000_000_000_000;
        let rec = record(vec![("createdAt", Some(value::Sum::Timestamp(micros)))]);

        let parsed = field_time(&rec, "createdAt")?;

        let expected = DateTime::from_timestamp_micros(micros)
            .expect("test fixture timestamp is out of range");
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
    fn field_list_len_counts_elements() -> Result<(), Error> {
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
    fn field_party_list_parses_each_element() -> Result<(), Error> {
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
            vec![
                ALICE.parse::<CantonId>().expect("valid party id"),
                BOB.parse::<CantonId>().expect("valid party id"),
            ]
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
