use base64::{Engine, engine::general_purpose::STANDARD};

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    UploadDarRequest, package_service_client::PackageServiceClient,
    upload_dar_request::UploadDarData,
};

use crate::{config::NodeConfig, error::Result, workflow::contracts::ContractsConfig};

/// Upload DAR files to the participant from ContractsConfig
///
/// Takes DAR files from the contracts config (base64-encoded) and uploads them
/// to the Canton participant. Used by the coordinator.
pub async fn upload_dars(config: &NodeConfig, contracts_config: &ContractsConfig) -> Result {
    let dar_files = &contracts_config.dar_files;

    if dar_files.is_empty() {
        tracing::debug!("No DAR files to upload, skipping");
        return Ok(());
    }

    tracing::info!("Uploading {count} DAR file(s)...", count = dar_files.len());

    let mut client = PackageServiceClient::connect(config.admin_api_url()).await?;

    for dar_file in dar_files {
        // Decode base64 data
        let dar_data = STANDARD.decode(&dar_file.data).map_err(|e| {
            anyhow::anyhow!(
                "Failed to decode base64 DAR data for {}: {e}",
                dar_file.filename
            )
        })?;

        upload_dar_bytes(&mut client, &dar_file.filename, dar_data).await?;
    }

    tracing::debug!("All DARs uploaded successfully");

    Ok(())
}

/// Upload DAR files from raw bytes
///
/// Takes a list of (filename, data) tuples and uploads them to the Canton participant.
/// Used by attestors who receive DAR files from the coordinator.
pub async fn upload_dars_from_bytes(
    config: &NodeConfig,
    dar_files: Vec<(String, Vec<u8>)>,
) -> Result {
    if dar_files.is_empty() {
        tracing::debug!("No DAR files to upload, skipping");
        return Ok(());
    }

    tracing::info!("Uploading {count} DAR file(s)...", count = dar_files.len());

    let mut client = PackageServiceClient::connect(config.admin_api_url()).await?;

    for (filename, data) in dar_files {
        upload_dar_bytes(&mut client, &filename, data).await?;
    }

    tracing::debug!("All DARs uploaded successfully");

    Ok(())
}

/// Upload a single DAR file by bytes
async fn upload_dar_bytes(
    client: &mut PackageServiceClient<tonic::transport::Channel>,
    filename: &str,
    dar_data: Vec<u8>,
) -> Result {
    // Generate description from filename (remove .dar extension)
    let description = filename
        .strip_suffix(".dar")
        .unwrap_or(filename)
        .to_string();

    let request = tonic::Request::new(UploadDarRequest {
        dars: vec![UploadDarData {
            bytes: dar_data,
            description: Some(description),
            expected_main_package_id: None,
        }],
        vet_all_packages: true,
        synchronize_vetting: true,
        synchronizer_id: None, // Auto-detect if single synchronizer
    });

    tracing::debug!("Uploading {filename}...");
    client.upload_dar(request).await?;
    tracing::debug!("Successfully uploaded {filename}");

    Ok(())
}
