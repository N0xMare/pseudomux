<!-- Generated 2026-07-28 by an adversarial audit of the validation
tooling: 6 dimensions, 52 findings raised, 19 surviving 3-lens refutation,
collapsed here to 8 distinct defects. Every line reference was verified by
reading. APPLIED 2026-07-28 -- see "Status" below before treating any of it as
outstanding work. -->

# Instrumentation fix plan (2026-07-28)

## Status: all 8 applied, 3 items deliberately deferred

**Every defect in sections 1 and 2 was fixed on 2026-07-28.** This file was
written as a work list and is retained as the reasoning behind those changes --
do not read sections 1-3 as outstanding work. Line numbers in them predate the
fixes and will not resolve; the prose explains *why*, which is the part worth
keeping.

Section 4 is the exception and is still live: it states what remains unverified
*after* every fix, and none of it was addressed by applying them.

Three things were found and deliberately NOT fixed. They are recorded here
because a finding that lives only in a commit message is a finding nobody will
find:

- **`HOOK_SELF_DEADLINE` has no drift fence.** `bin/pmux-hook/tests/process_blackbox.rs`
  hand-copies the 5 s `HOOK_CLIENT_IO_TIMEOUT` from `bin/pmux-hook/src/main.rs`,
  and nothing ties them together. The accepted band is therefore `[4 s, 120 s]`:
  a 5 s -> 60 s product regression passes 6/6. A 60 s hook would be `SIGKILL`ed
  by Claude Code long before returning -- the signalled-death mode two
  assertions in that test were added to catch -- and the harness never applies
  that pressure. **The fix is a compile-time fence in the same crate, NOT a
  wall-clock upper bound**; reintroducing one would be the original C9 mistake.
- **`matched_variant` is computed and never rendered.** `verify_calibration.py`
  records which byte variant reproduced a hash (`exact`, `trailing_newline`,
  `nfc`, ...) but the text report omits it, so a byte-exact match and one that
  only survived NFC normalization read identically. Same silent-detection family
  as section 2.1; affects no published number, which is why it was deferred.
- **`source_digest.py:1309` aborts a revision capture if the git control
  identity moves *during* a single capture.** Section 1.1 removed the
  before/after comparison's sensitivity to that, but not this narrower internal
  window. Same root cause, smaller blast radius.

Seven defects were found on 2026-07-28 and **four were in this tooling, not in
pmux**. That is the motivating fact for this audit: a product defect makes pmux
fail visibly, while an instrument defect either publishes a wrong number someone
believes, blames pmux for something it did not do, or hides a real problem. Both
verify_calibration defects would have printed "pmux dropped a byte" on a clean
run.

The recurring shape is **silent detection**: the tool notices the problem and
does not put it in the default output. Section 2.1 is that pattern in its purest
form -- eight `notes` write sites, zero render sites -- and it is the direct
successor to the grade-misattribution defect, which the tool also detected and
also did not show anyone.

Read section 4 before quoting anything from sections 1-3.

# ORDERED FIX PLAN — pmux measurement instrumentation

All line references verified by reading. Nineteen reported findings collapse to **eight distinct defects**; several are the same defect reported at two call sites or from two angles, and I have merged them so the parent applies each fix once.

---

## 1. FIX NOW

*Criterion: corrupts evidence already published, or burns one of the remaining irreplaceable ordinals on the very next `phase0.py probe --live`.* How many remain is not written here — it was, as "~53", against a ledger that had already reached ordinal 81, which ranked this whole plan against a denominator wrong by 3.5x. Ask the file: `python3 tools/phase0/phase0.py budget --ledger evidence/model-attempt-ledger.ndjson`.

### 1.1 The source-identity fence turns a paid, successful Claude turn into `status="failed"` and aborts the campaign
*(merges findings 5, 14, 17 — one defect, two call sites)*

**Files:** `tools/phase0/phase0_lib.py:5123-5127` (the comparison), `:5874` (the post-command call site), `:5697`, `:4913`.

Verified chain: `_verify_candidate_unchanged` does `current_source != self.source_identity` on the **whole** dict. `observe_source_identity` (`phase0_lib.py:1059-1068`) builds that dict with `"revision_identity": revision_identity`, and `source_digest.py:1335` puts `"repository_control": repository_control` inside it, whose nodes carry `"ctime_ns": str(metadata.st_ctime_ns)` (`source_digest.py:906`). Any `git status` from an editor rewrites `.git/index` and moves `ctime_ns`. The call at `:5874` sits **inside** the `try` at `:5837`; the `except` at `:5882` sets `status="failed"`, nulls `result_binding`, discards `usage`, and — because `:5881` never runs — throws away `drain_calibrations.append(...)` for a turn that succeeded. `:5935` then raises "pmux did not produce an authoritative successful public result", which `phase0.py` surfaces as a pmux failure.

There is a **second, wider window** the reports did not name: `source_digest.py:1309` already raises `SourceIdentityError("Git repository control identity changed during capture")` if the control snapshot moves *during* a single revision capture. So the same external poll can also fail the capture itself, not just the before/after compare.

**Change.** In `_verify_candidate_unchanged`, compare a *causal projection* of the identity — `digest`, `file_count`, `algorithm_sha256`, `implementation`, and from `revision_identity` only `head`, `head_ref`, `branch`, `detached`, `status_porcelain_v1_z_sha256`, `tracked_binary_diff_sha256`, `source_digest_implementation` — and record the full observation (including `repository_control`) in the artifact for forensics, downgrading a `repository_control`-only delta to a note on the observation record. Separately, hoist the post-command `_verify_candidate_unchanged` at `:5781` **out of** the `try` that owns the pmux verdict, so no harness-environment check can rewrite a completed turn's `status`, null its `public_result_binding`, or drop its drain sample.

**Tests that must accompany it** (`tools/phase0/tests/test_phase0.py`): (a) mutate only `identity["revision_identity"]["repository_control"]["files"]["index"]["ctime_ns"]` between bind and verify — assert no `EvidenceError`, and assert the delta appears as a recorded note; (b) mutate `identity["digest"]` — assert `EvidenceError` still raised; (c) an attempt whose pmux command succeeds but whose post-command verification raises — assert `status == "pmux_exit_zero"`, `public_result_binding` non-null, and `len(runner.drain_calibrations) == 1`.

### 1.2 Late-row classification is `gap <= 0`, not the one-poll-interval band the protocol defines
*(merges findings 3, 8, 13, 15 — one rule, two independent implementations)*

**Files:** `tools/phase0/verify_calibration.py:527` (`summarize_gaps`) and `tools/phase0/phase0_lib.py:4331` (`summarize_drain_calibration`). Both contain the identical `sum(1 for gap in gaps if gap <= 0)`.

Verified: `crates/protocol/src/v1.rs:1313-1319` states the rule literally — "the difference straddles zero by a few milliseconds at most: negative by the parse-and-analyze interval, **positive** by the interval between the confirming poll's stability measurement (a monotonic duration) and the completion timestamp read (a wall clock). Read a difference within one actor poll interval of zero as 'no late rows'." The interval is 20 ms (`crates/service/src/v1/actor.rs:80`). The aggravating detail is real: `phase0_lib.py:4352-4354` **prints the correct rule** ("A gap within one actor poll interval of zero is measurement noise, not a late row") inside the very object whose `late_row_attempts` count contradicts it. One +1 ms sample flips `late_row_attempts` to 1, which suppresses `ABSENCE_OF_EVIDENCE_BANNER` (gated on `== 0` at `verify_calibration.py:727` and the per-grade one at `:752`), and makes `headroom_ms = configured_drain - max(max,0)` read as 1999/2000 ms of *measured* headroom against a clock artifact. This is the "wrong number someone will believe" case exactly: it converts "we measured the absence of late rows" into "we measured a late row of 1 ms", inviting a cut of `transcript_drain_ms` to near zero.

**Change.** Add an explicit band parameter (default 20, sourced from `SessionActorConfig::default().poll_interval` and echoed into the record as `actor_poll_interval_ms`; a CLI `--actor-poll-interval-ms` on the verifier). Classify `gap <= band` as no-late-row in **both** functions. Add a third bucket `within_noise_band_attempts` for gaps in `(0, band]` so they stay visible rather than being either hidden or promoted. Compute `headroom_ms` against `max(maximum, 0)` **only when `maximum > band`**; when the maximum is inside the band, publish `headroom_ms: null` with the absence-of-evidence interpretation. Print the band value in the OVERALL line and in the banner text so the classification rule travels with the number.

**Tests:** in `test_verify_calibration.py`, gaps `(-3, -2, +1)` must print the ABSENCE OF EVIDENCE banner, `within-noise-band=1`, and **no** "headroom vs measured worst case"; a gap of +600 ms must still count as a late row (the existing `test_a_measured_late_row_suppresses_the_absence_of_evidence_banner` at `:508` uses 600 ms and must keep passing unchanged — verify it does). Mirror both in `test_phase0.py` against `summarize_drain_calibration`. Add one test asserting the two implementations agree on the same sample vector, otherwise the "deliberately second, independent implementation" continues to be a copy of the same bug.

### 1.3 A grade that requested three hashes and delivered one is tallied `match`
*(finding 2)*

**Files:** `tools/phase0/verify_calibration.py:120` (`load_prompt_suite`, `expects_hash = "SHA256" in text`), `:508` (`hash_overall`), `:283` (`verify_hash_lines`).

Verified: `verify_hash_lines` iterates only over the hash lines that were **present**; there is no expected-count or expected-label check anywhere. `prompts/06-poem-hash-triple-transform.txt:8-9` demands `SHA256(poem)`, `SHA256(reversed)`, `SHA256(upper)`; `05-poem-hash-reverse-transform.txt:8` demands two. A reply carrying only a correct `SHA256(poem)` yields `hash_overall = "match"` and exit 0. The two transform proofs are the entire reason grades 05 and 06 exist and are the ones a model is most likely to botch; they are not counted as absent, they are simply not counted.

**Change.** In `load_prompt_suite`, parse the `SHA256(<label>)` and bare `SHA256:` tokens out of the prompt text into `expected_labels: frozenset[str]` alongside `expects_hash`. In `analyze_attempt`, when the reported label set is a strict subset of `expected_labels`, set `hash_overall = "partial"` and record the missing labels. Render `partial` in the tally and legend, list the missing labels per attempt, and include `partial` in the nonzero-exit condition at `:818`.

**Tests:** a grade-06 attempt with only `SHA256(poem)` correct → `hash_overall == "partial"`, the text output names `reversed` and `upper` as missing, exit code 1. A grade-06 attempt with all three correct → `match`, exit 0. A grade-03 attempt (single bare `SHA256:`) → `match`, exit 0 (no regression on single-hash grades).

### 1.4 `expects_hash` is read off the entry that did *not* produce the grade, so a real proof-of-work failure is filed as "this prompt did not ask for a hash"
*(merges findings 7, 10, 18)*

**File:** `tools/phase0/verify_calibration.py:503-504`.

Verified: `expects_hash = bool(suite_entry and suite_entry["expects_hash"])`, but `suite_entry` is only set when the reservation's prompt sha256 is found in `--prompts-dir` (`:432`). When the grade came from `index_entry` instead (`:447`), `suite_entry` is `None`, so `expects_hash` is `False` and a reply with **no hash at all** is tallied `not_applicable`. The rendered legend at `:685` then asserts "not_applicable = this grade's prompt did not ask for a hash" — a positive claim about a prompt the tool has just admitted it could not identify. The attempt also disappears from `hash_missing_when_expected`. Once the prompts directory drifts by one edit, *every* missing hash in the tree becomes `not_applicable` at once.

**Change.** `entry = suite_entry or index_entry; expects_hash = bool(entry and entry["expects_hash"])`. When neither entry exists, use a distinct `"hash_expectation_unknown"` value, add it to the legend, and list those attempts alongside `hash_missing_when_expected` rather than folding them into the by-design bucket. Add `hash_missing_when_expected` (and the new bucket) to the nonzero-exit condition at `:818` — currently a correctly detected missing hash still exits 0.

**Tests:** attempt with `prompt_sha256_override="0"*64`, graded by index onto a prompt containing "SHA256", reply with no hash → `hash_overall == "missing"`, listed under "hash expected but absent", exit 1. Existing `test_hash_expected_but_absent_is_distinguished_from_not_applicable` (`:467`) must keep passing. Add a case where neither entry resolves → `hash_expectation_unknown`, not `not_applicable`.

---

## 2. FIX BEFORE THE NEXT LIVE CAMPAIGN

*Ordered. Each must precede live use for a stated reason.*

### 2.1 Render the eight `notes` sites and `grade_source` in the default text report
*(merges findings 1, 6, 9, 12 — the meta-pattern, in its purest form)*

**File:** `tools/phase0/verify_calibration.py` — `grep -c "notes\"\].append"` = 8 append sites (`:421`, `:441`, `:452`, `:473`, `:487`, `:496`, `:498`, and the outcome-missing one at `:465`); `render_report` (`:654-758`) references `notes` **zero** times, `grade_source` **zero** times, `gap_uncomputable_reason` zero times, and `attempts_without_computable_gap` zero times. `build_report` does not even copy `notes` or `grade_source` into the report dict, so `--json` reaches them only via the separate `{**report, "attempts": records}` splice at `:800`.

**Why before live use:** this is defect 3 reoccurring one layer up. The tool already detects grade relabelling, prompt-directory drift, missing artifacts, a missing blank separator, and "attempt did not succeed: `<error>`" — and a human reading the default output sees none of it. A grade established by content hash and a grade guessed from an argv position render as the identical string. The header line also does not partition: `discovered` = 9, `(successful: 5, incomplete: 0, fatal errors: 0)`, with `failed` attempts in none of the three categories and `unreadable` grades explicitly filtered out of `extra_grades` at `:550`, so a dir with no `reservation.json` appears in *no* row at all. Burnt ordinals literally vanish from the report.

**Change.** Carry `notes` and `grade_source` into `build_report`'s output. In `render_report`: (a) make the header partition — `discovered == successful + failed + incomplete + unreadable + fatal`, with `attempts_failed` carrying a `Counter` of `outcome["error"]`; (b) add a mandatory "ATTEMPTS THIS REPORT COULD NOT FULLY GRADE" block keyed by attempt_id/grade listing every non-empty notes list; (c) append `grade_source` to each by-grade row, e.g. `01-a: attempts=2 [1 graded by index, not content]`, and banner the by-grade heading whenever any member has `grade_source != "prompt_sha256"`.

**Test:** change `test_prompt_content_drift_is_flagged_without_crashing` (`:604-623`) to assert against the **text** output, not `--json`. Its own comment says "the report must say so rather than present the label as if it were established" — as written, the test asserts the note exists in JSON and therefore **defends the silence**. Same for `test_grade_comes_from_content_not_argv_position` (`:625`).

### 2.2 `phase0.py` never prints the campaign's error string
*(merges findings 16, 19)*

**File:** `tools/phase0/phase0.py:364-386`. `CampaignRunner.run()` catches everything into `error = self._redact(str(caught))` (`phase0_lib.py:4914-4915`) and returns it in the summary; the campaign never re-raises. `_run_probe` re-projects nine hand-listed keys and `error` is not among them, nor is the per-attempt list.

**Why before live use:** after a run that just spent irreplaceable ordinals, the operator's entire feedback is `"status": "failed"`. Diagnosing whether pmux failed or whether an instrument-side fence fired (1.1 above, or "process/source authority changed across finite command") requires opening `campaign.json` inside a 0600 tree. The natural inference from a bare failed status is that pmux failed — which is the "falsely blames pmux" category. This must land *with* 1.1, because 1.1's whole point is that the fence was misfiring and you need to be able to see when it does.

**Change.** Add `"error": result["error"]` and a compact per-attempt projection (`ordinal`, `attempt_id`, `status`, `error`) to the printed object, and echo the error to **stderr** when `status != "acquired"` so it survives piping stdout to `jq`.

**Test:** a runner whose scenario raises → probe stdout JSON contains the error string and stderr is non-empty.

### 2.3 `drain_deadline <= deadline` is a tautology, so every post-exit expiry is labelled `drain_timeout`
*(finding 4)*

**File:** `tools/evidence_common/bounded_process.py:2519` and `:2523`; assignment at `:2269`.

Verified: `drain_deadline` is only ever assigned `min(deadline, time.monotonic() + drain_timeout_seconds)`, so `drain_deadline <= deadline` is **true by construction**. The ternary reduces to "`drain_timeout` if the leader has been reaped, else `timeout`" — the `else` arm is unreachable once the leader exits. `phase0_lib.py:4066` passes `drain_timeout_seconds=min(5, timeout_seconds)`, so the readiness `pmux ping` at `:5299` runs with `timeout_seconds=5, drain=5` and `drain_deadline == deadline` identically. Downstream, `phase0_lib.py:4124` computes `timed_out = (failure.reason == "timeout")` → **False**, so a command that consumed its entire 5 s envelope is published with `timed_out: false`, `elapsed_ms ≈ 5000`, and a reason asserting a 5 s drain bound was exceeded after only ~4.8 s of draining. The managed twin gets this right: `managed_process.py:1239` tests `now >= self._deadline` *before* the drain check at `:1272`.

**Why before live use:** any live campaign that trips the readiness ping publishes a receipt whose `timed_out` field is affirmatively wrong and whose failure reason names the wrong bound. It is not ordinal-burning but it mislabels the one artifact used to decide whether pmux hung.

**Change.** Keep the unclamped expiry separately (`raw_drain_deadline = time.monotonic() + drain_timeout_seconds` at `:2269`) and select `"drain_timeout"` only when `raw_drain_deadline <= deadline` **and** `now >= raw_drain_deadline`; otherwise `"timeout"`. Or mirror the managed ordering and test `now >= deadline` first.

**Test** (`tools/evidence_common/tests/test_bounded_process.py`): leader exits promptly, a descendant holds stdout, `timeout_seconds == drain_timeout_seconds` → `failure.reason == "timeout"`, and `phase0_lib.run_command` reports `timed_out == True`. Plus the converse: `timeout_seconds=60, drain=1`, leader exits at t=0.2 → `"drain_timeout"` at ~t=1.2.

### 2.4 A run where no gap is computable exits 0 and never says why
*(finding 11)*

**File:** `tools/phase0/verify_calibration.py:713`; `gap_uncomputable_reason` set at `:480`; `attempts_without_computable_gap` computed per grade at `:560`. Neither is rendered anywhere, and `main` (`:818`) returns 0.

The module docstring (`:57-60`) promises that if `LATE_ARRIVAL_FIELD` is renamed upstream, "every gap in this report becomes 'uncomputable' with that reason spelled out, which is a loud, safe failure rather than a silently wrong number." It is neither loud nor spelled out: the output is the single line "no attempt produced a computable gap; nothing to calibrate from." and exit 0. The partial case is worse — a grade with 6 successful attempts of which 5 lack the field prints `attempts=6 count=1` and the reader must guess where the other five went.

**Why before live use:** the field-rename tripwire is the tool's stated defence against publishing a silently wrong drain number. It does not currently work, and any script gating on the exit status sees success.

**Change.** Aggregate `Counter(gap_uncomputable_reason)` into the report; print it under the gap section both overall and per grade alongside `attempts_without_computable_gap`; return nonzero when `overall_gap_distribution is None` or when any *successful* attempt lacks the field.

**Test:** a tree whose only successful attempt publishes `terminal_candidate_at_ms` but not `last_transcript_activity_at_ms` → text output names `last_transcript_activity_at_ms_not_published`, exit code nonzero.

---

## 3. RECORD ONLY

- **`p95` is arithmetically identical to `max` for every sample size this campaign can reach.** `_nearest_rank(samples, 95)` = `-(-95*n//100) - 1`; for n=19 that is index 18 = last element, for n=20 it is 18 ≠ 19. So the true boundary is **n ≤ 19**, not n ≤ 20 as originally stated. Per-grade counts will be ≤ 3, so `p95`, `max` and often `median`/`min` are the same element printed as four independent statistics. Real but presentation-only; no number is wrong, only its apparent independence. **Record** as a comment above `nearest_rank` in `verify_calibration.py:~200` and `_nearest_rank` in `phase0_lib.py:4298`, stating the n≤19 degeneracy, and suppress `p95` from the rendered line when `count <= 19`.
- **`drain_calibration.attempts_considered` counts only successful attempts**, so the campaign summary publishes a distribution without its true denominator. Fold into the 2.1 partition work if convenient; otherwise **record** in `tools/phase0/README.md` under the drain-calibration section.
- **The permission-prompt-mid-turn case** (`crates/service/src/v1/actor.rs:2596-2601` requiring `ready_prompt && quiet && drain.satisfies(...)`, predicate at `crates/service/src/v1/backend.rs:196`) is a *product* gate, not instrumentation — but the instrument's exposure is that it polls to the 600 s deadline and burns an ordinal. After fix 2.1/2.2 the operator will at least *see* "turn deadline exceeded after 600s". **Record** in `tools/phase0/README.md` as a known ordinal-burning mode with its signature error string, so a reader can recognise it without re-running.
- **`source_digest.py:1309`** aborts a revision capture if `_repository_control_snapshot` moves *during* capture. Fix 1.1 does not address this narrower internal window. **Record** as a follow-up next to the 1.1 change comment.

---

## 4. THE HONEST ASSESSMENT

After all of the above, here is exactly what this instrumentation would be, and what would still be unverified.

**What it would be:** a verifier whose *arithmetic* on the samples it receives has been checked against the protocol's own stated rules, and whose default text output no longer withholds facts it has already computed. That is a real improvement over the current state and it is all it is.

**What would still be unverified:**

1. **The band constant would be asserted, not measured.** Fix 1.2 hard-codes 20 ms because `actor.rs:80` says 20 ms. Nothing in this tooling verifies that the running daemon actually used that value, and `SessionActorConfig` is overridable. If a campaign runs with a different poll interval, the band is wrong in a direction that silently reclassifies samples. The band must be read from the record's `configured.actor_poll_interval_ms` and the tool must fail loudly if it is absent — and even then, that field is self-reported by the product being audited.

2. **The whole late-arrival distribution remains an absence measurement, and no fix changes that.** Across every prompt grade, the campaign has never observed a genuinely late transcript row. `headroom_ms` will keep being the configured drain minus zero. The banner exists precisely because there is no measured worst case, and after these fixes there will still be no measured worst case — only a correctly-labelled absence of one. Nothing here justifies any reduction of `transcript_drain_ms`. If a defensible drain number is the goal, the missing work is a prompt that *provokes* a late row, not better statistics on a distribution that is empty by construction.

3. **The "second, independent implementation" is not independent.** `verify_calibration.py:527` and `phase0_lib.py:4331` contain the same expression; the second implementation reproduced the first one's bug rather than catching it. Fix 1.2 adds a cross-check test, but the two functions still share an author, a mental model, and a reading of `v1.rs`. A shared misreading of the protocol is invisible to both. The only real independence would be a third check against the raw Claude JSONL, which does not exist.

4. **The hash check verifies presence and arithmetic, never provenance.** Even with the `partial` state, `verify_hash_lines` recomputes a digest over the poem text *pmux itself captured*. If pmux dropped or reordered bytes identically in both the body and whatever the model hashed, the check passes. It proves internal consistency of one artifact, not fidelity to what Claude emitted. The `.concat()`-vs-`"\n".join` defect (2026-07-28 #2) was exactly this class and was caught only by reading `engine.rs:843`, not by any test.

5. **~~Nothing verifies the ledger accounting end to end.~~** Records and ordinals in `evidence/model-attempt-ledger.ndjson` do not correspond one-to-one — ordinals 1-4 predate the file and four more were reserved in a discarded copy — so "consumed" is a derived, not a counted, quantity, and no tool derived it. `phase0_lib.summarize_attempt_ledger` now does: it refuses a ledger whose ordinals are not contiguous, refuses a record that spells its ordinal in none of `ORDINAL_SPELLINGS`, and reports `consumed = last_ordinal + detached` against the ceiling the records were actually reserved against. `phase0.py budget` prints it and Gate A checks that `evidence/README.md` states no figure of its own.

6. **The fixes in section 1 are themselves untested against a live run.** Every finding above was verified by reading and by reasoning about the code paths, not by executing a campaign. Fix 1.1 in particular changes *what counts as the frozen candidate*, which is a provenance claim. Loosening it is correct for the ctime case and cannot be proven safe for cases nobody has enumerated — a genuine mid-campaign source mutation that touches only `repository_control` would now pass. That trade (never falsely failing a paid turn, at the cost of a narrower fence) is the right one given the budget, but it is a trade, and it should be recorded as one rather than presented as a strict improvement.