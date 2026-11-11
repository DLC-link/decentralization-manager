use bytes::{Buf, BufMut, BytesMut};

use crate::error::Result;

/// Split the combined `varint(len_dns)||dns_proto||varint(len_p2p)||p2p_proto`
/// buffer peers send back into two separate `varint(len)||proto` blobs.
/// The output blobs are byte-identical to what each peer wrote as its
/// own `SIGNED_KICK_DNS` / `SIGNED_KICK_P2P` artefact, so re-encoding via
/// `read_first_message_from_bytes` round-trips cleanly.
///
/// `pub(crate)` — the add-party workflow ships its signed DNS/P2P pair over
/// the same wire format and reuses this splitter.
pub(crate) fn split_signed_kick_pair(combined: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut cursor: &[u8] = combined;

    let dns_len = prost::encoding::decode_varint(&mut cursor)? as usize;
    if cursor.remaining() < dns_len {
        anyhow::bail!(
            "Truncated combined kick signatures: expected {dns_len} DNS bytes, have {remaining}",
            remaining = cursor.remaining()
        );
    }
    let dns_proto = &cursor[..dns_len];
    let dns_blob = encode_length_prefixed_bytes(dns_proto);
    cursor.advance(dns_len);

    let p2p_len = prost::encoding::decode_varint(&mut cursor)? as usize;
    if cursor.remaining() < p2p_len {
        anyhow::bail!(
            "Truncated combined kick signatures: expected {p2p_len} P2P bytes, have {remaining}",
            remaining = cursor.remaining()
        );
    }
    let p2p_proto = &cursor[..p2p_len];
    let p2p_blob = encode_length_prefixed_bytes(p2p_proto);
    cursor.advance(p2p_len);

    if cursor.has_remaining() {
        anyhow::bail!(
            "Trailing {remaining} bytes after parsing combined kick signatures",
            remaining = cursor.remaining()
        );
    }

    Ok((dns_blob, p2p_blob))
}

/// Re-emit a single proto with its varint length prefix. Round-trips the
/// `varint(len)||proto` framing produced by `utils::write_message_to_file`.
fn encode_length_prefixed_bytes(proto: &[u8]) -> Vec<u8> {
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(proto.len() as u64, &mut buffer);
    buffer.put_slice(proto);
    buffer.to_vec()
}

// Tests below exercise the round-trip between the per-peer blob format
// (what sign.rs writes to storage) and the combined wire-format the peer
// sends to the coordinator.
#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `varint(len)||bytes` blob — the per-message framing the
    /// production sign step uses, here applied to opaque payload bytes so the
    /// round-trip test stays decoupled from any concrete protobuf shape.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = BytesMut::new();
        prost::encoding::encode_varint(payload.len() as u64, &mut out);
        out.put_slice(payload);
        out.to_vec()
    }

    #[test]
    fn split_round_trips_for_two_messages() -> Result<()> {
        let dns_payload: &[u8] = &[1, 2, 3, 4];
        let p2p_payload: &[u8] = &[5, 6, 7, 8, 9, 10];

        let dns_blob = frame(dns_payload);
        let p2p_blob = frame(p2p_payload);

        let mut combined = Vec::new();
        combined.extend_from_slice(&dns_blob);
        combined.extend_from_slice(&p2p_blob);

        let (dns_back, p2p_back) = split_signed_kick_pair(&combined)?;
        assert_eq!(dns_back, dns_blob);
        assert_eq!(p2p_back, p2p_blob);
        Ok(())
    }

    #[test]
    fn split_rejects_trailing_garbage() {
        let mut combined = Vec::new();
        combined.extend_from_slice(&frame(&[1, 2]));
        combined.extend_from_slice(&frame(&[3, 4]));
        combined.push(0xff);
        assert!(split_signed_kick_pair(&combined).is_err());
    }

    #[test]
    fn split_rejects_truncated_p2p() {
        let mut combined = Vec::new();
        combined.extend_from_slice(&frame(&[1, 2]));
        // Encode a length larger than the remaining bytes.
        let mut header = BytesMut::new();
        prost::encoding::encode_varint(8, &mut header);
        combined.extend_from_slice(&header);
        combined.extend_from_slice(&[9, 9]);
        assert!(split_signed_kick_pair(&combined).is_err());
    }
}
