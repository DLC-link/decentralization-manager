use crate::{
    consts::{ATTESTOR_KEYS_PREFIX, PARTICIPANT_ID_PREFIX, SIGNED_ADD_PARTY_PROPOSALS_PREFIX},
    error::Result,
    noise::client::NoiseClient,
    utils,
};

use super::AddPartyDirs;

/// Send generated keys to coordinator (for new member only)
pub async fn send_keys_to_coordinator(client: &NoiseClient, dirs: &AddPartyDirs) -> Result {
    // Read keys file
    let keys_data = crate::workflow::find_and_read_file(
        &dirs.keys_dir,
        ATTESTOR_KEYS_PREFIX,
        ".bin",
        "Keys file not found",
    )
    .await?;

    // Read participant ID file
    let id_data = crate::workflow::find_and_read_file(
        &dirs.ids_dir,
        PARTICIPANT_ID_PREFIX,
        ".bin",
        "Participant ID file not found",
    )
    .await?;

    // Combine keys and participant ID for upload
    let combined = utils::encode_length_prefixed(&[&keys_data, &id_data]);

    client.upload_add_party_keys(combined).await?;
    Ok(())
}

/// Send add party signatures to coordinator (for existing members only)
pub async fn send_add_party_signatures_to_coordinator(
    client: &NoiseClient,
    dirs: &AddPartyDirs,
) -> Result {
    let data = crate::workflow::find_and_read_file(
        &dirs.add_party_signed_dir,
        SIGNED_ADD_PARTY_PROPOSALS_PREFIX,
        ".bin",
        "Signed add party proposals file not found",
    )
    .await?;
    client.send_add_party_signatures(data).await?;
    Ok(())
}
