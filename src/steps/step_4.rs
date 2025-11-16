use crate::config::NodeConfig;
use ed25519_dalek::{Signer, SigningKey};

use crate::{
    dirs::WorkflowDirs,
    error::Result,
    proto::com::{
        daml::ledger::api::v2::interactive::PrepareSubmissionResponse,
        digitalasset::canton::crypto::{
            admin::v30::{
                ExportKeyPairRequest, ListKeysFilters, ListMyKeysRequest,
                vault_service_client::VaultServiceClient,
            },
            v30::{Signature, SignatureFormat, SigningAlgorithmSpec, public_key},
        },
    },
    utils,
};

/// Canton protocol version for key export operations
const CANTON_PROTOCOL_VERSION: i32 = 33;

/// Name of the DAML signing key in Canton's vault
const DAML_SIGNING_KEY_NAME: &str = "cbtc-network-daml-transactions";

/// DER OCTET STRING tag
const DER_OCTET_STRING_TAG: u8 = 0x04;

/// Expected length of Ed25519 private key in bytes (32 bytes)
const ED25519_PRIVATE_KEY_LENGTH: u8 = 0x20;

/// Sign prepared ledger submissions with DAML key
///
/// Corresponds to: 04_SignSubmissions.sc
///
/// This step must be run by each attestor participant to sign the prepared submissions.
/// Each attestor signs with their DAML signing key.
///
/// # Arguments
/// * `config` - Configuration with Admin API connection details
/// * `dirs` - WorkflowDirs containing all directory paths
pub async fn sign_submissions(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Signing submissions...");

    // Step 1: Get participant number
    let participant_num = utils::get_participant_number(config, &dirs.ids_dir).await?;
    tracing::debug!("Determined participant number: {participant_num}");

    // Step 2: Find the DAML signing key
    tracing::info!("Finding DAML signing key...");
    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: String::new(),
                name: DAML_SIGNING_KEY_NAME.to_string(),
                purpose: vec![],
                usage: vec![],
            }),
        }))
        .await?
        .into_inner();

    tracing::debug!(
        "Found {} private keys",
        keys_response.private_keys_metadata.len()
    );

    let daml_key_metadata = keys_response
        .private_keys_metadata
        .first()
        .ok_or_else(|| anyhow::anyhow!("DAML signing key '{DAML_SIGNING_KEY_NAME}' not found"))?;

    tracing::debug!(
        "Key name: {}",
        daml_key_metadata
            .public_key_with_name
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or("N/A")
    );

    // Extract public key and compute fingerprint
    let public_key_with_name = daml_key_metadata
        .public_key_with_name
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No public key in metadata"))?;

    let public_key = public_key_with_name
        .public_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No public key"))?;

    let signing_public_key = match &public_key.key {
        Some(public_key::Key::SigningPublicKey(spk)) => spk,
        _ => anyhow::bail!("Expected signing public key"),
    };

    // Compute fingerprint using the standard Canton fingerprinting function
    let key_fingerprint = utils::compute_fingerprint(signing_public_key);

    tracing::debug!("Found DAML key with fingerprint: {key_fingerprint}");

    // Step 2: Load the 3 prepared submissions
    tracing::info!("Loading prepared submissions...");
    let ledger_submissions_dir = dirs.workflow_dir.join("ledger-submissions");
    let prepared_dir = ledger_submissions_dir.join("prepared");

    let prepared_sub1: PrepareSubmissionResponse =
        utils::read_first_message_from_file(&prepared_dir.join("prepared-submission-1.bin"))
            .await?;
    let prepared_sub2: PrepareSubmissionResponse =
        utils::read_first_message_from_file(&prepared_dir.join("prepared-submission-2.bin"))
            .await?;
    let prepared_sub3: PrepareSubmissionResponse =
        utils::read_first_message_from_file(&prepared_dir.join("prepared-submission-3.bin"))
            .await?;

    tracing::debug!("Loaded 3 prepared submissions");

    // Step 3: Export the private key
    tracing::info!("Exporting private key from Canton...");

    let export_response = vault_client
        .export_key_pair(tonic::Request::new(ExportKeyPairRequest {
            fingerprint: key_fingerprint.clone(),
            protocol_version: CANTON_PROTOCOL_VERSION,
            password: String::new(), // No password encryption
        }))
        .await?
        .into_inner();

    // Step 4: Extract Ed25519 private key from Canton's export response
    tracing::debug!("Parsing exported key pair...");
    tracing::debug!("Key pair bytes length: {}", export_response.key_pair.len());

    // Canton protocol version 33 returns the private key in DER-encoded (PKCS#8) format.
    // The raw 32-byte Ed25519 key is embedded as a DER OCTET STRING within the structure.
    // We search for the pattern: 0x04 0x20 (OCTET STRING tag + length 32) followed by the key.

    let exported_key_data = &export_response.key_pair;
    let key_size = ED25519_PRIVATE_KEY_LENGTH as usize;
    let search_window = exported_key_data.len().saturating_sub(key_size + 2);

    let mut ed25519_key_bytes: Option<[u8; 32]> = None;

    for offset in 0..search_window {
        if exported_key_data[offset] == DER_OCTET_STRING_TAG
            && exported_key_data[offset + 1] == ED25519_PRIVATE_KEY_LENGTH
        {
            // Found the DER OCTET STRING containing the 32-byte Ed25519 private key
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&exported_key_data[offset + 2..offset + 2 + key_size]);
            ed25519_key_bytes = Some(key_bytes);
            tracing::debug!("Found Ed25519 private key at offset {offset}");
            break;
        }
    }

    let key_bytes = ed25519_key_bytes
        .ok_or_else(|| anyhow::anyhow!("Could not find Ed25519 private key in exported data"))?;

    tracing::debug!("Successfully extracted Ed25519 private key");

    // Step 5: Create Ed25519 signing key from raw bytes
    tracing::info!("Signing transaction hashes...");
    let signing_key = SigningKey::from_bytes(&key_bytes);

    // Sign each prepared transaction hash
    let signature1_bytes = signing_key
        .sign(&prepared_sub1.prepared_transaction_hash)
        .to_bytes();
    let signature2_bytes = signing_key
        .sign(&prepared_sub2.prepared_transaction_hash)
        .to_bytes();
    let signature3_bytes = signing_key
        .sign(&prepared_sub3.prepared_transaction_hash)
        .to_bytes();

    tracing::debug!("Generated 3 signatures");

    // Step 6: Create Signature protobuf messages
    // Ed25519 signatures use CONCAT format (r || s in little-endian)
    let signature1 = Signature {
        format: SignatureFormat::Concat as i32,
        signature: signature1_bytes.to_vec(),
        signed_by: key_fingerprint.clone(),
        signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
        signature_delegation: None,
    };

    let signature2 = Signature {
        format: SignatureFormat::Concat as i32,
        signature: signature2_bytes.to_vec(),
        signed_by: key_fingerprint.clone(),
        signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
        signature_delegation: None,
    };

    let signature3 = Signature {
        format: SignatureFormat::Concat as i32,
        signature: signature3_bytes.to_vec(),
        signed_by: key_fingerprint.clone(),
        signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
        signature_delegation: None,
    };

    // Step 7: Save signatures to file
    let execution_dir = dirs.workflow_dir.join("execution");
    let signatures_dir = execution_dir.join("signatures");
    tokio::fs::create_dir_all(&signatures_dir).await?;

    let signatures_file =
        signatures_dir.join(format!("submission-signatures-{}.bin", participant_num));
    tracing::info!("Saving signatures to {}", signatures_file.display());

    utils::write_messages_to_file(&[signature1, signature2, signature3], &signatures_file).await?;

    tracing::info!("Signatures saved successfully");
    Ok(())
}
