# rmux upstream state — are the three unfiled issue drafts still worth filing?

**Session date: 2026-08-12. Host: macOS 15 / arm64, `rustc 1.97.1`.** This document answers one
question about `.context/rmux-issue-drafts/`: file, revise, or drop. It also inventories what
`vendor/rmux-{client,server}` actually carries against the pristine published crates, and sizes an
upgrade to the current release.

**Answer, up front: file all three, after the revisions §2 lists. Do not upgrade rmux for the sake
of these defects — an upgrade retires none of them.**

## 0. Evidence classes, and what the network gave

Three labels are used and they are not interchangeable:

* **MEASURED** — a command was run this session and its output is quoted or counted here.
* **VERIFIED-EXTERNAL** — read this session from crates.io, the GitHub API, or upstream release
  notes, with the URL or `gh api` route given.
* **UNVERIFIED** — believed, not established here.

**The network was reachable.** `crates.io` (HTTP API and `static.crates.io` archives) and the GitHub
API through an authenticated `gh` both answered. Nothing in this document is inferred from the
vendored copy where upstream itself was obtainable, and every upstream source claim below was made
against bytes downloaded this session, not against `vendor/`.

Scratch trees used and removed: `/tmp/rmux-up` (upstream 0.9.1 and 0.10.0 archives for
`rmux-{client,server,proto,sdk}`), `/tmp/frag-repro` and `/tmp/frag-fixed` (draft 01),
`/tmp/srv-pristine`, `/tmp/srv-repro2` and `/tmp/srv-090` (draft 02 and the build-cell check).

## 1. Upstream state

**The project is actively maintained, not dormant.** VERIFIED-EXTERNAL, `gh api repos/Helvesec/rmux`:
2,568 stars, issues enabled, not archived, `pushed_at` 2026-08-09, 8 open issues.
`gh api repos/Helvesec/rmux/compare/v0.9.0...main` reports **487 commits ahead** of the tag this tree
vendors, and `compare/v0.10.0...main` reports 25 more since the latest release.

| fact | value | source |
|---|---|---|
| latest release | **0.10.0**, published **2026-08-05** (tag `v0.10.0` dated 2026-08-04) | crates.io API; `gh api repos/Helvesec/rmux/releases` |
| vendored release | **0.9.0**, published **2026-07-18** | crates.io API |
| intervening release | 0.9.1, 2026-07-24 | crates.io API |
| yank status | none of 0.9.0, 0.9.1, 0.10.0 is yanked | `api/v1/crates/rmux-{client,server}/<v>` |
| release cadence | 17 versions since 2026-05-15, i.e. roughly weekly | crates.io API |

What landed in 0.10.0, from the release body (`gh api repos/Helvesec/rmux/releases/tags/v0.10.0`):
capability-gated `Pane::recover_output()` and `Pane::surface_stream()`, authoritative recovery
frames, `render_stream()` moved onto a daemon-side surface projection, Web-Share hardening, a batch
of Windows fixes (#92, #177, #179, #181, #180, #182, #183), `Request`/`Response` envelopes made
`non_exhaustive`, and detached RPC moved **from wire version 5 to version 8**. The notes state
plainly: *"RMUX 0.10.0 is not wire-compatible with 0.9.x daemons."*

**No upstream issue covers any of the three drafts.** VERIFIED-EXTERNAL, `gh api search/issues` over
`repo:Helvesec/rmux` for `attach` (45 hits), `revision`, `snapshot`, `EOF`, `half-close`,
`fragmented`, `input loss`, `frame decode`. The nearest misses, and why each is a miss:

* **#94** (closed) *"SDK: index-based pane handles return silent all-blank snapshots (revision: 0)"*
  and its fix **#97** — about resolving a pane handle, not about what `revision` means.
* **#216** (open, 2026-08-10) *"display-popup: large popup paint floods attach control backlog and
  detaches the client"* — attach backlog, not frame decoding or EOF.
* **#92** (open) bracketed-paste over Windows/ConPTY, which the 0.10.0 notes say was fixed for
  *"fragmented reads"* — Windows console input path, a different file from the Unix attach loop.

Commit history for the two files in question, VERIFIED-EXTERNAL via
`gh api "repos/Helvesec/rmux/commits?path=…&since=2026-07-18"`: **`crates/rmux-client/src/attach.rs`
has had no commits at all since v0.9.0**; `crates/rmux-server/src/pane_io.rs` has had five
(`554998c6`, `af470d5b`, `284fcd5a`, `e099169d`, `23321146`), none touching EOF classification.

## 2. The three verdicts

### 2.1 Draft 01 — `rmux-client` unbounded attach slice: **STILL VALID**, and now provable at 0.10.0

The claim: the Unix attach fast path hands `decode_attach_data_frame` the whole remainder of the
reusable 8 KiB scratch buffer instead of the bytes actually read, so the decoder's
"incomplete frame" signal is unreachable and a fragmented frame is decoded against bytes that never
arrived. The contract the draft asks for is not new API — it is that the fast path bound its slice
the way the line eleven rows below it already does.

**The file is byte-identical across every release since the one we vendor, and on `main`.** MEASURED,
`md5`: `rmux-client/src/attach.rs` hashes to `ccddf8572567fd0943a47433312cefc9` at 0.9.0 (cargo
registry copy), 0.9.1, 0.10.0 (both `static.crates.io` archives), **and at `main` HEAD** fetched
through `gh api repos/Helvesec/rmux/contents/crates/rmux-client/src/attach.rs?ref=main`. The buggy
expression is at **line 694** in all four, and the correctly-bounded sibling is at line 709:

```
694:                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..])
709:            decoder.push_bytes(&read_buffer[consumed..bytes_read]);
```

**Reproduced against pristine upstream 0.10.0.** MEASURED. A scratch crate depending on
`rmux-client = "=0.10.0"` straight from crates.io — no patch, no `vendor/` — running this
repository's own regressions copied verbatim from
`crates/rmux/tests/attach_fragmentation.rs`, which drive the public `drive_attach_stream` and
`attach_with_terminal` entry points over a `UnixStream` pair:

```
test direct_rmux_attach_preserves_fragmented_frame_prefixes ... FAILED
test managed_rmux_attach_preserves_fragmented_frame_prefixes ... FAILED
direct fragmented case 0: protocol error: failed to decode frame payload:
  unknown attach-stream message tag 27
```

**And the draft's one-line fix is sufficient at 0.10.0.** MEASURED. Editing only line 694 of that
otherwise-pristine 0.10.0 source to `&read_buffer[consumed..bytes_read]` and re-running the same two
tests: `test result: ok. 2 passed; 0 failed`. That is the whole patch, on the current release, with
the current regressions.

**Revise before filing — four items, three of them corrections of the draft's own text:**

1. **Affected versions.** `0.9.0 and 0.9.1` understates it. Say 0.9.0, 0.9.1, 0.10.0 and `main`,
   byte-identical, with the md5 above. The draft's `src/attach.rs:694` survives; its reference to
   line **705** is now line **709**.
2. **The quoted decoder elides its first guard.** `rmux-proto` 0.10.0 `src/attach.rs:329-331` opens
   `decode_attach_data_frame_with_limit` with `if input.first().copied() != Some(DATA_TAG) { return
   Ok(None); }`, which the draft's excerpt omits. A maintainer reading the excerpt cannot check the
   argument.
3. **The reproduction table's second row is wrong.** It predicts that the residual `\r\n` is read as
   a new frame header producing *"unknown attach-stream message tag 13"*. `13` is
   `RENDER_TAG` (`rmux-proto` 0.10.0 `src/attach.rs:18`) — a **valid** tag. The tag guard in item 2
   returns `Ok(None)`, the residue goes to the incremental decoder, and the stream desynchronises
   **silently** rather than erroring. This was already noted at `docs/archive/repo-review.md:473-475`; it is
   confirmed here against 0.10.0's constants. Replace the row with the measured case above, where the
   payload contains `ESC` (`0x1B` = 27) and the failure is loud.
4. **Offer the regression that exists.** `crates/rmux/tests/attach_fragmentation.rs` is 162 lines,
   has no dependency on pmux, and fails on unpatched 0.10.0 and passes on patched 0.10.0 — both
   MEASURED above. It is a better attachment than the prose repro.

The defect narrative and the fix are right; only the failure-mode detail and the version line are
wrong. Filing it with those three corrections costs an hour and makes the report unassailable.

### 2.2 Draft 02 — `rmux-server` drops buffered attach frames at EOF: **STILL VALID**, reproduced at 0.10.0

The claim: both attach read sites `return Ok(())` on EOF *before* `process_attach_socket_messages`
runs, so a complete frame already sitting in the decoder is discarded when the client half-closes.
The contract asked for is that a complete frame read before an orderly EOF is dispatched exactly
once, that a truncated final frame fails closed with `UnexpectedEof` without mutating the pane, and
that inter-frame ordering survives the deferral.

**Present in 0.10.0 and on `main`.** MEASURED (`md5` of `rmux-server/src/pane_io.rs` at 0.10.0 and at
`main` HEAD both `ac809b2997cf4e00b181a055f91dd73e`). The shape the draft describes, at 0.10.0 line
numbers: the burst loop's `TryAttachRead::Closed` arm is `:461-469` and ends in `return Ok(())`; the
only call that drains the decoder into the pane is at `:480`. The `select!` read arm repeats it at
`:611-620`, with its dispatch at `:628`. `try_read_socket_bytes` still appends into the decoder and
returns only a status (`src/pane_io/wire.rs:145-150`). No deferred-close machinery exists: `grep -c`
for `prepare_orderly_attach_eof` and for `attach_stream_closed` returns **0** at both 0.9.1 and
0.10.0, against 3 each in `vendor/rmux-server/src/pane_io.rs:503-525`.

**Reproduced against pristine upstream 0.10.0.** MEASURED. Into an unmodified 0.10.0 crate tree from
`static.crates.io` I appended, to upstream's own `src/pane_io/tests.rs`, only the helper
`run_preclosed_attach_input` and the single test
`complete_input_frame_is_dispatched_before_orderly_eof` copied from
`vendor/rmux-server/src/pane_io/tests.rs`. Every other symbol they need
(`create_attach_input_test_session`, `test_attach_target`, `LiveAttachInputContext::current_for_test`,
`attached_input_capture_for_test`, `RequestHandler::register_attach`) already exists upstream at
0.10.0, and `forward_attach`'s ten-argument signature is unchanged between 0.9.0 and 0.10.0. Result:

```
test pane_io::tests::complete_input_frame_is_dispatched_before_orderly_eof ... FAILED
assertion `left == right` failed: every complete frame read before EOF must mutate the pane
  left: []
 right: [121, 13]
```

The pane received nothing where `y\r` was written and the peer then half-closed — the exact failure
the draft describes, one release later, on code the maintainer publishes.

**Revise before filing — three items:**

1. **Affected versions and line numbers.** Add 0.10.0 and `main`. The draft's 0.9.1 numbers are
   accurate (`TryAttachRead::Closed` at `:451`, dispatch at `:470`, `select!` arm at `:595-604`,
   dispatch at `:612`); at 0.10.0 they are `:461`, `:480`, `:611-620`, `:628`, and `wire.rs:121-126`
   is now `wire.rs:145-150`.
2. **Replace "reproduced with `--no-default-features`" with the default cell.** MEASURED and this is
   a real obstacle for the maintainer: **pristine `rmux-server` 0.10.0 does not compile its own unit
   tests with `--no-default-features`.** `cargo check --all-targets --no-default-features` fails with
   two `E0425`s in `src/handler_attach_tests/set_titles.rs:585` and `:593`, both naming functions
   gated behind `#[cfg(feature = "web")]`. The same command **succeeds** on pristine 0.9.0. The
   reproduction above therefore had to run in the default (web-on) cell. Either mention it in the
   draft or file it separately — see §5.
3. **Keep the scope split the draft already draws.** It offers the minimal fix plus three
   regressions and holds back the wider ordering work. That is the right shape and should not grow:
   the local patch is far larger than the defect (§3).

### 2.3 Draft 03 — the `revision` contract: **STILL VALID as a question, and its implementation section is now stale**

The claim is not a bug. It is that two published sentences compare *two captures* and can be read
either as licensing an interval proof or as forbidding it, and that pmux needs to know which. The
contract asked for is one of two doc sentences: the strong one (revision advances on every observable
mutation, whether or not a capture observes it — so equal revisions prove a quiet interval) or the
weak one (revision is derived per capture and says nothing about the gap).

**The documentation the draft quotes is unchanged in the current release.** MEASURED:
`rmux-sdk/src/snapshot.rs` is byte-identical at 0.9.0, 0.9.1 and 0.10.0 (`md5`
`090bee4c6ca170154e3920f7ec728fbf`), so *"Equal revisions therefore mean 'nothing observable
changed'"* still sits at `src/snapshot.rs:44-49`. `rmux-proto/src/response/pane.rs` did change, but
the sentence did not: *"changes whenever any observable field (cells, cursor, output_sequence,
history bytes/lines, pane id) changes"* moved from `:528-531` to **`:769-771`**. The ambiguity the
draft is about is intact.

**But the draft's "What the shipped implementation actually does" section is now false in two ways,
and this is the finding that changes the filing.** MEASURED against 0.10.0's
`rmux-server/src/handler_pane/snapshot.rs` (`md5` differs from 0.9.0/0.9.1):

* `revision_for` as the draft quotes it no longer runs in production. It is now
  `#[cfg(test)]`-only, delegating to **`revision_for_at_least(pane_id, fingerprint, minimum)`**
  (`:396-411`), which raises the stored revision to a floor a caller supplies.
* The draft's load-bearing sentence — *"`revision_for` is reached only from
  `handle_pane_snapshot_inputs`, that is, only when a capture RPC is served"* — **no longer holds.**
  There are two writers into the shared registry at 0.10.0, both in
  `rmux-server/src/handler/pane_stream_capture.rs`: the snapshot RPC at **`:249`**
  (`assign_pane_snapshot_revision`, inside `materialize_typed_snapshot`) and the **surface-stream
  frame builder** at **`:156`** (`assign_pane_snapshot_revision_at_least`, inside
  `materialize_surface_frame`). Upstream's own comment on the latter says
  `PaneSurfaceSnapshot::revision` and `PaneSnapshotResponse::revision` *"are documented as one shared
  monotonic counter"*.

**This corrects a claim already in the tree.** `docs/archive/repo-review.md:471-472` states that *"all three
defects survive into it byte-identically"* at 0.10.0. That is exact for drafts 01 and 02 — MEASURED,
identical `md5` for both files — and **wrong for draft 03**, whose implementation quote is of a
function that upstream has since rewritten and demoted to test-only. The doc sentences the draft is
actually about did survive byte-identically; the code section did not.

This makes the draft's question **sharper**, not weaker. The counter is still a transition-on-
observation counter, so the weak reading still describes it; but *who else is observing* now changes
what a consumer's two equal revisions mean, and a consumer cannot see the other observers. The
mechanism the draft identified as invisible is still invisible: `compute_snapshot_fingerprint` still
hashes `output_sequence` (0.10.0 `handler_pane/snapshot.rs:360`), and `output_sequence` is still
`pub(crate)` and still not on the wire.

**pmux's own side is unchanged and still not depending on the strong reading.** The 8 → 67
measurement over 30 turns, and the decision it refuted, are recorded in `docs/current-state.md`
§9.29, which names this draft as the unblocking step. `crates/rmux/src/backend.rs:59` and `:185`
carry `revision` into `TerminalSnapshot` and `StyledScreen`, and nothing in the service decides a
quiet window from it — the 250 ms poll is still spent. So the draft is neither superseded nor
obsolete: pmux still cannot take the saving, and still cannot know whether it is allowed to.

**Revise before filing — three items:**

1. **Rewrite the implementation section against 0.10.0**, with `revision_for_at_least` and both call
   sites named. Filing a 0.9.0 code quote at a maintainer whose file has moved invites the reply
   "that is not the code" and loses the question.
2. **Update the citation line numbers**: `rmux-sdk` `src/snapshot.rs:44-49` still holds; `rmux-proto`
   `src/response/pane.rs:528-531` → `:769-771`.
3. **Add the second-observer case to the ask.** Under the strong reading, does a surface-stream
   observer's transition count toward a snapshot consumer's interval proof? Upstream has already
   written internally that these are one counter; the public docs say nothing about it.

## 3. What the vendored fork actually carries

Both crates were diffed file-by-file against the pristine published 0.9.0 archives extracted in the
local cargo registry (`~/.cargo/registry/src/index.crates.io-*/rmux-{client,server}-0.9.0`), whose
`.crate` archives MEASURED at the SHA-256 that each `PMUX-PATCH.md` publishes and that crates.io
serves: `5b9e5393…0e8d` for the server, `0229231128141add…e3f` for the client — VERIFIED-EXTERNAL
against `api/v1/crates/rmux-{server,client}/0.9.0`.

| crate | files differing from pristine 0.9.0 | documented? |
|---|---|---|
| `rmux-client` | `src/attach.rs` (exactly **one line**), plus the added `PMUX-PATCH.md` | yes |
| `rmux-server` | `src/pane_io.rs`, `src/pane_io/tests.rs`, plus the added `PMUX-PATCH.md` | yes |

**There is no undocumented patch.** MEASURED via `diff -rq` in both directions; the file lists above
are complete. Two further checks passed: the vendored server holds **597** files, which is the
published **596** plus its patch document, and the client's single-line change is exactly the
substitution its document describes —

```
-                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..])
+                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..bytes_read])
```

at `vendor/rmux-client/src/attach.rs:694`. The server patch is much larger: 422 changed lines in
`src/pane_io.rs` and 821 in `src/pane_io/tests.rs`, adding the deferred-EOF machinery at
`vendor/rmux-server/src/pane_io.rs:499-525` and `:701`, `prepare_orderly_attach_eof` at `:2017`, and
fourteen named regressions.

**Both patches are still necessary. Neither is upstream.** §2.1 and §2.2 establish that at 0.10.0 and
at `main`.

**One documentation defect, in the server's patch document, that its own gate cannot see.**
`vendor/rmux-server/PMUX-PATCH.md:115` cites
`apps/pmux-rmuxd/tests/process_blackbox.rs::real_attach_half_close_delivers_the_final_complete_frame_exactly_once`
and `:117` cites `TESTING.md`. Neither path exists: MEASURED, there is no `apps/` directory and no
root `TESTING.md`; the test really lives at `bin/pmux-rmuxd/tests/process_blackbox.rs:311` and the
lane document is `docs/testing.md` (ownership) plus
`docs/archive/testing-gate-a-census.md` (freeze-census commands). The repository restructure that moved them is recorded as DONE in
`docs/current-state.md` §13's first row. The gate at
`crates/rmux/tests/vendor_server_patch.rs:825-833` requires the document to *contain* the test's
**name** and the string `crates/rmux/tests/vendor_server_patch.rs`, so the stale **directory
prefixes** pass through it unread. This is a two-word fix and it is worth making, because that
document is the only description of the patch a future reader gets. The client's
`vendor/rmux-client/PMUX-PATCH.md` cites `crates/rmux/tests/attach_fragmentation.rs` and
`crates/rmux/tests/vendor_patch.rs`, and both resolve.

## 4. The upgrade question — sized, and not recommended for this reason

`Cargo.toml:52-58` pins `=0.9.0` for all three rmux crates and patches `rmux-client` and
`rmux-server` to `vendor/`; `Cargo.lock` locks `rmux-proto` and `rmux-sdk` to the published 0.9.0.
Moving to 0.10.0 costs, MEASURED unless marked:

1. **Porting the server patch: six conflicts.** `git merge-file` with pristine 0.9.0 as the base, the
   vendored file as ours and pristine 0.10.0 as theirs conflicts in **6 regions** of `pane_io.rs`
   (12, 9, 14, 12, 36 and 12 lines) — unsurprising, since upstream changed 360 lines of that file
   over the same range the patch changed 422. `src/pane_io/tests.rs` merges **cleanly** (0
   conflicts), but upstream changed 359 lines there, so a clean textual merge is not evidence the
   fourteen regressions still test what they name.
2. **Re-deriving the whole vendor identity.** `crates/rmux/tests/vendor_server_patch.rs` (1,133
   lines) and `crates/rmux/tests/vendor_patch.rs` (330 lines) pin the archive SHA-256, the published
   VCS SHA-1, the published file count (596 → **768** at 0.10.0), the canonical tree hash, and the
   patched/upstream hashes of each changed file. Every one of those constants, in the tests and in
   both `PMUX-PATCH.md` files, has to be recomputed and re-checked.
3. **A new, unbudgeted second patch.** `docs/archive/testing-gate-a-census.md:476-477` runs
   `cargo check --all-targets --no-default-features` and `docs/archive/testing-gate-a-census.md:487-490` runs
   `cargo test --lib --no-default-features pane_io::tests::` against the vendored server. At 0.10.0
   the first of those **fails on upstream's own code** with two `E0425`s in
   `src/handler_attach_tests/set_titles.rs`. Adopting 0.10.0 therefore means either patching an
   upstream *test* file — enlarging the fork beyond the one bounded repair its document describes —
   or changing the feature set the lane validates, which would stop validating the product cell.
4. **A new dependency.** `rmux-server` 0.10.0 adds `bincode`.
5. **Wire incompatibility, contained.** The release notes state 0.10.0 is not wire-compatible with
   0.9.x daemons. pmux ships both ends — `bin/pmux-rmuxd/src/main.rs:63` embeds
   `rmux_server::ServerDaemon` behind a private socket — so this is contained provided both vendored
   crates move together and no 0.9.x sidecar survives an upgrade in a user's runtime directory (that
   last clause is UNVERIFIED; nobody checked the on-disk sidecar lifecycle this session).
6. **The API pmux uses is intact.** All of `PaneSnapshot`, `PaneCell`, `PaneGlyph`, `PaneCursor`,
   `PaneColor`, `PaneAttributes`, `RmuxError` (with `WaitTimeout`), `AttachTransition`, `connect`,
   `attach_terminal`, `attach_with_terminal`, `drive_attach_stream`, `ServerHandle` and
   `ServerDaemon` are present at 0.10.0. `Request`/`Response` becoming `non_exhaustive` is a risk
   only if pmux matches them exhaustively; it does not appear to (UNVERIFIED — not compiled).
7. **The client side of the upgrade is nearly free**, and §2.1 measured it: `attach.rs` is
   byte-identical, so the one-line patch applies unchanged and the two fragmentation regressions pass
   against otherwise-pristine 0.10.0.

**Benefit: zero patches retired.** Both defects survive into 0.10.0 and `main`. The reasons to
upgrade are upstream's new features (`recover_output`, `surface_stream`, the daemon-side surface
projection) and staying near a weekly-releasing dependency — neither of which is what these drafts
are about. **Recommendation: do not upgrade now.** If it is taken up later, item 3 is the one that
does not appear in any existing estimate and should be settled first.

## 5. Recommendation

| draft | verdict | action |
|---|---|---|
| 01 client unbounded slice | **STILL VALID** at 0.9.0/0.9.1/0.10.0/`main`; reproduced and fixed at 0.10.0 this session | **File, after the four revisions in §2.1.** Attach `attach_fragmentation.rs`; offer the one-line PR. |
| 02 server EOF drops buffered frames | **STILL VALID** at 0.9.0/0.9.1/0.10.0/`main`; reproduced at 0.10.0 this session | **File, after the three revisions in §2.2.** Keep the minimal scope; offer the three regressions. |
| 03 `revision` contract | **STILL VALID as a question**, but its implementation section is stale at 0.10.0 | **Revise, then file.** Rewrite against `revision_for_at_least` and the two call sites; add the second-observer question. |

Two additions worth making at the same time:

* **A fourth report, new this session:** `rmux-server` 0.10.0 does not compile its unit tests with
  `--no-default-features` (two `E0425`s in `src/handler_attach_tests/set_titles.rs:585` and `:593`,
  both reaching for `#[cfg(feature = "web")]` functions). 0.9.0 builds that cell clean. It is a
  one-line `cfg` on a test module, it is trivially reproducible with two commands, and it blocks any
  downstream that ships the server without `web` — which is exactly what pmux does.
* **A two-word repair in `vendor/rmux-server/PMUX-PATCH.md`** (§3): `apps/` → `bin/` and `TESTING.md`
  → `docs/testing.md`. Local, not upstream.

## 6. What this document does not establish

* Nothing here was run on Linux, or on the pinned 1.88.0 toolchain — all scratch builds used the
  host default `rustc 1.97.1`. Both defects are name-resolution and slice-bounds behaviour, not
  toolchain-sensitive, but the claim is only made where it was measured.
* The port of the server patch to 0.10.0 was **not attempted** beyond counting conflicts. Whether the
  fourteen regressions still hold their meaning after that port is unknown.
* Whether upstream *wants* these reports, and how it responds, is obviously not established. The
  activity picture in §1 says only that someone is there.
* The wider local ordering work described in `vendor/rmux-server/PMUX-PATCH.md` beyond the minimal
  EOF repair was not re-derived or re-justified here; §3 reports its size, not its necessity.

## 7. Refresh, 2026-09-01

Re-checked on macOS 15 / arm64 with the same three labels. **Nothing moved upstream in the twenty
days since §1: the answer is still file all four, do not bump.**

| fact | 2026-08-12 | 2026-09-01 |
|---|---|---|
| latest release (all four crates, none yanked) | 0.10.0, 2026-08-04 | 0.10.0, unchanged |
| `main` HEAD / `pushed_at` | `1f4571e7` / 2026-08-09 | unchanged |
| commits ahead of v0.10.0 | 25 | 25 |
| open issues touching attach framing, EOF, or `revision` | none | none (new: #217, #218, #219; PR #220) |
| filed by anyone | no | no |

Per draft, VERIFIED-EXTERNAL against the 0.10.0 tarballs (sha256 equal to the crates.io checksums)
and `main` through the contents API:

* **01** `rmux-client/src/attach.rs` still md5 `ccddf8572567fd0943a47433312cefc9` at 0.10.0 and
  `main`; `:694` unbounded, `:709` bounded. MEASURED: the draft's standalone test re-run against
  `rmux-client = "=0.10.0"` from crates.io fails with `unknown attach-stream message tag 121` and
  nine stale `A`s delivered as the second frame; the one-line fix makes it pass.
* **02** `rmux-server/src/pane_io.rs` still md5 `ac809b2997cf4e00b181a055f91dd73e`; the `Closed`
  arms still `return Ok(())` at `:461-469` and `:611-620` ahead of the dispatch at `:480` / `:628`.
  Source read against a byte-identical file; the §2.2 execution stands.
* **03** `rmux-sdk/src/snapshot.rs` still md5 `090bee4c6ca170154e3920f7ec728fbf`; every citation in
  the rewritten draft resolves at 0.10.0 and `main`.
* **04** (the `--no-default-features` test build break §5 names) MEASURED again: two `E0425`s at
  `src/handler_attach_tests/set_titles.rs:585` and `:593` on pristine 0.10.0 and `main`; 0.9.0 and
  0.9.1 build the cell clean (the file does not exist there). A one-line
  `#[cfg(feature = "web")]` on `the_web_and_snapshot_renders_carry_no_title` fixes it with no
  new warnings; it is filed as an issue that carries that fix.

The drafts in `docs/upstream-issues/` carry all §2 revisions, were re-read line by line against
today's release, and were then reviewed once more for a maintainer's eyes (re-verification
paragraphs cut, 03 retitled as the contract question it is, 04 reshaped as an issue carrying
its one-line fix). The `PMUX-PATCH.md` path repair §5 lists is done in this refresh.

Upgrade sizing is unchanged: six `pane_io.rs` conflicts, the tests file merges clean, every symbol
pmux uses is present at 0.10.0, `non_exhaustive` is not a risk (pmux matches on no `Request` /
`Response`), `bincode` is a new dependency, and the wire move 5 -> 8 is contained behind the private
socket. Benefit: zero patches retired.

### Filed, 2026-09-02

| text | upstream issue |
|---|---|
| 01 client unbounded slice | https://github.com/Helvesec/rmux/issues/221 |
| 02 server drops buffered frames at EOF | https://github.com/Helvesec/rmux/issues/222 |
| 03 `revision` contract question | https://github.com/Helvesec/rmux/issues/223 |
| 04 `--no-default-features` test build break | https://github.com/Helvesec/rmux/issues/224 |

Each file under `docs/upstream-issues/` is the title and body as submitted, with the issue URL on
its second line. The vendored patches stay until upstream ships a fix and the vendor gates are
re-derived against that release.
