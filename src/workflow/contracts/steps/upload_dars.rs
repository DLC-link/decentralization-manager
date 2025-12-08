use tokio::fs;

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    UploadDarRequest, package_service_client::PackageServiceClient,
    upload_dar_request::UploadDarData,
};

use crate::{config::NodeConfig, error::Result, workflow::contracts::ContractsDirs};

/// Upload DAR files to the participant
///
/// Scans the dars directory and uploads all .dar files found to the Canton participant.
pub async fn upload_dars(config: &NodeConfig, dirs: &ContractsDirs) -> Result {
    tracing::info!("Uploading DARs from {path}", path = dirs.dars_dir.display());

    let mut client = PackageServiceClient::connect(config.admin_api_url()).await?;

    // Scan directory for all .dar files
    let mut dar_entries = fs::read_dir(&dirs.dars_dir).await?;
    let mut dar_files = Vec::new();

    while let Some(entry) = dar_entries.next_entry().await? {
        let path = entry.path();

        // Check if file has .dar extension
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("dar") {
            dar_files.push(path);
        }
    }

    // Sort for consistent ordering
    dar_files.sort();

    if dar_files.is_empty() {
        anyhow::bail!(
            "No .dar files found in {path}",
            path = dirs.dars_dir.display()
        );
    }

    tracing::info!(
        "Found {count} DAR file(s) to upload",
        count = dar_files.len()
    );

    // Upload each DAR file
    for dar_path in dar_files {
        let filename = dar_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        tracing::debug!("Reading {path}", path = dar_path.display());
        let dar_data = fs::read(&dar_path).await?;

        // Generate description from filename (remove .dar extension)
        let description = filename
            .strip_suffix(".dar")
            .unwrap_or(filename)
            .to_string();

        let request = tonic::Request::new(UploadDarRequest {
            dars: vec![UploadDarData {
                bytes: dar_data,
                description: Some(description.clone()),
                expected_main_package_id: None,
            }],
            vet_all_packages: true,
            synchronize_vetting: true,
            synchronizer_id: None, // Auto-detect if single synchronizer
        });

        tracing::info!("Uploading {filename}...");
        client.upload_dar(request).await?;
        tracing::info!("Successfully uploaded {filename}");
    }

    tracing::info!("All DARs uploaded successfully");

    Ok(())
}
