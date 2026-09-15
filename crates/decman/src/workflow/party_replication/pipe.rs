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

use crate::{error::Result, workflow::party_replication::now_ms};

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
    /// Unix ms this session opened. A broken transfer opens a fresh session
    /// from block 1, so the reported rate is always about the current attempt.
    started_at_ms: i64,
    /// Unix ms a NEW block was last served. Replays do not touch it: the same
    /// bytes going out twice is a retry, not progress, and a reader watching
    /// for a stall needs the moment the counters last actually moved.
    last_served_at_ms: i64,
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
            started_at_ms: now_ms(),
            last_served_at_ms: now_ms(),
        }
    }

    /// Total bytes handed out so far, for progress logging.
    pub fn served_bytes(&self) -> u64 {
        self.total_len
    }

    /// Sequence number of the last block served, for progress readouts.
    pub fn served_blocks(&self) -> u64 {
        self.served_seq
    }

    /// Unix ms this session opened, so the UI can show elapsed and rate.
    pub fn started_at_ms(&self) -> i64 {
        self.started_at_ms
    }

    /// Unix ms a new block was last served, so a reader can tell a moving
    /// transfer from a stalled one. Stamping this at read time instead would
    /// make every poll look like progress and no export would ever appear
    /// stalled.
    pub fn last_served_at_ms(&self) -> i64 {
        self.last_served_at_ms
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
        self.last_served_at_ms = now_ms();
        Ok(block)
    }
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

    /// The two block shapes stay distinct values: an empty data block is not
    /// an end block, and a trailer keeps both halves.
    #[test]
    fn block_shapes_are_distinct() {
        let empty = PipeBlock::Data {
            seq: 3,
            bytes: Vec::new(),
        };
        let end = PipeBlock::End {
            seq: 3,
            trailer: trailer(0, &"a".repeat(64)),
        };
        assert_ne!(empty, end);
        if let PipeBlock::End { trailer: t, .. } = end {
            assert_eq!(t.total_len, 0);
            assert_eq!(t.sha256.len(), 64);
        }
    }
}
