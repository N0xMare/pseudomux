use crate::{SourceLocation, TranscriptError};

/// A single JSONL record may not grow without bound while waiting for `\n`.
pub const MAX_TRANSCRIPT_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Stable file identity supplied by the platform-specific watcher/tailer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

/// Metadata observed before reading an append range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileMetadata {
    pub identity: FileIdentity,
    pub len: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorChange {
    Initialized,
    Unchanged,
    Replaced {
        previous: FileIdentity,
        current: FileIdentity,
    },
    Truncated {
        previous_offset: u64,
        current_len: u64,
    },
}

/// The exact range the caller should read after observing metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorObservation {
    pub change: CursorChange,
    pub read_from: u64,
    pub read_to: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteLine {
    pub location: SourceLocation,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorUpdate {
    pub lines: Vec<CompleteLine>,
    pub next_offset: u64,
    pub pending_bytes: usize,
    pub generation: u64,
}

/// Incremental, offset-checked JSONL framing state.
///
/// The cursor intentionally keeps an unterminated final line buffered. A caller
/// must never parse it as a turn result, even when the file is otherwise quiet.
///
/// Framing is strictly sequential from a monotonic offset, so **file order is
/// read order**: a line can never be surfaced before a line that precedes it in
/// the file. That is the property the `turn_duration` arrival-order measurement
/// rests on, which is why no observation timestamp is recorded here. The cursor
/// has no clock, and the instant that matters is not the framing boundary but
/// the boundary at which pmux could act: a whole completed read, ingested and
/// analyzed. `ArrivalOrderObservations` in the session actor stamps that one.
#[derive(Clone, Debug, Default)]
pub struct TranscriptCursor {
    identity: Option<FileIdentity>,
    observed_len: u64,
    next_offset: u64,
    pending_offset: u64,
    pending: Vec<u8>,
    /// First byte in `pending` that has not yet been checked for a newline.
    /// Without this cursor, fragmented unterminated records are rescanned from
    /// byte zero on every append and byte-at-a-time delivery becomes quadratic.
    scan_from: usize,
    next_line_number: u64,
    generation: u64,
    #[cfg(test)]
    scanned_bytes: usize,
}

impl TranscriptCursor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_line_number: 1,
            ..Self::default()
        }
    }

    /// Establishes an exact turn boundary without reading historical content.
    ///
    /// Callers must first validate the file identity and verify that `len` ends
    /// on a complete JSONL record. New bytes appended after this point are the
    /// only bytes exposed by subsequent observations.
    pub fn seek_to_eof(&mut self, metadata: FileMetadata) {
        self.reset_for(metadata.identity);
        self.observed_len = metadata.len;
        self.next_offset = metadata.len;
        self.pending_offset = metadata.len;
    }

    /// Reconciles watcher metadata and returns the exact range to read.
    pub fn observe(&mut self, metadata: FileMetadata) -> CursorObservation {
        let change = match self.identity {
            None => {
                self.reset_for(metadata.identity);
                CursorChange::Initialized
            }
            Some(previous) if previous != metadata.identity => {
                self.reset_for(metadata.identity);
                CursorChange::Replaced {
                    previous,
                    current: metadata.identity,
                }
            }
            Some(_) if metadata.len < self.next_offset => {
                let previous_offset = self.next_offset;
                self.reset_for(metadata.identity);
                CursorChange::Truncated {
                    previous_offset,
                    current_len: metadata.len,
                }
            }
            Some(_) => CursorChange::Unchanged,
        };

        self.observed_len = metadata.len;
        CursorObservation {
            change,
            read_from: self.next_offset,
            read_to: metadata.len,
            generation: self.generation,
        }
    }

    /// Applies bytes read at the offset returned by [`Self::observe`].
    pub fn push(
        &mut self,
        identity: FileIdentity,
        read_offset: u64,
        bytes: &[u8],
    ) -> Result<CursorUpdate, TranscriptError> {
        let expected_identity = self.identity.unwrap_or(identity);
        if expected_identity != identity {
            return Err(TranscriptError::FileIdentityMismatch {
                expected: expected_identity,
                actual: identity,
            });
        }
        if read_offset != self.next_offset {
            return Err(TranscriptError::CursorOffsetMismatch {
                expected: self.next_offset,
                actual: read_offset,
            });
        }

        let read_end = read_offset.checked_add(bytes.len() as u64).ok_or(
            TranscriptError::ReadBeyondObservedFile {
                read_end: u64::MAX,
                file_len: self.observed_len,
            },
        )?;
        if read_end > self.observed_len {
            return Err(TranscriptError::ReadBeyondObservedFile {
                read_end,
                file_len: self.observed_len,
            });
        }

        if self.pending.is_empty() {
            self.pending_offset = read_offset;
        }
        self.pending.extend_from_slice(bytes);
        self.next_offset = read_end;

        let scan_start = self.scan_from;
        #[cfg(test)]
        {
            self.scanned_bytes += self.pending.len().saturating_sub(scan_start);
        }
        let mut lines = Vec::new();
        let mut consumed = 0;
        for newline in self
            .pending
            .iter()
            .enumerate()
            .skip(scan_start)
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
        {
            let mut line_end = newline;
            if line_end > consumed && self.pending[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            let line_bytes = line_end.saturating_sub(consumed);
            if line_bytes > MAX_TRANSCRIPT_LINE_BYTES {
                return Err(TranscriptError::LineTooLong {
                    bytes: line_bytes,
                    limit: MAX_TRANSCRIPT_LINE_BYTES,
                    byte_offset: self.pending_offset + consumed as u64,
                });
            }
            lines.push(CompleteLine {
                location: SourceLocation {
                    line: self.next_line_number,
                    byte_offset: self.pending_offset + consumed as u64,
                },
                bytes: self.pending[consumed..line_end].to_vec(),
            });
            self.next_line_number += 1;
            consumed = newline + 1;
        }

        // Every byte currently buffered has now been inspected. If complete
        // lines are drained below, translate the cursor into the retained
        // suffix's coordinate space.
        self.scan_from = self.pending.len().saturating_sub(consumed);

        if consumed > 0 {
            self.pending.drain(..consumed);
            self.pending_offset += consumed as u64;
        }
        if self.pending.len() > MAX_TRANSCRIPT_LINE_BYTES {
            return Err(TranscriptError::LineTooLong {
                bytes: self.pending.len(),
                limit: MAX_TRANSCRIPT_LINE_BYTES,
                byte_offset: self.pending_offset,
            });
        }

        Ok(CursorUpdate {
            lines,
            next_offset: self.next_offset,
            pending_bytes: self.pending.len(),
            generation: self.generation,
        })
    }

    #[must_use]
    pub fn has_partial_line(&self) -> bool {
        !self.pending.is_empty()
    }

    #[must_use]
    pub fn next_offset(&self) -> u64 {
        self.next_offset
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn reset_for(&mut self, identity: FileIdentity) {
        self.identity = Some(identity);
        self.next_offset = 0;
        self.pending_offset = 0;
        self.pending.clear();
        self.scan_from = 0;
        self.next_line_number = 1;
        self.generation = self.generation.saturating_add(1);
        #[cfg(test)]
        {
            self.scanned_bytes = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_partial_record_is_scanned_linearly() {
        const PAYLOAD_BYTES: usize = 256 * 1024;
        let identity = FileIdentity {
            device: 1,
            inode: 2,
        };
        let mut cursor = TranscriptCursor::new();

        for offset in 0..PAYLOAD_BYTES {
            let metadata = FileMetadata {
                identity,
                len: (offset + 1) as u64,
            };
            let observation = cursor.observe(metadata);
            let update = cursor
                .push(identity, observation.read_from, b"x")
                .expect("a bounded partial record remains valid");
            assert!(update.lines.is_empty());
        }

        let metadata = FileMetadata {
            identity,
            len: (PAYLOAD_BYTES + 1) as u64,
        };
        let observation = cursor.observe(metadata);
        let update = cursor
            .push(identity, observation.read_from, b"\n")
            .expect("the terminating newline completes the record");

        assert_eq!(update.lines.len(), 1);
        assert_eq!(update.lines[0].bytes.len(), PAYLOAD_BYTES);
        assert_eq!(
            cursor.scanned_bytes,
            PAYLOAD_BYTES + 1,
            "each received byte must be inspected exactly once"
        );
    }
}
