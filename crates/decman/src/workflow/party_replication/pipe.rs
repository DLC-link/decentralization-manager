//! A forward-only pipe from the source's ACS export to the target's ACS import.
//!
//! The staged design wrote the snapshot to disk on both sides so ranges could
//! be re-read. That does not scale: a terabyte ACS needs a terabyte of scratch
//! on two nodes, and the volumes are sized for a SQLite file and a keypair.
//!
//! Both Canton endpoints are already streams — `ExportPartyAcs` streams out of
//! the source, `ImportPartyAcs` streams into the target — so nothing has to
//! land in between. The source keeps its export stream open and hands out
//! successive blocks; the target feeds each block straight into its open import
//! stream. One block is in flight, neither side accumulates, and the size of
//! the party stops mattering.
//!
//! The cost of never storing it is that the pipe cannot seek. There is no
//! resume: a break means a fresh export from the top, because Canton offers no
//! offset into `ImportPartyAcs` and re-exporting to skip forward would depend
//! on an export byte-ordering that Canton does not document. What is bounded is
//! a *single* block failing — the source keeps the last block it served, so one
//! retry on a fresh connection is safe.

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::ExportPartyAcsResponse;
use sha2::{Digest, Sha256};

use crate::error::Result;

/// The final block's trailer: total bytes served plus the SHA-256 over all of
/// them, so the target can prove after the fact what it fed Canton.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipeTrailer {
    pub total_len: u64,
    pub sha256: String,
}

/// What one `GetNextAcsBlock` yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipeBlock {
    /// Snapshot bytes at this sequence number.
    Data { seq: u64, bytes: Vec<u8> },
    /// The export is exhausted; no more data follows.
    End { seq: u64, trailer: PipeTrailer },
}

/// One in-progress export, held open on the source between block requests.
///
/// Forward-only by construction: the Canton stream is never rewound, so the
/// source needs no copy of what it already sent beyond the single block kept
/// for an in-place retry.
pub struct ExportSession {
    stream: tonic::Streaming<ExportPartyAcsResponse>,
    /// Bytes received from Canton but not yet handed out. Canton's chunk
    /// boundaries are unrelated to our block size, so a block is assembled
    /// across them and the remainder carries over.
    carry: Vec<u8>,
    /// Sequence number of the block served most recently.
    served_seq: u64,
    /// The block served most recently, replayed if the target asks for the same
    /// sequence again. Bounds the source's memory to two blocks.
    last: Option<PipeBlock>,
    total_len: u64,
    hasher: Sha256,
    /// True once Canton's stream has ended and the trailer has been built.
    drained: bool,
}

impl ExportSession {
    /// Wrap an open `ExportPartyAcs` stream.
    pub fn new(stream: tonic::Streaming<ExportPartyAcsResponse>) -> Self {
        Self {
            stream,
            carry: Vec::new(),
            served_seq: 0,
            last: None,
            total_len: 0,
            hasher: Sha256::new(),
            drained: false,
        }
    }

    /// Total bytes handed out so far, for progress logging.
    pub fn served_bytes(&self) -> u64 {
        self.total_len
    }

    /// Whether this session has served past the first block.
    ///
    /// A target that asks for block 1 again after this is retrying the step
    /// from the top, which a forward-only stream cannot answer: the session has
    /// to be thrown away and re-opened.
    pub fn served_past_first(&self) -> bool {
        self.served_seq > 1
    }

    /// Serve block `seq`, reading from Canton only as far as `block_size`
    /// requires.
    ///
    /// `seq` must be either the block just served — replayed from memory, which
    /// is what makes a single transport retry safe — or the next one. Anything
    /// else means the two sides disagree about where they are, and since the
    /// stream cannot rewind the only honest answer is to fail the run.
    ///
    /// # Errors
    /// Returns an error if `seq` is neither a replay nor the next block, or if
    /// the Canton stream fails.
    pub async fn block(&mut self, seq: u64, block_size: usize) -> Result<PipeBlock> {
        if let Some(last) = &self.last
            && seq == self.served_seq
        {
            return Ok(last.clone());
        }
        if seq != self.served_seq + 1 {
            anyhow::bail!(
                "ACS pipe out of sync: asked for block {seq} after serving \
                 {served} — the export stream cannot rewind, so this run must \
                 restart the export",
                served = self.served_seq
            );
        }

        while !self.drained && self.carry.len() < block_size {
            match self.stream.message().await? {
                Some(response) => self.carry.extend_from_slice(&response.chunk),
                None => self.drained = true,
            }
        }

        let block = if self.carry.is_empty() && self.drained {
            PipeBlock::End {
                seq,
                trailer: PipeTrailer {
                    total_len: self.total_len,
                    sha256: hex::encode(self.hasher.clone().finalize()),
                },
            }
        } else {
            let take = self.carry.len().min(block_size);
            let bytes: Vec<u8> = self.carry.drain(..take).collect();
            self.hasher.update(&bytes);
            self.total_len += bytes.len() as u64;
            PipeBlock::Data { seq, bytes }
        };

        self.served_seq = seq;
        self.last = Some(block.clone());
        Ok(block)
    }
}

/// Wire encoding of a served block, sent as the `AcsBlock` / `AcsBlockEnd`
/// payload.
///
/// `Data` is `[seq u64][bytes]`; `End` is `[seq u64][total u64][sha256 hex]`.
/// The sequence number rides along so the target can prove the response
/// belongs to the block it asked for rather than a stale one.
pub fn encode_block(block: &PipeBlock) -> (bool, Vec<u8>) {
    match block {
        PipeBlock::Data { seq, bytes } => {
            let mut out = Vec::with_capacity(8 + bytes.len());
            out.extend_from_slice(&seq.to_be_bytes());
            out.extend_from_slice(bytes);
            (false, out)
        }
        PipeBlock::End { seq, trailer } => {
            let digest = trailer.sha256.as_bytes();
            let mut out = Vec::with_capacity(16 + digest.len());
            out.extend_from_slice(&seq.to_be_bytes());
            out.extend_from_slice(&trailer.total_len.to_be_bytes());
            out.extend_from_slice(digest);
            (true, out)
        }
    }
}

/// Decode an `AcsBlock` payload into `(seq, bytes)`.
///
/// # Errors
/// Returns an error if the payload is too short to carry a sequence number.
pub fn decode_data(payload: &[u8]) -> Result<(u64, Vec<u8>)> {
    if payload.len() < 8 {
        anyhow::bail!(
            "ACS block payload is {} bytes, need at least 8",
            payload.len()
        );
    }
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&payload[..8]);
    Ok((u64::from_be_bytes(seq), payload[8..].to_vec()))
}

/// Decode an `AcsBlockEnd` payload into `(seq, trailer)`.
///
/// # Errors
/// Returns an error if the payload is too short or the digest is not UTF-8.
pub fn decode_end(payload: &[u8]) -> Result<(u64, PipeTrailer)> {
    if payload.len() < 16 {
        anyhow::bail!(
            "ACS block-end payload is {} bytes, need at least 16",
            payload.len()
        );
    }
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&payload[..8]);
    let mut total = [0u8; 8];
    total.copy_from_slice(&payload[8..16]);
    let sha256 = String::from_utf8(payload[16..].to_vec())
        .map_err(|e| anyhow::anyhow!("ACS pipe trailer digest is not UTF-8: {e}"))?;
    Ok((
        u64::from_be_bytes(seq),
        PipeTrailer {
            total_len: u64::from_be_bytes(total),
            sha256,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trailer(total: u64, digest: &str) -> PipeTrailer {
        PipeTrailer {
            total_len: total,
            sha256: digest.to_string(),
        }
    }

    /// A data block must survive the wire unchanged, sequence number included —
    /// the target appends it into an import stream it cannot rewind.
    #[test]
    fn data_blocks_round_trip() -> Result {
        let block = PipeBlock::Data {
            seq: u64::from(u32::MAX) + 7,
            bytes: b"active contract bytes".to_vec(),
        };
        let (is_end, payload) = encode_block(&block);
        assert!(!is_end);

        let (seq, bytes) = decode_data(&payload)?;
        assert_eq!(seq, u64::from(u32::MAX) + 7);
        assert_eq!(bytes, b"active contract bytes");
        Ok(())
    }

    /// The trailer carries the only proof the target gets that it fed Canton
    /// the whole snapshot, so both halves must survive exactly.
    #[test]
    fn end_blocks_round_trip() -> Result {
        let digest = "a".repeat(64);
        let block = PipeBlock::End {
            seq: 9,
            trailer: trailer(1_099_511_627_776, &digest),
        };
        let (is_end, payload) = encode_block(&block);
        assert!(is_end);

        let (seq, got) = decode_end(&payload)?;
        assert_eq!(seq, 9);
        assert_eq!(got.total_len, 1_099_511_627_776);
        assert_eq!(got.sha256, digest);
        Ok(())
    }

    /// A truncated frame must be rejected rather than silently decoded into a
    /// zero offset, which would feed Canton bytes at the wrong position.
    #[test]
    fn short_payloads_are_rejected() {
        assert!(decode_data(&[0u8; 7]).is_err());
        assert!(decode_end(&[0u8; 15]).is_err());
    }

    /// An empty data block is distinguishable from an end block: only the
    /// message type says the export is finished, so a zero-length read in the
    /// middle of a stream must not be mistaken for completion.
    #[test]
    fn an_empty_data_block_is_not_an_end_block() -> Result {
        let (is_end, payload) = encode_block(&PipeBlock::Data {
            seq: 3,
            bytes: Vec::new(),
        });
        assert!(!is_end);
        let (seq, bytes) = decode_data(&payload)?;
        assert_eq!(seq, 3);
        assert!(bytes.is_empty());
        Ok(())
    }
}
