use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::{MessageType, client::NoiseClient},
    workflow::{change_threshold::ARTIFACTS, topology},
};

/// Send the locally-signed DNS + P2P change-threshold proposals to the
/// coordinator.
///
/// The two artefacts (`SIGNED_CHANGE_THRESHOLD_DNS` and
/// `SIGNED_CHANGE_THRESHOLD_P2P`) were written by `sign_proposals` as
/// `varint(len)||proto` blobs. Concatenating them produces exactly the buffer
/// the coordinator's submit step decodes as two protobuf messages.
///
/// # Errors
///
/// Returns an error if either signed artefact cannot be read or is missing.
/// Returns an error if the send fails or the coordinator does not answer with
/// an Ack.
pub async fn send_change_threshold_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    topology::send_signed_pair(
        client,
        storage,
        instance_name,
        node_config,
        ARTIFACTS,
        MessageType::ChangeThresholdSignatures,
    )
    .await
}
