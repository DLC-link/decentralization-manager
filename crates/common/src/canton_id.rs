use std::{fmt, marker::PhantomData};

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::error::Result;

/// Length of a namespace: 2 bytes multihash prefix + 32 bytes SHA-256 hash
pub const NAMESPACE_LENGTH: usize = 34;

/// Newtype wrapper around a fixed-length namespace byte array
///
/// Canton namespaces are multihash-encoded SHA-256 hashes:
/// - First 2 bytes: multihash prefix (0x1220 for SHA-256)
/// - Next 32 bytes: SHA-256 hash
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Namespace(#[serde(with = "BigArray")] [u8; NAMESPACE_LENGTH]);

impl Namespace {
    /// Create a new Namespace from a fixed-length array
    pub fn new(bytes: [u8; NAMESPACE_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Parse a Namespace from a hex string
    pub fn from_hex(hex_str: &str) -> Result<Self> {
        let bytes = hex::decode(hex_str)?;
        if bytes.len() != NAMESPACE_LENGTH {
            anyhow::bail!(
                "Invalid namespace length: expected {NAMESPACE_LENGTH} bytes, got {count}",
                count = bytes.len()
            );
        }
        let mut arr = [0u8; NAMESPACE_LENGTH];
        arr.copy_from_slice(&bytes);
        Ok(Self(arr))
    }

    /// Get the namespace as a hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Get a reference to the underlying byte array
    pub fn as_bytes(&self) -> &[u8; NAMESPACE_LENGTH] {
        &self.0
    }

    /// Get the underlying byte array
    pub fn into_bytes(self) -> [u8; NAMESPACE_LENGTH] {
        self.0
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Represents a Canton ID with prefix and namespace
///
/// Canton IDs have the format: `{prefix}::{hex_encoded_namespace}`
/// Examples:
/// - `participant::1220c4010d6883f367...`
/// - `sv::1220034c3a6a9454...`
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CantonId {
    /// The prefix (e.g., "participant", "sv")
    pub prefix: String,
    /// The namespace (multihash-encoded identifier)
    pub namespace: Namespace,
    _p: PhantomData<()>,
}

impl serde::Serialize for CantonId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for CantonId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

// `CantonId` serializes as a plain `prefix::namespace` string, so its generated
// TypeScript form is `string`. (A manual impl rather than `#[derive(TS)]`, which
// would emit a struct from the fields.)
#[cfg(feature = "typegen")]
impl ts_rs::TS for CantonId {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;
    fn name(_: &ts_rs::Config) -> String {
        "string".to_owned()
    }
    fn inline(cfg: &ts_rs::Config) -> String {
        <Self as ts_rs::TS>::name(cfg)
    }
}

impl CantonId {
    /// Create a new Canton ID from prefix and namespace
    pub fn new(prefix: String, namespace: Namespace) -> Self {
        Self {
            prefix,
            namespace,
            _p: PhantomData,
        }
    }

    /// Parse a Canton ID from Canton's string format
    ///
    /// Expected format: `prefix::hex_encoded_namespace`
    /// Example: `participant::1220c4010d6883f367...`
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split("::").collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid Canton ID format: expected 'prefix::namespace', got '{s}'");
        }

        let prefix = parts[0].to_string();
        let namespace = Namespace::from_hex(parts[1])?;

        Ok(Self {
            prefix,
            namespace,
            _p: PhantomData,
        })
    }

    /// Parse a Canton ID from file content (strips "PAR::" prefix if present)
    pub fn parse_from_file(content: &str) -> Result<Self> {
        let trimmed = content.trim();
        let id_str = match trimmed.strip_prefix("PAR::") {
            Some(stripped) => stripped,
            None => trimmed,
        };
        Self::parse(id_str)
    }

    /// Convert to file storage format with "PAR::" prefix
    pub fn to_file_format(&self) -> String {
        format!("PAR::{self}")
    }
}

impl fmt::Display for CantonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{prefix}::{namespace}",
            prefix = self.prefix,
            namespace = self.namespace.to_hex()
        )
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for CantonId {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .description(Some("Canton ID in format 'prefix::hex_namespace'"))
            .examples([Some(serde_json::json!(
                "participant::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892"
            ))])
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for CantonId {}

impl std::str::FromStr for CantonId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// Maximum length allowed for a decentralized-party prefix. The prefix becomes
/// the identifier part of a Canton party id (`<prefix>::<namespace>`), which
/// Canton caps at 185 characters (`LfPartyId` / `String185`); we keep a margin.
pub const MAX_PARTY_ID_PREFIX_LEN: usize = 180;

/// Validate a decentralized-party prefix before it is used to build a Canton
/// party id (`<prefix>::<namespace>`).
///
/// Canton validates that identifier (via `LfPartyId`, ≤185 chars, no `::`) when
/// it deserialises the onboarding topology transaction; a character outside its
/// safe set makes the submission fail with an opaque proto error deep inside
/// the workflow. We enforce a stricter, unambiguous allowlist up-front so a bad
/// prefix is rejected immediately with a clear message:
///
/// - ASCII letters, digits, `-` and `_` only,
/// - must start with a letter,
/// - 1..=[`MAX_PARTY_ID_PREFIX_LEN`] characters.
///
/// `:` and space are deliberately excluded even though `LfPartyId` permits them:
/// `::` is the party-id delimiter we split on, and spaces break CSV sharing and
/// URLs.
///
/// # Errors
///
/// Returns a human-readable message describing the first rule violated.
pub fn validate_party_id_prefix(prefix: &str) -> Result<(), String> {
    if prefix.is_empty() {
        return Err("Party prefix must not be empty".to_string());
    }

    let len = prefix.chars().count();
    if len > MAX_PARTY_ID_PREFIX_LEN {
        return Err(format!(
            "Party prefix must be at most {MAX_PARTY_ID_PREFIX_LEN} characters (got {len})"
        ));
    }

    if !prefix
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return Err("Party prefix must start with a letter (a-z, A-Z)".to_string());
    }

    if let Some(bad) = prefix
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(format!(
            "Party prefix contains invalid character {bad:?}; only ASCII letters, \
             digits, '-' and '_' are allowed"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_from_hex() -> Result {
        let ns = Namespace::from_hex(
            "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
        )?;
        assert_eq!(
            ns.to_hex(),
            "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892"
        );
        Ok(())
    }

    #[test]
    fn test_namespace_invalid_length() {
        let result = Namespace::from_hex("1220abcd");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse() -> Result {
        let id = CantonId::parse(
            "participant::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
        )?;
        assert_eq!(id.prefix, "participant");
        assert_eq!(
            id.namespace.to_hex(),
            "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892"
        );
        Ok(())
    }

    #[test]
    fn test_parse_from_file() -> Result {
        let id = CantonId::parse_from_file(
            "PAR::participant::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
        )?;
        assert_eq!(id.prefix, "participant");
        Ok(())
    }

    #[test]
    fn test_to_file_format() -> Result {
        let ns = Namespace::from_hex(
            "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
        )?;
        let id = CantonId::new("participant".to_string(), ns);
        assert_eq!(
            id.to_file_format(),
            "PAR::participant::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892"
        );
        Ok(())
    }

    #[test]
    fn test_roundtrip() -> Result {
        let original =
            "participant::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let id = CantonId::parse(original)?;
        assert_eq!(id.to_string(), original);
        Ok(())
    }

    #[test]
    fn party_prefix_accepts_valid() {
        for ok in ["UAT1", "test-network-1", "iBTC_catalyst-testnet", "a"] {
            assert!(
                validate_party_id_prefix(ok).is_ok(),
                "expected {ok:?} to be accepted"
            );
        }
    }

    #[test]
    fn party_prefix_rejects_empty() {
        assert!(validate_party_id_prefix("").is_err());
    }

    #[test]
    fn party_prefix_rejects_disallowed_chars() {
        // Includes the user-reported set plus the delimiter, space, and unicode.
        for bad in [
            "a.b", "a,b", "a<b", "a>b", "a?b", "a!b", "a b", "a:b", "a::b", "a@b", "a/b", "café",
        ] {
            assert!(
                validate_party_id_prefix(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn party_prefix_requires_leading_letter() {
        for bad in ["1abc", "-abc", "_abc"] {
            assert!(
                validate_party_id_prefix(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn party_prefix_enforces_max_length() {
        let at_limit = "a".repeat(MAX_PARTY_ID_PREFIX_LEN);
        assert!(validate_party_id_prefix(&at_limit).is_ok());
        let too_long = "a".repeat(MAX_PARTY_ID_PREFIX_LEN + 1);
        assert!(validate_party_id_prefix(&too_long).is_err());
    }

    /// A valid 34-byte (68 hex char) namespace, reused across the rejection
    /// tests below.
    const VALID_NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    #[test]
    fn parse_rejects_malformed_input() {
        // The parser is the gateway for every participant/peer id read from
        // Canton and from files; its rejection branches must hold.
        let bad_inputs = [
            "".to_string(),                             // empty
            "noColons".to_string(),                     // missing the "::" delimiter
            "a::b::c".to_string(),                      // too many segments
            "participant::".to_string(),                // empty namespace
            "participant::zz".to_string(),              // non-hex namespace
            format!("participant::{}", "g".repeat(68)), // right length, non-hex
        ];
        for bad in &bad_inputs {
            assert!(
                CantonId::parse(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn namespace_from_hex_rejects_bad_input() {
        // Non-hex characters.
        assert!(Namespace::from_hex("zz").is_err());
        // Odd-length hex (hex::decode rejects).
        assert!(Namespace::from_hex("121").is_err());
        // Valid hex but wrong (over-) length: 35 bytes instead of 34.
        assert!(Namespace::from_hex(&"12".repeat(NAMESPACE_LENGTH + 1)).is_err());
        // Valid hex but wrong (under-) length: 1 byte.
        assert!(Namespace::from_hex("12").is_err());
    }

    #[test]
    fn parse_from_file_without_par_prefix() -> Result {
        // The `None` branch: file content lacking the "PAR::" prefix still parses.
        let id = CantonId::parse_from_file(&format!("participant::{VALID_NS}"))?;
        assert_eq!(id.prefix, "participant");
        Ok(())
    }

    #[test]
    fn parse_from_file_trims_surrounding_whitespace() -> Result {
        let id = CantonId::parse_from_file(&format!("  PAR::participant::{VALID_NS}\n"))?;
        assert_eq!(id.prefix, "participant");
        assert_eq!(id.namespace.to_hex(), VALID_NS);
        Ok(())
    }
}
