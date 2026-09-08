//! Deterministic encoding primitives shared by every hashing scheme version.
//!
//! Protobuf serialization is not canonical, so Canton defines its own byte
//! encoding over the decoded message tree and hashes that. This module is a
//! port of the primitives in Canton's `HashBuilder` / `LfValueHashBuilder`
//! (`community/base/.../protocol/hash/`), cross-checked against the reference
//! Python implementation Canton ships for external signers
//! (`examples/08-interactive-submission/daml_transaction_hashing_common.py`).

use canton_proto_rs::com::daml::ledger::api::v2::{Identifier, Value, value};
use sha2::{Digest, Sha256};

use crate::error::Result;

/// `HashPurpose.PreparedSubmission` (48) as a 4-byte big-endian integer. Every
/// hash in the scheme is domain-separated with this prefix.
pub(super) const HASH_PURPOSE: [u8; 4] = [0x00, 0x00, 0x00, 0x30];

/// Nesting limit for [`Encoder::value`]. The payload is supplied by a
/// potentially hostile coordinator, so a deeply nested `Value` must not be
/// able to blow the stack — it is refused instead. Real Daml arguments are
/// nowhere near this deep.
const MAX_VALUE_DEPTH: usize = 100;

/// SHA-256 of `data`, the only hash algorithm the scheme uses.
pub(super) fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Accumulates the deterministic encoding of a message tree.
#[derive(Default)]
pub(super) struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// An encoder pre-loaded with the hash purpose — the starting point for
    /// every hash the scheme produces (as opposed to a nested encoding whose
    /// bytes are folded into an enclosing one).
    pub(super) fn with_purpose() -> Self {
        let mut encoder = Self::new();
        encoder.raw(&HASH_PURPOSE);
        encoder
    }

    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub(super) fn digest(&self) -> [u8; 32] {
        sha256(&self.buf)
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Append a fixed-size hash. No length prefix: hashes have a fixed width.
    pub(super) fn add_hash(&mut self, hash: &[u8]) {
        self.raw(hash);
    }

    pub(super) fn byte(&mut self, byte: u8) {
        self.buf.push(byte);
    }

    pub(super) fn bool(&mut self, value: bool) {
        self.byte(u8::from(value));
    }

    pub(super) fn int32(&mut self, value: i32) {
        self.raw(&value.to_be_bytes());
    }

    pub(super) fn int64(&mut self, value: i64) {
        self.raw(&value.to_be_bytes());
    }

    /// Canton encodes lengths as Java `int`s. A payload claiming more than
    /// `i32::MAX` elements cannot be encoded the way Canton would, so refuse
    /// rather than truncate into a different (attacker-chosen) encoding.
    fn length(&mut self, length: usize) -> Result {
        let length = i32::try_from(length)
            .map_err(|_| anyhow::anyhow!("length {length} exceeds the int32 the encoding uses"))?;
        self.int32(length);
        Ok(())
    }

    pub(super) fn bytes(&mut self, bytes: &[u8]) -> Result {
        self.length(bytes.len())?;
        self.raw(bytes);
        Ok(())
    }

    pub(super) fn string(&mut self, value: &str) -> Result {
        self.bytes(value.as_bytes())
    }

    /// Canton hashes contract ids as their raw bytes; the protobuf carries the
    /// hex rendering of those bytes.
    pub(super) fn hex_string(&mut self, value: &str) -> Result {
        let raw = hex::decode(value)
            .map_err(|e| anyhow::anyhow!("expected a hex-encoded value, got {value:?}: {e}"))?;
        self.bytes(&raw)
    }

    /// A `Set[String]` on the Canton side: sorted and deduplicated before
    /// hashing, so the encoding does not depend on the order the coordinator
    /// happened to serialize the repeated field in.
    pub(super) fn string_set(&mut self, values: &[String]) -> Result {
        let mut sorted: Vec<&str> = values.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.dedup();
        self.length(sorted.len())?;
        for value in sorted {
            self.string(value)?;
        }
        Ok(())
    }

    /// A repeated field whose order is part of the encoding (list elements,
    /// node children, query results).
    pub(super) fn repeated<T>(
        &mut self,
        values: &[T],
        mut encode: impl FnMut(&mut Self, &T) -> Result,
    ) -> Result {
        self.length(values.len())?;
        for value in values {
            encode(self, value)?;
        }
        Ok(())
    }

    pub(super) fn optional<T>(
        &mut self,
        value: Option<&T>,
        encode: impl FnOnce(&mut Self, &T) -> Result,
    ) -> Result {
        match value {
            Some(value) => {
                self.byte(0x01);
                encode(self, value)
            }
            None => {
                self.byte(0x00);
                Ok(())
            }
        }
    }

    pub(super) fn identifier(&mut self, identifier: &Identifier) -> Result {
        self.string(&identifier.package_id)?;
        self.dotted_name(&identifier.module_name)?;
        self.dotted_name(&identifier.entity_name)
    }

    /// Canton hashes a dotted name as the list of its segments, so the
    /// segment boundaries are part of the hash.
    fn dotted_name(&mut self, name: &str) -> Result {
        let segments: Vec<&str> = name.split('.').collect();
        self.length(segments.len())?;
        for segment in segments {
            self.string(segment)?;
        }
        Ok(())
    }

    /// Encode a Daml-LF value. Each variant carries a distinct type tag so a
    /// value of one type can never hash to the same bytes as a value of
    /// another.
    pub(super) fn value(&mut self, value: &Value) -> Result {
        self.value_at_depth(value, 0)
    }

    fn value_at_depth(&mut self, value: &Value, depth: usize) -> Result {
        if depth > MAX_VALUE_DEPTH {
            anyhow::bail!("value nesting exceeds the {MAX_VALUE_DEPTH} level limit");
        }
        let sum = value
            .sum
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("value has no variant set"))?;
        match sum {
            value::Sum::Unit(()) => self.byte(0x00),
            value::Sum::Bool(v) => {
                self.byte(0x01);
                self.bool(*v);
            }
            value::Sum::Int64(v) => {
                self.byte(0x02);
                self.int64(*v);
            }
            value::Sum::Numeric(v) => {
                self.byte(0x03);
                self.string(v)?;
            }
            value::Sum::Timestamp(v) => {
                self.byte(0x04);
                self.int64(*v);
            }
            value::Sum::Date(v) => {
                self.byte(0x05);
                self.int32(*v);
            }
            value::Sum::Party(v) => {
                self.byte(0x06);
                self.string(v)?;
            }
            value::Sum::Text(v) => {
                self.byte(0x07);
                self.string(v)?;
            }
            value::Sum::ContractId(v) => {
                self.byte(0x08);
                self.hex_string(v)?;
            }
            value::Sum::Optional(v) => {
                self.byte(0x09);
                match v.value.as_deref() {
                    Some(inner) => {
                        self.byte(0x01);
                        self.value_at_depth(inner, depth + 1)?;
                    }
                    None => self.byte(0x00),
                }
            }
            value::Sum::List(v) => {
                self.byte(0x0a);
                self.length(v.elements.len())?;
                for element in &v.elements {
                    self.value_at_depth(element, depth + 1)?;
                }
            }
            value::Sum::TextMap(v) => {
                self.byte(0x0b);
                self.length(v.entries.len())?;
                for entry in &v.entries {
                    self.string(&entry.key)?;
                    self.value_at_depth(
                        required(entry.value.as_ref(), "text map entry value")?,
                        depth + 1,
                    )?;
                }
            }
            value::Sum::Record(v) => {
                self.byte(0x0c);
                self.optional(v.record_id.as_ref(), Self::identifier)?;
                self.length(v.fields.len())?;
                for field in &v.fields {
                    // The label is always encoded as a set optional: Canton
                    // enriches the prepared transaction, so every record field
                    // carries its label by the time it reaches a signer.
                    self.byte(0x01);
                    self.string(&field.label)?;
                    self.value_at_depth(
                        required(field.value.as_ref(), "record field value")?,
                        depth + 1,
                    )?;
                }
            }
            value::Sum::Variant(v) => {
                self.byte(0x0d);
                self.optional(v.variant_id.as_ref(), Self::identifier)?;
                self.string(&v.constructor)?;
                self.value_at_depth(required(v.value.as_deref(), "variant value")?, depth + 1)?;
            }
            value::Sum::Enum(v) => {
                self.byte(0x0e);
                self.optional(v.enum_id.as_ref(), Self::identifier)?;
                self.string(&v.constructor)?;
            }
            value::Sum::GenMap(v) => {
                self.byte(0x0f);
                self.length(v.entries.len())?;
                for entry in &v.entries {
                    self.value_at_depth(
                        required(entry.key.as_ref(), "gen map entry key")?,
                        depth + 1,
                    )?;
                    self.value_at_depth(
                        required(entry.value.as_ref(), "gen map entry value")?,
                        depth + 1,
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Unwrap a protobuf field the encoding treats as required.
pub(super) fn required<T>(value: Option<T>, what: &str) -> Result<T> {
    value.ok_or_else(|| anyhow::anyhow!("prepared transaction is missing its {what}"))
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{
        List, Optional, Record, RecordField, TextMap, text_map,
    };

    use super::*;

    fn text(value: &str) -> Value {
        Value {
            sum: Some(value::Sum::Text(value.to_string())),
        }
    }

    fn encode(value: &Value) -> Result<Vec<u8>> {
        let mut encoder = Encoder::new();
        encoder.value(value)?;
        Ok(encoder.into_bytes())
    }

    /// Canton's own trace for a text value: type tag, then the length-prefixed
    /// UTF-8 bytes (see `v2/NodeHashTest.scala`, "Arg" section).
    #[test]
    fn encodes_text_like_canton() -> Result {
        assert_eq!(encode(&text("hello"))?, b"\x07\x00\x00\x00\x05hello");
        Ok(())
    }

    #[test]
    fn encodes_identifier_as_package_and_dotted_names() -> Result {
        let mut encoder = Encoder::new();
        encoder.identifier(&Identifier {
            package_id: "package".to_string(),
            module_name: "module".to_string(),
            entity_name: "name".to_string(),
        })?;
        let expected = [
            b"\x00\x00\x00\x07package".as_slice(),
            b"\x00\x00\x00\x01\x00\x00\x00\x06module",
            b"\x00\x00\x00\x01\x00\x00\x00\x04name",
        ]
        .concat();
        assert_eq!(encoder.into_bytes(), expected);
        Ok(())
    }

    #[test]
    fn splits_dotted_module_names_into_segments() -> Result {
        let mut encoder = Encoder::new();
        encoder.identifier(&Identifier {
            package_id: "p".to_string(),
            module_name: "a.b".to_string(),
            entity_name: "c".to_string(),
        })?;
        let expected = [
            b"\x00\x00\x00\x01p".as_slice(),
            b"\x00\x00\x00\x02\x00\x00\x00\x01a\x00\x00\x00\x01b",
            b"\x00\x00\x00\x01\x00\x00\x00\x01c",
        ]
        .concat();
        assert_eq!(encoder.into_bytes(), expected);
        Ok(())
    }

    /// Party sets are hashed sorted and deduplicated, so a coordinator cannot
    /// change the encoding by reordering the repeated field.
    #[test]
    fn string_set_is_order_and_duplicate_independent() -> Result {
        let mut ordered = Encoder::new();
        ordered.string_set(&["alice".to_string(), "bob".to_string()])?;
        let mut shuffled = Encoder::new();
        shuffled.string_set(&["bob".to_string(), "alice".to_string(), "bob".to_string()])?;
        assert_eq!(ordered.into_bytes(), shuffled.into_bytes());
        Ok(())
    }

    /// A list's order IS part of the encoding — the sorting above must not
    /// leak into ordered collections.
    #[test]
    fn list_order_changes_the_encoding() -> Result {
        let forwards = List {
            elements: vec![text("a"), text("b")],
        };
        let backwards = List {
            elements: vec![text("b"), text("a")],
        };
        let encode_list = |list: List| {
            encode(&Value {
                sum: Some(value::Sum::List(list)),
            })
        };
        assert_ne!(encode_list(forwards)?, encode_list(backwards)?);
        Ok(())
    }

    /// The type tags exist to stop a value of one type colliding with another
    /// (Canton's example: `Some(42)` vs a plain integer).
    #[test]
    fn distinct_types_do_not_collide() -> Result {
        let some_text = Value {
            sum: Some(value::Sum::Optional(Box::new(Optional {
                value: Some(Box::new(text("hello"))),
            }))),
        };
        assert_ne!(encode(&some_text)?, encode(&text("hello"))?);
        Ok(())
    }

    #[test]
    fn none_and_some_differ() -> Result {
        let none = Value {
            sum: Some(value::Sum::Optional(Box::new(Optional { value: None }))),
        };
        let some_unit = Value {
            sum: Some(value::Sum::Optional(Box::new(Optional {
                value: Some(Box::new(Value {
                    sum: Some(value::Sum::Unit(())),
                })),
            }))),
        };
        assert_ne!(encode(&none)?, encode(&some_unit)?);
        Ok(())
    }

    #[test]
    fn record_field_label_is_part_of_the_encoding() -> Result {
        let record = |label: &str| Value {
            sum: Some(value::Sum::Record(Record {
                record_id: None,
                fields: vec![RecordField {
                    label: label.to_string(),
                    value: Some(text("hello")),
                }],
            })),
        };
        assert_ne!(encode(&record("amount"))?, encode(&record("recipient"))?);
        Ok(())
    }

    #[test]
    fn text_map_keys_are_part_of_the_encoding() -> Result {
        let map = |key: &str| Value {
            sum: Some(value::Sum::TextMap(TextMap {
                entries: vec![text_map::Entry {
                    key: key.to_string(),
                    value: Some(text("v")),
                }],
            })),
        };
        assert_ne!(encode(&map("a"))?, encode(&map("b"))?);
        Ok(())
    }

    #[test]
    fn rejects_a_value_with_no_variant() {
        assert!(encode(&Value { sum: None }).is_err());
    }

    #[test]
    fn rejects_a_non_hex_contract_id() {
        let value = Value {
            sum: Some(value::Sum::ContractId("not-hex".to_string())),
        };
        assert!(encode(&value).is_err());
    }

    /// A hostile payload must not be able to blow the stack.
    #[test]
    fn rejects_deeply_nested_values() {
        let mut value = text("bottom");
        for _ in 0..(MAX_VALUE_DEPTH + 5) {
            value = Value {
                sum: Some(value::Sum::Optional(Box::new(Optional {
                    value: Some(Box::new(value)),
                }))),
            };
        }
        assert!(encode(&value).is_err());
    }
}
