use std::path::Path;

use tokio::fs;

use crate::{
    config::Config,
    error::Result,
    proto::com::digitalasset::canton::admin::participant::v30::{
        UploadDarRequest, package_service_client::PackageServiceClient,
        upload_dar_request::UploadDarData,
    },
};

/// Upload DAR files to the participant
///
/// Corresponds to: 00_UploadDars.sc
///
/// Uploads both CBTC and governance DAR files to the Canton participant.
pub async fn upload_dars(config: &Config, dars_dir: &Path) -> Result {
    tracing::info!("Uploading DARs from {}", dars_dir.display());

    let mut client = PackageServiceClient::connect(config.admin_api_url()).await?;

    // Read both DAR files
    let cbtc_dar_path = dars_dir.join("cbtc-1.0.0.dar");
    let gov_dar_path = dars_dir.join("cbtc-governance-1.0.0.dar");

    // Upload CBTC DAR
    tracing::info!("Reading {}", cbtc_dar_path.display());
    let cbtc_dar_data = fs::read(&cbtc_dar_path).await?;

    let cbtc_request = tonic::Request::new(UploadDarRequest {
        dars: vec![UploadDarData {
            bytes: cbtc_dar_data,
            description: Some("CBTC main application".to_string()),
            expected_main_package_id: None,
        }],
        vet_all_packages: true,
        synchronize_vetting: true,
        synchronizer_id: None, // Auto-detect if single synchronizer
    });

    tracing::info!("Uploading CBTC DAR to Canton...");
    client.upload_dar(cbtc_request).await?;
    tracing::info!("CBTC DAR uploaded successfully");

    // Upload governance DAR
    tracing::info!("Reading {}", gov_dar_path.display());
    let gov_dar_data = fs::read(&gov_dar_path).await?;

    let gov_request = tonic::Request::new(UploadDarRequest {
        dars: vec![UploadDarData {
            bytes: gov_dar_data,
            description: Some("CBTC governance rules".to_string()),
            expected_main_package_id: None,
        }],
        vet_all_packages: true,
        synchronize_vetting: true,
        synchronizer_id: None, // Auto-detect if single synchronizer
    });

    tracing::info!("Uploading governance DAR to Canton...");
    client.upload_dar(gov_request).await?;
    tracing::info!("Governance DAR uploaded successfully");

    tracing::info!("All DARs uploaded successfully");
    Ok(())
}

/// Generate cryptographic keys and export participant ID
///
/// Corresponds to: 01_GenerateKeys.sc
///
/// Generates:
/// 1. Namespace signing key (for namespace delegation)
/// 2. DAML transaction key (for signing transactions)
/// 3. Exports both keys to attestor-public-keys.bin
/// 4. Exports participant ID to participant-id.bin
///
/// # Note
///
/// This function generates signing keys and exports them along with the participant ID.
/// The namespace delegation step from the original Scala script is currently skipped
/// and should be implemented separately using TopologyManagerWriteService.
pub async fn generate_keys(config: &Config, output_dir: &Path) -> Result {
    tracing::info!("Generating cryptographic keys...");

    use crate::{
        proto::com::digitalasset::canton::{
            crypto::{
                admin::v30::{
                    GenerateSigningKeyRequest, GenerateSigningKeyResponse,
                    vault_service_client::VaultServiceClient,
                },
                v30::{SigningKeySpec, SigningKeyUsage},
            },
            topology::admin::v30::{
                GetIdRequest,
                identity_initialization_service_client::IdentityInitializationServiceClient,
            },
        },
        utils,
    };

    // Connect to VaultService for key generation
    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    // Generate namespace signing key
    tracing::info!("Generating namespace signing key...");
    let namespace_key_request = tonic::Request::new(GenerateSigningKeyRequest {
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        name: "cbtc-network-namespace".to_string(),
        usage: vec![SigningKeyUsage::Namespace as i32],
    });

    let namespace_key_response: GenerateSigningKeyResponse = vault_client
        .generate_signing_key(namespace_key_request)
        .await?
        .into_inner();

    let namespace_key = namespace_key_response
        .public_key
        .ok_or_else(|| anyhow::anyhow!("No namespace key returned from VaultService"))?;

    tracing::info!("Namespace key generated successfully");

    // TODO: Implement namespace delegation using TopologyManagerWriteService
    // This requires:
    // 1. Create namespace from key fingerprint
    // 2. Get synchronizer ID for "global"
    // 3. Propose namespace delegation with DelegationRestriction::CanSignAllMappings

    // Generate DAML signing key for transactions
    tracing::info!("Generating DAML signing key...");
    let daml_key_request = tonic::Request::new(GenerateSigningKeyRequest {
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        name: "cbtc-network-daml-transactions".to_string(),
        usage: vec![SigningKeyUsage::Protocol as i32],
    });

    let daml_key_response: GenerateSigningKeyResponse = vault_client
        .generate_signing_key(daml_key_request)
        .await?
        .into_inner();

    let daml_key = daml_key_response
        .public_key
        .ok_or_else(|| anyhow::anyhow!("No DAML key returned from VaultService"))?;

    tracing::info!("DAML key generated successfully");

    // Export both keys to attestor-public-keys.bin
    let keys_output = output_dir.join("attestor-public-keys.bin");
    tracing::info!("Exporting keys to {}", keys_output.display());
    utils::write_messages_to_file(&[namespace_key, daml_key], &keys_output).await?;

    // Get participant ID
    let mut id_client =
        IdentityInitializationServiceClient::connect(config.admin_api_url()).await?;
    let id_response = id_client
        .get_id(tonic::Request::new(GetIdRequest {}))
        .await?
        .into_inner();

    let participant_id = &id_response.unique_identifier;

    if participant_id.is_empty() {
        anyhow::bail!("No participant ID returned");
    }

    // Export participant ID to participant-id.bin
    let participant_id_output = output_dir.join("participant-id.bin");
    tracing::info!(
        "Exporting participant ID to {}",
        participant_id_output.display()
    );
    fs::write(&participant_id_output, participant_id.as_bytes()).await?;

    tracing::info!("Keys and participant ID exported successfully");
    Ok(())
}
