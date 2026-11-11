//! Deadlines for the single topology reads of party discovery.

use std::time::Duration;

use crate::error::Result;

/// Deadline for a single topology read.
///
/// Canton applies no deadline of its own, so a read the participant cannot
/// answer quickly holds the request open indefinitely, well past any gateway
/// timeout in front of it, and every retry stacks another one on the
/// participant.
const TOPOLOGY_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Run one fallible call under [`TOPOLOGY_READ_TIMEOUT`].
pub(crate) async fn bounded_call<T>(
    what: &str,
    call: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(TOPOLOGY_READ_TIMEOUT, call).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "{what} did not answer within {secs}s",
            secs = TOPOLOGY_READ_TIMEOUT.as_secs()
        ),
    }
}

/// Run one topology read under [`TOPOLOGY_READ_TIMEOUT`].
pub(crate) async fn bounded_read<T>(
    what: &str,
    read: impl std::future::Future<Output = std::result::Result<T, tonic::Status>>,
) -> Result<T> {
    match tokio::time::timeout(TOPOLOGY_READ_TIMEOUT, read).await {
        Ok(result) => Ok(result?),
        Err(_) => anyhow::bail!(
            "{what} did not answer within {secs}s",
            secs = TOPOLOGY_READ_TIMEOUT.as_secs()
        ),
    }
}
