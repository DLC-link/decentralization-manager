//! Peer-side step for the decentrally-hosted external-party workflow.
//!
//! A hosting peer receives the coordinator's `AllocateExternalParty` command
//! carrying the party-signed onboarding bundle and authorizes hosting on its own
//! participant. There is no per-peer artifact to return — the peer signals
//! completion with a status update (see the command arm in `workflow::start_peer`).

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::{Commitable, SchemaWrite},
    error::Result,
    workflow::external_party::steps::{ExternalPartyAllocatePayload, allocate_party},
};

/// Authorize hosting the external party on this peer's own participant, using
/// the party-signed bundle the coordinator fanned out in the command payload,
/// then record the hosted party on this peer's run so `GET /external-parties`
/// lists it here too (this node is a host, not just a bystander).
///
/// # Errors
/// Returns an error if the payload can't be deserialized, the participant's
/// `AllocateExternalParty` call fails, or persisting the party id fails.
pub async fn authorize_hosting(
    node_config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    payload: &[u8],
) -> Result {
    let bundle: ExternalPartyAllocatePayload =
        serde_json::from_slice(payload).context("deserialize external-party allocate bundle")?;
    allocate_party(node_config, &bundle).await?;

    let party_id = CantonId::parse(bundle.party_id.trim())?;
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_dec_party_id(instance_name, &party_id)
        .await?;
    Commitable::commit(tx).await?;
    Ok(())
}
