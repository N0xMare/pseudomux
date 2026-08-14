# pmux patch for rmux-server 0.9.0

This directory is the exact published `rmux-server` 0.9.0 crate plus one
bounded attach-EOF correctness repair and its fourteen regression tests.

- crates.io archive SHA-256:
  `5b9e539353499018407a602ab5f916288bfa6b07d7a93764eeaf850effff0e8d`
- published VCS SHA-1:
  `b2f80522bae2927e22d81e5c902b727623f934d0`
- published file count: `596`
- canonical published-tree SHA-256:
  `ce1048e1d02c7274119df5b9b13ad2b83ed0d14301969848e1a8c65049dfa54a`

The tree hash is computed over every published relative path and file body in
byte-sorted path order. Each item contributes an unsigned 64-bit big-endian
path length, the UTF-8 path, an unsigned 64-bit big-endian body length, and the
exact body. Cargo's extraction-only `.cargo-ok` marker is not published and is
not vendored.

Only these published files differ:

- `src/pane_io.rs` no longer exits an opportunistic socket drain before
  dispatching complete attach frames buffered ahead of orderly EOF. It
  dispatches each complete frame exactly once, then closes normally only when
  the decoder is empty. A truncated final frame returns `UnexpectedEof` and
  never reaches the pane. Its constant-size processing disposition also
  re-enters the outer attach loop after an intentional Unlock or pending-escape
  barrier, without cloning or rescanning the frame decoder, so barrier output
  and queued terminal controls retain ownership before EOF classification.
  Due retained-input barriers re-enter even when the frame decoder is empty;
  Unlock remains conditional on actual decoder residual. Before orderly EOF
  owns detach, any ambiguous suffix retained by a complete client Data frame
  is forced through the existing identity-checked pending-input semantics.
  A freshly retained suffix must strictly decrease on every forced pass, which
  bounds that drain by the original retained byte count without interpreting
  the input a second way. At orderly EOF the forwarder closes its control
  receiver, then await-drains it to `None` into the existing deferred queue.
  This is the send-versus-EOF linearization boundary: every send that won
  earlier remains ordered and is applied, while every later send fails. A
  terminal winner is recognized even before its producer publishes the
  closing latch, and stale decoder or retained-input bytes are discarded with
  that identity before the next due-input flush. A close producer that loses
  the receiver seal cannot turn its later identity invalidation into a
  spurious EOF error, and a closing latch without a queued control cannot spin.
- `src/pane_io/tests.rs` adds deterministic complete-frame, truncated-frame,
  complete-prefix/truncated-tail, preclosed Unlock/Data barrier, and
  selected-read Unlock/Data barrier EOF regressions. It also covers a final
  complete Data frame with a newly retained escape, an already-due retained
  escape with no decoder residual, pre- and post-validation close publication,
  and the bounded no-terminal-control close fallback.
  One cross-product regression proves an already-published close outranks
  a due retained-input barrier before the next outer-loop flush can reject the
  removed identity. Three deterministic receiver-seal regressions cover the
  normal queue-before-closing-publication window with due stale input, the
  overload-style closing-publication-before-terminal-enqueue window, and a
  losing close that invalidates identity during EOF-owned retained-input
  validation.

The exact regression names are:

- `complete_input_frame_is_dispatched_before_orderly_eof`;
- `truncated_input_frame_at_eof_fails_without_mutating_the_pane`;
- `complete_prefix_is_dispatched_once_before_truncated_tail_fails`;
- `complete_frame_after_unlock_barrier_is_dispatched_before_orderly_eof`;
- `selected_eof_reenters_after_unlock_barrier_before_classifying_residual`;
- `final_complete_data_frame_flushes_newly_retained_escape_before_orderly_eof`;
- `already_due_retained_escape_reenters_and_flushes_before_selected_eof`;
- `preclosed_session_exit_before_input_validation_drains_final_output`;
- `preclosed_session_exit_after_input_validation_drains_final_output`;
- `preclosed_closing_without_terminal_control_is_bounded`;
- `published_close_outranks_due_eof_barrier_before_pending_flush`;
- `terminal_control_enqueued_before_closing_publication_wins_over_orderly_eof`;
- `orderly_eof_seal_rejects_terminal_enqueue_after_closing_publication`; and
- `orderly_eof_seal_wins_close_during_retained_input_validation`.

The minimized regression preloads a complete `AttachMessage::Data(b"y\r")`
and EOF into a bounded Tokio duplex transport before `forward_attach` starts.
The unmodified published implementation deterministically captured no pane
input; the repaired implementation captures the exact two bytes. The other
two first-generation regressions prove incomplete input is fail-closed and a
complete prefix is dispatched once before a truncated tail reports
`UnexpectedEof`. The two barrier regressions preload
`Unlock` followed by `Data(b"y\r")` and prove that both an already-known EOF and
an EOF selected after an opportunistic processing pass preserve Unlock's
inter-frame ordering while still delivering exactly `y\r`.

The retained-input regressions deterministically failed before this extension:
the final frame captured `A` instead of `A ESC`, and the empty-decoder due
probe returned no re-entry. The post-validation close regression precloses the
peer, pauses after a successful identity check but before Data mutation, then
publishes final output and `KillSession`; the terminal control must retain
output ownership. The pre-validation variant anchors stale-validation
recovery, while the no-control variant proves that recovery is bounded and
never exposes its stale input.

The queue-before-latch regression failed the previous repair because the
stream closed before final output or `[exited]`; the inverse regression failed
because a terminal enqueue after the claimed EOF boundary still succeeded.
The first now also arms an already-due ESC and proves terminal classification
clears it before the next outer-loop flush. Removing receiver close makes the
inverse late send succeed, removing terminal classification exposes the ESC,
and removing the post-seal identity-invalidated recovery reports a stale-input
EOF error. Earlier mutations that removed stale-close recovery, post-validation
close handling, unconditional due re-entry, forced EOF flushing, or close
priority over a due barrier each failed their owning regression. Both Unlock
barriers and all nine extended regressions passed 20 exact serialized
repetitions (220/220 total) in the no-default-feature product cell.

`crates/rmux/tests/vendor_server_patch.rs` reconstructs both upstream files in
memory, verifies their published hashes and the complete published-tree hash,
checks the local patched hashes, validates this document, and proves Cargo
resolves the exact `=0.9.0` dependency to this path. It also proves
`pmux-rmuxd` declares `uses_default_features = false`, requests no explicit
features, and resolves the server node with an exactly empty feature set.
`apps/pmux-rmuxd/tests/process_blackbox.rs::real_attach_half_close_delivers_the_final_complete_frame_exactly_once`
repeats the invariant across the actual private sidecar, rmux client wire, and
PTY. `TESTING.md` defines the standalone offline validation lane. Pmux uses
`rmux-server` with no default or explicit features; `web`, `fuzzing`, and
`perf-instrument` are outside this patch and product path.
