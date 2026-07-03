use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{storage::artifact_kinds, topology},
};

/// Sign the change-threshold proposals.
///
/// Every party member signs both the DNS and P2P proposals. `proposal_data` is
/// the `[dns, p2p]` pair received from the coordinator. Delegates to
/// [`topology::sign_dns_p2p_proposals`], persisting the per-peer results as
/// `SIGNED_CHANGE_THRESHOLD_DNS` / `SIGNED_CHANGE_THRESHOLD_P2P`.
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
        artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
        artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
    )
    .await
}
