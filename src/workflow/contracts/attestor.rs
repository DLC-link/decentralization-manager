use crate::{
    consts::{EXECUTION_DIR, SIGNATURES_DIR, SUBMISSION_SIGNATURES_PREFIX},
    error::Result,
    noise::client::NoiseClient,
};

use super::ContractsDirs;

pub async fn send_submission_signatures_to_coordinator(
    client: &NoiseClient,
    dirs: &ContractsDirs,
) -> Result {
    let signatures_dir = dirs.workflow_dir.join(EXECUTION_DIR).join(SIGNATURES_DIR);
    let data = crate::workflow::find_and_read_file(
        &signatures_dir,
        SUBMISSION_SIGNATURES_PREFIX,
        ".bin",
        "Submission signatures file not found",
    )
    .await?;
    client.send_submission_signatures(data).await?;
    Ok(())
}
