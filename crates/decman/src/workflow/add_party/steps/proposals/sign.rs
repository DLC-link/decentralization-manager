use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{add_party::ARTIFACTS, topology},
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
        "add-party",
        ARTIFACTS,
    )
    .await
}
