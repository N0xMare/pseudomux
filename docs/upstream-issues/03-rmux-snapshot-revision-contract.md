# What does `PaneSnapshot::revision` promise between two captures? Docs read as a mutation counter, the registry is a per-capture comparator

**Crates:** `rmux-sdk` (`PaneSnapshot::revision`), `rmux-proto` (`PaneSnapshotResponse::revision`)
**Affected:** the documented text is unchanged across 0.9.0, 0.9.1, 0.10.0 and `main` (`1f4571e7`).
`rmux-sdk/src/snapshot.rs` is byte-identical in all four (md5 `090bee4c6ca170154e3920f7ec728fbf`).
Line numbers below are 0.10.0 unless stated.
**Severity:** documentation. Nothing misbehaves; the daemon is internally consistent and its own
tests describe the implemented semantics exactly. The published prose describes a different one.
**Platform:** all.

Checked against `main` at `1f4571e7` on 2026-09-01; I could not find an existing issue
covering this.

Why I am asking: I drive terminal panes programmatically over the rmux client/server API
and poll `snapshot()` to decide when a pane has settled, so what `revision` promises about the
interval between two captures is load-bearing for me — and I could not determine it from the docs.

## Summary

The three published sentences about `revision` support two incompatible readings:

* **mutation counter** — it advances whenever the pane changes, so two captures reporting the same
  revision prove the pane was unmodified for the whole interval between them;
* **capture comparator** — it advances when a capture observes a fingerprint different from the one
  the *previous capture* observed, so equal revisions say only that those two captures saw the same
  thing.

The implementation is the second, unambiguously. The type-level doc states the first.

## What the docs say

`rmux-sdk` `src/snapshot.rs:25-27`, on the type — this is the mutation-counter reading:

> `revision` is a daemon-derived counter that changes whenever the captured pane state mutates —
> output, resize, clear, exit, or any other visible change. Consumers use it as the canonical
> "did the pane move?" signal

`rmux-sdk` `src/snapshot.rs:44-47`, on the field — this compares two captures, and its "therefore"
is only valid under the first sentence's reading:

> The producer guarantees that when any observable pane field (`cols`, `rows`, `cells`, `cursor`,
> the underlying process state) changes between two captures, the revision changes too. Equal
> revisions therefore mean "nothing observable changed".

`rmux-proto` `src/response/pane.rs:769-771` (identical text at 0.9.0/0.9.1 `:528-530`):

> The daemon-derived `revision` is non-zero for every captured live pane and changes whenever any
> observable field (cells, cursor, output_sequence, history bytes/lines, pane id) changes.

## What the daemon does

`rmux-server/src/handler_pane/snapshot.rs:372-411`. The registry's entire state for a pane is one
`{fingerprint, revision}` pair — the fingerprint the **last materialised capture** produced:

```rust
fn revision_for_at_least(&mut self, pane_id: PaneId, fingerprint: u64, minimum: u64) -> u64 {
    let revision = match self.panes.get(&pane_id) {
        Some(state) if state.fingerprint == fingerprint => state.revision,
        Some(state) => state.revision.saturating_add(1),
        None => 1,
    }
    .max(minimum);
    self.panes.insert(pane_id, PaneSnapshotRevisionState { fingerprint, revision });
    revision
}
```

No pane mutation reaches this registry. The only code that assigns a revision is the three capture
paths that call `assign_pane_snapshot_revision{,_at_least}` — everything else merely prunes retired
panes (`forget_pane_snapshot_coalescers`, `:238`):

| site | producer |
|---|---|
| `handler_pane/snapshot.rs:144` | the `PaneSnapshotRequest` endpoint |
| `handler/pane_stream_capture.rs:156` | `materialize_surface_frame` (surface stream), with a floor |
| `handler/pane_stream_capture.rs:249` | `materialize_typed_snapshot` (recovery snapshot) |

At 0.9.0 and 0.9.1 there was only the first (`handler_pane/snapshot.rs:131`). The second and third
arrived with 0.10.0's `surface_stream()` / `recover_output()`.

Two consequences follow directly, and upstream's own tests assert both:

1. **A repeat of an earlier state is a new revision, not the earlier one.**
   `pane_snapshot_revisions_are_monotone_for_state_transitions` (`:629-633`) asserts
   `revision_for(pane, 10)` returns `3` after `10, 10, 20`, with the message *"returning to prior
   content is still a new transition"*. So `revision` is not a content hash; it counts transitions
   in the sequence of fingerprints that were **actually materialised**.
2. **The counter is shared, so it advances for reasons a snapshot consumer cannot see.**
   `pane_snapshot_revision_floor_is_recorded_not_just_returned` (`:637-667`) asserts that after a
   surface-stream reset raises the floor, a snapshot read of *byte-identical* pane state returns the
   raised value — *"an identical reset publishes a strictly newer revision"*. The design comment at
   `:192-201` is explicit that `PaneSurfaceSnapshot::revision` and `PaneSnapshotResponse::revision`
   *"are documented as one shared monotonic counter"*.

## Demonstration

The whole argument is one consumer's two captures, run twice over the same pane history, differing
only in whether a second producer looked in between. Appended to
`crates/rmux-server/src/handler_pane/snapshot.rs` as a `#[cfg(test)]` sibling module (it uses the
crate's own test-only `revision_for`), this **passes** on pristine 0.10.0:

```rust
#[test]
fn two_captures_of_the_same_pane_history_report_different_revisions() {
    let pane_id = PaneId::new(7);

    // Run A: a consumer captures, the pane moves to another observable
    // state and back, the consumer captures again. Nothing else looked.
    let mut a = PaneSnapshotRevisionRegistry::default();
    let a0 = a.revision_for(pane_id, 10);
    let a1 = a.revision_for(pane_id, 10);

    // Run B: identical pane history and identical consumer captures, but
    // a surface-stream frame materialised while the pane was at 20.
    let mut b = PaneSnapshotRevisionRegistry::default();
    let b0 = b.revision_for(pane_id, 10);
    let _other_observer = b.revision_for(pane_id, 20);
    let b1 = b.revision_for(pane_id, 10);

    assert_eq!((a0, a1), (1, 1));
    assert_eq!((b0, b1), (1, 3));
}
```

```
running 1 test
test two_captures_of_the_same_pane_history_report_different_revisions ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4081 filtered out; finished in 0.01s
```

The consumer's own two captures are identical in both runs. Its revisions are `1, 1` in one and
`1, 3` in the other. What differs is only whether some *other* producer materialised a capture in
between — which is invisible to it. Under the mutation-counter reading, `1, 1` would be a proof that
the pane held still; it is not one.

## The field lists do not match the fingerprint either

`compute_snapshot_fingerprint` (`handler_pane/snapshot.rs:333-341`) takes eight inputs and hashes
all of them (`:344-363`): `cols`, `rows`, `cells`, `cursor`, `output_sequence`, `history_size`,
`history_bytes`, `pane_id`.

| list | omits | consequence for a reader |
|---|---|---|
| sdk `:45` — *"`cols`, `rows`, `cells`, `cursor`, the underlying process state"* | `output_sequence`, `history_size`, `history_bytes`, `pane_id` | four inputs are folded into an undefined phrase |
| proto `:771` — *"cells, cursor, output_sequence, history bytes/lines, pane id"* | `cols`, `rows` | a resize is not in the documented list, but is hashed |

The gap that matters to a consumer: `PaneSnapshotResponse` carries only `cols`, `rows`, `cells`,
`cursor` and `revision`. Four of the eight fingerprint inputs are not on the wire at all, so
`revision` routinely advances between two responses that are equal in every field the client can
read. That is the expected behaviour, but neither list says so.

Pointing the docs at `compute_snapshot_fingerprint` would keep them in sync.

## What I am asking for

Replace the two `revision` doc comments with the implemented semantics. Something like:

> `revision` is assigned when the daemon materialises a capture of the pane — the snapshot endpoint,
> a surface-stream frame, or a recovery snapshot. A materialisation whose fingerprint differs from
> the previous materialisation's advances the counter by one; a repeat of the previous fingerprint
> returns the same value, and a return to an *older* state counts as a new transition. All producers
> share one counter per pane, so the revision can advance between two responses that are identical
> in every field this type carries.
>
> Equal revisions on two captures therefore mean those two captures observed identical daemon-side
> state. They do not prove the pane was unmodified in the interval between them: a change that no
> capture observed leaves no trace. A consumer that needs a quiet-window proof must sample it.

and fix both input lists to match `compute_snapshot_fingerprint`.

If instead the interval property *is* intended, then it needs a mutation hook rather than a doc
change, and it would be worth saying so — the crate already has a genuine mutation-driven counter
next door (`PaneStateSnapshot::revision`, *"Global pane-state journal revision"*, proto `:254`;
`PaneStateCursorResponse` *"Events delivered in strictly increasing revision order"*, `:365`), which
is a large part of why the snapshot `revision` reads like one.

## One question I could not answer from outside

`compute_snapshot_fingerprint` hashes `output_sequence`, which is monotone
(`pane_transcript.rs:142`, `:250`, `:440` are the only advancement sites), so an *output-driven*
change and revert does still move the fingerprint. That narrows the gap in practice — but it is an
invariant of a field that is not on the wire, is not in the sdk's list, and a consumer could not
detect its removal.

`PaneTranscript::resize` (`:479-484`) does **not** advance `output_sequence`. So for a pane resized
away and back with no intervening output, whether the fingerprint changes rests entirely on whether
the reflow is lossy in a way that shows up in `cells` / `history_size` / `history_bytes`. I could
not establish that from outside the crate, and it is exactly the sort of thing a consumer should not
have to establish.

## Not a behaviour report

I am not asking for the semantics to change. A per-capture change detector is a reasonable thing for
this endpoint to be, the implementation is coherent, and the sdk field doc's second sentence is
already close to correct under the weak reading. The problem is the first sentence and the
`PaneSurfaceSnapshot`/`PaneSnapshotResponse` asymmetry: `PaneSurfaceSnapshot::revision` (proto
`:624`) documents the sharing — *"Monotonic pane-grid revision shared with `PaneSnapshotResponse`"*
— while `PaneSnapshotResponse::revision` (`:782`) says only *"Daemon-derived revision counter for
this captured state"*, and the sdk mirror of it says nothing about other observers at all.

## Method

Read from the published crates (`rmux-sdk`, `rmux-proto`, `rmux-server` 0.9.0 / 0.9.1 / 0.10.0) and
from `main` at `1f4571e7`. Every rmux source file cited above is byte-identical between 0.10.0 and
`main`. The test was run against an unmodified `rmux-server` 0.10.0 tree from `static.crates.io`,
default features, macOS arm64, `rustc 1.97.1`.

Thanks for any clarification you can give on which reading is intended.
