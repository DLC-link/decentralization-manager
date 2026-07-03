use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{storage::artifact_kinds, topology},
};

/// Sign kick proposals.
///
/// Each remaining member (not the kicked member) signs both the DNS and P2P
/// proposals. `proposal_data` is the `[dns, p2p]` pair received from the
/// coordinator. Delegates to [`topology::sign_dns_p2p_proposals`], persisting
/// the per-peer results as `SIGNED_KICK_DNS` / `SIGNED_KICK_P2P`.
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
        "kick",
        artifact_kinds::SIGNED_KICK_DNS,
        artifact_kinds::SIGNED_KICK_P2P,
    )
    .await
}
