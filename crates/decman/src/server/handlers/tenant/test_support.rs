//! A participant id shared by the tenant tests.

/// A full namespace fingerprint — `CantonId` deserializes through
/// `Namespace::from_hex`, which refuses a truncated stub.
pub fn test_participant() -> String {
    format!("participant-3::1220{}", "bb".repeat(32))
}
