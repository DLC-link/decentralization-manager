use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{storage::artifact_kinds, topology},
};

/// All-peer step: sign both add-party proposals.
///
/// Every invited peer signs — existing members authorize the owner/threshold
/// change with their namespace keys; the new member's signature covers both
/// its namespace joining the DNS owner set and its participant accepting to
/// host the party (Canton auto-selects the appropriate keys).
///
/// `proposal_data` is the `[dns, p2p]` pair from the coordinator (the config
/// item was already stripped by the peer loop). Delegates to
/// [`topology::sign_dns_p2p_proposals`], persisting the per-peer results as
/// `SIGNED_ADD_PARTY_DNS` / `SIGNED_ADD_PARTY_P2P`.
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
        "add-party",
        artifact_kinds::SIGNED_ADD_PARTY_DNS,
        artifact_kinds::SIGNED_ADD_PARTY_P2P,
    )
    .await
}
