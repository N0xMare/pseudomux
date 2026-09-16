# `attach.rs:694`: a fragmented attach data frame is completed with stale bytes from the previous read, silently corrupting the payload

Filed 2026-09-02 as https://github.com/Helvesec/rmux/issues/221.

**Crate:** `rmux-client` (Unix attach path)
**Affected:** 0.9.0, 0.9.1, 0.10.0 and `main`. `crates/rmux-client/src/attach.rs` is byte-identical
across all four (md5 `ccddf8572567fd0943a47433312cefc9`: the three published crate sources, and
`main` at `1f4571e7` via the contents API), and the expression below is at line 694 in each.
**Severity:** silent data corruption, then desynchronisation of the attach stream.

Checked against `main` at `1f4571e7` on 2026-09-01; I could not find an existing issue covering
this.

Context, in one sentence: I drive terminal panes programmatically over the rmux client/server API,
which fragments attach writes more often than an interactive user does. Nothing below depends on
that — the reproduction is a socket pair and one dependency on the published crate.

## Summary

In the attach output loop, the complete-frame fast path hands `decode_attach_data_frame` **the whole
remainder of the reusable 8 KiB scratch buffer** rather than the bytes just read:

```rust
// crates/rmux-client/src/attach.rs:691-710 (0.10.0)
        let mut consumed = 0;
        if decoder.is_empty() {
            while consumed < bytes_read {
                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..])  // :694
                    .map_err(ClientError::from)?
                else {
                    break;
                };
                handle_attach_data_payload(
                    &mut output,
                    &locked,
                    &mut stop_detector,
                    frame.payload(),
                )?;
                consumed += frame.frame_len();
            }
        }
        if consumed < bytes_read {
            decoder.push_bytes(&read_buffer[consumed..bytes_read]);   // :709 — bounded correctly
        }
```

`read_buffer` is `[0_u8; READ_BUFFER_SIZE]` with `READ_BUFFER_SIZE = 8192` (`:49`), allocated once
outside the loop (`:475`) and reused by every read. The decoder's "need more bytes" answer is
length-based (`rmux-proto` 0.10.0 `src/attach.rs:325-357`, elisions marked):

```rust
pub fn decode_attach_data_frame_with_limit(
    input: &[u8],
    max_data_length: usize,
) -> Result<Option<AttachDataFrame<'_>>, RmuxError> {
    if input.first().copied() != Some(DATA_TAG) {   // :329  DATA_TAG = 1 (:6)
        return Ok(None);
    }
    if input.len() < DATA_HEADER_LEN {              // :332  DATA_HEADER_LEN = 5 (:20)
        return Ok(None);
    }
    let length = /* ... u32-LE from input[1..5] ... */;
    // ... FrameTooLarge check ...
    let frame_len = DATA_HEADER_LEN + length;
    if input.len() < frame_len {                    // :349  the incomplete-frame signal
        return Ok(None);
    }
    Ok(Some(AttachDataFrame {
        payload: &input[DATA_HEADER_LEN..frame_len],
        frame_len,
    }))
}
```

Because the caller passes a slice of length `8192 - consumed`, the guard at `:349` cannot fire
unless the declared frame is longer than the whole remaining scratch buffer. A frame split across
two reads is therefore decoded as complete, and its
payload tail is read out of buffer positions the peer has not written yet — whatever the *previous*
read left there. Line 709, eleven lines below, bounds the same buffer to `..bytes_read`; only 694
omits the bound.

Every public attach entry point (`attach_terminal`, `attach_terminal_with_initial_bytes`,
`attach_with_terminal`, `drive_attach_stream`) funnels through `drive_attach_stream_inner`, called
at `:241` and `:267`, into `output_loop_with_termination` (`:305`).

## What the consumer sees

**First, silently:** a payload of the correct declared length whose tail is stale bytes from the
previous read (or zeros on the buffer's first use). No error, no warning — the wrong bytes are
written to the terminal.

**Then the tail of the real frame arrives** and, because `consumed` was advanced by the full
`frame_len`, it is read as the start of a new message. Unless that tail happens to start with
`DATA_TAG`, the guard at `:329` returns `Ok(None)` and line 709 hands it to the incremental decoder,
which takes its first byte for a message tag. Whether the desync is loud then depends entirely on
which byte that is:

* an unknown tag → `Decode("unknown attach-stream message tag N")`, and the attach session fails.
  This is the case in the reproduction below;
* a known tag → the bytes are read as a fresh message header and the stream desynchronises **with no
  error at all**. `13` is `RENDER_TAG` (`rmux-proto` 0.10.0 `src/attach.rs:18`), so a fragment
  boundary that leaves the tail beginning at a `\r` is the quiet one. With the second
  frame's payload changed to `b"\r\n"` and a third complete frame sent afterwards: the client emitted
  66 bytes (the 64-byte first frame plus two stale `A`s), **never delivered the third frame at all**,
  reported no decode error, and failed only once the peer closed, with `attach stream closed before
  attach-stop sequence`.

## Reproduction

Against the published crate, no patches. `Cargo.toml`:

```toml
[dev-dependencies]
rmux-client = "=0.10.0"
```

`tests/fragmented_attach.rs`:

```rust
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Second frame's payload; ends with the attach-stop sequence so a healthy
/// client returns `Ok`.
const SECOND: &[u8] = b"y\x1b[?1049l";

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `DATA_TAG | u32-LE payload length | payload`
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![1_u8];
    out.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    out.extend_from_slice(payload);
    out
}

#[test]
fn fragmented_data_frame_is_delivered_intact() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let (input, _input_peer) = UnixStream::pair().unwrap();
    let (_resize_tx, resize_rx) = mpsc::channel();
    let sink = Sink::default();
    let observed = sink.clone();

    let writer = thread::spawn(move || {
        // Frame 1, whole: leaves 'A's in the client's scratch buffer.
        let _ = server.write_all(&frame(&[b'A'; 64]));
        let _ = server.flush();
        thread::sleep(Duration::from_millis(50));
        // Frame 2, split: header now, payload after the client has read the header.
        let second = frame(SECOND);
        let _ = server.write_all(&second[..5]);
        let _ = server.flush();
        thread::sleep(Duration::from_millis(50));
        let _ = server.write_all(&second[5..]);
        let _ = server.flush();
    });

    let result = rmux_client::drive_attach_stream(client, input, sink, resize_rx);
    writer.join().unwrap();
    let bytes = observed.0.lock().unwrap().clone();
    println!("drive_attach_stream returned: {result:?}");
    assert_eq!(&bytes[..64], &[b'A'; 64], "first frame");
    assert_eq!(&bytes[64..], SECOND, "second frame");
    result.expect("attach stream");
}
```

`cargo test --test fragmented_attach` on 0.10.0, macOS 15.7.7 arm64, `rustc 1.97.1`:

```
running 1 test
test fragmented_data_frame_is_delivered_intact ... FAILED

---- fragmented_data_frame_is_delivered_intact stdout ----
drive_attach_stream returned: Err(Protocol(Decode("unknown attach-stream message tag 121")))

thread 'fragmented_data_frame_is_delivered_intact' (498462416) panicked at tests/fragmented_attach.rs:62:5:
assertion `left == right` failed: second frame
  left: [65, 65, 65, 65, 65, 65, 65, 65, 65]
 right: [121, 27, 91, 63, 49, 48, 52, 57, 108]

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

`left` is nine `A`s — bytes from the *first* frame, handed to the consumer as if they were the
second frame's payload. `121` is `y`, the first byte of the payload that did arrive, now being read
as a message tag. Reproduced 10/10 runs.

(The two 50 ms sleeps only have to be long enough that the client's `read` returns the header alone;
a stricter version polls `FIONREAD` on a clone of the writer's socket instead of sleeping.)

## Fix

```diff
--- a/crates/rmux-client/src/attach.rs
+++ b/crates/rmux-client/src/attach.rs
@@ -691,7 +691,7 @@
         let mut consumed = 0;
         if decoder.is_empty() {
             while consumed < bytes_read {
-                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..])
+                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..bytes_read])
                     .map_err(ClientError::from)?
                 else {
                     break;
```

That restores the incomplete-frame signal: the partial frame falls through to `break` and is handed
to the incremental `decoder` by line 709, which is what that branch exists for.

I ran this on an otherwise-pristine 0.10.0 source tree with only that line changed: the test above
passes 10/10, and `cargo test` over the crate's own suite is unchanged at **160 passed, 0 failed**
across its 8 test binaries, before and after.

Happy to open a PR with the fix and a regression test if that is useful.
