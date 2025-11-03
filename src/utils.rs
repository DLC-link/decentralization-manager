use std::path::Path;

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio::fs;

use crate::error::Result;

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
pub async fn write_messages_to_file<M: Message>(
    messages: &[M],
    path: impl AsRef<Path>,
) -> Result<()> {
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
pub async fn write_message_to_file<M: Message>(message: &M, path: impl AsRef<Path>) -> Result<()> {
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

/// Write raw bytes to a file
pub async fn write_bytes_to_file(data: &[u8], path: impl AsRef<Path>) -> Result<()> {
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
) -> Result<()>
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

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::Timestamp;

    #[tokio::test]
    async fn test_write_and_read_single_message() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.bin");

        let message = Timestamp {
            seconds: 123456789,
            nanos: 987654321,
        };

        // Write
        write_message_to_file(&message, &file_path).await.unwrap();

        // Read
        let read_message: Timestamp = read_first_message_from_file(&file_path).await.unwrap();

        assert_eq!(message.seconds, read_message.seconds);
        assert_eq!(message.nanos, read_message.nanos);
    }

    #[tokio::test]
    async fn test_write_and_read_multiple_messages() {
        let temp_dir = tempfile::tempdir().unwrap();
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
        write_messages_to_file(&messages, &file_path).await.unwrap();

        // Read
        let read_messages: Vec<Timestamp> = read_all_messages_from_file(&file_path).await.unwrap();

        assert_eq!(messages.len(), read_messages.len());
        for (original, read) in messages.iter().zip(read_messages.iter()) {
            assert_eq!(original.seconds, read.seconds);
            assert_eq!(original.nanos, read.nanos);
        }
    }
}
