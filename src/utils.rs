use std::path::Path;

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio::fs;

use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        admin::{
            party_management_service_client::PartyManagementServiceClient,
            user_management_service_client::UserManagementServiceClient,
        },
        interactive::interactive_submission_service_client::InteractiveSubmissionServiceClient,
        state_service_client::StateServiceClient,
    },
    digitalasset::canton::{
        admin::participant::v30::{
            GetSynchronizerIdRequest,
            synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
        },
        crypto::v30::SigningPublicKey,
        topology::admin::v30::{
            GetIdRequest,
            identity_initialization_service_client::IdentityInitializationServiceClient,
        },
    },
};

use crate::{config::NodeConfig, error::Result, participant_id::CantonId};

/// Multihash prefix for SHA-256 hashes in Canton
/// - 0x12 = SHA-256 hash algorithm identifier
/// - 0x20 = 32 bytes (length of SHA-256 output)
pub const MULTIHASH_SHA256_PREFIX: &str = "1220";

/// Read all protobuf messages from a file
///
/// Canton writes multiple protobuf messages to a single file with length prefixes.
/// Each message is prefixed with a varint indicating its length.
pub async fn read_all_messages_from_file<M: Message + Default>(
    path: impl AsRef<Path>,
) -> Result<Vec<M>> {
    let data = fs::read(path.as_ref()).await?;
    let mut cursor = &data[..];
    let mut messages = Vec::new();

    while cursor.has_remaining() {
        // Read the length prefix (varint)
        let len = prost::encoding::decode_varint(&mut cursor)? as usize;

        // Read the message bytes
        if cursor.remaining() < len {
            let remaining = cursor.remaining();
            anyhow::bail!(
                "Incomplete message: expected {len} bytes, but only {remaining} remaining"
            );
        }

        let message_bytes = &cursor[..len];
        cursor.advance(len);

        // Decode the message
        let message = M::decode(message_bytes)?;
        messages.push(message);
    }

    Ok(messages)
}

/// Read the first protobuf message from a file
pub async fn read_first_message_from_file<M: Message + Default>(
    path: impl AsRef<Path>,
) -> Result<M> {
    let messages = read_all_messages_from_file(path).await?;
    messages
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("File contains no messages"))
}

/// Write multiple protobuf messages to a file
///
/// Each message is prefixed with a varint indicating its length, matching Canton's format.
pub async fn write_messages_to_file<M: Message>(messages: &[M], path: impl AsRef<Path>) -> Result {
    let mut buffer = BytesMut::new();

    for message in messages {
        // Encode the message to get its length
        let encoded = message.encode_to_vec();
        let len = encoded.len();

        // Write length prefix (varint)
        prost::encoding::encode_varint(len as u64, &mut buffer);

        // Write message bytes
        buffer.put_slice(&encoded);
    }

    fs::write(path.as_ref(), &buffer[..]).await?;
    Ok(())
}

/// Write a single protobuf message to a file
pub async fn write_message_to_file<M: Message>(message: &M, path: impl AsRef<Path>) -> Result {
    let mut buffer = BytesMut::new();
    let encoded = message.encode_to_vec();
    let len = encoded.len();

    // Write length prefix (varint)
    prost::encoding::encode_varint(len as u64, &mut buffer);

    // Write message bytes
    buffer.put_slice(&encoded);

    fs::write(path.as_ref(), &buffer[..]).await?;
    Ok(())
}

/// Read raw bytes from a file (for simple binary data like participant IDs)
pub async fn read_bytes_from_file(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let data = fs::read(path.as_ref()).await?;
    Ok(data)
}

/// Find files in a directory matching a prefix and suffix pattern
///
/// Returns a sorted list of file paths that match `{prefix}*{suffix}`.
/// Commonly used for discovering attestor keys, signed proposals, etc.
pub async fn find_files_by_pattern(
    dir: impl AsRef<Path>,
    prefix: &str,
    suffix: &str,
) -> Result<Vec<std::path::PathBuf>> {
    let dir = dir.as_ref();
    let mut entries = fs::read_dir(dir).await?;
    let mut files = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with(prefix)
            && name.ends_with(suffix)
        {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

/// Write raw bytes to a file
pub async fn write_bytes_to_file(data: &[u8], path: impl AsRef<Path>) -> Result {
    fs::write(path.as_ref(), data).await?;
    Ok(())
}

/// Retry a future until it returns true or timeout is reached
///
/// Used for waiting for topology propagation or ledger state changes.
pub async fn retry_until_true<F, Fut>(
    mut check: F,
    max_attempts: usize,
    delay: std::time::Duration,
) -> Result
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    for attempt in 1..=max_attempts {
        match check().await {
            Ok(true) => {
                tracing::info!("Condition met after {attempt} attempt(s)");
                return Ok(());
            }
            Ok(false) => {
                tracing::debug!("Attempt {attempt}/{max_attempts}: condition not met, retrying...");
            }

            Err(e) => {
                tracing::warn!("Attempt {attempt}/{max_attempts}: error checking condition: {e}");
            }
        }

        if attempt < max_attempts {
            tokio::time::sleep(delay).await;
        }
    }

    anyhow::bail!("Condition not met after {max_attempts} attempts")
}

/// Compute fingerprint (hash) of a Canton SigningPublicKey
///
/// Canton uses multihash format for fingerprints:
/// - Prefix "1220" indicates SHA-256 hash (0x12) with 32 bytes length (0x20)
/// - Followed by the hex-encoded SHA-256 hash of the protobuf-serialized key
///
/// The fingerprint serves as the namespace identifier and unique key identifier in Canton.
pub fn compute_fingerprint(key: &SigningPublicKey) -> String {
    use sha2::{Digest, Sha256};
    use x509_parser::prelude::*;

    // Canton uses domain-separated hashing with a purpose ID prefix
    // HashPurpose.PublicKeyFingerprint = 12
    // For Curve25519 keys in X.509 format, Canton extracts the raw key bytes first
    const PURPOSE_PUBLIC_KEY_FINGERPRINT: i32 = 12;

    tracing::debug!(
        "Computing fingerprint from {count} bytes of X.509 key material",
        count = key.public_key.len()
    );

    let mut hasher = Sha256::new();

    // Add purpose ID as 4-byte big-endian integer (domain separation)
    hasher.update(PURPOSE_PUBLIC_KEY_FINGERPRINT.to_be_bytes());

    // Extract raw key bytes from X.509 SubjectPublicKeyInfo and add to hash
    match SubjectPublicKeyInfo::from_der(&key.public_key) {
        Ok((_, spki)) => {
            // Get the BIT STRING containing the raw public key
            let raw_bytes = spki.subject_public_key.data;
            tracing::debug!(
                "Extracted {count} raw key bytes from X.509 structure",
                count = raw_bytes.len()
            );
            hasher.update(raw_bytes.as_ref());
        }
        Err(e) => {
            tracing::warn!("Failed to parse X.509 structure: {e}, falling back to full key bytes");
            hasher.update(&key.public_key);
        }
    }

    let hash_result = hasher.finalize();

    let fingerprint = format!(
        "{MULTIHASH_SHA256_PREFIX}{hash}",
        hash = hex::encode(hash_result)
    );
    tracing::debug!("Computed fingerprint: {fingerprint}");

    fingerprint
}

/// Get synchronizer ID from config
///
/// Queries the participant's synchronizer connectivity service to get the physical
/// synchronizer ID for the configured synchronizer alias.
pub async fn get_synchronizer_id(config: &NodeConfig) -> Result<String> {
    let mut conn_client =
        SynchronizerConnectivityServiceClient::connect(config.admin_api_url()).await?;

    let response = conn_client
        .get_synchronizer_id(tonic::Request::new(GetSynchronizerIdRequest {
            synchronizer_alias: config.synchronizer().to_string(),
        }))
        .await?
        .into_inner();

    if response.physical_synchronizer_id.is_empty() {
        anyhow::bail!(
            "No synchronizer ID returned for synchronizer alias '{id}'",
            id = config.synchronizer()
        );
    }

    Ok(response.physical_synchronizer_id)
}

/// Extract synchronizer fingerprint from full synchronizer ID
///
/// Canton 3.4+ returns synchronizer IDs in format: `<alias>::<fingerprint>::<protocol-version>`
/// For party allocation, we need the format `<alias>::<fingerprint>` (removing only the protocol version).
///
/// Example:
/// - Input: `global-domain::122033d02b977e2b698d6a6397eb62e43f7bff34bc8fa814384c4a533d1162239df8::34-0`
/// - Output: `global-domain::122033d02b977e2b698d6a6397eb62e43f7bff34bc8fa814384c4a533d1162239df8`
pub fn extract_synchronizer_fingerprint(synchronizer_id: &str) -> Result<String> {
    let parts: Vec<&str> = synchronizer_id.split("::").collect();

    if parts.len() == 3 {
        // Format: alias::fingerprint::version -> return alias::fingerprint
        Ok(format!(
            "{alias}::{fingerprint}",
            alias = parts[0],
            fingerprint = parts[1]
        ))
    } else if parts.len() == 2 {
        // Already in alias::fingerprint format
        Ok(synchronizer_id.to_string())
    } else {
        anyhow::bail!(
            "Invalid synchronizer ID format '{synchronizer_id}': expected format '<alias>::<fingerprint>::<version>' or '<alias>::<fingerprint>'"
        )
    }
}

/// Get participant ID from Canton
///
/// Queries the participant's identity initialization service to get the unique participant ID.
pub async fn get_participant_id(config: &NodeConfig) -> Result<CantonId> {
    let mut id_client =
        IdentityInitializationServiceClient::connect(config.admin_api_url()).await?;
    let response = id_client
        .get_id(tonic::Request::new(GetIdRequest {}))
        .await?
        .into_inner();

    if response.unique_identifier.is_empty() {
        anyhow::bail!("No participant ID returned");
    }

    CantonId::parse(&response.unique_identifier)
}

/// Find which participant number corresponds to the current participant ID
///
/// Reads all participant-id-*.bin files and matches the current participant ID
/// against them to determine which number this participant is.
pub async fn find_participant_number(
    ids_dir: &Path,
    current_participant_id: &CantonId,
) -> Result<u32> {
    let id_files = find_files_by_pattern(ids_dir, "participant-id", ".bin").await?;

    if id_files.is_empty() {
        anyhow::bail!(
            "No participant ID files found in {path}",
            path = ids_dir.display()
        );
    }

    // Read each file and match against current participant ID
    for (idx, id_file) in id_files.iter().enumerate() {
        let file_content = fs::read_to_string(id_file).await?;
        let stored_id = CantonId::parse_from_file(&file_content)?;

        if &stored_id == current_participant_id {
            return Ok((idx + 1) as u32);
        }
    }

    anyhow::bail!("Current participant ID '{current_participant_id}' not found in ids directory")
}

/// Get participant number for current participant
///
/// Determines participant number from network config order (1-indexed).
pub async fn get_participant_number(config: &NodeConfig) -> Result<u32> {
    let network_config = config.load_network_config().await?;
    let current_node_id = &config.node.node_id;

    for (idx, participant) in network_config.participants.iter().enumerate() {
        if &participant.id == current_node_id {
            return Ok((idx + 1) as u32);
        }
    }

    anyhow::bail!(
        "Current node '{current_node_id}' not found in network config participants"
    )
}

/// Macro to define authenticated gRPC client creator functions
macro_rules! define_client_creator {
    ($fn_name:ident, $client_type:ident) => {
        pub async fn $fn_name(
            config: &NodeConfig,
        ) -> Result<
            $client_type<
                tonic::service::interceptor::InterceptedService<
                    tonic::transport::Channel,
                    impl Fn(
                        tonic::Request<()>,
                    ) -> std::result::Result<tonic::Request<()>, tonic::Status>
                    + Clone,
                >,
            >,
        > {
            let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
                .connect()
                .await?;

            let token = config.canton.ledger_api_token.clone();
            let interceptor = move |mut req: tonic::Request<()>| {
                if let Some(ref token) = token {
                    let bearer_token = format!("Bearer {token}");
                    let token_value = tonic::metadata::MetadataValue::try_from(&bearer_token)
                        .map_err(|e| {
                            tonic::Status::unauthenticated(format!("Invalid token: {e}"))
                        })?;
                    req.metadata_mut().insert("authorization", token_value);
                }
                Ok(req)
            };

            Ok($client_type::with_interceptor(channel, interceptor))
        }
    };
}

define_client_creator!(create_party_client, PartyManagementServiceClient);
define_client_creator!(create_user_client, UserManagementServiceClient);
define_client_creator!(create_submission_client, InteractiveSubmissionServiceClient);
define_client_creator!(create_state_client, StateServiceClient);

#[cfg(test)]
mod tests {
    use super::*;

    use prost_types::Timestamp;

    #[tokio::test]
    async fn test_write_and_read_single_message() -> Result {
        let temp_dir = tempfile::tempdir()?;
        let file_path = temp_dir.path().join("test.bin");

        let message = Timestamp {
            seconds: 123456789,
            nanos: 987654321,
        };

        // Write
        write_message_to_file(&message, &file_path).await?;

        // Read
        let read_message: Timestamp = read_first_message_from_file(&file_path).await?;

        assert_eq!(message.seconds, read_message.seconds);
        assert_eq!(message.nanos, read_message.nanos);
        Ok(())
    }

    #[tokio::test]
    async fn test_write_and_read_multiple_messages() -> Result {
        let temp_dir = tempfile::tempdir()?;
        let file_path = temp_dir.path().join("test_multiple.bin");

        let messages = vec![
            Timestamp {
                seconds: 1,
                nanos: 100,
            },
            Timestamp {
                seconds: 2,
                nanos: 200,
            },
            Timestamp {
                seconds: 3,
                nanos: 300,
            },
        ];

        // Write
        write_messages_to_file(&messages, &file_path).await?;

        // Read
        let read_messages: Vec<Timestamp> = read_all_messages_from_file(&file_path).await?;

        assert_eq!(messages.len(), read_messages.len());
        for (original, read) in messages.iter().zip(read_messages.iter()) {
            assert_eq!(original.seconds, read.seconds);
            assert_eq!(original.nanos, read.nanos);
        }
        Ok(())
    }
}
