use crate::{
    consts::{ATTESTOR_KEYS_PREFIX, SIGNED_DNS_PROPOSAL_PREFIX, SIGNED_P2P_PROPOSALS_PREFIX},
    error::Result,
    noise::client::NoiseClient,
};

use super::OnboardingDirs;

pub async fn send_keys_to_coordinator(client: &NoiseClient, dirs: &OnboardingDirs) -> Result {
    let data = crate::workflow::find_and_read_file(
        &dirs.keys_dir,
        ATTESTOR_KEYS_PREFIX,
        ".bin",
        "Attestor public keys file not found",
    )
    .await?;
    client.upload_keys(data).await?;
    Ok(())
}

pub async fn send_dns_signature_to_coordinator(
    client: &NoiseClient,
    dirs: &OnboardingDirs,
) -> Result {
    let data = crate::workflow::find_and_read_file(
        &dirs.dns_signed_dir,
        SIGNED_DNS_PROPOSAL_PREFIX,
        ".bin",
        "Signed DNS proposal file not found",
    )
    .await?;
    client.send_dns_signature(data).await?;
    Ok(())
}

pub async fn send_p2p_signatures_to_coordinator(
    client: &NoiseClient,
    dirs: &OnboardingDirs,
) -> Result {
    let data = crate::workflow::find_and_read_file(
        &dirs.final_signed_dir,
        SIGNED_P2P_PROPOSALS_PREFIX,
        ".bin",
        "Signed P2P proposals file not found",
    )
    .await?;
    client.send_p2p_signatures(data).await?;
    Ok(())
}
