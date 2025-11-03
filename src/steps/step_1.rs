use crate::error::Result;

/// Upload DAR files to the participant
///
/// Corresponds to: 00_UploadDars.sc
pub async fn upload_dars() -> Result {
    tracing::info!("Uploading DARs...");
    // TODO: Implement
    Ok(())
}

/// Generate cryptographic keys and export participant ID
///
/// Corresponds to: 01_GenerateKeys.sc
pub async fn generate_keys() -> Result {
    tracing::info!("Generating keys...");
    // TODO: Implement
    Ok(())
}
