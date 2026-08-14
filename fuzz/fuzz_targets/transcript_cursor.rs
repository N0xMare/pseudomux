#![no_main]

use libfuzzer_sys::fuzz_target;
use pseudomux_claude::{
    CursorChange, FileIdentity, FileMetadata, MAX_TRANSCRIPT_LINE_BYTES, TranscriptCursor,
    TranscriptError,
};

const MAX_INPUT_BYTES: usize = 512 * 1024;
const MAX_STEPS: usize = 4096;
const MAX_APPEND_BYTES: usize = 256;

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let mut cursor = TranscriptCursor::new();
    let mut identity = FileIdentity {
        device: 7,
        inode: 1,
    };
    let mut observed_identity = None;
    let mut file = Vec::<u8>::new();
    let mut index = 0;

    for _ in 0..MAX_STEPS {
        if index >= input.len() {
            break;
        }
        let operation = input[index] % 5;
        index += 1;

        match operation {
            // Append a bounded fragment and deliver exactly the range admitted
            // by the production cursor.
            0 => {
                if index >= input.len() {
                    break;
                }
                let available = input.len() - index;
                let count = (usize::from(input[index]) + 1)
                    .min(MAX_APPEND_BYTES)
                    .min(available);
                file.extend_from_slice(&input[index..index + count]);
                index += count;
                reconcile(&mut cursor, identity, &file, &mut observed_identity);
            }
            // Truncate to a data-derived prefix. A shrink below the committed
            // cursor must be reported as a new generation.
            1 => {
                if !file.is_empty() {
                    let selected = input.get(index).copied().unwrap_or_default() as usize;
                    index = (index + 1).min(input.len());
                    file.truncate(selected % (file.len() + 1));
                }
                reconcile(&mut cursor, identity, &file, &mut observed_identity);
            }
            // Replace the file identity, optionally retaining its bytes. The
            // cursor must report replacement and never join generations.
            2 => {
                identity.inode = identity.inode.saturating_add(1);
                if input.get(index).copied().unwrap_or_default() & 1 == 0 {
                    file.clear();
                }
                index = (index + 1).min(input.len());
                reconcile(&mut cursor, identity, &file, &mut observed_identity);
            }
            // Arm at exact EOF. The production API deliberately clears any
            // buffered suffix and starts a fresh generation at that boundary.
            3 => {
                if !file.is_empty() && !file.ends_with(b"\n") {
                    // `seek_to_eof` requires a caller-validated complete JSONL
                    // boundary. Keep arbitrary partial files on the ordinary
                    // reconcile path rather than violating that precondition.
                    reconcile(&mut cursor, identity, &file, &mut observed_identity);
                    continue;
                }
                let previous_generation = cursor.generation();
                cursor.seek_to_eof(FileMetadata {
                    identity,
                    len: file.len() as u64,
                });
                observed_identity = Some(identity);
                assert_eq!(cursor.generation(), previous_generation.saturating_add(1));
                assert_eq!(cursor.next_offset(), file.len() as u64);
                assert!(!cursor.has_partial_line());
            }
            // Identity, offset, and observed-range violations must reject with
            // the exact typed error and leave all public cursor state intact.
            _ => {
                reconcile(&mut cursor, identity, &file, &mut observed_identity);
                let generation = cursor.generation();
                let offset = cursor.next_offset();
                let had_partial = cursor.has_partial_line();
                let wrong_identity = FileIdentity {
                    device: identity.device ^ u64::MAX,
                    inode: identity.inode,
                };

                assert!(matches!(
                    cursor.push(wrong_identity, offset, b""),
                    Err(TranscriptError::FileIdentityMismatch { expected, actual })
                        if expected == identity && actual == wrong_identity
                ));
                assert!(matches!(
                    cursor.push(identity, offset.saturating_add(1), b""),
                    Err(TranscriptError::CursorOffsetMismatch { expected, actual })
                        if expected == offset && actual == offset.saturating_add(1)
                ));
                assert!(matches!(
                    cursor.push(identity, offset, b"x"),
                    Err(TranscriptError::ReadBeyondObservedFile { read_end, file_len })
                        if read_end == offset.saturating_add(1)
                            && file_len == file.len() as u64
                ));
                assert_eq!(cursor.generation(), generation);
                assert_eq!(cursor.next_offset(), offset);
                assert_eq!(cursor.has_partial_line(), had_partial);

                reconcile(&mut cursor, identity, &file, &mut observed_identity);
            }
        }
    }
});

fn reconcile(
    cursor: &mut TranscriptCursor,
    identity: FileIdentity,
    file: &[u8],
    observed_identity: &mut Option<FileIdentity>,
) {
    let previous_offset = cursor.next_offset();
    let previous_generation = cursor.generation();
    let expected_change = match *observed_identity {
        None => CursorChange::Initialized,
        Some(previous) if previous != identity => CursorChange::Replaced {
            previous,
            current: identity,
        },
        Some(_) if (file.len() as u64) < previous_offset => CursorChange::Truncated {
            previous_offset,
            current_len: file.len() as u64,
        },
        Some(_) => CursorChange::Unchanged,
    };
    let reset = !matches!(expected_change, CursorChange::Unchanged);
    let metadata = FileMetadata {
        identity,
        len: file.len() as u64,
    };
    let observation = cursor.observe(metadata);
    assert_eq!(observation.change, expected_change);
    assert_eq!(
        observation.generation,
        previous_generation.saturating_add(u64::from(reset))
    );
    assert_eq!(
        observation.read_from,
        if reset { 0 } else { previous_offset }
    );
    assert_eq!(observation.read_to, file.len() as u64);
    *observed_identity = Some(identity);

    let bytes = &file[observation.read_from as usize..observation.read_to as usize];
    let update = cursor
        .push(identity, observation.read_from, bytes)
        .expect("bounded exact observed ranges must be accepted");
    assert_eq!(update.next_offset, observation.read_to);
    assert_eq!(update.generation, observation.generation);
    let expected_pending = file
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(file.len(), |newline| file.len() - newline - 1);
    assert_eq!(update.pending_bytes, expected_pending);
    assert_eq!(cursor.has_partial_line(), expected_pending != 0);
    assert!(update.pending_bytes <= MAX_TRANSCRIPT_LINE_BYTES);
    // The cursor strips at most ONE trailing carriage return, which is exactly
    // CRLF normalization (`cursor.rs:188`). A line whose source ended in several
    // CRs therefore legitimately still ends in `\r`; asserting otherwise
    // over-specifies the contract and was the cause of the libFuzzer
    // `deadly signal` on crash-3f4d09ccb50dbe940c95532eb00a64f65f24f3da.
    assert!(
        update
            .lines
            .iter()
            .all(|line| line.bytes.len() <= MAX_TRANSCRIPT_LINE_BYTES
                && !line.bytes.contains(&b'\n'))
    );
}
