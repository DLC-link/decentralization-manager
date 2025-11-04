use std::path::Path;

use tokio::fs;

use crate::{
    config::Config,
    error::Result,
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
            AuthorizeRequest, GetIdRequest, StoreId, Synchronizer, authorize_request,
            identity_initialization_service_client::IdentityInitializationServiceClient, store_id,
            synchronizer, topology_manager_write_service_client::TopologyManagerWriteServiceClient,
        },
    },
    utils,
};

// Constants
const CBTC_DAR_FILENAME: &str = "cbtc-1.0.0.dar";
const GOVERNANCE_DAR_FILENAME: &str = "cbtc-governance-1.0.0.dar";
const CBTC_DAR_DESCRIPTION: &str = "CBTC main application";
const GOVERNANCE_DAR_DESCRIPTION: &str = "CBTC governance rules";
const NAMESPACE_KEY_NAME: &str = "cbtc-network-namespace";
const DAML_KEY_NAME: &str = "cbtc-network-daml-transactions";
const ATTESTOR_KEYS_FILENAME_PREFIX: &str = "attestor-public-keys";
const PARTICIPANT_ID_FILENAME_PREFIX: &str = "participant-id";
const PARTICIPANT_ID_PREFIX: &str = "PAR::";

/// Upload DAR files to the participant
///
/// Corresponds to: 00_UploadDars.sc
///
/// Uploads both CBTC and governance DAR files to the Canton participant.
pub async fn upload_dars(config: &Config, dars_dir: &Path) -> Result {
    tracing::info!("Uploading DARs from {}", dars_dir.display());

    let mut client = PackageServiceClient::connect(config.admin_api_url()).await?;

    // Read both DAR files
    let cbtc_dar_path = dars_dir.join(CBTC_DAR_FILENAME);
    let gov_dar_path = dars_dir.join(GOVERNANCE_DAR_FILENAME);

    // Upload CBTC DAR
    tracing::debug!("Reading {}", cbtc_dar_path.display());
    let cbtc_dar_data = fs::read(&cbtc_dar_path).await?;

    let cbtc_request = tonic::Request::new(UploadDarRequest {
        dars: vec![UploadDarData {
            bytes: cbtc_dar_data,
            description: Some(CBTC_DAR_DESCRIPTION.to_string()),
            expected_main_package_id: None,
        }],
        vet_all_packages: true,
        synchronize_vetting: true,
        synchronizer_id: None, // Auto-detect if single synchronizer
    });

    tracing::debug!("Uploading CBTC DAR to Canton...");
    client.upload_dar(cbtc_request).await?;
    tracing::debug!("CBTC DAR uploaded successfully");

    // Upload governance DAR
    tracing::debug!("Reading {}", gov_dar_path.display());
    let gov_dar_data = fs::read(&gov_dar_path).await?;

    let gov_request = tonic::Request::new(UploadDarRequest {
        dars: vec![UploadDarData {
            bytes: gov_dar_data,
            description: Some(GOVERNANCE_DAR_DESCRIPTION.to_string()),
            expected_main_package_id: None,
        }],
        vet_all_packages: true,
        synchronize_vetting: true,
        synchronizer_id: None, // Auto-detect if single synchronizer
    });

    tracing::debug!("Uploading governance DAR to Canton...");
    client.upload_dar(gov_request).await?;
    tracing::debug!("Governance DAR uploaded successfully");

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
pub async fn generate_keys(config: &Config, keys_dir: &Path, ids_dir: &Path) -> Result {
    tracing::info!("Generating cryptographic keys...");

    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    // Generate namespace signing key
    tracing::debug!("Generating namespace signing key with name '{NAMESPACE_KEY_NAME}'");
    let namespace_key = generate_signing_key(
        &mut vault_client,
        NAMESPACE_KEY_NAME,
        vec![SigningKeyUsage::Namespace as i32],
    )
    .await?;

    let namespace_fingerprint = crate::utils::compute_fingerprint(&namespace_key);
    tracing::debug!("Namespace key fingerprint: {namespace_fingerprint}");

    // Propose namespace delegation
    propose_namespace_delegation(config, &namespace_key, &namespace_fingerprint).await?;

    // Generate DAML signing key for transactions
    tracing::debug!("Generating DAML signing key with name '{DAML_KEY_NAME}'");
    let daml_key = generate_signing_key(
        &mut vault_client,
        DAML_KEY_NAME,
        vec![SigningKeyUsage::Protocol as i32],
    )
    .await?;

    // Get participant ID and export keys
    let participant_id = get_participant_id(config).await?;
    let participant_num = extract_participant_number(&participant_id);

    export_keys(keys_dir, &namespace_key, &daml_key, participant_num).await?;
    export_participant_id(ids_dir, &participant_id, participant_num).await?;

    tracing::info!("Keys and participant ID exported successfully");

    Ok(())
}

/// Helper: Generate a signing key via VaultService
async fn generate_signing_key(
    vault_client: &mut VaultServiceClient<tonic::transport::Channel>,
    name: &str,
    usage: Vec<i32>,
) -> Result<crate::proto::com::digitalasset::canton::crypto::v30::SigningPublicKey> {
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

/// Helper: Get participant ID
async fn get_participant_id(config: &Config) -> Result<String> {
    let mut id_client =
        IdentityInitializationServiceClient::connect(config.admin_api_url()).await?;
    let response = id_client
        .get_id(tonic::Request::new(GetIdRequest {}))
        .await?
        .into_inner();

    if response.unique_identifier.is_empty() {
        anyhow::bail!("No participant ID returned");
    }

    Ok(response.unique_identifier)
}

/// Helper: Extract participant number from ID (e.g., "participant1" -> 1)
fn extract_participant_number(participant_id: &str) -> u32 {
    participant_id
        .split("::")
        .next()
        .and_then(|name| name.strip_prefix("participant"))
        .and_then(|num_str| num_str.parse::<u32>().ok())
        .unwrap_or(1)
}

/// Helper: Export keys to file
async fn export_keys(
    keys_dir: &Path,
    namespace_key: &SigningPublicKey,
    daml_key: &SigningPublicKey,
    participant_num: u32,
) -> Result {
    let filename = format!("{ATTESTOR_KEYS_FILENAME_PREFIX}-{participant_num}.bin");
    let output_path = keys_dir.join(&filename);
    tracing::debug!("Exporting keys to {}", output_path.display());
    utils::write_messages_to_file(&[namespace_key.clone(), daml_key.clone()], &output_path).await
}

/// Helper: Export participant ID to file
async fn export_participant_id(
    ids_dir: &Path,
    participant_id: &str,
    participant_num: u32,
) -> Result {
    let id_with_prefix = format!("{PARTICIPANT_ID_PREFIX}{participant_id}");
    let filename = format!("{PARTICIPANT_ID_FILENAME_PREFIX}-{participant_num}.bin");
    let output_path = ids_dir.join(&filename);
    tracing::debug!("Exporting participant ID to {}", output_path.display());
    fs::write(&output_path, id_with_prefix.as_bytes()).await?;
    Ok(())
}

/// Propose namespace delegation for the generated namespace key
async fn propose_namespace_delegation(
    config: &Config,
    namespace_key: &SigningPublicKey,
    namespace_fingerprint: &str,
) -> Result {
    tracing::debug!("Proposing namespace delegation for {namespace_fingerprint}");

    let synchronizer_id = crate::utils::get_synchronizer_id(config).await?;

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
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::Id(synchronizer_id)),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_client.authorize(request).await?;
    tracing::debug!("Namespace delegation proposed successfully");
    Ok(())
}
