use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{kick::ARTIFACTS, topology},
};

/// Sign kick proposals.
///
/// Each remaining member (not the kicked member) signs both the DNS and P2P
/// proposals. `proposal_data` is the `[dns, p2p]` pair received from the
/// coordinator. Delegates to [`topology::sign_dns_p2p_proposals`], persisting
/// the per-peer results as `SIGNED_KICK_DNS` / `SIGNED_KICK_P2P`.
///
/// # Errors
///
/// Returns an error if the synchronizer ID cannot be read, or if
/// `proposal_data` does not decode as a DNS and P2P pair. Returns an error if
/// the `SignTransactions` call fails, still finds no signing key after every
/// retry, or does not return exactly two transactions. Returns an error if
/// writing `SIGNED_KICK_DNS` or `SIGNED_KICK_P2P` fails.
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
        ARTIFACTS,
    )
    .await
}
