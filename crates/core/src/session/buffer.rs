use crate::output::chunk::OutputChunk;
use std::collections::VecDeque;

/// Ring-buffer of raw PTY output chunks, capped at a configurable byte limit.
///
/// Chunks are assigned monotonically increasing sequence numbers so callers can
/// efficiently poll for new data with [`Scrollback::read_since`].
pub struct Scrollback {
    capacity_bytes: usize,
    total_bytes: usize,
    next_seq: u64,
    chunks: std::collections::VecDeque<OutputChunk>,
}

impl Scrollback {
    /// Create a new scrollback buffer with the given byte capacity.
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            total_bytes: 0,
            next_seq: 1,
            chunks: VecDeque::default(),
        }
    }

    /// Append a chunk and return its sequence number. Evicts oldest chunks when over capacity.
    pub fn append(&mut self, bytes: Vec<u8>) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let len = bytes.len();
        self.total_bytes += len;
        self.chunks.push_back(OutputChunk { seq, bytes });
        while self.total_bytes > self.capacity_bytes {
            if let Some(old) = self.chunks.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(old.bytes.len());
            } else {
                break;
            }
        }
        seq
    }

    /// Return all chunks with `seq > since_seq_exclusive` plus the next sequence number.
    pub fn read_since(&self, since_seq_exclusive: u64) -> (Vec<OutputChunk>, u64) {
        let mut out = Vec::new();
        for c in &self.chunks {
            if c.seq > since_seq_exclusive {
                out.push(OutputChunk {
                    seq: c.seq,
                    bytes: c.bytes.clone(),
                });
            }
        }
        let next_seq = self.next_seq;
        (out, next_seq)
    }
}
