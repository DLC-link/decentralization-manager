use std::path::Path;

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio::{fs, sync::OnceCell};

use anyhow::Context;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        admin::{
            party_management_service_client::PartyManagementServiceClient,
            user_management_service_client::UserManagementServiceClient,
        },
        event_query_service_client::EventQueryServiceClient,
        interactive::interactive_submission_service_client::InteractiveSubmissionServiceClient,
        state_service_client::StateServiceClient,
        update_service_client::UpdateServiceClient,
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

use crate::{canton_id::CantonId, config::NodeConfig, error::Result};

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

/// Read the first protobuf message from a byte slice
///
/// This is useful when receiving message data over the network instead of from a file.
pub fn read_first_message_from_bytes<M: Message + Default>(data: &[u8]) -> Result<M> {
    let mut cursor = data;

    if !cursor.has_remaining() {
        anyhow::bail!("Data is empty, no messages to read");
    }

    // Read the length prefix (varint)
    let len = prost::encoding::decode_varint(&mut cursor)? as usize;

    // Read the message bytes
    if cursor.remaining() < len {
        let remaining = cursor.remaining();
        anyhow::bail!("Incomplete message: expected {len} bytes, but only {remaining} remaining");
    }

    let message_bytes = &cursor[..len];
    let message = M::decode(message_bytes)?;
    Ok(message)
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
/// Commonly used for discovering peer keys, signed proposals, etc.
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

/// Process-wide cache for the resolved physical synchronizer ID.
///
/// The ID is stable for the lifetime of a DPM process (it only changes if the
/// node is reprovisioned against a different synchronizer, which requires a
/// restart). Resolving it costs a fresh gRPC channel + a one-shot
/// `get_synchronizer_id` call against the Canton admin API — observed at
/// ~4.3s median over a kubectl-tunneled devnet (see #149). Caching it
/// eliminates that cost from every call site that previously resolved it
/// per request.
static SYNCHRONIZER_ID_CACHE: OnceCell<String> = OnceCell::const_new();

/// Get synchronizer ID from config (cached after first successful resolution).
///
/// Queries the participant's synchronizer connectivity service to get the
/// physical synchronizer ID for the configured synchronizer alias. The result
/// is memoised in a process-wide [`SYNCHRONIZER_ID_CACHE`]; subsequent calls
/// return the cached value without any network round trip.
pub async fn get_synchronizer_id(config: &NodeConfig) -> Result<String> {
    SYNCHRONIZER_ID_CACHE
        .get_or_try_init(|| async {
            get_synchronizer_id_from_url(&config.admin_api_url(), config.synchronizer()).await
        })
        .await
        .cloned()
}

/// Get the physical synchronizer ID from a Canton Admin API URL and alias
pub async fn get_synchronizer_id_from_url(
    admin_api_url: &str,
    synchronizer_alias: &str,
) -> Result<String> {
    let mut conn_client =
        SynchronizerConnectivityServiceClient::connect(admin_api_url.to_string()).await?;

    let response = conn_client
        .get_synchronizer_id(tonic::Request::new(GetSynchronizerIdRequest {
            synchronizer_alias: synchronizer_alias.to_string(),
        }))
        .await?
        .into_inner();

    if response.physical_synchronizer_id.is_empty() {
        anyhow::bail!("No synchronizer ID returned for synchronizer alias '{synchronizer_alias}'");
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

/// Resolve the participant ID from Canton if not already set in the config.
///
/// If the participant_id is not set, queries Canton Admin API and stores
/// the result in memory. The ID is not persisted — it will be re-queried
/// on every startup.
pub async fn resolve_participant_id(config: &mut NodeConfig) -> Result {
    if config.has_participant_id() {
        tracing::debug!(
            "Participant ID already configured: {}",
            config.participant_id()
        );
        return Ok(());
    }

    tracing::info!("Participant ID not configured, querying Canton...");
    let participant_id = get_participant_id(config).await?;
    tracing::info!("Got participant ID from Canton: {participant_id}");

    config.node.participant_id = Some(participant_id);
    Ok(())
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

/// Max gRPC message size (512MB) - Canton ledger can return large responses
pub const MAX_GRPC_MESSAGE_SIZE: usize = 512 * 1024 * 1024;

/// Macro to define authenticated gRPC client creator functions
macro_rules! define_client_creator {
    ($fn_name:ident, $client_type:ident) => {
        pub async fn $fn_name(
            config: &NodeConfig,
            token: Option<String>,
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

            Ok($client_type::with_interceptor(channel, interceptor)
                .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE))
        }
    };
}

define_client_creator!(create_party_client, PartyManagementServiceClient);
define_client_creator!(create_user_client, UserManagementServiceClient);
define_client_creator!(create_submission_client, InteractiveSubmissionServiceClient);
define_client_creator!(create_state_client, StateServiceClient);
define_client_creator!(create_update_client, UpdateServiceClient);
define_client_creator!(create_event_query_client, EventQueryServiceClient);

/// Create a directory with context for error messages
pub async fn create_directory(path: &Path) -> Result {
    fs::create_dir_all(path)
        .await
        .with_context(|| format!("Failed to create dir '{path}'", path = path.display()))
}

/// Encode files (filename + data pairs) into a single payload
///
/// Format: [count (4 bytes)] + encode_length_prefixed([filename1, data1, filename2, data2, ...])
pub fn encode_files(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(files.len() as u32).to_be_bytes());

    let items: Vec<&[u8]> = files
        .iter()
        .flat_map(|(name, data)| [name.as_bytes(), data.as_slice()])
        .collect();

    payload.extend(encode_length_prefixed(&items));
    payload
}

/// Decode files from a payload
///
/// Returns a vector of (filename, data) pairs
pub fn decode_files(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    if data.len() < 4 {
        anyhow::bail!("Invalid file payload: too short for count");
    }

    let count = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let items = decode_length_prefixed(&data[4..], count * 2)?;

    items
        .chunks(2)
        .map(|chunk| {
            let filename = String::from_utf8(chunk[0].clone())?;
            Ok((filename, chunk[1].clone()))
        })
        .collect()
}

/// Create multiple directories with context for error messages
pub async fn create_directories(paths: &[&Path]) -> Result {
    for path in paths {
        create_directory(path).await?;
    }
    Ok(())
}

/// Encode multiple byte slices into a length-prefixed payload
///
/// Each slice is prefixed with a 4-byte big-endian length.
/// This is used for combining multiple data items into a single payload
/// for transmission over the noise protocol.
pub fn encode_length_prefixed(items: &[&[u8]]) -> Vec<u8> {
    let total_len: usize = items.iter().map(|item| 4 + item.len()).sum();
    let mut payload = Vec::with_capacity(total_len);

    for item in items {
        payload.extend_from_slice(&(item.len() as u32).to_be_bytes());
        payload.extend_from_slice(item);
    }

    payload
}

/// Decode a length-prefixed payload into multiple byte vectors
///
/// Each item is prefixed with a 4-byte big-endian length.
/// Returns the requested number of items or an error if the data is malformed.
pub fn decode_length_prefixed(data: &[u8], expected_count: usize) -> Result<Vec<Vec<u8>>> {
    let mut items = Vec::with_capacity(expected_count);
    let mut offset = 0;

    for i in 0..expected_count {
        if offset + 4 > data.len() {
            anyhow::bail!(
                "Invalid payload: expected length prefix at offset {offset}, but only {len} bytes available (item {i}/{expected_count})",
                len = data.len()
            );
        }

        let item_len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + item_len > data.len() {
            anyhow::bail!(
                "Invalid payload: expected {item_len} bytes at offset {offset}, but only {remaining} remaining (item {i}/{expected_count})",
                remaining = data.len() - offset
            );
        }

        items.push(data[offset..offset + item_len].to_vec());
        offset += item_len;
    }

    Ok(items)
}

/// Whether dotted-numeric version `v` is at least `min` (component-wise
/// numeric compare, missing components treated as 0). Returns `false` for
/// anything that doesn't parse as dotted numerics — for version gating,
/// "can't parse" must mean "can't verify", never "assume compatible".
pub fn version_at_least(v: &str, min: &str) -> bool {
    fn parse(s: &str) -> Option<Vec<u64>> {
        s.trim().split('.').map(|c| c.parse::<u64>().ok()).collect()
    }
    let (Some(v), Some(min)) = (parse(v), parse(min)) else {
        return false;
    };
    let len = v.len().max(min.len());
    for i in 0..len {
        let a = v.get(i).copied().unwrap_or(0);
        let b = min.get(i).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_at_least_compares_numerically() {
        assert!(version_at_least("0.1.9", "0.1.9"));
        assert!(version_at_least("0.1.10", "0.1.9"));
        assert!(version_at_least("0.2.0", "0.1.9"));
        assert!(version_at_least("1.0", "0.1.9"));
        assert!(!version_at_least("0.1.8", "0.1.9"));
        assert!(!version_at_least("0.1", "0.1.9"));
        // Unparseable must never pass the gate.
        assert!(!version_at_least("", "0.1.9"));
        assert!(!version_at_least("0.1.9-rc1", "0.1.9"));
        assert!(!version_at_least("abc", "0.1.9"));
    }

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

    #[test]
    fn test_encode_decode_length_prefixed() -> Result {
        let item1 = b"hello";
        let item2 = b"world";
        let item3 = b"test data with more bytes";

        let encoded = encode_length_prefixed(&[item1, item2, item3]);
        let decoded = decode_length_prefixed(&encoded, 3)?;

        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0], item1);
        assert_eq!(decoded[1], item2);
        assert_eq!(decoded[2], item3);
        Ok(())
    }

    #[test]
    fn test_encode_decode_empty_items() -> Result {
        let empty: &[u8] = b"";
        let non_empty = b"data";

        let encoded = encode_length_prefixed(&[empty, non_empty]);
        let decoded = decode_length_prefixed(&encoded, 2)?;

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0], empty);
        assert_eq!(decoded[1], non_empty);
        Ok(())
    }

    #[test]
    fn test_decode_invalid_payload_short() {
        let short_data = [0u8; 2]; // Less than 4 bytes
        let result = decode_length_prefixed(&short_data, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_invalid_payload_truncated() {
        let mut data = Vec::new();
        data.extend_from_slice(&10u32.to_be_bytes()); // Says 10 bytes
        data.extend_from_slice(b"short"); // Only 5 bytes
        let result = decode_length_prefixed(&data, 1);
        assert!(result.is_err());
    }

    #[test]
    fn extract_synchronizer_fingerprint_strips_version() -> Result {
        // Three-segment form: drop only the trailing protocol version.
        assert_eq!(
            extract_synchronizer_fingerprint("global-domain::1220abcd::34-0")?,
            "global-domain::1220abcd"
        );
        // Two-segment form: already alias::fingerprint, returned unchanged.
        assert_eq!(
            extract_synchronizer_fingerprint("global-domain::1220abcd")?,
            "global-domain::1220abcd"
        );
        Ok(())
    }

    #[test]
    fn extract_synchronizer_fingerprint_rejects_bad_segment_count() {
        assert!(extract_synchronizer_fingerprint("noColons").is_err());
        assert!(extract_synchronizer_fingerprint("a::b::c::d").is_err());
    }

    #[test]
    fn encode_decode_files_round_trip() -> Result {
        let files = vec![
            ("a.txt".to_string(), b"hello".to_vec()),
            ("b.bin".to_string(), vec![1u8, 2, 3, 0, 255]),
            ("empty".to_string(), Vec::new()),
        ];
        let encoded = encode_files(&files);
        let decoded = decode_files(&encoded)?;
        assert_eq!(decoded, files);
        Ok(())
    }

    #[test]
    fn decode_files_rejects_short_payload() {
        // Fewer than the 4 bytes needed for the count prefix.
        assert!(decode_files(&[0u8, 0, 0]).is_err());
    }

    #[test]
    fn decode_files_rejects_non_utf8_filename() {
        // count = 1, then a length-prefixed [filename, data] pair whose
        // filename bytes are not valid UTF-8.
        let mut payload = 1u32.to_be_bytes().to_vec();
        let bad_name: &[u8] = &[0xff, 0xfe];
        let data: &[u8] = b"data";
        payload.extend(encode_length_prefixed(&[bad_name, data]));
        assert!(decode_files(&payload).is_err());
    }

    #[test]
    fn decode_files_rejects_count_overrun() {
        // Claims 2 files (=> 4 length-prefixed items) but only supplies 2 items.
        let mut payload = 2u32.to_be_bytes().to_vec();
        let a: &[u8] = b"a";
        let b: &[u8] = b"x";
        payload.extend(encode_length_prefixed(&[a, b]));
        assert!(decode_files(&payload).is_err());
    }

    #[test]
    fn read_first_message_from_bytes_rejects_empty() {
        let result = read_first_message_from_bytes::<Timestamp>(b"");
        assert!(result.is_err());
    }

    #[test]
    fn read_first_message_from_bytes_rejects_truncated() {
        // Length prefix claims 10 bytes but only 2 follow.
        let mut buf: Vec<u8> = Vec::new();
        prost::encoding::encode_varint(10, &mut buf);
        buf.extend_from_slice(b"ab");
        let result = read_first_message_from_bytes::<Timestamp>(&buf);
        assert!(result.is_err());
    }
}
