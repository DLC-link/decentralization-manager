use anyhow::Context;
use bytes::{Buf, BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::version::v1::{
    UntypedVersionedMessage, untyped_versioned_message,
};
use prost::Message;

use crate::error::Result;

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

/// Read every `varint(len)||proto` message in `data`, in order. Empty data
/// gives an empty list.
///
/// # Errors
///
/// Errors if a length prefix is not a valid varint, if a message is cut
/// short, or if a message does not decode as `M`.
pub fn read_all_messages_from_bytes<M: Message + Default>(data: &[u8]) -> Result<Vec<M>> {
    read_all_messages_with(data, |len, remaining| {
        anyhow::anyhow!("Incomplete message: expected {len} bytes, but only {remaining} remaining")
    })
}

/// [`read_all_messages_from_bytes`] with the caller's own error for a message
/// cut short. `truncated` gets the expected length and the bytes left.
///
/// # Errors
///
/// Errors if a length prefix is not a valid varint, if a message is cut
/// short (the error `truncated` returns), or if a message does not decode as
/// `M`.
pub fn read_all_messages_with<M: Message + Default>(
    data: &[u8],
    truncated: impl Fn(usize, usize) -> anyhow::Error,
) -> Result<Vec<M>> {
    let mut cursor = data;
    let mut messages = Vec::new();
    while cursor.has_remaining() {
        let len = prost::encoding::decode_varint(&mut cursor)? as usize;
        if cursor.remaining() < len {
            return Err(truncated(len, cursor.remaining()));
        }
        let message_bytes = &cursor[..len];
        cursor.advance(len);
        messages.push(M::decode(message_bytes)?);
    }
    Ok(messages)
}

/// Unwrap Canton's protocol-versioning envelope.
///
/// Canton wraps every protocol-versioned message in an
/// `UntypedVersionedMessage`, so the bytes in e.g.
/// `SignedTopologyTransaction::transaction` are the envelope, not the inner
/// message. This peels one layer and decodes what is inside.
///
/// # Errors
///
/// Errors if the envelope or the message inside it fails to decode.
pub fn decode_versioned<M: Message + Default>(bytes: &[u8]) -> Result<M> {
    let versioned = UntypedVersionedMessage::decode(bytes)
        .context("Failed to decode UntypedVersionedMessage envelope")?;
    let inner = match versioned.wrapper {
        Some(untyped_versioned_message::Wrapper::Data(data)) => data,
        None => anyhow::bail!("UntypedVersionedMessage has no wrapper data"),
    };
    M::decode(inner.as_slice()).context("Failed to decode the versioned message payload")
}

/// Encode a single protobuf message as `varint(len)||proto` — the same
/// length-prefixed framing [`read_first_message_from_bytes`] reads back.
/// Workflows share this to build artefact / on-wire bytes without
/// re-implementing the framing.
pub fn encode_length_prefixed_message<M: Message>(message: &M) -> Vec<u8> {
    let encoded = message.encode_to_vec();
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
    buffer.put_slice(&encoded);
    buffer.to_vec()
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

#[cfg(test)]
mod tests {
    use prost_types::Timestamp;

    use super::*;

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

    fn stamp(seconds: i64) -> Timestamp {
        Timestamp { seconds, nanos: 0 }
    }

    #[test]
    fn read_all_messages_from_bytes_reads_every_message_in_order() -> Result {
        let mut stream = encode_length_prefixed_message(&stamp(1));
        stream.extend(encode_length_prefixed_message(&stamp(2)));
        stream.extend(encode_length_prefixed_message(&stamp(3)));

        let messages: Vec<Timestamp> = read_all_messages_from_bytes(&stream)?;

        assert_eq!(messages, vec![stamp(1), stamp(2), stamp(3)]);
        Ok(())
    }

    #[test]
    fn read_all_messages_from_bytes_reads_empty_data_as_no_messages() -> Result {
        let messages: Vec<Timestamp> = read_all_messages_from_bytes(b"")?;
        assert!(messages.is_empty());
        Ok(())
    }

    #[test]
    fn read_all_messages_from_bytes_rejects_a_truncated_stream() {
        // One whole message, then a prefix that claims 10 bytes with 2 left.
        let mut stream = encode_length_prefixed_message(&stamp(1));
        prost::encoding::encode_varint(10, &mut stream);
        stream.extend_from_slice(b"ab");

        let err = read_all_messages_from_bytes::<Timestamp>(&stream).unwrap_err();

        assert_eq!(
            err.to_string(),
            "Incomplete message: expected 10 bytes, but only 2 remaining"
        );
    }

    #[test]
    fn read_all_messages_with_returns_the_callers_truncation_error() {
        let mut stream = Vec::new();
        prost::encoding::encode_varint(5, &mut stream);
        stream.extend_from_slice(b"abc");

        let err = read_all_messages_with::<Timestamp>(&stream, |len, remaining| {
            anyhow::anyhow!("cut short: {len}/{remaining}")
        })
        .unwrap_err();

        assert_eq!(err.to_string(), "cut short: 5/3");
    }
}
