use std::path::Path;

use tokio::fs;

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{ATTESTOR_KEYS_PREFIX, PARTICIPANT_ID_PREFIX},
    dirs::WorkflowDirs,
    error::Result,
    participant_id::CantonId,
    proto::com::digitalasset::canton::{
        admin::participant::v30::{
            UploadDarRequest, package_service_client::PackageServiceClient,
            upload_dar_request::UploadDarData,
        },
        crypto::{
            admin::v30::{GenerateSigningKeyRequest, vault_service_client::VaultServiceClient},
            v30::{SigningKeySpec, SigningKeyUsage, SigningPublicKey},
        },
        protocol::v30::{
            NamespaceDelegation, TopologyMapping, enums::TopologyChangeOp, namespace_delegation,
            topology_mapping,
        },
        topology::admin::v30::{
            AuthorizeRequest, StoreId, authorize_request, store_id,
            topology_manager_write_service_client::TopologyManagerWriteServiceClient,
        },
    },
    utils::{compute_fingerprint, get_participant_id, write_messages_to_file},
};

/// Upload DAR files to the participant
///
/// Corresponds to: 00_UploadDars.sc
///
/// Scans the dars directory and uploads all .dar files found to the Canton participant.
pub async fn upload_dars(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Uploading DARs from {}", dirs.dars_dir.display());

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
        anyhow::bail!("No .dar files found in {}", dirs.dars_dir.display());
    }

    tracing::info!("Found {} DAR file(s) to upload", dar_files.len());

    // Upload each DAR file
    for dar_path in dar_files {
        let filename = dar_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        tracing::debug!("Reading {}", dar_path.display());
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
pub async fn generate_keys(
    config: &NodeConfig,
    dirs: &WorkflowDirs,
    network_config: &NetworkConfig,
) -> Result {
    tracing::info!("Generating cryptographic keys...");

    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    let namespace_key_name = &network_config.application.namespace_key_name;
    let daml_key_name = &network_config.application.daml_key_name;

    // Generate namespace signing key
    tracing::debug!("Generating namespace signing key with name '{namespace_key_name}'");
    let namespace_key = generate_signing_key(
        &mut vault_client,
        namespace_key_name,
        vec![SigningKeyUsage::Namespace as i32],
    )
    .await?;

    let namespace_fingerprint = compute_fingerprint(&namespace_key);
    tracing::debug!("Namespace key fingerprint: {namespace_fingerprint}");

    // Propose namespace delegation
    propose_namespace_delegation(config, &namespace_key, &namespace_fingerprint).await?;

    // Generate DAML signing key for transactions
    tracing::debug!("Generating DAML signing key with name '{daml_key_name}'");
    let daml_key = generate_signing_key(
        &mut vault_client,
        daml_key_name,
        vec![SigningKeyUsage::Protocol as i32],
    )
    .await?;

    // Get participant ID and export keys
    let canton_participant_id = get_participant_id(config).await?;
    let participant_num = get_participant_position(&config.node.node_id, &network_config)?;
    tracing::info!(
        "Participant ID: {canton_participant_id}, participant number: {participant_num}"
    );

    export_keys(&dirs.keys_dir, &namespace_key, &daml_key, participant_num).await?;
    export_participant_id(&dirs.ids_dir, &canton_participant_id, participant_num).await?;

    tracing::info!("Keys and participant ID exported successfully");

    Ok(())
}

/// Helper: Generate a signing key via VaultService
async fn generate_signing_key(
    vault_client: &mut VaultServiceClient<tonic::transport::Channel>,
    name: &str,
    usage: Vec<i32>,
) -> Result<SigningPublicKey> {
    let request = tonic::Request::new(GenerateSigningKeyRequest {
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        name: name.to_string(),
        usage,
    });

    let response = vault_client
        .generate_signing_key(request)
        .await?
        .into_inner();
    response
        .public_key
        .ok_or_else(|| anyhow::anyhow!("No public key returned from VaultService"))
}

/// Helper: Get participant position (1-based index) from network config
fn get_participant_position(node_id: &str, network_config: &NetworkConfig) -> Result<u32> {
    network_config
        .participants
        .iter()
        .position(|p| p.id == node_id)
        .map(|pos| (pos + 1) as u32)
        .ok_or_else(|| anyhow::anyhow!("Node ID '{node_id}' not found in network configuration"))
}

/// Helper: Export keys to file
async fn export_keys(
    keys_dir: &Path,
    namespace_key: &SigningPublicKey,
    daml_key: &SigningPublicKey,
    participant_num: u32,
) -> Result {
    let filename = format!("{ATTESTOR_KEYS_PREFIX}-{participant_num}.bin");
    let output_path = keys_dir.join(&filename);
    tracing::debug!("Exporting keys to {}", output_path.display());
    write_messages_to_file(&[namespace_key.clone(), daml_key.clone()], &output_path).await
}

/// Helper: Export participant ID to file
async fn export_participant_id(
    ids_dir: &Path,
    participant_id: &CantonId,
    participant_num: u32,
) -> Result {
    let filename = format!("{PARTICIPANT_ID_PREFIX}-{participant_num}.bin");
    let output_path = ids_dir.join(&filename);
    tracing::debug!("Exporting participant ID to {}", output_path.display());
    fs::write(&output_path, participant_id.to_file_format().as_bytes()).await?;
    Ok(())
}

/// Propose namespace delegation for the generated namespace key
async fn propose_namespace_delegation(
    config: &NodeConfig,
    namespace_key: &SigningPublicKey,
    namespace_fingerprint: &str,
) -> Result {
    tracing::debug!("Proposing namespace delegation for {namespace_fingerprint}");

    let namespace_delegation = NamespaceDelegation {
        // fingerprint of the root key defining the namespace
        namespace: namespace_fingerprint.to_string(),

        // target key of getting full rights on the namespace (if target == namespace, it's a root CA)
        target_key: Some(namespace_key.clone()),

        #[allow(deprecated)]
        is_root_delegation: false,

        // restricts target_key to only sign transactions with the specified mapping types.
        // for backwards compatibility, only the following combinations are valid:
        //
        // * is_root_delegation = true,  restriction = empty: the key can sign all mappings
        // * is_root_delegation = false, restriction = empty: the key can sign all mappings but namespace delegations
        // * is_root_delegation = false, restriction = non-empty: the key can only sign the mappings according the restriction that is set
        restriction: Some(namespace_delegation::Restriction::CanSignAllMappings(
            namespace_delegation::CanSignAllMappings {},
        )),
    };

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::NamespaceDelegation(
                        namespace_delegation,
                    )),
                }),
            },
        )),
        must_fully_authorize: true,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Authorized(store_id::Authorized {})),
        }),
        wait_to_become_effective: None,
    });

    topology_client.authorize(request).await?;
    tracing::debug!("Namespace delegation proposed successfully");
    Ok(())
}
