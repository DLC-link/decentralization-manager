use crate::{
    consts::SIGNED_KICK_PROPOSALS_PREFIX,
    error::Result,
    noise::client::NoiseClient,
};

use super::KickDirs;

pub async fn send_kick_signatures_to_coordinator(
    client: &NoiseClient,
    dirs: &KickDirs,
) -> Result {
    let data = crate::workflow::find_and_read_file(
        &dirs.kick_signed_dir,
        SIGNED_KICK_PROPOSALS_PREFIX,
        ".bin",
        "Signed kick proposals file not found",
    )
    .await?;
    client.send_kick_signatures(data).await?;
    Ok(())
}
