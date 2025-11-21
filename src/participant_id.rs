use std::fmt;

use crate::error::Result;

/// Length of a namespace: 2 bytes multihash prefix + 32 bytes SHA-256 hash
pub const NAMESPACE_LENGTH: usize = 34;

/// Newtype wrapper around a fixed-length namespace byte array
///
/// Canton namespaces are multihash-encoded SHA-256 hashes:
/// - First 2 bytes: multihash prefix (0x1220 for SHA-256)
/// - Next 32 bytes: SHA-256 hash
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Namespace([u8; NAMESPACE_LENGTH]);

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
                "Invalid namespace length: expected {NAMESPACE_LENGTH} bytes, got {}",
                bytes.len()
            );
        }
        let mut arr = [0u8; NAMESPACE_LENGTH];
        arr.copy_from_slice(&bytes);
        Ok(Self(arr))
    }

    /// Get the namespace as a hex string
    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
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

/// Represents a Canton participant ID with prefix and namespace
///
/// Canton participant IDs have the format: `{prefix}::{hex_encoded_namespace}`
/// Examples:
/// - `participant::1220c4010d6883f367...`
/// - `sv::1220034c3a6a9454...`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantId {
    /// The prefix (e.g., "participant", "sv")
    pub prefix: String,
    /// The namespace (multihash-encoded identifier)
    pub namespace: Namespace,
}

impl ParticipantId {
    /// Create a new ParticipantId from prefix and namespace
    pub fn new(prefix: String, namespace: Namespace) -> Self {
        Self { prefix, namespace }
    }

    /// Parse a ParticipantId from Canton's string format
    ///
    /// Expected format: `prefix::hex_encoded_namespace`
    /// Example: `participant::1220c4010d6883f367...`
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split("::").collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid participant ID format: expected 'prefix::namespace', got '{s}'");
        }

        let prefix = parts[0].to_string();
        let namespace = Namespace::from_hex(parts[1])?;

        Ok(Self { prefix, namespace })
    }

    /// Parse a ParticipantId from file content (strips "PAR::" prefix if present)
    pub fn parse_from_file(content: &str) -> Result<Self> {
        let trimmed = content.trim();
        let id_str = match trimmed.strip_prefix("PAR::") {
            Some(stripped) => stripped,
            None => trimmed,
        };
        Self::parse(id_str)
    }

    /// Convert to Canton's string format: `prefix::hex_encoded_namespace`
    pub fn to_string(&self) -> String {
        format!("{}::{}", self.prefix, self.namespace.to_hex())
    }

    /// Convert to file storage format with "PAR::" prefix
    pub fn to_file_format(&self) -> String {
        format!("PAR::{}", self.to_string())
    }
}

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string())
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
        let id = ParticipantId::parse(
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
        let id = ParticipantId::parse_from_file(
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
        let id = ParticipantId::new("participant".to_string(), ns);
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
        let id = ParticipantId::parse(original)?;
        assert_eq!(id.to_string(), original);
        Ok(())
    }
}
