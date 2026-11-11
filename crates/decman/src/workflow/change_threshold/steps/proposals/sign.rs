use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{change_threshold::ARTIFACTS, topology},
};

/// Sign the change-threshold proposals.
///
/// Every party member signs both the DNS and P2P proposals. `proposal_data` is
/// the `[dns, p2p]` pair received from the coordinator. Delegates to
/// [`topology::sign_dns_p2p_proposals`], persisting the per-peer results as
/// `SIGNED_CHANGE_THRESHOLD_DNS` / `SIGNED_CHANGE_THRESHOLD_P2P`.
///
/// # Errors
///
/// Returns an error if the synchronizer ID cannot be read, or if
/// `proposal_data` does not decode as two topology transactions. Returns an
/// error if `SignTransactions` fails or does not return exactly two
/// transactions. Returns an error if a signed artefact cannot be written.
pub async fn sign_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    topology::sign_dns_p2p_proposals(
        config,
        storage,
        instance_name,
        proposal_data,
        "change-threshold",
        ARTIFACTS,
    )
    .await
}
