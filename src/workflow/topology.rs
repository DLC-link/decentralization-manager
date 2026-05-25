//! Cross-workflow Canton topology helpers.
//!
//! Workflows (onboarding, kick, …) that submit signed topology transactions
//! to Canton share the same write path
//! ([`TopologyManagerWriteServiceClient::sign_transactions`]) and the same
//! transient failure mode while a freshly-restarted participant's local
//! topology store is reconciling. This module owns the retry policy so
//! callers don't reach across workflow boundaries to share it.

use std::time::Duration;

use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    SignTransactionsRequest, SignTransactionsResponse,
    topology_manager_write_service_client::TopologyManagerWriteServiceClient,
};

use crate::{
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
};

/// Call `sign_transactions` on the participant's TopologyManagerWriteService,
/// retrying only when Canton returns
/// `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` — the transient that
/// surfaces while a freshly-restarted participant's local topology store is
/// still reconciling its own signing keys.
///
/// All other gRPC errors bubble up immediately. On a healthy synchronizer
/// the first attempt succeeds, so production code paths pay no retry-loop
/// overhead.
///
/// The retry budget is [`topology_retry_max_attempts`] ×
/// [`topology_retry_delay_secs`] (env-configurable via
/// `DPM_TOPOLOGY_RETRY_MAX_ATTEMPTS` / `DPM_TOPOLOGY_RETRY_DELAY_SECS`,
/// defaults 30 × 2s = 60s), shared with the post-write topology-propagation
/// polls in `submit.rs::wait_for_dns_in_topology` /
/// `wait_for_p2p_in_topology`.
///
/// `label` is a short tag included in log lines (e.g. `"DNS"`, `"P2P"`,
/// `"kick"`) so operators can distinguish which sign path is retrying.
pub async fn sign_transactions_with_topology_retry(
    config: &NodeConfig,
    request: SignTransactionsRequest,
    label: &str,
) -> Result<SignTransactionsResponse> {
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = Duration::from_secs(topology_retry_delay_secs());

    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match topology_client
            .sign_transactions(tonic::Request::new(request.clone()))
            .await
        {
            Ok(response) => {
                if attempt > 1 {
                    tracing::info!(
                        "{label}: sign_transactions succeeded on attempt {attempt}/{max_attempts}",
                    );
                }
                return Ok(response.into_inner());
            }
            Err(status) if is_topology_signing_key_not_ready(&status) => {
                if attempt >= max_attempts {
                    anyhow::bail!(
                        "{label}: sign_transactions still returning \
                         TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE after \
                         {max_attempts} attempts: {status}",
                    );
                }
                tracing::warn!(
                    "{label}: TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE \
                     on attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}",
                );
                tokio::time::sleep(retry_delay).await;
            }
            Err(status) => return Err(status.into()),
        }
    }
}

/// Returns true iff the gRPC status is Canton's signal that a participant's
/// local topology store doesn't yet have a usable signing key for the
/// transaction it was asked to sign. This is a transient that resolves once
/// Canton finishes reconciling the participant's `OwnerToKeyMapping` /
/// `NamespaceDelegation` — typically within seconds of participant startup
/// (longer on slow/tunneled deployments).
///
/// Matches on the Canton error name in the status message rather than the
/// gRPC code, because Canton surfaces this error as different gRPC codes in
/// different paths — observed as `NOT_FOUND` from `sign_transactions`
/// (devnet run 2026-05-21, four occurrences on P2 with code
/// `'Some requested entity was not found'`), but historically documented
/// as `FAILED_PRECONDITION` elsewhere. The error-name string is the stable
/// semantic identifier; the gRPC code is implementation detail that varies
/// across Canton versions and call paths.
fn is_topology_signing_key_not_ready(status: &tonic::Status) -> bool {
    status
        .message()
        .contains("TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real status text Canton returned from `sign_transactions` on devnet
    /// (2026-05-21 IT run). Code is `NOT_FOUND`, not `FAILED_PRECONDITION` —
    /// this is the exact case the original predicate missed.
    #[test]
    fn detects_canton_not_found_form() {
        let status = tonic::Status::not_found(
            "TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE(11,0): \
             Could not find an appropriate signing key to issue the topology transaction",
        );
        assert!(is_topology_signing_key_not_ready(&status));
    }

    /// Canton has historically surfaced the same error via FAILED_PRECONDITION
    /// in other paths. Match this too so future Canton-version changes don't
    /// reintroduce the flake.
    #[test]
    fn detects_failed_precondition_form() {
        let status = tonic::Status::failed_precondition(
            "TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE(9,abc): \
             No appropriate signing key for namespace …",
        );
        assert!(is_topology_signing_key_not_ready(&status));
    }

    #[test]
    fn rejects_other_canton_errors() {
        let status =
            tonic::Status::failed_precondition("SOME_OTHER_TOPOLOGY_ERROR: irrelevant detail");
        assert!(!is_topology_signing_key_not_ready(&status));
    }

    #[test]
    fn rejects_empty_message() {
        let status = tonic::Status::internal("");
        assert!(!is_topology_signing_key_not_ready(&status));
    }
}
