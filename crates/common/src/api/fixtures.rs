//! Fixtures shared by the api tests.

use anyhow::Result;

use crate::canton_id::CantonId;

pub fn party(prefix: &str) -> Result<CantonId> {
    CantonId::parse(&format!("{prefix}::1220{}", "ab".repeat(32)))
}
