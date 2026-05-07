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

impl utoipa::ToSchema for CantonId {}

impl std::str::FromStr for CantonId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
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
}
