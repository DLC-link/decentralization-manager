use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::{MessageType, client::NoiseClient},
    workflow::{kick::ARTIFACTS, topology},
};

/// Send the locally-signed DNS + P2P kick proposals to the coordinator.
///
/// The two artefacts (`SIGNED_KICK_DNS` and `SIGNED_KICK_P2P`) were written
/// by `sign_proposals` as `varint(len)||proto` blobs. Concatenating them
/// produces exactly the buffer the previous file-based implementation
/// shipped (i.e. the contents of the combined signed proposals file), which
/// is what the coordinator's submit step expects to decode as two protobuf
/// messages.
///
/// # Errors
///
/// Returns an error if this node's `SIGNED_KICK_DNS` or `SIGNED_KICK_P2P`
/// artefact is missing or cannot be read. Returns an error if the Noise send
/// fails or the coordinator does not answer with an Ack.
pub async fn send_kick_signatures_to_coordinator(
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
        MessageType::KickSignatures,
    )
    .await
}
