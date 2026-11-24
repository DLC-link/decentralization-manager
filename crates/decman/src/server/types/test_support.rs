//! A party id shared by the wire type tests.

use crate::canton_id::CantonId;

pub fn test_party(prefix: &str) -> anyhow::Result<CantonId> {
    CantonId::parse(&format!("{prefix}::1220{}", "ab".repeat(32)))
}
