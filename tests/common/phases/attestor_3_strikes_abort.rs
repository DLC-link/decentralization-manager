//! G8: Attestor 3-strikes abort flips the row to Failed and unblocks future
//! starts.
//!
//! This test requires sending the attestor a step where the payload
//! deliberately fails to deserialize three times in a row. Producing that
//! without a custom Noise client (or modifying production code) is harness
//! work that is outside this PR's scope — the existing test helpers only
//! speak the well-formed Noise protocol via real coordinator processes.
//!
//! TODO: G8 requires a custom malicious Noise client. Implement once we have
//! a test harness for raw Noise frame injection.

use tracing::info;

use crate::common::Fixture;

pub async fn run(_f: &mut Fixture) -> anyhow::Result<()> {
    info!(
        "[G8] SKIPPED: G8 requires a custom Noise client to inject malformed \
         payloads — harness work pending."
    );
    Ok(())
}
