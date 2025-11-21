use ed25519_dalek::{Signer, SigningKey};

use crate::{
    config::NodeConfig,
    dirs::WorkflowDirs,
    error::Result,
    proto::com::{
        daml::ledger::api::v2::interactive::PrepareSubmissionResponse,
        digitalasset::canton::crypto::{
            admin::v30::{
                ExportKeyPairRequest, ListKeysFilters, ListMyKeysRequest,
                vault_service_client::VaultServiceClient,
            },
            v30::{Signature, SignatureFormat, SigningAlgorithmSpec, SigningPublicKey},
        },
    },
    utils,
};

/// Canton protocol version for key export operations
/// This Canton instance requires protocol version 34
const CANTON_PROTOCOL_VERSION: i32 = 34;

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

    // Step 2: Load the DAML public key that was exported in step 1
    // This ensures we use the newly generated key, not an old key from a previous run
    tracing::info!("Loading DAML public key from exported file...");
    let keys_file = dirs
        .keys_dir
        .join(format!("attestor-public-keys-{participant_num}.bin"));

    if !keys_file.exists() {
        anyhow::bail!(
            "Keys file not found: {}. Run step 1 (generate keys) first.",
            keys_file.display()
        );
    }

    let exported_keys: Vec<SigningPublicKey> =
        utils::read_all_messages_from_file(&keys_file).await?;

    if exported_keys.len() != 2 {
        anyhow::bail!(
            "Expected 2 keys in {}, but found {}",
            keys_file.display(),
            exported_keys.len()
        );
    }

    // Second key is the DAML signing key (first is namespace key)
    let signing_public_key = &exported_keys[1];

    // Compute fingerprint of the newly generated DAML key
    let key_fingerprint = utils::compute_fingerprint(signing_public_key);

    tracing::info!("Using DAML key with fingerprint: {key_fingerprint}");
    tracing::debug!("This is the key that was generated in step 1 and added to P2P mapping");

    // Verify this key exists in Canton's vault
    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: key_fingerprint.clone(),
                name: String::new(), // Search by fingerprint, not name
                purpose: vec![],
                usage: vec![],
            }),
        }))
        .await?
        .into_inner();

    if keys_response.private_keys_metadata.is_empty() {
        anyhow::bail!(
            "DAML signing key with fingerprint {key_fingerprint} not found in Canton vault. \
             This should not happen - the key was generated in step 1."
        );
    }

    tracing::debug!(
        "Verified key exists in Canton vault (found {} matching keys)",
        keys_response.private_keys_metadata.len()
    );

    // Step 3: Load the 3 prepared submissions
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

    // Step 4: Export the private key
    tracing::info!("Exporting private key from Canton...");
    tracing::debug!("Key fingerprint: {key_fingerprint}");

    let export_response = vault_client
        .export_key_pair(tonic::Request::new(ExportKeyPairRequest {
            fingerprint: key_fingerprint.clone(),
            protocol_version: CANTON_PROTOCOL_VERSION,
            password: String::new(), // No password encryption
        }))
        .await
        .map_err(|e| {
            tracing::error!("ExportKeyPair RPC failed with error: {e:?}");
            tracing::error!("Attempted fingerprint: {key_fingerprint}");
            e
        })?
        .into_inner();

    // Step 5: Extract Ed25519 private key from Canton's export response
    // Canton returns the key in a custom format with embedded metadata
    tracing::debug!("Parsing exported key pair...");
    tracing::debug!("Key pair bytes length: {}", export_response.key_pair.len());

    let exported_key_data = &export_response.key_pair;

    // Dump first 256 bytes of exported data for analysis
    let dump_len = exported_key_data.len().min(256);
    tracing::debug!("First {dump_len} bytes of exported key data:");
    for chunk_start in (0..dump_len).step_by(32) {
        let chunk_end = (chunk_start + 32).min(dump_len);
        let chunk = &exported_key_data[chunk_start..chunk_end];
        tracing::debug!(
            "  [{:03}-{:03}]: {:02x?}",
            chunk_start,
            chunk_end - 1,
            chunk
        );
    }

    // Strategy: Try ALL possible 32-byte sequences and test each one
    // The correct private key should verify against the public key
    let key_size = ED25519_PRIVATE_KEY_LENGTH as usize;
    let max_offset = exported_key_data.len().saturating_sub(key_size);

    tracing::info!("Searching for valid Ed25519 private key among {max_offset} possible positions");

    let mut candidate_keys = Vec::new();

    // First, try DER-tagged sequences (0x04 0x20 pattern)
    for offset in 0..max_offset.saturating_sub(2) {
        if exported_key_data[offset] == DER_OCTET_STRING_TAG
            && exported_key_data[offset + 1] == ED25519_PRIVATE_KEY_LENGTH
        {
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&exported_key_data[offset + 2..offset + 2 + key_size]);
            candidate_keys.push((offset + 2, key_bytes, "DER-tagged"));
            tracing::debug!(
                "Found DER-tagged 32-byte sequence at offset {}: {:02x?}...",
                offset + 2,
                &key_bytes[..8]
            );
        }
    }

    if candidate_keys.is_empty() {
        tracing::warn!("No DER-tagged sequences found, trying all possible 32-byte sequences");

        // Try every possible 32-byte sequence in the exported data
        for offset in (0..max_offset).step_by(4) {
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&exported_key_data[offset..offset + key_size]);
            candidate_keys.push((offset, key_bytes, "raw"));
        }

        tracing::debug!("Found {} raw 32-byte candidates", candidate_keys.len());
    }

    if candidate_keys.is_empty() {
        anyhow::bail!("Could not find any Ed25519 key candidates in exported data");
    }

    tracing::info!(
        "Found {} candidate Ed25519 key positions to try",
        candidate_keys.len()
    );

    // Step 6: Try each candidate key and verify it produces the correct public key
    tracing::info!("Verifying candidates against expected public key...");

    // Get the public key bytes from Canton's metadata for verification
    // Canton stores Ed25519 public keys in DER format with this structure:
    // - Bytes 0-11: DER wrapper (SEQUENCE + algorithm OID + BIT STRING header)
    // - Bytes 12-43: Raw 32-byte Ed25519 public key
    let expected_public_key_der = &signing_public_key.public_key;
    tracing::debug!(
        "Expected public key DER (first 16 bytes): {:02x?}",
        &expected_public_key_der[..16.min(expected_public_key_der.len())]
    );

    // Extract raw Ed25519 public key from DER format
    const DER_HEADER_LENGTH: usize = 12;
    const ED25519_PUBLIC_KEY_LENGTH: usize = 32;

    if expected_public_key_der.len() < DER_HEADER_LENGTH + ED25519_PUBLIC_KEY_LENGTH {
        anyhow::bail!(
            "Expected public key is too short: {} bytes (need at least {})",
            expected_public_key_der.len(),
            DER_HEADER_LENGTH + ED25519_PUBLIC_KEY_LENGTH
        );
    }

    let expected_raw_public_key = &expected_public_key_der[DER_HEADER_LENGTH..];
    tracing::debug!(
        "Expected raw public key (first 16 bytes): {:02x?}",
        &expected_raw_public_key[..16.min(expected_raw_public_key.len())]
    );

    let mut verified_key_bytes: Option<[u8; 32]> = None;

    for (offset, key_bytes, source) in &candidate_keys {
        let signing_key = SigningKey::from_bytes(key_bytes);
        let derived_public_key = signing_key.verifying_key();
        let derived_public_bytes = derived_public_key.to_bytes();

        tracing::debug!(
            "Testing candidate at offset {offset} ({source}): derived public key {:02x?}...",
            &derived_public_bytes[..8]
        );

        // Compare raw Ed25519 public keys (32 bytes)
        if derived_public_bytes.as_slice() == expected_raw_public_key {
            tracing::info!("✓ Found matching private key at offset {offset} ({source})");
            tracing::debug!("Private key (first 16 bytes): {:02x?}", &key_bytes[..16]);
            verified_key_bytes = Some(*key_bytes);
            break;
        }
    }

    let key_bytes = verified_key_bytes.ok_or_else(|| {
        anyhow::anyhow!(
            "None of the {} candidate keys produced the expected public key. \
            This indicates the private key is not in the expected format in the exported data.",
            candidate_keys.len()
        )
    })?;

    tracing::info!("Successfully verified Ed25519 private key");

    // Step 7: Sign transaction hashes with verified key
    tracing::info!("Signing transaction hashes...");
    tracing::debug!(
        "Transaction hash 1: {:02x?}",
        &prepared_sub1.prepared_transaction_hash
    );
    tracing::debug!(
        "Transaction hash 2: {:02x?}",
        &prepared_sub2.prepared_transaction_hash
    );
    tracing::debug!(
        "Transaction hash 3: {:02x?}",
        &prepared_sub3.prepared_transaction_hash
    );
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
    tracing::debug!(
        "Signature 1 (first 32 bytes): {:02x?}",
        &signature1_bytes[..32]
    );
    tracing::debug!(
        "Signature 2 (first 32 bytes): {:02x?}",
        &signature2_bytes[..32]
    );
    tracing::debug!(
        "Signature 3 (first 32 bytes): {:02x?}",
        &signature3_bytes[..32]
    );

    // Verify the signatures locally before sending to Canton
    use ed25519_dalek::{Signature as DalekSignature, Verifier};

    let verifying_key = signing_key.verifying_key();
    let verifying_key_bytes = verifying_key.to_bytes();

    tracing::debug!(
        "Verifying key (raw 32 bytes): {:02x?}",
        &verifying_key_bytes
    );
    tracing::debug!(
        "Expected Canton key (raw): {:02x?}",
        &expected_raw_public_key[..32.min(expected_raw_public_key.len())]
    );
    tracing::debug!("Key fingerprint used in signatures: {key_fingerprint}");

    let sig1 = DalekSignature::from_bytes(&signature1_bytes);
    if verifying_key
        .verify(&prepared_sub1.prepared_transaction_hash, &sig1)
        .is_ok()
    {
        tracing::info!("✓ Signature 1 verified locally");
    } else {
        tracing::error!("✗ Signature 1 failed local verification!");
    }

    let sig2 = DalekSignature::from_bytes(&signature2_bytes);
    if verifying_key
        .verify(&prepared_sub2.prepared_transaction_hash, &sig2)
        .is_ok()
    {
        tracing::info!("✓ Signature 2 verified locally");
    } else {
        tracing::error!("✗ Signature 2 failed local verification!");
    }

    let sig3 = DalekSignature::from_bytes(&signature3_bytes);
    if verifying_key
        .verify(&prepared_sub3.prepared_transaction_hash, &sig3)
        .is_ok()
    {
        tracing::info!("✓ Signature 3 verified locally");
    } else {
        tracing::error!("✗ Signature 3 failed local verification!");
    }

    // Step 8: Create Signature protobuf messages
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

    // Step 9: Save signatures to file
    let execution_dir = dirs.workflow_dir.join("execution");
    let signatures_dir = execution_dir.join("signatures");
    tokio::fs::create_dir_all(&signatures_dir).await?;

    let signatures_file =
        signatures_dir.join(format!("submission-signatures-{participant_num}.bin"));
    tracing::info!("Saving signatures to {}", signatures_file.display());

    utils::write_messages_to_file(&[signature1, signature2, signature3], &signatures_file).await?;

    tracing::info!("Signatures saved successfully");
    Ok(())
}
