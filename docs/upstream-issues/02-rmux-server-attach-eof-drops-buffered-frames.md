# Issue draft — rmux-server

**Title:** `pane_io.rs`: attach input already buffered in the frame decoder is dropped when the
client half-closes, so the last complete frame never reaches the pane

**Repo:** https://github.com/Helvesec/rmux
**Crate:** `rmux-server`, default features
**Affected:** 0.9.0 (`src/pane_io.rs:402`), 0.9.1 (`:451`), 0.10.0 and `main` (`:461`).
`crates/rmux-server/src/pane_io.rs` is byte-identical at 0.10.0 and `main` (`1f4571e7`, md5
`ac809b2997cf4e00b181a055f91dd73e`). Line numbers below are 0.10.0.
**Severity:** silent input loss — the client observes a successful write and an orderly close, and
the pane never receives the bytes.

Context, in one sentence: I drive terminal panes programmatically over the rmux client/server API,
so my clients write and then half-close rather than staying attached; that is the shape that hits
this. The reproduction below is upstream's own test module plus one test.

---

## Summary

`forward_attach` reads attach-socket bytes **into** an `AttachFrameDecoder` and dispatches decoded
frames to the pane in a **separate** step (`process_attach_socket_messages`). Both read sites
`return Ok(())` the moment they observe EOF — before that dispatch step runs — so a complete frame
that is already sitting in the decoder is discarded.

A client that writes a complete frame and then half-closes (`write`, then `shutdown(Write)`) can
have its final input silently dropped. Both sides report success.

## Root cause

### Site 1 — the immediate read burst (`:458-472`), the one reproduced below

```rust
            for _ in 0..MAX_IMMEDIATE_ATTACH_READS {              // :458
                match try_read_socket_bytes(&stream, &mut decoder)? {
                    TryAttachRead::Read => {}                     // a COMPLETE frame lands in `decoder`
                    TryAttachRead::Closed => {                    // :461 — peer half-closed
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachStreamClosed,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());                            // :468 — `decoder` is non-empty
                    }
                    TryAttachRead::WouldBlock => break,
                }
            }
            // ...
            process_attach_socket_messages(                       // :480 — skipped on the EOF path
```

`try_read_socket_bytes` is `stream.try_read_into(decoder)` (`src/pane_io/wire.rs:145-150`): it
appends to the decoder and returns only a status. Bytes read on an earlier iteration of the burst
are therefore already in `decoder` when a later iteration sees `Closed`. `process_attach_socket_messages`
is the only thing that drains `decoder` into the pane, and `forward_attach` calls it in exactly two
places — `:480` here and `:628` in the `select!` arm below — each of them after the early return.

### Site 2 — the `select!` read arm (`:611-620`), same shape

```rust
                result = read_socket_bytes(&stream, &mut decoder) => {   // :611
                    if !result? {                                        // false == EOF
                        log_attach_exit(/* ... */);
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());                                   // :619
                    }
                    // ...
                    process_attach_socket_messages(                      // :628
```

## Reproduction

Deterministic, no PTY and no timing dependence: both the complete frame and the EOF are already
pending before `forward_attach` runs, so the burst loop's first `try_read_socket_bytes` returns
`Read` and its second returns `Closed`.

Unpack the published `rmux-server` 0.10.0 crate (or check out `v0.10.0`) and append this to the
crate's own `src/pane_io/tests.rs` — `crates/rmux-server/src/pane_io/tests.rs` in the repo. Every
other symbol it uses already exists in that module at 0.10.0:

```rust
async fn run_preclosed_attach_input(
    name: &str,
    wire_bytes: &[u8],
) -> (std::io::Result<()>, Vec<u8>) {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = 910_030;
    let target = create_attach_input_test_session(&handler, name).await;
    let session_name = target.session_name().clone();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;

    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    // Both the complete frame and the EOF are pending before forward_attach runs.
    let (stream, mut peer) = tokio::io::duplex(4096);
    peer.write_all(wire_bytes)
        .await
        .expect("write final attach input");
    peer.shutdown()
        .await
        .expect("orderly-close the attach input half");
    let attach_task = tokio::spawn(forward_attach(
        AttachTransport::from_io(stream),
        test_attach_target(&session_name, b"BASE", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await,
        false,
    ));

    let result = tokio::time::timeout(Duration::from_secs(2), attach_task)
        .await
        .expect("attach must stop after peer input EOF")
        .expect("attach task join");
    let captured = handler
        .attached_input_capture_for_test(&target)
        .await
        .expect("input capture remains installed");
    (result, captured)
}

#[tokio::test]
async fn complete_input_frame_is_dispatched_before_orderly_eof() {
    let input = b"y\r";
    let wire = encode_attach_message(&AttachMessage::Data(input.to_vec()))
        .expect("encode final attach input");
    let (result, captured) =
        run_preclosed_attach_input("complete-input-before-orderly-eof", &wire).await;
    result.expect("attach exits cleanly");
    assert_eq!(
        captured, input,
        "every complete frame read before EOF must mutate the pane"
    );
}
```

`cargo test --lib pane_io::tests::complete_input_frame_is_dispatched_before_orderly_eof` on
0.10.0, macOS 15.7.7 arm64, `rustc 1.97.1`:

```
running 1 test
test pane_io::tests::complete_input_frame_is_dispatched_before_orderly_eof ... FAILED

---- pane_io::tests::complete_input_frame_is_dispatched_before_orderly_eof stdout ----
assertion `left == right` failed: every complete frame read before EOF must mutate the pane
  left: []
 right: [121, 13]

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4081 filtered out
```

The pane received nothing where `y\r` was written and the peer then half-closed. Note that
`result.expect("attach exits cleanly")` passes: `forward_attach` returns `Ok(())`, so nothing
anywhere reports a problem. Reproduced 10/10 runs.

## Fix

Defer the exit instead of returning from inside the read step: record that the stream closed, leave
the read loop, let the existing dispatch run, and only then classify the close.

```diff
--- a/crates/rmux-server/src/pane_io.rs
+++ b/crates/rmux-server/src/pane_io.rs
@@ -455,17 +455,13 @@
             // A pending repaint must not stop input from reaching the pane.
             // The repaint is rendered from the current transcript when its
             // deadline fires, so fresh input can safely pull the deadline in.
+            let mut attach_stream_closed = false;
             for _ in 0..MAX_IMMEDIATE_ATTACH_READS {
                 match try_read_socket_bytes(&stream, &mut decoder)? {
                     TryAttachRead::Read => {}
                     TryAttachRead::Closed => {
-                        log_attach_exit(
-                            &live_input,
-                            &current_target,
-                            AttachExitReason::AttachStreamClosed,
-                        );
-                        let _ = emit_attach_stop(&stream, &current_target).await;
-                        return Ok(());
+                        attach_stream_closed = true;
+                        break;
                     }
                     TryAttachRead::WouldBlock => break,
                 }
@@ -492,6 +488,21 @@
             )
             .await?;
             drop(socket_batch);
+            if attach_stream_closed {
+                if !decoder.is_empty() {
+                    return Err(io::Error::new(
+                        io::ErrorKind::UnexpectedEof,
+                        "attach stream closed mid-frame",
+                    ));
+                }
+                log_attach_exit(
+                    &live_input,
+                    &current_target,
+                    AttachExitReason::AttachStreamClosed,
+                );
+                let _ = emit_attach_stop(&stream, &current_target).await;
+                return Ok(());
+            }
             if attach_shutdown_observable(&shutdown) {
                 continue;
             }
```

Measured with exactly that diff applied to an otherwise-pristine 0.10.0 tree:
`cargo test --lib pane_io::` goes from **206 passed, 1 failed** (the new test) to **207 passed,
0 failed**. That patch covers site 1 only; the `select!` arm at `:611-620` needs the same treatment
and was not changed in that run.

Three behaviours a fix should hold, because a naive one gets them wrong:

* **Truncated final frame + EOF** must fail closed with `UnexpectedEof` and must *not* reach the
  pane. Dispatching a partial frame is worse than dropping it.
* **Complete prefix + truncated tail**: the complete prefix is dispatched exactly once, and only
  then is `UnexpectedEof` reported.
* **Inter-frame ordering across the deferred close**: an `Unlock` frame followed by a `Data` frame
  in the same burst must keep its order, so the dispatch step has to run to completion before the
  close is classified rather than being short-circuited once EOF is known.

Happy to open the PR with the deferral at both sites plus those three regressions.
