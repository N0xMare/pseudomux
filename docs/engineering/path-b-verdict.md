# The Path B verdict

**Date:** 2026-08-10, **last measured 2026-08-12 — §12, which is the newest section and the one to
read first**. **Head reviewed:** `48aee00`. **Head this was written at:** `28bd6b2`.
**Claude Code installed when this was written:** 2.1.226 — **it is 2.1.227 now**, and §8 is what
that costs.

This is the final verdict on Path B against the owner's five criteria, written after triaging a
29-finding review round produced by five parallel read-only reviewers. Every finding this document
calls CONFIRMED was reproduced here, by command, at `48aee00` or `28bd6b2`. Five are CORRECTED —
the defect is real and the reviewer's account of it is wrong in a way that changes what to do. One
suggested fix is REFUTED outright by a measurement already in the tree, and applying it would have
refused every wrapping prompt Path B serves.

**Do not read a verdict out of this document.** Since 2026-08-11 there is a script that measures the
five criteria — `scripts/path-b-done.sh` — and §8 records what it said when it was run end to end.
The annotations after the em dashes in §1 are dated records of what was true when each was written;
the script never reads them, and neither should you.

---

## 0. The one finding no reviewer made

`gate_a/rust_clippy` was **RED at `48aee00`**, the head this review round started from.

```
$ cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
error: this manual char comparison can be written more succinctly
   --> crates/service/tests/path_b_doc_citations.rs:774:72
    |
774 |         let closes = rest.starts_with('`') || rest.starts_with(|c: char| c == '-');
    = note: `-D clippy::manual-pattern-char-comparison` implied by `-D warnings`
error: could not compile `pseudomux-service` (test "path_b_doc_citations")
```

The line is `48aee00`'s own — `git show 684ff46:...` does not contain it and
`git show 48aee00:...` has it at line 706 — so **Gate A at `48aee00` was 60/62, not the 61/62 this
review round was briefed with**. `-D warnings` is not optional decoration: it is the exact argv of
the `gate_a/rust_clippy` cell in `tools/gate-a-candidate/phase-manifest.json`. A `cargo clippy` run
without it reports the same thing as a warning and exits 0, which is what "fmt/clippy clean" in the
previous session's report was measuring.

All five reviewers declared, in their own words, that they did not run Gate A. Between them they
produced 29 findings and 5 coverage statements and none of them ran the one command that was
already failing. **The lesson is not about clippy.** A review round that reads and probes but never
runs the gate cannot tell you whether the gate is green, and "Gate A 61/62" survived two commits and
five reviewers as an inherited sentence rather than a measurement.

Fixed in `28bd6b2` (one character class to one char).

---

## 1. The five criteria

**These five headings are now the input to a script, and the verdicts written into them are not.**
`scripts/path-b-done.sh` reads the ordinal and the title of every `###` heading in this section,
binds each to a function that measures it, and refuses — before measuring anything — if the set it
implements is not the set this section publishes, in either direction. So a sixth criterion added
here is a refusal rather than a criterion nobody checks, and the count word in the heading above is
checked against the number of headings under it. What each heading says after its em dash is a dated
record of what was true when the section under it was written; the script never reads it, because
reading a verdict out of a document is the thing it exists to stop doing. Run it, and take its
answer over the annotations below.

### 1. No known unfixed defect in the Path B path — **MET, measured 2026-08-12: 0 OPEN rows, 0 undispositioned survivors, 0 files drifted — §10.2**

Four confirmed defects in the Path B path were unfixed at `28bd6b2`. The first is the one that
matters. **All four are closed as of 2026-08-10**, each with the mutants that prove its tests can
fail; the paragraphs below are kept as written and annotated, so what was claimed and what was done
can be read against each other. §6 records what those four fixes did NOT close.

**(a) `rendered_prompt_head_is_proven` accepts any non-empty prefix, so a truncated composer is
entered.** `crates/service/src/driver_io.rs:1019`. Reproduced here, in-tree, with a probe test built
from the module's own fixtures and then removed:

```
PROBE RESULT: gate accepted head "W" for prompt "What is 2 plus 2?" = true;
              submit = Ok("Ok"); (pastes, enters) = (1, 1)
```

`composer_head_proof`'s `prompt.starts_with(head)` has no lower bound, so one delivered character of
a seventeen-character prompt satisfies the clause the gate is *named* for, Enter goes in, and the
post-Enter equality then refuses the turn with `UnexpectedTypedPrompt` and destroys the pooled
instance. This is the availability failure mode `48aee00` spent a commit removing four instances of,
reachable again through the last check before the irreversible write.

**The reviewer's suggested fix is refuted by a measurement already in this tree, and must not be
applied.** It proposed: if the head is shorter than the available columns the composer did not wrap,
so require equality with the first line. The tree's own MEASURED wrapping render
(`crates/service/src/driver_io.rs`, `the_measured_2_1_226_composer_renders_prove_their_own_prompts`)
is 114 characters on a pane with 118 available — the composer broke at a **word boundary four
columns early**, and the next character of the prompt is the space the wrap consumed. Under the
proposed rule that render is refused, and with it every ordinary English prompt long enough to wrap.

The correct repair needs a wrap model, and the wrap model needs a width model: a break happens
either at a whitespace the wrap consumes or at the row edge, and "the row edge" is a *column* count
while the head is a *character* count, which diverge on the CJK prompts §11's live verification
already sends. That is a measured change, not a passing one. **Reported, not attempted.**

> **CLOSED in `8bcb2b8`, and this section's last paragraph was wrong twice over.** The repair needs
> no wrap model and no width model. `composer_render_proof` takes EVERY rendered row and requires
> them, in order, to spell the prompt from its first character to its last; it never asks how wide a
> row is. The rows are the whole buffer, because `active_editor` takes them from the `❯` anchor
> through the cursor and the cursor sits at the buffer's last character.
>
> The wrap WAS measured, on 2026-08-10, by driving the shipped `pmuxd` with `PMUX_SCREEN_CORPUS_DIR`
> set: eighteen more renders. Three things came out of it that this document could not have known.
> **The two tables recording the same measured render disagreed** — `composer.rs` had the wrapping
> row as `long_wrapping[..118]` and `driver_io.rs` had 114 — and both passed, because a rule that
> accepts any prefix accepts both; it is 114. **The content region is 116 columns on a 120-column
> pane, not the `cols - 2` = 118 this document and three others claimed**, established by a 200-`x`
> word breaking at exactly 116, a CJK line breaking at 58 double-width characters, and a wrapping
> prompt's second row filled to exactly 116. And **a width-based repair is refuted too**: a
> 600-character prompt renders six rows whose third ends 8 columns short of that width with a
> 7-character word next, so no greedy-fill model admits a render Claude actually produced.
>
> Six mutants, applied and restored, each caught by the test written for it.

**(b) `is_trimmed_from_the_end` trims one character JS `trimEnd` keeps.** Partially addressed in
`28bd6b2`: the false identity claim is retracted, the trade is stated, and the divergence is now
derived by a test (`the_shipped_trim_set_differs_from_js_trimend_by_exactly_the_unmeasured_one`,
three mutants caught). **The predicate is deliberately unchanged** and that is the unfixed half — a
trailing U+0085 is still silently deleted from a caller's prompt. The correction table in §2 says
why narrowing it without a live turn would be the worse error. **Now CLOSED — see the block below,
which is also where the count in this heading is corrected: it was four characters, not one.**

> **The live turn was spent on 2026-08-10 and the stated trade is REFUTED.** U+0085 was removed from
> the set, the release binaries were rebuilt, and a prompt ending in one was sent: **pmux never
> pastes it.** U+0085 is a C1 control character and `validate_prompt`'s next guard refuses every
> control character but `\n`, so the prompt came back `invalid_config` in 0 ms with the instance
> untouched. The `UnexpectedTypedPrompt` that justified keeping the superset is unreachable from the
> guard three lines below the one that would have paid it.
>
> The real trade is *silently alter the caller's prompt* against *refuse it with a message*, and it
> makes U+0085 the one character whose treatment depends on where it stands: interior, it is refused
> as a control character; trailing, it is deleted without a word. **The predicate is still
> unchanged**, now for a stated reason rather than a wrong one — whether Claude keeps a trailing NEL
> is STILL unmeasured, because reaching the composer with one needs the control-character guard
> relaxed as well, and which of the two behaviours should ship is a design call and not a
> measurement. Both halves of the asymmetry are now pinned by a test, derived over the whole set.
>
> **CLOSED on 2026-08-11, and both halves of the paragraph above are wrong.** The measurement needed
> neither guard relaxed: it is a question about the composer, and a composer can be driven without
> pmux. Nine turns against an isolated Claude Code 2.1.227 in a 120x24 pane, with the paste framing
> `bracketed_paste_payload` builds and an Enter after it, reading the child's own recorded `user`
> rows (`docs/path-b-adversarial.md` §12): **the composer KEEPS a trailing U+0085** — `… else.`
> U+0085, bytes `65 6c 73 65 2e c2 85`, turn answered. Trimming it was never matching the composer.
>
> It was also never *one* character. U+0009, U+000B and U+000C were refused inside a prompt and
> deleted from the end of one exactly as U+0085 was, and U+000B and U+000C are not trimmed by the
> composer at all — they are RECORDED as `^K` and `^L`, which no trim rule of any shape describes.
> The count in the sentence above, and in the register row's own title, was inferred from the one
> character being looked at.
>
> The fix is the subtraction that makes the two sets one rule: `is_trimmed_from_the_end` is what the
> composer removes, less everything `is_refused_wherever_it_stands` names, so a character pmux
> refuses can no longer be silently deleted instead. `validate_prompt` is unchanged — the refusal a
> caller meets was always there, and nothing deletes the character in front of it any more.
>
> The test named in the paragraph this section opens with is gone by that name: it is
> `the_shipped_trim_set_is_both_spellings_less_what_pmux_refuses` now. What it used to assert is
> still true of the two SPELLINGS and is no longer true of the shipped set, which is both of them
> less what pmux refuses — and the difference they disagree about is a measured row of the sweep
> rather than the one nothing had sent.

**(c) The MCP adapter discards every daemon refusal message and the `recommendation` advice
channel.** `bin/pmux-mcp/src/tools.rs:128`. `redact_client_error` keeps `code` and `retryable` and
throws away `message` and the whole of `details`; `result()` then renders the constant string
`"pmuxd rejected the native request"`. On the one Path B surface whose reader cannot ask a human,
"Path B is not enabled on this daemon" and "your prompt starts with `/`" are byte-identical
payloads.

> **HALF CLOSED in `731c8ab`, and the half that is not is deliberate.** `details.recommendation`
> now crosses in both channels — the `content` text a model reads and the structured error — read
> through the same `RECOMMENDATION_KEY` the daemon writes it with. `message` does NOT cross: a
> daemon message can be composed from caller bytes (MEASURED, `{"environment":{"set":{"SECRET":42}}}`
> renders as ``invalid type: integer `42`, expected a string``), while `recommendation` is written by
> `ErrorBody::advising` and by nothing else. A test pins the asymmetry.
>
> The two payloads in the last sentence above are no longer identical, and closing that needed a
> second change: `ComposerRefusal` published its remedy only inside the message, so on a redacting
> surface a mode-prefix refusal still said nothing. `explain()` and `remedy()` are now separate,
> `describe()` is the two joined byte for byte as before, and the daemon puts the remedy in the
> advice channel. Splitting them found a variant with no remedy at all.

**(d) `composer_head_proof`'s stated reason for choosing a prefix over an equality is false.**
`crates/claude/src/composer.rs:674` argues it "is measured rather than argued: `\"   \"` is a prompt
`validate_prompt` accepts". `48aee00` — the next commit — made that prompt refused, and
`driver_io.rs`'s own `a_prompt_of_only_whitespace_is_refused_before_any_terminal_is_touched` proves
it. The real reason (one row of a 1 MiB prompt) is stated two sections away. Not fixed here only
because the tree had to be settled before Gate A; it is a two-line comment.

> **CLOSED in `8bcb2b8`**, by the fix for (a) rather than as a comment edit: the prefix it justified
> no longer exists, so neither does the justification.

### 2. The adversarial suite passes — **MET, and since 2026-08-12 the live half is driven from a derived guard list rather than a written one — §10.3**

`docs/path-b-adversarial.md` §§1-11 and the tests that carry them are green.
`cargo test --workspace` at `28bd6b2`: **1157 passed, 0 failed**. `crates/service`: 415 lib +
integration, including `path_b_pool` 46, `minified_cell` 22, `v1::minified` 18, `paste_injection` 7,
`path_b_doc_citations` 4. The §11 refusals (mode prefix, rewritten character, line continuation,
whitespace-only) each have a firing test, and `48aee00`'s live receipts are in the document.

**I spent no live turn, so the live half of this is inherited rather than re-run**, and it is
inherited on a checkable basis: `git diff 48aee00..28bd6b2` touches no runtime code path. Every
source change since the live receipts were taken is a doc comment, a test, or the one character in a
test file that clippy refused — `is_trimmed_from_the_end`'s body, `composer_head_proof`,
`composer_refusal` and `normalize_prompt` are byte-identical to the tree §11 was measured on.

The honest qualification: **the suite passes and it is not the same thing as the suite being
complete.** §1(a) above is a live Path B defect that the suite does not see, and it was found by a
reviewer reading the predicate rather than by any test. That is the third time in this session that
the adversarial suite has been green over a real Path B hole.

### 3. A promoted profile for the installed version, from machinery that exercises minified cells — **MET; the installed 2.1.227 is inside the promoted `2.1.220..=2.1.227`, and it was NOT MET when this section was written**

> **CLOSED 2026-08-11 by the owner's call to proceed (§8.3).** The range is now
> `2.1.220..=2.1.227`, `evidence/promotion-2.1.227-macos-aarch64.json` records
> `verdict: promotable` over all nine checks, and `scripts/path-b-done.sh --only 3` reports
> `promoted_range=2.1.220..=2.1.227` with `claude_version_installed=2.1.227`. What made it worth
> doing before widening anything is `docs/2.1.227-compatibility.md`: every version-keyed instrument
> run at 2.1.226 and at 2.1.227, **zero disagreements**, and the per-version drain fit landing at
> 250 ms — under `POST_MARKER_CATCH_WINDOW_FLOOR_MS` — which is why the shipped bound is read from
> the pooled receipt and never fitted here. The section as it was written:

`crates/service/src/compatibility.rs:484`: floor 2.1.220, `claude_version_tested_through` 2.1.226,
macos/aarch64. Installed `claude --version` is **2.1.226 (Claude Code)**. The `range_provenance` is
generated by the promotion path and not written beside it: *"promote_claude_version.py drove 5
minified-cell turns through `pmux ask` at claude-sonnet-5 low/high — every graded reply exact, the
four-grade suite served by one unchanging process across a `/clear` per turn, sidechain and cache
zero on every result, the pool never halted — and measured 5 reachable post-answer arrival(s) at
this version, max 223 ms against the pooled 1000 ms bound."*

MET, with one caveat that is not a criterion failure and is a real risk: **`tools/promotion/` is the
only tool directory in the repository with no `tests/` and no cell in any gate phase.** It holds
1,066 lines of Python whose exit codes a shipped runtime refusal message cites, and
`RepromotionTrigger` 1 and 2 are detected by nothing else. The two Rust guards that bind them are
substring searches over the source, not executions. A refactor that turned exit 4 into exit 0 would
leave every gate green and retire two of the five triggers silently.

### 4. Gate A green except the deliberate Linux cell — **a receipt per commit, and no commit can carry its own: run the script — §10.5**

Run at `28bd6b2` on a settled tree, 62 cells across `gate_a`, `gate_c`, `gate_d`, `gate_e`,
`gate_f` and `residue`:

```
[62/62] residue/gate_a_residue ok 512ms
receipt: /tmp/gate-final2/receipt.json
FAIL 61/62 cells passed, 1 failed, 62 executed failed: linux_docker_self_tests
```

**61/62 in 34.9 minutes, sole red `gate_f/linux_docker_self_tests`.** That cell ran 111 tests and
failed exactly one — `test_linux_manifest_is_the_exact_ordered_candidate_projection`, listing six
names the Linux projection declares that the candidate manifest no longer has and seven the
candidate has that the projection does not. That is debt row **C6** and nothing else.

The receipt is fresh against this run and not inherited:

| | |
|---|---|
| started / completed | `2026-08-10T04:28:04.874Z` / `2026-08-10T05:02:57.072Z` |
| `source_unchanged` | `True` |
| `source_digest_before` = `source_digest_after` | `fcf329ec618deef189eddbf69c3006a0c01a510b309cdcfae2ef702e782376cd`, 950 files |
| recomputed after the run, independently | `fcf329ec…82376cd`, 950 files — identical |

`gate_f/phase0_self_tests` passed in 255 s; the one-in-five flake a reviewer reported did not appear
in this run, which is one sample and not a refutation.

Two things this MET does not cover, and neither is hidden anywhere else in this document:
**`gate_b` was not run** (§4 item 2), and Gate A at `48aee00` was **60/62**, not the 61/62 this
review was briefed with (§0).

### 5. Path B doc claims reconciled to measurement — **MET as the script measures it; §8.5 and §10.6 say what that does not cover**

Better than it was, and still not there — but the part that is done is done totally, and what is
left is one named list rather than a class of unknowns.

**What is now true, measured at `d17221d`.**

1. **All 130 citations in the six linted documents are graded**, up from 62 of 132. Rule 2 is total:
   a citation the grader cannot check is REFUSED with a message naming what to add, on the reasoning
   the abbreviation rule already used — a citation that escapes the checker is worth less than no
   citation at all. Closing the 60 offences that surfaced meant nine repairs of real rot, three of
   them invisible until then: two rows of `docs/2.1.226-compatibility.md`'s MEASURED-site table
   pointed 126 lines above the composer anchors they name, and `docs/version-drift.md` pointed 168
   lines above the docstring it quotes.
2. **`docs/` outside the linted set is scanned.** The scan set is the whole workspace minus build
   output and `vendor`, derived by subtraction rather than named. It found **47 line citations of a
   linted document** in documents nothing had ever opened — 37 into `docs/path-b.md` from
   `docs/archive/sandbox-spike.md` and `docs/archive/linux-handoff.md`, most already pointing at unrelated
   paragraphs — plus five that meant `tools/linux-docker/README.md` and one that meant
   microsandbox's. All are section citations or fully-qualified paths now. **This is what unblocks
   editing `docs/path-b.md` at all**: §0.4's previous repair was written line-count neutral purely
   to avoid disturbing citations nothing could see.
3. **Four grader defects and one hand-written set**, each of which had been silently narrowing the
   coverage the name promised: a quoted span containing a slash was read as a path and discarded; a
   `path:line::test_name` span was discarded whole, losing the test name inside it; a bullet or
   table row could not reach its own wrapped continuation; `>` was structural, so one blockquote was
   two claims; and the citation scanner and the path-filter kept two copies of the extension list,
   which disagreed about `.tsx`. An anchor is now any of four quoting marks, and either an
   identifier the file holds or a phrase that occurs in it verbatim — which is what makes a citation
   of a MEASURED **comment** checkable at all, and half of these documents cite one.

**What is not.** One thing, and it is a list rather than a hole.

**Rule 2's predicate over source is measured and not shipped.** 55 of the `path:line` citations in
`.rs`, `.py` and `.sh` sources are gradable and **38 do not land on what they name**. The 23 whose
anchor sat in exactly one place are repaired at `a36c2c5` — `crates/service/tests/actor_model.rs`
cited `docs/spec.md` for R1's normal turn path about 650 lines out, and `bin/pmux/src/cli.rs` cited
`crates/claude/src/engine.rs` for `UnexpectedTypedPrompt` one line out. The remaining 38 need either
judgment about which of several candidate lines a comment meant, or a grader that matches anchors to
citations PAIRWISE within a line: ``InvariantViolation` (`instance.rs:186`), `InstanceClass`
(`class.rs:258`)` is two claims, and pooling their anchors tests the first against the second's
identifier. **That change was written, measured and reverted**: it cost seven correct citations in
the linted documents and bought three here. Turning the rule on is a defect list of 38, mostly Path
A, and the module doc states the number rather than the intention.

**Not established here.** Gate A has not been re-run: the 61/62 receipt is at `28bd6b2` and seven
commits of runtime, tooling and document code have landed since, including a change to the driver's
own source digest, which moves `source_digest_before` by construction. No mutation run. Neither is a
regression this work introduced; both are the first thing the next session should do.

---

## 2. The review round, triaged

29 findings. **28 confirmed as real defects, 1 not adjudicated, 0 refuted as non-defects.** Five of
the 28 carry a correction to the reviewer's account, and one carries a suggested fix that is
refuted outright by a measurement already in the tree. The reviewers were accurate about *what is
wrong*; where they went wrong was *what it is* or *what to do about it*.

That ratio — 5 corrections and 1 refuted fix in 29, about 20% — is in the same band as the previous
adjudicated round recorded in `docs/repo-review.md` (16 overstated and 11 downgraded in 104, about
26%). It is not a rubber stamp and it is not a cull, which is what an honest triage of a competent
review round looks like.

### Corrections

| # | Finding | The correction |
|---|---|---|
| M1 | head proof accepts any prefix | Defect CONFIRMED by probe. **Suggested fix REFUTED**: the tree's own measured wrapping render is 114 characters on a 118-column pane, so "shorter than the row means it did not wrap" is false and the proposed equality would refuse every wrapping prompt. |
| M2 | `is_trimmed_from_the_end` superset | Set divergence CONFIRMED exactly (JS 25 points, White_Space 25, shipped 26, differing by U+0085). **The hazard framing is half right**: this superset costs the *caller* a trailing NEL, while narrowing it costs the *instance* (paste `"ok\u{85}"`, Claude records `"ok"`, `UnexpectedTypedPrompt`). Direction matters and the finding did not say so. — **RETRACTED 2026-08-11: the parenthesis is measurably false.** Claude records `"ok\u{85}"`; narrowing the set costs the instance nothing, and the finding was right without the qualification this row added to it (§1(b), `docs/path-b-adversarial.md` §12). |
| M3 | "6 of the 8 refusal constructors" | **5 of 7.** `crates/service/src/pool/refusal.rs` has seven `pub fn` returning `ErrorBody`; `path_b_not_enabled` and `pool_halted` carry a recommendation, five do not. The module also already has the census derivation the fix asks for (`the_refusal_census_names_every_constructor_this_module_has`), so the work is smaller than described. |
| M4 | README `ask` refusal table | **Split.** The missing trailing-`\` line-continuation refusal is CONFIRMED and fixed here. The `daemon_lost` half is **overstated**: the table is headed "What `ask` refuses", and `daemon_lost` is a runtime failure, not a refusal. |
| H1 | MCP discards refusals | Core CONFIRMED. One sub-claim is wrong: the `run_stateless` tool description does **not** list both meanings of `unsupported_feature` — it names only the daemon-not-enabled one, which makes the surface *less* informative than the finding says, not more. |

### Four of these were already in the repo's own register, and are still open

`docs/repo-review.md` §4 and §5 already record, by `path:line`: the MCP surface at **13 of
`Request`'s 16** with `ClearSession` and `Diagnose` named; `tools/gate-a/run_gate.py:62-65` omitting
`.gitignore`, `.dockerignore`, `LICENSE-*` **and `evidence/`** from the digest; `fuzz/README.md:30`
pointing at a `../TESTING.md` that moved; and `.dockerignore:117` re-admitting a directory that has
never existed. `bin/pmux/tests/cli_contract_matrix.rs:40` is there too, at **11 of 13**, with the
fix costed at "~10 lines against `Cli::command().get_subcommands()`".

So for at least five of the 29, **the finding is a rediscovery and the register is not the
bottleneck — working it is.** The one genuinely new part of the Gate A digest finding is the
`.DS_Store` half, which runs the *other* way: those ten gitignored files are IN the digest and
Finder rewrites them, so browsing a directory during a run can void a receipt for a reason that is
not source at all.

The register is itself unscanned and rotting — `docs/repo-review.md:98` still cites
`bin/pmux/src/cli.rs:1867` for a `strip_suffix` line `48aee00` deleted, and `:280` uses the
abbreviated `` `:NNN` `` shape the grader was taught to refuse inside linted documents. A defect
register whose own citations do not resolve is the house bug class pointed at the place defects go
to be remembered.

### Confirmed, by severity

**High**

| Finding | Where | How confirmed |
|---|---|---|
| MCP adapter discards refusal message and `recommendation` | `bin/pmux-mcp/src/tools.rs:128` | `redact_client_error` keeps only `code`/`retryable`; `result()` emits a constant string. **`recommendation` now crosses (`731c8ab`); `message` deliberately does not** |
| Grader graded 56 of 133 citations under a doc saying "every" | `crates/service/tests/path_b_doc_citations.rs:1` | instrumented run: `total=133 graded=56 unresolved=5 no_identifier=72` — the reviewer's figures exactly. **FIXED at `a36c2c5`: 130 of 130, total — an ungradable citation is refused, not skipped** |
| Line-number ban knew one spelling of four | same, `crates/service/tests/path_b_doc_citations.rs:264` | probe: bare, `./docs/`-relative and qualified spellings; only the qualified one was caught at `48aee00`. **Fixed**; blast radius was exactly one site, in §0.4 itself |
| Gate A `source_unchanged` omits `evidence/`, hashes 10 gitignored `.DS_Store` | `tools/gate-a/run_gate.py:62-65` | `SOURCE_ROOT_DIRS` had no `evidence`; 951 files scanned, 10 of them `.DS_Store`; `gate_f/gate_driver_self_tests` reads `evidence/model-attempt-ledger.ndjson` at `test_run_gate.py:888`. **FIXED at `d17221d`**: the set is derived from `.gitignore` and asserted equal to `git ls-files --cached --others --exclude-standard` — 953 files, both directions |

**Medium**

| Finding | Where | How confirmed |
|---|---|---|
| Head proof accepts any non-empty prefix | `crates/service/src/driver_io.rs:1019` | probe test: head `"W"`, `(pastes, enters) = (1, 1)`. **Fixed in `8bcb2b8`: every rendered row is compared** |
| `is_trimmed_from_the_end` is not `trimEnd`'s set | `crates/claude/src/composer.rs:390` | node enumeration (25 points, no U+0085) vs rustc enumeration (25 points, with U+0085). **Doc + derived test fixed** |
| `contains` weakening named in a comment, untested | `crates/claude/src/composer.rs:1680` | mutant survived `pseudomux-claude` (31) and `pseudomux-service` (415). **Fixed; mutant now caught** |
| 5 of 7 Path B refusals write no `recommendation` | `crates/service/src/pool/refusal.rs:315` | mechanical scan of the non-test half. **Fixed in `d8c4020`, derived from the module's own census** |
| README `invalid_config` row omitted the line-continuation refusal | README, *What `ask` refuses* | `ComposerRefusal::LineContinuation` maps to `InvalidConfig` at `driver_io.rs:552`. **Fixed** |
| `ask` and `agent` absent from the CLI contract matrix | `bin/pmux/tests/cli_contract_matrix.rs:42` | 13 `Command` variants, `const ALL: [Self; 11]`; `pmux --help` calls `ask` "the entire surface" of Path B. **FIXED at `d17221d`**: both added, all five boundaries pass in all three output modes, and the set is derived from `pmux --help` |
| `Diagnose` has no MCP tool | `bin/pmux-mcp/src/tools.rs:203` | 16 `Request` variants, 13 tools; `Ping`, `ClearSession`, `Diagnose` missing |
| `tools/promotion/` has no tests and no gate cell | `crates/service/src/compatibility.rs:213` | no `tests/` dir (6 of 9 tool dirs have one); `grep -c promotion phase-manifest.json` = 0 |
| Evidence prune is O(corpus) on every teardown | `crates/service/src/pool/evidence.rs:177` | mechanism confirmed by reading: `prune` runs on every `retain_instance_transcripts`; one `spawn_blocking` in all of `crates/service`, in `attach.rs`. Timing not re-measured here |
| Three of five geometry clauses survive mutation | `crates/service/src/driver_io.rs:1046` | **re-run here after the gate**: `!empty_cursor_position`, `cursor_moved \|\| rendered_rows_changed` and `same_editor_geometry` each disabled in turn, `pseudomux-service --lib` **415 passed, 0 failed** every time; the control — disabling `head_is_this_prompt` — reddens 2. And `gate_b`'s mutation cell is configured with `scope_does_not_cover=crates/service/src/driver_io.rs`, so it could never have caught them. **Fixed in `cb72000`: `!empty_cursor_position` and `same_editor_geometry` are load-bearing and now have tests built from a MEASURED placeholder render and from sec. 8's second-editor screen; `cursor_moved \|\| rendered_rows_changed` could never have refused anything and is deleted, replaced by the fence invariant it silently relied on** |
| `docs/testing.md` §F lists 7 shell scripts, the manifest runs 8 | `docs/testing.md:899` | manifest `shell_syntax`/`shellcheck` argv both carry `scripts/gate-a-mutants.sh`; the doc block did not. **FIXED at `d17221d`**, and the block is now checked against the two cells' argv |
| `seed_corpus.py` ignores argv, has no root check | `tools/screen-corpus/seed_corpus.py:23` | confirmed by reading, then by running it from `/tmp`: it wrote a corpus with the 2.1.220 frame and none of the five 2.1.70 captures, exit 0. **FIXED at `d17221d`**: paths from `__file__`, argv parsed, a fixture set that is not exactly five is fatal |
| `gate-a-fuzz.sh` hand-lists its 3 targets in 5 places | `scripts/gate-a-fuzz.sh:193` | 3 `[[bin]]` in `fuzz/Cargo.toml`, 3 hand-written `run_fuzz` lines plus a hand-written `for` list. **FIXED at `d17221d`**: the set comes from `fuzz/Cargo.toml`, and a target with no declared seed aborts the gate |
| `tools/gate-a/README.md` publishes "gate_b 6/6 in 138 s" | `tools/gate-a/README.md:105` | its own line 4 says gate_b is 8 cells; the last real gate_b receipt spent **5,285 s** on the mutation cell alone. **FIXED at `d17221d`**: the claim is retracted in place and the census above it is derived from the manifest |

**Low** — all confirmed: 32 ungraded `.rs` citations with ≥8 rotted; `cli.rs`'s "exactly one text-file
terminator" (**fixed**); `composer_head_proof`'s falsified prefix justification; `repo-review.md`
ungraded and rotted (`:98` cites `cli.rs:1867` for a `strip_suffix` line `48aee00` deleted);
`normalize_prompt`'s four copies; `claude-p`'s surviving `trim_start().starts_with('/')`;
`fuzz/README.md`'s link to a `../TESTING.md` that does not exist; `.dockerignore`'s re-admission of
`tools/phase0/fixtures/*.jsonl`, a path with zero commits in history; `gate-c-linux-handoff.md`'s
`target_os` census of 25 against a measured 30.

Not adjudicated: the one-in-five `tools/phase0/tests` flake. It was not reproduced here and Gate A
ran that cell once.

---

## 3. What `28bd6b2` changed

Six things, each with a mutant or a probe behind it.

1. **The line-number ban resolves a cited path the way a reader does**, by path-component suffix, so
   `path-b.md:NNN` and `./docs/path-b.md:NNN` join the qualified spelling. `evidence/README.md`
   stays out because it is *longer* than the reading order's `README.md` and is therefore a
   different file — the direction of the suffix relation is the whole content of the rule. Proven on
   all three spellings.
2. **The grader joins the line below as well as above**, under the guards that were always
   symmetric. 56 → 62 graded, and it immediately caught real rot in `docs/version-drift.md` §1.4
   (repaired). Proven able to fail by re-rotting the repaired citation.
3. **The module doc stops saying "every"** and the vacuity assertion prints the pair it saw, so the
   coverage figure comes from the run.
4. **`a_head_that_is_not_this_prompts_head_proves_nothing` now tests the `contains` weakening its
   comment named.** The two assertions the comment pointed at cannot discriminate
   (`"hello".contains("hello world")` is false either way); the new one uses the module's own
   reproduction with the shell command moved to the end.
5. **`is_trimmed_from_the_end`'s doc retracts the JS identity**, states which way the superset errs
   and why that is the cheaper error, and a new test derives the difference from both sets. Three
   mutants caught, including the narrowing that would "fix" it.
6. **README's `invalid_config` row and `cli.rs`'s normalization comment** say what the code does.

And the clippy error of §0.

---

## 4. What remains for pmux overall, ordered

1. **The head proof's lower bound (Path B, availability).** §1(a). Needs a measured wrap-and-width
   model; the obvious fix is refuted. Everything else on this list is cheaper.
2. **`gate_b` has no receipt at this head and the mutation score is stale two different ways.**
   I could find no completed `gate_b` from the Evidence phase on this host: the only artifact dated
   to this session is an 86-second **enumeration** at `a5e4d49` (`Found 1646 mutants to test`,
   `scope=full`) that never ran a mutant. The last *complete* `gate_b` receipt is 2026-08-07 at
   `0d7f2ca` — 39 commits back — 8/8 green with
   `mutation_score_agent_launch_pool_protocol` at **93%** (561 caught / 39 missed / 600 decided) in
   5,285 s, and its own receipt records `scope=gate`, `scope_does_not_cover=crates/service/src/driver_io.rs`
   and `scope_does_not_cover=crates/service/src/native.rs`. The **96.33%** this session was briefed
   with is from `09f5f41`, 36 commits back, and no receipt on this host states its scope, so I
   cannot say whether it covered `driver_io.rs` either.

   The consequence is concrete and not bookkeeping: **the head proof, the composer refusals, the
   render gate and the three surviving geometry clauses are all in `driver_io.rs`, which the gate's
   mutation cell is configured not to mutate.** A `scope=full` run is the single highest-value
   measurement left, and the enumeration says it is 1,646 mutants rather than 702.

   **CLOSED. §7 is that run**, twice: 1,654 mutants at `1882dee` scoring **88**, and 1,653 at
   `0b1cff6` scoring **94** after 78 of the first run's 136 survivors were closed. The paragraph
   above under-states what it found — the 94 floor was not merely unmeasured on that scope, it was
   six points above it. Every remaining survivor now carries a written disposition in
   `evidence/mutation-survivor-register.json`, and the gate refuses a run that produces one the
   register does not hold.
3. **Gate A's `source_unchanged` digest.** Both directions are live: an edit to `evidence/` mid-run
   leaves the receipt saying nothing changed, and Finder touching a directory voids one for a reason
   that is not source. Deliberately not fixed here — changing the gate driver in the same commit as
   running it is how a receipt stops meaning anything.
4. **`tools/promotion/` self-tests and a `gate_f` cell.** Two of five re-promotion triggers rest on
   1,066 untested lines.
5. **The MCP surface**: the `recommendation` passthrough and a read-only `diagnose` tool. Both are
   Path B completeness, both are the sort of change that wants the owner's eye on the redaction
   boundary.
6. **Path A**: the 32 ungraded `.rs` citations (most of them `actor_model.rs`'s spec anchors,
   drifted ~680 lines), `claude-p`'s divergent slash guard, §6.3's `SchemaDrift` on a model refusal
   (now n = 2).
7. **Linux**: `gate_f/linux_docker_self_tests` (debt row C6) and the 84-vs-70 manifest divergence
   behind it; `docs/gate-c-linux-handoff.md`'s stale census.
8. **Non-Path-B docs**: `docs/archive/sandbox-spike.md` and `docs/archive/linux-handoff.md` hold 37 unchecked line
   citations into `docs/path-b.md` with at least six already rotted. Either scope them into the
   grader or give them a status that says they are frozen. **This document is deliberately not
   listed in `docs/path-b.md` §0.0**, and that is the same finding seen from the other side: adding
   a row to that table inserts a line into `docs/path-b.md` and moves all 37 of those unchecked
   citations by one. Close the scanning gap, then promote this file — at which point the grader
   will start checking the citations below, which is the point of promoting it.
9. **The three unfiled `.context/rmux-issue-drafts/`.** Last on this list and not last in value:
   they are gitignored, so nothing in the product repo records the work, and they name 0.9.0/0.9.1
   as current. Whether upstream rmux 0.10.0 exists and carries the three defects was not established
   here. Filing them costs an afternoon and stops the next person rediscovering them.

### 4.1 The same list, re-ordered and re-costed on 2026-08-11

Items 1 and 2 above are closed (§1(a), §7, §8.1). What is left, in the order the owner set:

1. **Linux.** Unchanged and still item 7: `gate_f/linux_docker_self_tests` is the one cell Gate A is
   allowed to be red on, and it is red on exactly one assertion — `test_runner.py:277`, the ordered
   projection of `phase-manifest.json` into `tools/linux-docker/gate-a-manifest.json`. Row C6 records
   that repairing it makes two *other* tests in the same file fail, both by hand-written `required`
   lists, and that both are Gate C decisions. **Re-project the Linux manifest as the first act of
   picking Gate C back up.** It is also the only thing standing between this repository and a Gate A
   with no admissible red cell at all.
2. **Non-Path-B docs.** Unchanged and still item 8, and this session added two named examples of what
   the gap costs. `docs/repo-review.md:275` cites `bin/pmux/tests/cli_contract_matrix.rs:40` for
   `const ALL: [Self; 11]`; that constant is at line **42** and reads `[Self; 13]`. This document
   quotes that citation faithfully at §3 and inherits the rot, because neither file is in
   `docs/path-b.md` §0.0 and so neither is graded. The order matters: close the scanning gap in
   `sandbox-spike.md` and `linux-handoff.md` first, then promote this file into the reading order,
   at which point the grader starts checking §8's citations too.
3. **The three unfiled `.context/rmux-issue-drafts/`.** The unknown this list recorded is now
   answered on one side: **rmux 0.10.0 exists**, published 2026-08-05, and this tree vendors and
   locks 0.9.0. Read on `docs.rs` at 0.10.0 — **not compiled and not reproduced** — both reported
   defects are still there: `rmux-client`'s attach loop still passes `&read_buffer[consumed..]` to
   the frame decoder while bounding the decoder push at `[consumed..bytes_read]`, which is exactly
   the asymmetry draft 01 is about; and `rmux-server`'s `TryAttachRead::Closed` arm still
   `return Ok(())`s without draining the decoder, which is draft 02. That raises the value of filing
   rather than lowering it, and it makes the drafts' "affected versions: 0.9.0 and 0.9.1" the one
   line in them that must be re-checked before they are sent.
4. **Path A.** Parked by owner decision and untouched again here. Item 6 above is its list.

One thing joined the list this session and belongs above Path A: **a promoted profile for 2.1.227**
(§8.3), which is five real turns, no ledger ordinal, and one range edit — but a claim about what
pmux supports, so it waits on the owner. **DONE 2026-08-11**, the owner having made that call: the
range is `2.1.220..=2.1.227` and criterion 3 is MET. The eight `gate_b` cells left the list: they were run at
`06a6cdc`, 8/8, and the `gate` mutation number they carried is now 97 (§8.4).

---

## 5. What this document does not establish

**Every bullet below is as it was written on 2026-08-10; two of them were overtaken on 2026-08-11
and say so in place rather than being deleted, because what was claimed and what was later measured
are both worth being able to read.**

- **No live turn was spent.** The ledger is `consumed 85, remaining 15`, digest
  `439e48533a77679d15bcc24a5a555366dcf426131cc8a0ae1e2c105afb167153`, identical before and after.
  Everything about the composer here is offline: whether Claude Code 2.1.226 really keeps a trailing
  U+0085 is inferred from Node's `trimEnd`, not measured.

  > **OVERTAKEN 2026-08-11 (§8.2).** Thirty live turns were spent at 2.1.226 and the ledger is
  > still `consumed 85, remaining 15` at the same digest — `pmux ask` reserves no ordinal. A prompt
  > ending in U+0085 was sent and **answered**, so the trailing character is deleted in fact and not
  > only by inference; the same character one position earlier is refused. What is still unmeasured
  > is the other half of the original question — whether *Claude* would keep a trailing NEL — because
  > reaching the composer with one needs the control-character guard relaxed as well.
  >
  > **MEASURED 2026-08-11, and it needed no guard relaxed** (`docs/path-b-adversarial.md` §12):
  > nine turns against an isolated Claude Code 2.1.227, driven with the paste framing pmux uses.
  > **Claude KEEPS a trailing U+0085**, so the inference from Node's `trimEnd` above was the right
  > answer for the wrong set — `trimEnd` does not strip U+0085 either, and the shipped predicate did.
  > The pmux half is fixed and §1(b) carries it.
- **The composer below its first row** is still unproven by anything pre-Enter. A prompt whose first
  118 characters match and whose 539th differs passes the head gate.
- **`active_editor` can still anchor on the caller's own text** (`docs/path-b-adversarial.md` §8).
  Unchanged.
- **The evidence-prune timing (128 ms at 46,000 files) was not re-measured**, only the mechanism.
- **The `tools/phase0/tests` flake was not reproduced.**
- **No daemon was stood up here.** The reviewers' live `pmux ask` and MCP `tools/call` transcripts
  behind H1 and M3 were re-confirmed by reading `redact_client_error`, `ToolCallError::result` and
  every `pub fn` in `pool/refusal.rs` — not by running two daemons again. Where a live transcript is
  the only evidence for a claim, this document says so rather than adopting it.

  > **OVERTAKEN 2026-08-11 (§8.2).** Two bounded daemons were stood up — one at `pool_size 1` for
  > the guard and statelessness cases, one at the 15-instance cap — and torn down with nothing left
  > running. The MCP surface was still not exercised live; H1 and M3 remain read rather than run.
- **`gate_b` was not run.** §4 item 2 says what exists instead.

  > **NO LONGER TRUE as of 2026-08-11 (§8.4).** It was run at `06a6cdc`, 8/8 and exit 0, which is
  > the first complete `gate_b` on this host since 2026-08-07 and the first ever recorded against a
  > named commit.
- **Nothing here re-measures Path A.** The `.rs` citation rot is mostly `actor_model.rs`, and the
  brief's rule is that Path A is reported and not touched.

---

## 6. The four closures of 2026-08-10, and what they do not close

`d8c4020`, `731c8ab`, `cb72000`, `8bcb2b8`. `cargo test --workspace`: **1168 passed, 0 failed**, up
from 1157. `cargo fmt --all -- --check` and
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` are both clean —
run before each commit, which is the check §0 records five reviewers reading past.

### What was measured, and what it cost

**30 live turns**, all `pmux ask` at `claude-sonnet-5` low, so no ledger ordinal was consumed: the
ledger reads `consumed 85, remaining 15` with digest `439e485…67153` before and after, unchanged.
Eighteen of them recorded composer frames through `PMUX_SCREEN_CORPUS_DIR`, one was the U+0085
experiment of §1(b), and twelve were the live verification below.

The wrap measurement is the one that mattered, and it produced three facts this document did not
have. **The tree recorded the same measured render two ways and they disagreed by four characters**
— `composer.rs` had `long_wrapping[..118]` and `driver_io.rs` had 114 — and both tests passed,
because a rule that accepts any prefix accepts both. It is **114**. **The composer's content region
is `cols - 4` = 116 columns, not the `cols - 2` = 118 this document, `path-b-adversarial.md` §11.3
and `spec.md` all asserted.** And **the width-based repair this document proposed instead is refuted
too**: a 600-character prompt renders six rows whose third ends 8 columns short of a width its
neighbours reach, with a 7-character word next, so the composer is not a greedy word-wrapper at a
constant width and no fill-based rule admits a render Claude actually produced.

### The live verification of the stricter gate

The render proof now compares every rendered row rather than a prefix of the first, so the question
worth answering about it is whether it refuses something it should admit. Twelve prompts, one pooled
instance, rebuilt release binaries, one shape per composer behaviour: short, wrapping, six-row,
a 200-character unbroken word, CJK wrapping, two lines, three lines with an indented middle line, a
four-line collapse, a 3000-character collapse, trailing spaces, a trailing U+200B, and a single line
sized to the 116-column boundary. **12 answered, 0 refused**, each with the word it was asked for.

### What these four fixes do NOT close

1. **Gate A has not been re-run at these commits.** §4's 61/62 receipt is at `28bd6b2` and four
   commits of runtime code have landed since. The workspace suite, `fmt`, `clippy` and the citation
   grader are green at `8bcb2b8`; **Gate A itself is inherited and must be re-run before that
   criterion is read as current.**
2. **No mutation run was made.** §4 item 2 is unchanged and is now the highest-value measurement
   left by a wider margin, because `driver_io.rs` and `composer.rs` both changed substantially and
   the gate's mutation cell is configured not to mutate the first of them.
3. **The collapsed-paste variant still proves no prompt text.** A prompt of 1000 characters or more
   on one line, or of four lines or more, reaches Enter with a placeholder row and a line-break
   count as its whole proof. That is not a regression and it is not a hole that closed: the screen
   carries nothing else.
4. **§8's second-editor hazard is refused, not fixed.** `same_editor_geometry` now has a test built
   from that screen, so a prompt containing a `❯` line is reliably refused — and reliably refused is
   still an availability cost for a legal prompt.
5. **The trim set's design question is open**, and §1(b) states it: the trade that justified the
   shipped predicate is refuted, whether Claude keeps a trailing NEL is still unmeasured, and which
   behaviour should ship is the owner's call.

   > **CLOSED 2026-08-11 by measuring it** (`docs/path-b-adversarial.md` §12). Claude keeps a
   > trailing U+0085, so one of the two behaviours was never defensible: pmux was deleting a
   > character the composer records. §1(b) carries the closure and the three other characters the
   > same measurement found.
6. **Criterion 5 is not met and is not much closer.** 121 `path:line` citations moved by this work
   were repaired by mapping each one's anchor TEXT from `8c3d387` to where that line is now, which
   covers the 59 in unscanned documents and the ones in `.rs` and `.py` sources that no grader sees.
   That is a repair, not a check: the grader still grades 62 of 132, `docs/` outside the linted set
   is still unscanned, and the eight rotted `.rs` citations §1.5 names are still rotted.
7. **`ErrorBody::message` still does not cross the MCP boundary**, by decision. §1(c) says why and
   what pins it.

---

## 7. The full-scope mutation measurement, the survivor register, and the two-tier floor

**§4 item 2 called a `scope=full` run "the single highest-value measurement left". This is it.**
Measured 2026-08-11 at `0b1cff6` — `PMUX_MUTANTS_SCOPE=full`, `PMUX_MUTANTS_JOBS=4`, pinned 1.88.0,
on an idle machine, run complete in 10,443 s. Evidence: `run.bbUDg3`, exit 0.

```
scope=full
enumerated=1653 unviable=504
caught=1086 missed=63 decided=1149
mutation_score_percent=94 minimum=94
```

### 7.1 What the number was, and what closed the gap

The gate cell's `gate` scope scores 95 and passes. `full` — the same script over the same list plus
`native.rs` and `driver_io.rs` — scored **88** at `1882dee`, six points below the floor that has
only ever been enforced against the scope that omits them. **105 of that run's 136 survivors lived
in those two files.** That was not a regression; it was the first honest measurement, and it said
the 94 floor had never been asked the question it claims to answer.

`full` is now **94** and passes at the same floor `gate` does. 78 of the 136 survivors are closed:
35 in the guard commit `b3e3589` and 35 more here, in two commits, plus 8 the closures made
unreachable. Every one of the 71 mutations behind those closures was applied by hand, watched red
against the test written for it, and restored. The three that carried the most weight:

* **`completion_evidence`'s two modal returns dropped the whole lifecycle observation.** Three
  fields, two return sites, and `..TerminalEvidence::default()` makes every omission compile into
  the value that means "this turn was never armed and no Stop hook arrived". A modal screen is
  negative readiness evidence the actor takes and polls again on, which is exactly why the loss was
  silent.
* **`claude_version_of` had no caller in any test.** It is the input to `RequireTested`: the whole
  compatibility decision rests on the string it returns. Its body replaced by `Ok("xyzzy")` left the
  suite green, and so did deleting the `!` from `if !output.status.success()`, which admits exactly
  the runs that FAILED.
* **`StartSessionRequest::serialize`'s field count is now checked against the emissions it
  counts.** Thirteen mutants — every term of that arithmetic wrong in turn — survived because the
  only serializer this workspace runs it through is `serde_json`, which discards the number. A
  format that writes it produces a corrupt frame rather than a wrong one. The count is a second
  statement of the emission rules below it, so it is compared against them: every field leaves
  through one `emit!` that counts itself, and a disagreement is a serialization error naming both
  numbers.

### 7.2 Every survivor is dispositioned, in a file, keyed by something that does not rot

`evidence/mutation-survivor-register.json` holds one row per mutant that survived either run:
**141 entries — 65 KILLED, 16 EQUIVALENT, 47 ACCEPTED, 13 REMOVED.** 136 in, 136 out, plus the five
described in §7.3. Completeness is enforced rather than claimed:
`scripts/mutation_register.py check` runs inside `scripts/gate-a-mutants.sh` on **every** run, at
either scope, and refuses one that produced a survivor the register does not hold, or where a mutant
the register calls KILLED or REMOVED survived again. It does not refuse a survivor that has since
been caught; those print as `retired_survivor=` so the row can be pruned, because refusing them
would make closing a survivor break the gate.

**The key is not `file:line:column`.** That is how `cargo mutants` names a mutant and it is the one
thing about a mutant guaranteed to rot — and the rot here is measured, not hypothetical. Of the 136
survivors this register ratchets from, **123 still exist at this head and 100 of those are at a
different line.** A register keyed on the tool's own name would have lost 100 of 136 rows to two
commits of test-writing, which is precisely the change that closes a survivor. The key is
`(file, function, genre, replacement, occurrence)`, where `occurrence` orders the mutants agreeing
on the first four by the position the tool reported. Nothing in it moves when a test is added, and
two mutants of the same shape in one function are still told apart — which is what makes a **new**
survivor inside an already-accepted function visible instead of absorbed by that function's row.
Line and column are recorded under `observed_at` as a reader's aid and never as identity.

The 13 keys the new enumeration does not hold are every one a real code change, recorded as REMOVED:
`active_editor`'s zero-dimension bound, `composer_head`'s separator guard and `prompt_glyph_col`'s
disjunct (merged into `prompt_glyph_split`), `validate_prompt`'s two literal comparisons,
`prove_stable_empty_editor`'s re-derivation, `record_lifecycle_stop_instant`'s three (extracted to
`representable_stop_instant`) and `list_transcripts`' four (moved to `list_transcripts_within`).
A derived "where did it go" hint was written and then deleted: it matched on file, genre and
operator, so it offered `prompt_glyph_split` as the successor of a `validate_prompt` clause. A field
that promises where a clause went and delivers any new function with the same operator is this
repository's own bug class, in the instrument built to enumerate it.

**The checker was proved to fail before it was trusted**, both ways and against real data: with five
rows removed from the committed register it names those five survivors and exits 1 with the score
still printed; with the register whole and the floor raised to 99 the score refuses and the register
census is still printed. Both halves run and the refusal is at the end, because a score below the
floor with no survivor list is a number nobody can act on and a survivor list under a score nobody
printed is a list nobody reads.

### 7.3 Five survivors nobody had seen, and what they measure

Five mutants that the prior run counted as **caught** are **missed** in this one, and no edit
between the two runs touched any of them: `AgentStore::advance_head`, `normalize_path` (twice),
`admit_claude_version`, and one of `prove_transcript_inert`'s two `- 1` slices. The prior run's own
per-mutant logs name the sole catcher of every one of the five, and every one is a real-PTY or
real-rmux test: `repeated_real_rmux_cycles_remain_resource_bounded_and_leave_no_residue` (three),
`a_terminal_resize_after_creation_is_delivered_and_not_silently_clamped`, and
`a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default`.

That is `docs/testing.md`'s "THE SCORE DRIFTS UPWARD" bullet, measured at the `full` scope for the
first time and worth **five mutants**. Two things follow. The first is that the bullet's list of
three drifting tests is four: `a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default`
is the fourth and was found by this comparison. The second is `admit_claude_version` itself, which
is not a bookkeeping row: `POOL_CELL == SessionCell::Minified` read as `!=` skips
`require_tested_for_minified_cell` for **pool** admission, so the pool admits a Claude version no
promoted profile covers into the minified cell — the one thing `RequireTested` exists to refuse. It
had a catcher that was never really testing it.

### 7.4 The two-tier floor

* **`gate` keeps 94.** Declared in `scripts/gate-a-mutants.sh` as the constant it has always been,
  defended against a measured 95.50%, untouched here. The register is a record of a `full` run and
  says nothing about that scope.
* **`full` gets 93**, read out of `recorded_at.floor_percent` in the register rather than written
  into the script beside it — so the tree holds exactly one statement of the number, it sits next to
  the survivor list that explains it, and it moves only when a new measurement is recorded.
  `PMUX_MUTANTS_MINIMUM_SCORE` may **raise** that floor and is refused below it; both refusals were
  exercised and both exit 2 before a single mutant is built.

**The measured score is 94 and the enforced floor is 93, and the one point is measured rather than
chosen.** `floor_percent` is the same measurement with every mutant whose only failing test was a
drifting one counted as missed: 17 of the 1,086 caught, giving 93. The arithmetic agrees with the
observation from the other direction — at a floor of 94 the headroom is exactly five mutants, and
five is exactly what drifted between these two runs, so a floor at 94 would have no margin at all.
`docs/testing.md` defends `gate`'s 94 against 95.50 the same way and for the same reason.

**The register inherits the drift and the answer is not to loosen it.** A drifting test can flip one
caught mutant to missed and `check` will refuse naming exactly that mutant — which is the useful
failure, because it names one mutant to re-test with a file/line filter where a score dropping a
point names nothing. Re-test the one mutant; do not add a row to make the refusal go away.

### 7.5 What this does not establish

* **63 survivors remain and every one is a written disposition, not a hole.** 16 EQUIVALENT with the
  argument written out, 47 ACCEPTED with the risk each leaves open and a `closeable` field naming
  what closing it costs.
* **19 of the 47 are marked `seam`, and they are one root cause.** `NativeService` holds a concrete
  `Arc<PrivateRuntime>`, the only integration tests that build one are `#[ignore]`d, and so
  `reap_idle_sessions`' three clauses, `clear_session`'s deadline domain, the `clear_boundary` and
  `attach` and `close_session_with_state` generation fences, `wait_for_turn`'s safety guard and
  `diagnose`'s two owner filters are unreachable from any test that runs. **The fix for that bucket
  is a seam, not a test.** `wait_for_turn` read as `<` returns `DaemonLost` on the first iteration
  of every turn and the whole suite stays green; `diagnose` read as `!=` publishes a pool instance's
  session id on the wire, which the comment above it records happening live once already.
* **13 are marked `cheap`** — a fixture that already exists in the module, or a pure function with no
  seam problem at all. They are not closed here for one reason, stated so it is not read as a
  judgement about their value: the floor has to come out of one named `outcomes.json`, and each
  further round of closing costs another three-hour re-measurement. They are the next session's
  cheapest work and the register names the fixture for each.
* **Six `#[cfg(not(unix))]` twins are counted against us by the tool and cannot be closed on this
  host.** `platform_file_metadata` ×4, `resource_key`, `is_executable`: cargo-mutants patches source
  text without evaluating `cfg`, so the mutant builds, nothing compiles it, every test passes and it
  is scored as missed.
* **The `gate` cell has not been re-measured.** Thirteen of the survivors closed here are in
  `crates/protocol/src/v1.rs`, which IS in the `gate` scope, so that number is now stale in the safe
  direction. `current-state.md` §9.23's census of 27 is from 2026-08-07 at a different head and is
  not re-derived here.

  > **RETIRED 2026-08-11 (§8.4).** Measured at `06a6cdc` inside `gate_b`, evidence `run.ar6ndL`:
  > 740 enumerated, 103 unviable, 620 caught, 17 missed, **97% against the floor of 94**. The "safe
  > direction" guess was right and is now a number. The `current-state.md` §9.23 census is still not
  > re-derived.
* **The measurement is of `0b1cff6` and this document is committed after it.** Everything the commit
  that carries this section changes is under `scripts/`, `docs/` and `evidence/`; `git diff` over
  the six globs `scripts/gate-a-mutants.sh` itself declares as `FULL_GLOBS` is empty between the two,
  so the tree the number describes and the tree it is committed against are the same tree in every
  file the number is about.

---

## 8. The certification of 2026-08-11

Everything above was measured before there was a script. This section is the first run of the whole
thing end to end: the five criteria by `scripts/path-b-done.sh`, a second full-scope mutation
measurement, Gate A in a pinned worktree, and — for the first time in this document — the
adversarial suite against a **real model**, every guard watched firing rather than inferred.

### 8.0 `gate_a/rust_fmt` had been red for three commits

`cargo fmt --all --check` was red at `b3e3589`, `0a20815`, `86c2510` and `d73075a`, and clean at
`1882dee`. Every report that landed those commits said `cargo clippy --workspace --all-targets --
-D warnings` was clean, and it was. The red cell was **`rust_fmt`**, which no report ran.

This is §0's finding again, one cell over, and the shape is identical: a green sentence about a
neighbouring command standing in for the command nobody ran. Repaired in `23e81db` — seven hunks,
all in `crates/service/src/driver_io.rs`'s test module, 21 lines and no token.

It was left unrepaired the session before because `driver_io.rs` is inside the mutation gate's
`FULL_GLOBS`, so touching it rots the survivor register's currency. That cost was paid here rather
than deferred again, because a full-scope re-measurement was being taken anyway; `23e81db` is the
head it was taken at.

**It rotted no citation, and that was checked rather than assumed.** The earliest line the commit
moves in that file is 6358, and every `path:line` citation into `driver_io.rs` in every markdown
file in this repository — 24 of them — names a line above it.

### 8.1 The second full-scope mutation run

`scope=full` at `23e81db`, evidence `run.7OrHM9`, read out of its own `outcomes.json`:

```
enumerated=1653 unviable=504
caught=1085 missed=64 decided=1149
mutation_score_percent=94 minimum=93
```

**94 against the `full` floor of 93 and against `gate`'s 94.** The `gate` scope was re-run too, in
`gate_b` — **97 against its own floor of 94** (§8.4), which retires §7.5's note that it was stale.

**The register refused the run, and that refusal is the most useful thing this measurement
produced.** Five mutants it called KILLED survived, and two survivors had no row at all. The
run-to-run diff, keyed the way the register is keyed rather than by line, says what happened:

* the two runs enumerate the **same 1653 mutants** — the `fmt` commit changed no AST;
* **nine flipped**: five caught → missed, four missed → caught;
* the five that flipped out are exactly the three "regressions" plus the two undispositioned rows;
* and for **every one of the nine**, the sole catcher in the run that caught it was one of three
  tests that need a real rmux sidecar or a real PTY —
  `bounded_soak.rs::repeated_real_rmux_cycles_remain_resource_bounded_and_leave_no_residue` (five of
  them), `private_runtime.rs::a_terminal_resize_after_creation_is_delivered_and_not_silently_clamped`,
  `private_runtime.rs::a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default` — or
  a bare timeout.

**No product regression is in that list.** The clearest single case is `native.rs`'s `unix_now_ms`:
`Ok(0)` and `Ok(1)` are the same function replaced by two different constants, and between the two
runs the pair **swapped** which of them a real-runtime test happened to catch. The catcher decided
the disposition, not the code.

All five are now dispositioned ACCEPTED with that measurement written into each row, and
`scripts/mutation_register.py check` passes at `undispositioned=0 regressed=0`. The register held,
at this certification, **143 rows: 62 KILLED, 16 EQUIVALENT, 52 ACCEPTED, 13 REMOVED**; §9 is what
the test seam moved, and `scripts/mutation_register.py report` is where to read today's census.

**The floor stays 93, and it is no longer a carried number.** `floor_percent` is the score with
every mutant counted missed whose only catcher is a measured drifter or a timeout. That is 15 of the
1085, giving 93. The wider rule — every mutant caught only by *some* test in `bounded_soak`,
`private_runtime`, `lifecycle_faults` or `performance_diagnostics` — selects the same 15 and the
same 93, so the floor does not depend on which derivation is used. The previous 93 was reached by a
name-based rule over a single run; this one is reached from a measured flip set across two.

**One row is closeable and was deliberately not closed.** `unix_now_ms` is a free `pub(crate)`
function of no arguments — no seam problem at all — and two clock reads bracketing one call kill
every constant replacement of it at once. Writing that test edits `native.rs`, which is inside
`FULL_GLOBS`, and would rot the measurement it is recorded against. It is the cheapest single thing
on the mutation list and it costs one more three-hour run.

### 8.2 The adversarial suite against a real model

Run against Claude Code **2.1.226** — named explicitly at
`~/.local/share/claude/versions/2.1.226`, because the `claude` on `PATH` is 2.1.227 (§8.3). One
bounded daemon, `--path-b-pool-size 1`, watchdog and `trap`-reaped.

**Every refusal guard fired, with its own message, before any instance was touched:**

| prompt | outcome | ms |
|---|---|---|
| `!echo … > /tmp/…` | `unsupported_feature`, *"switches the composer into bash mode, so Enter would RUN THE REST AS A SHELL COMMAND"* | 40 |
| `  !touch …` (leading spaces) | same refusal — the prefix scan skips ignorable leading characters | 50 |
| `/clear` | `unsupported_feature`, *"opens the composer's command menu"* | 50 |
| trailing `\` | `invalid_config`, *"Enter would INSERT A NEWLINE instead of sending the prompt"* | 38 |
| interior tab | `invalid_config`, *"recorded by the composer as four spaces"* | 49 |
| whitespace only | `invalid_config`, *"prompt must not be empty"* | 63 |
| interior `ESC` | `invalid_config`, *"unsafe control character"* | 45 |
| interior U+0085 | `invalid_config`, *"unsafe control character"* | 38 |

The named files the two bash-mode prompts would have created do not exist. Nine turns were delivered
across the whole suite and **not one instance was destroyed** — the quarantine directory is empty.

**The rest, delivered and correct:** trailing whitespace → `W1-8`; an NFD prompt (128 bytes
decomposed, 122 composed) → `N1-8`; agentic induction ("use your Write tool, then Bash") →
`A1-NOTOOLS` with no file created; subagent spawning → `G1-NOSUBAGENT`. `isSidechain` is `false` on
all 78 transcript rows and `cache_read_input_tokens` is 0 on every turn.

**Statelessness across `/clear`, measured rather than argued.** Pool size 1, so one process served
both turns. Turn one: *"Remember this secret word for later: ZARQUON"* → `S1-STORED`. Turn two, the
next turn on that same instance: *"Earlier in this conversation I told you a secret word"* →
**`S2-NONE`**.

**The 15-instance wave at the cap.** Fifteen warm instances, fifteen concurrent callers, each asked
a different sum: **15 of 15 exact answers**, the whole wave in **3,973 ms** against a slowest single
caller of 3,815 ms, sidechain tokens 0/0 on every one, `isSidechain` false on all 179 transcript
rows, **0 instances quarantined**, and nothing left running at teardown.

**And the one guard that is not a guard.** A prompt ending in U+0085 was **delivered** — answered
`C4-8` — while the same character one position earlier is refused as a control character. That is
defect (b) of §1, reproduced live for the first time instead of inferred from Node's `trimEnd`, and
it is the sole reason criterion 1 is NOT MET.

> **FIXED on 2026-08-11.** The prompt that was delivered as `C4-8` is refused now, and what this
> paragraph called *one* guard was four characters wide: U+0009, U+000B and U+000C behaved exactly
> the same way. §1(b) and `docs/path-b-adversarial.md` §12.

**No ledger ordinal was spent.** 30 live turns, `consumed 85 / remaining 15` before and after,
ledger digest `439e48533a77679d15bcc24a5a555366dcf426131cc8a0ae1e2c105afb167153` unchanged.

### 8.3 Claude Code moved under the tree

The `~/.local/bin/claude` symlink was repointed at **2.1.227** at 19:32 on 2026-08-10.
`PROMOTED_PROFILES` covers `macos/aarch64 2.1.220..=2.1.226`, so criterion 3 is **NOT MET** and no
promotion evidence exists for what is installed.

The remedy is mechanical and it was not taken here, because widening the range is a claim about what
pmux supports and that is the owner's to make: `tools/promotion/promote_claude_version.py` against
2.1.227, which costs **five real turns and no ledger ordinal** (only `grades_answer` carries
`costs_real_turns=True`), one new `evidence/promotion-2.1.227-macos-aarch64.json`, and one edit to
`crates/service/src/compatibility.rs` — which is **not** in the mutation gate's `FULL_GLOBS`, so it
would not rot §8.1.

> **TAKEN 2026-08-11, the owner having made the call.** The estimate above held exactly: five real
> turns, no ordinal, one receipt, one range edit, and `FULL_GLOBS` untouched. A sixth real turn was
> spent proving the loop closes — `pmux ask` served under `RequireTested` on a daemon carrying no
> `--tested-claude-profile` — and it is on no receipt, which is the shortfall
> `real_turns_outside_the_ledger` already describes itself as. What the estimate did not include is
> the part worth keeping: **the A/B that came first**. `docs/2.1.227-compatibility.md` ran every
> version-keyed instrument at both versions and found **no difference in any of them**, which is the
> first datum on how much calibration a release actually moves — and the site scan it derives went
> from 16 to 25 to **44**, so the number that moves fastest is the count of things keyed to a
> version, not the versions.

### 8.4 All 70 cells graded, and `gate_b` run for the first time since 2026-08-07

Two pinned-worktree runs, both at **`06a6cdc`** on a clean tree, both receipts in `.context/gate-a/`
naming that commit in `describes_commit` — which is the whole point of the pinned runner, because a
receipt read beside a repository is otherwise silently taken to describe HEAD.

| run | phases | result | wall |
|---|---|---|---|
| `pinned-receipt-06a6cdc.json` | `gate_a` `gate_c` `gate_d` `gate_e` `gate_f` `residue` | **61/62**, sole red `gate_f/linux_docker_self_tests` | 2,343 s |
| `pinned-receipt-06a6cdc-gate-b.json` | `gate_b` | **8/8, exit 0** | 6,247 s |

`gate_a/rust_fmt` is `ok 875ms` — the cell §8.0 is about. The six-phase receipt was verified
independently of the runner: `describes_commit` and `tree_sha` both equal HEAD's, the inner gate
receipt's digest recomputes to the recorded value, its `manifest.sha256` equals this tree's
manifest, and `source_digest_before == source_digest_after` over 959 files.

**`gate_b` had never been run since this document was written**, and §4 item 2 and §5 both said so.
It needed four tool paths the driver refuses to guess — it failed closed on
`{cargo_fuzz}`, `{nightly_cargo}`, `{nightly_rustc}` and `{cargo_mutants}` before running a single
cell, which is the correct behaviour and is why the first attempt cost ninety seconds rather than
two hours. With them resolved: 8/8, including the 50,000-run production fuzz in 81 s.

**The `gate` scope is no longer stale.** Its mutation cell, read out of `run.ar6ndL`:

```
scope=gate   enumerated=740 unviable=103
caught=620 missed=17 decided=637
mutation_score_percent=97 minimum=94
```

**97 against 94**, up from the 95.50 the floor was defended against — the thirteen `v1.rs`
serializer closures are in this scope, and §7.5's note that the number was "stale in the safe
direction" is now a measurement.

The deliberately-red cell is derived and stays exactly one: a cell may be red only if **both**
criterion 4's own section and an **open** row of `docs/current-state.md` §9.4 name it. That is
`gate_f/linux_docker_self_tests`, granted by row **C6**.

**Criterion 4 is MET at `06a6cdc` with `cells_executed=70`, and it will not stay MET.** A pinned
receipt is bound to a commit and to a tree hash, so *any* later commit — including the one carrying
this paragraph — leaves it describing an ancestor and takes criterion 4 back to NOT MET with no cell
having changed. Measured, at the commit after: *"pinned-receipt-06a6cdc.json describes commit
06a6cdc and this gate is judging c14c9e3"*, twice, and then all 70 cells reported ungraded. That is
the binding working, not failing; it is also why this section states the commit rather than the word
"now", and why the receipts are the artefact and the table above is only a reader's copy of them.

**And there is no flag that gets the verdict back.** This paragraph first said
`scripts/path-b-done.sh --commit 06a6cdc` re-reads it, which was written without being run. Run, it
**exits 2 and refuses**: *"this working tree is at c14c9e3 and the commit being judged is 06a6cdc.
Criteria 2 and 5 run tests against the tree in front of them, so a verdict taken here would be a
verdict about neither."* That refusal is right, and it means the only two honest ways to re-read
criterion 4 are to put `06a6cdc` in front of the gate — a checkout or a worktree — or to re-run both
pinned gates at the new head, which is 2 hours 20 minutes. **A verdict on this criterion costs a
Gate A run per commit, and that is a property of the criterion, not of the runner.**

### 8.5 What the done-gate does not measure, found by reading it

`criterion_adversarial_suite` derives its work from the `cargo test` commands the adversarial
document's own "Verification at this commit" tables name — and its accounting of what it discarded
named `cargo fmt`, `cargo clippy`, `ruff` and the residue audit, all of which are Gate A cells.
Printed instead of asserted, the discards are six, and two of them are the rows reading **"live
re-verification, rebuilt release binaries"** and **"live verification, rebuilt release binaries"** —
the only rows in either table that record a real model turn.

So a criterion titled *"The adversarial suite passes"* was reporting seven offline commands and had
never looked at the live half. Fixed in `dc3ed27`: the third kind of row is derived alongside the
other two and printed, one line per label, so the criterion states what it did not measure. It is
printed and not refused — which rows count as live turns is a reading of the owner's criterion, and
a script that promoted a dropped row to a failure would be legislating it. §8.2 is what fills the
gap for this commit.

The same commit corrects `deliberate_red_cells`'s comment, which credited row **C10** with naming
`release_full_stack_e2e`; printing the per-row name sets says it is **C6**.

### 8.6 What this certification does not establish

* **Criterion 1 turns on one design call nobody has made.** Row (b) is OPEN because the choice
  between *silently altering a caller's prompt* and *refusing it with a message* is the owner's.
  §8.2 measured what the current behaviour is; it does not decide it.
* **Criterion 4 is MET only at `06a6cdc`.** The commit carrying this section is not that commit, so
  a run of the done-gate against HEAD reports it NOT MET again — on receipt binding alone, with no
  cell having changed, and `--commit` does not recover it (§8.4). The verdict of record for this
  work is therefore the one taken at `06a6cdc` with both receipts: **NOT DONE 3/5, not met 1 and 3**.
* **One remedy in this section was written without being run and was wrong.** §8.4's `--commit`
  sentence. It was caught by running it, which is the only reason it is not still there.
* **The four retired survivors were left in the register.** They are the mirror image of the five
  that flipped out — the same three tests, the other direction — and pruning them would only
  guarantee they come back as undispositioned rows on the next run.
* **`scripts/path_b_done.py` still has no unit tests.** Its derivations are guarded by vacuity
  refusals and by printing what they derived; that is weaker than a test.
* **The pinned-worktree runner still has no disk pre-flight.** ~19 GB of stale gate scratch was
  removed from `/tmp` to make room for this session's runs, which is not a fix.
* **Nothing here re-measures Path A**, and Path A remains parked by owner decision.

## 9. The test seam, and the survivor debt it closed

### 9.1 One cause under twenty survivors

`evidence/mutation-survivor-register.json` carried **22 rows whose `closeable` was `seam`**, and the
guards under them are not incidental: the completion proof (`wait_for_turn`), three generation
fences (`clear_boundary`, `attach`, `close_session_with_state`), all three clauses of the idle
reaper, the pool-disclosure filter in `diagnose`, the minified cell's `RequireTested` admission,
`shutdown`'s first-error rule, and the clear deadline's domain.

Not one of the twenty this section closes survived because its guard is weak. Every one survived
because **nothing in the fast suite could reach the method**: they sit behind `NativeService`,
`NativeService` held an `Arc<PrivateRuntime>`, and a `PrivateRuntime` cannot exist without a real
`pmux-rmuxd` sidecar, a real launcher socket and a completed rmux handshake. The only tests that
build one are the three in `crates/service/tests/native_service.rs`, and all three are `#[ignore]`d. A mutation run is how that was discovered rather than argued:
`wait_for_turn`'s safety guard read as `<` returns `daemon_lost` on the FIRST poll of every turn —
the whole of Path B failing on its happy path — and the entire suite stayed green.

The other two rows of the 22 were filed under the same word and are not the same problem: one is
behind a **clock** rather than behind a service, and one changes nothing observable at all. §9.5
says what each is.

### 9.2 The seam: eight methods, and a double that refuses

`SessionRuntime` (`crates/service/src/runtime.rs`) is the interface `NativeService` now depends on.
Its eight methods are **derived from the call sites** rather than from the type: they are exactly
what `native.rs` calls on its runtime, no more, and `PrivateRuntime`'s inherent copies were deleted
rather than kept beside the trait, so each of them is stated once. `runtime.rs` is not in the
mutation gate's `FULL_GLOBS`, so the seam itself adds no mutants to the measured set.

`ScriptedRuntime`, beside it and `#[cfg(test)]`, is the double. **A double that answers everything
plausibly makes every guard above it pass whatever the guard says**, which is this repository's
house bug class in its purest form, so each method has a refusing default and a scripting method
beside it:

| method | unscripted answer |
|---|---|
| `create_terminal` | `LaunchRegistration`, "nothing scripted a terminal for session …" |
| `probe_request_path` | `Err(ControlPlaneFault::Unreachable)` |
| `probe_launch_broker` | `ConnectFailed`, "nothing scripted a launch-broker probe" |
| `launch_broker_is_accepting` | `false` |
| `shutdown` | `Ok(())`, and counted |

`runtime_dir` is a real 0700 `TempDir` and `rmux_socket` a real path inside it, because the callers
that read them do filesystem work with what they are handed.

The tests are `crates/service/src/native/tests/seam.rs`, a child of `native`'s own `mod tests` and
therefore able to reach its private methods and private session map. **Everything except the
runtime is real**: the real `SessionRegistry`, real `SessionActor`s, the real `RmuxTerminalControl`
over a scripted `TerminalSession`, a real `FileTranscriptSource` over a real directory, a real
`AgentStore`, and — for the pool census — a real `Pool` with an empty warm set, which mints no
Claude. The double stops exactly at the process boundary. All 16 tests run in **0.5 s**.

**The refusing default is itself under test.** `a_start_whose_terminal_never_renders_a_prompt_…`
scripts a terminal into the same start that stops at `rmux_unavailable` with nothing scripted, and
the start gets its pane and stops one step later, at readiness, with the pane closed behind it. A
double that could only refuse would make every refusal above it unfalsifiable.

### 9.3 What it killed

Every one was proven the way this repository requires: the mutation applied to the working tree by
hand, the named test watched going red **by name**, the file restored by copying back the copy taken
before the edit, and the suite green again. All 21 were done one at a time, and `git diff` at the
end carries only the intended change.

| register row | test that now refuses it |
|---|---|
| `clear_boundary` `==`→`!=` | `a_clear_boundary_is_the_pair_of_the_generation_the_caller_named` |
| `attach` `==`→`!=` | `a_writable_attach_is_refused_when_the_backend_is_another_generations` |
| `clear_session` `>`→`<`, `>=`, `==` | `the_clear_deadline_domain_admits_its_own_top_and_refuses_a_synthesised_deadline_past_it` |
| `clear_timeout_ms` →`0`, →`1` | `the_default_clear_deadline_is_read_from_the_configuration` |
| `diagnose` `==`→`!=` (×2), `pool` →`None` | `a_diagnosis_names_caller_sessions_and_counts_pool_instances_without_naming_them` |
| `reap_idle_sessions` owner `==`→`!=` | `the_idle_reaper_expires_caller_sessions_and_enumerates_no_pool_instance` |
| `reap_idle_sessions` delete `!` | `a_session_whose_process_was_not_proven_reaped_survives_the_reaper` |
| `reap_idle_sessions` generation `==`→`!=` | `a_successor_published_while_the_reaper_was_in_flight_is_not_removed_by_it` |
| `close_session_with_state` `==`→`!=` | `a_close_removes_only_the_generation_it_named` |
| `shutdown` match guard →`false`, →`true` | `a_private_runtime_that_will_not_stop_is_reported_unless_a_close_already_failed` |
| `start_session_owned_with_retention` `==`→`!=` | `only_the_minified_cell_requires_a_tested_profile_to_start` |
| `wait_for_turn` `>=`→`<` | `a_turn_still_running_is_waited_for_rather_than_declared_lost` |
| `resolve_agent_reference` →`Ok(None)` | `a_start_naming_a_stored_agent_resolves_that_agents_configuration` |
| `unix_now_ms` →`Ok(0)`, →`Ok(1)` | `unix_now_ms_is_a_reading_of_this_hosts_clock` |

**The tally: of the 22 `seam` rows, 20 are now KILLED, one is EQUIVALENT and one is still
OPEN** (§9.5 for both). One further row closed with them — `unix_now_ms` → `Ok(1)`, whose
`closeable` was `cheap` rather than `seam` — so **21 rows moved from ACCEPTED to KILLED**, and the
register's census is now 83 KILLED, 17 EQUIVALENT, 30 ACCEPTED, 13 REMOVED across the same 143 rows.

**Then re-tested by the tool rather than by hand.** A filtered `cargo mutants` run over
`native.rs`, restricted to those functions, enumerated **37 mutants: 25 caught, 1 missed, 11
unviable, 0 timeouts** — every row above caught, and the single survivor is the `Drop` row §9.5
re-dispositions as EQUIVALENT. The receipt, with the argv that produced it and all three lists, is
`evidence/mutation-filtered-run-native-seam.json`. It is not a score: 37 of 1,653 mutants say
nothing about the other 1,616, and none is computed there.

**Three of these needed an interleaving, not an assertion.** The two generation fences in the reaper
and in `close_session_with_state` read the session map only on the far side of an `await`, so a test
that never moves anything in that window cannot tell `==` from `!=`. `CloseGate` holds a terminal's
close open, the test publishes a successor generation of the same session id while it is held, and
the fence is then asked the question it exists to answer. `wait_for_turn` is the same idea in time:
the turn is held open across the waiter's FIRST poll, because a turn that has already published its
outcome is returned by the check *above* the guard and would prove nothing about it.

**Two of them assert both directions,** because half a differential passes for a guard that refuses
everything: `only_the_minified_cell_requires_a_tested_profile_to_start` drives a real start of each
cell against a real executable printing an untested version, and the clear-deadline test admits the
top of the domain and refuses a synthesised value past it.

### 9.4 Two defects this work found in its own instruments

1. **A register row whose reason described a different guard.** The two `MatchArmGuard` rows on
   `NativeService::shutdown` are keyed at column 27, which is the arm that records a **private
   runtime** shutdown failure; their written reason described the close loop above it (column 20).
   The rows now say which they are, and both guards have a test:
   `a_private_runtime_that_will_not_stop_is_reported_unless_a_close_already_failed` for the arm the
   rows key, and `a_shutdown_reports_the_first_close_failure_and_not_the_last` for the loop their
   reason described — the second reads the first-closed session id out of an order the terminals
   record, because a `HashMap` walk is not an order a test may assume.
2. **The differential entry-path test read a test file as production.** `declared_functions()` cut
   each source file at its inline `mod tests {` — a rule that has no file-tree form — so
   `native/tests/seam.rs`, whose subject is the start funnel, was reported as two new **routes into
   admission** that `ADMISSION_ROUTES` had to classify. The scan now excludes the same module in its
   directory form, and checks the exclusion rather than trusting it: the owning file must declare
   `mod tests` under `#[cfg(test)]`, or the scan panics by name.

### 9.5 What this does not establish

* **The register's measurement is stale until a full-scope run is re-recorded.** `recorded_at.head`
  is `23e81db` and `native.rs` has moved, so `scripts/path-b-done.sh` reports criterion 1 NOT MET on
  drift — exactly as §8.1 predicted the price of closing these rows would be. The dispositions were
  moved on the evidence below; the score they belong to has not been re-measured.
* **The filtered run is not the full run.** It re-tested the mutants in `native.rs` whose function
  names match the seam's subjects and nothing else, so it says nothing about the other ~1,600.
* **`clear_timeout_ms` is stated as an accessor, not as a use.** The pool's clear is its only
  caller, and the deadline it computes is first read past `clear_and_rebind`'s transcript watch,
  which needs a rotating transcript on disk.
* **One `seam` row remains OPEN and is not this seam's.**
  `RmuxTerminalControl::interrupt`'s `<`→`<=` differs from the original only when the clock reads
  exactly the deadline instant; it needs an injectable clock in `driver_io.rs`, and the runtime seam
  does not reach it.
* **One row moved to EQUIVALENT rather than being closed.** `<impl Drop for SessionLifecycle>::drop`
  → `()` still drops both fields, in the same order the body uses; dropping the `oneshot::Sender`
  wakes the same `select!` arm that `request_shutdown`'s send wakes, and the arm discards the value.
  No observation distinguishes the two programs. It is expected to survive every run.
* **Exactly one seam test creates a terminal, and it stops at readiness.** So the publication half
  of `start_session` — a screen that renders, registration, the cleanup guard's success path — is
  still covered only by the tests that were already there, which are the `#[ignore]`d ones.
* **The filtered run was not driven by `scripts/gate-a-mutants.sh`.** It used that script's pinned
  toolchain, `mutants` profile, test-package set and pre-built candidate binaries, and it skipped
  its preflight, its evidence directory and its register check, because the register check is a
  statement about a full-scope run. So this is a tool result, not a gate cell.
* **Gate A was not run for this work, and neither was a full-scope mutation run.** Both are what
  criteria 1 and 4 need, and neither is claimed here.

---

## 10. The second certification of 2026-08-12

Section 8 was the first end-to-end run of the whole thing. This is the second, at `c94612d`, with
one difference that changes what it is worth: **the two long gates were re-run rather than quoted**,
and the live half was driven from a list of guards nobody wrote down.

### 10.1 The offline gates

| | |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean; the four `vendor/rmux-server` warnings are outside `--workspace` and are not `-D`-graded |
| `ruff check --no-cache .` / `ruff format --check --no-cache .` | All checks passed; 40 files formatted |
| `PMUX_E2E_BIN_DIR=$PWD/target/release bash scripts/gate-a-residue.sh` | passed, 8 candidate executables — **after** §10.6 |
| `python3 -m unittest discover -s tools/phase0/tests` | 261 tests OK |

### 10.2 The third full-scope mutation run, and a floor that moved up

`scripts/gate-a-mutants.sh` at `PMUX_MUTANTS_SCOPE=full`, in a pinned worktree at `c94612d`, on the
pinned 1.88.0 toolchain and `cargo-mutants 27.1.0`. **11,765 s — 3 h 16 m.** Evidence `run.6k6C2J`:

```
scope=full
enumerated=1661 unviable=505
caught=1120 missed=36 decided=1156
mutation_score_percent=96 minimum=93
```

**96, against the 93 the run was floored at and the 94 the register now defends.** The previous
full-scope run at `23e81db` enumerated 1,653 and scored 94; the Seam and Refuse phases added mutable
surface (1,661 enumerated, +8) and the score went **up two points**, which is the answer to the
question §8.1 could not ask: the new guards are not decorative.

**The floor was re-derived, not inherited.** The rule is the one `23e81db` established and is
restated in the register rather than re-invented: every mutant whose only catching test target is a
MEASURED drifter, or which was caught by a timeout and nothing else, counted as missed. The
*measurement* is this campaign's, read out of its own 1,661 per-mutant logs — `cargo test` without
`--no-fail-fast` stops at the first failing target, so each caught mutant's log names exactly one
catcher. **19 of 1,120 were caught only by a drifter target and 7 only by a timeout, giving 94**, up
from 93. Reading "the first catcher" as "the only catcher" over-counts drifter-caught mutants, which
pushes the floor *down* — the safe direction for a floor, and stated so rather than left as an
assumption.

**One survivor the record did not account for, out of 36.**
`crates/service/src/driver_io.rs:3669:35: replace - with / in read_rotation_anchor` — the CR strip
before the anchor row is parsed. Dispositioned **EQUIVALENT, and measured rather than argued**: a
probe test, written and removed, showed `serde_json::from_slice::<Value>` returns an *equal* value
with and without a trailing U+000D, because CR is JSON whitespace; `line` is read by nothing else,
and a row that is exactly `\r` is `unparseable_row` either way. The strip stays because it states
the intent, which is not a thing a test can assert.

The other 35 were all held, and **nothing regressed**: no row the register calls closed came back.
Census: **83 KILLED, 18 EQUIVALENT, 30 ACCEPTED, 13 REMOVED = 144**, and `register_position_drift`
is **0** because every row's `observed_at` was re-read from this run. Twelve rows this campaign
CAUGHT kept their dispositions and say so — see §10.2.1, which is why.

#### 10.2.1 A disposition that said "closed" on one sample, and the run that caught it

The first version of this register did move those twelve to **KILLED** — ten `ACCEPTED`, two
`EQUIVALENT` — on the reasoning that a mutant this campaign caught is a mutant that is dead.
`gate_b`'s own mutation cell, run at the commit carrying that register, **refused it**:

```
a mutant the register calls KILLED survived this run: crates/service/src/pool/evidence.rs:96:47: replace * with +
a mutant the register calls KILLED survived this run: crates/service/src/pool/mod.rs:1311:48: replace > with == in Pool::destroy
a mutant the register calls KILLED survived this run: crates/service/src/pool/mod.rs:1311:33: replace match guard retained.files > 0 with false in Pool::destroy
a mutant the register calls KILLED survived this run: crates/service/src/pool/mod.rs:957:48: replace < with <= in Pool::spawn_rewarm
register_regressed=4
```

Four of the twelve, missed by the `gate`-scope campaign at the **same commit**, within four hours of
being caught by the `full`-scope one. Scope does not change which tests run — only which files are
mutated — so nothing about the two runs differs except load and ordering, and three of the four are
in the campaign's own **drifter-only** list: their sole catching target was a real-resource test.

**KILLED in this register means CLOSED, and `check` refuses a closed row that comes back.** Writing
it on the strength of one run is the same promise-outruns-the-predicate shape this repository is
named for, committed in the instrument that exists to catch it — and §7.4's floor derivation had
already measured the phenomenon it ignores, in the sentence about nine mutants that flipped between
two runs of the same tree.

All twelve are restored to their prior dispositions, each carrying a sentence saying this campaign
caught it and why that is not a closure. **The rule, stated once: a row moves to KILLED when a named
test kills it, not when a run happens to catch it.** Both campaigns' `outcomes.json` were then
re-checked against the corrected register: `register_undispositioned=0`, `register_regressed=0`, in
`full` and in `gate`.

### 10.3 The live adversarial suite, driven from a derived list

`docs/path-b-adversarial.md` §14 is the run; the receipt is
`evidence/live-adversarial-suite-2.1.227-macos-aarch64.json`.

The thing worth carrying here is the shape, not the numbers. **The guards are not listed anywhere in
the harness.** It parses `COMPOSER_MODE_PREFIXES`, `COMPOSER_REWRITTEN_CHARACTERS`,
`COMPOSER_LINE_CONTINUATION` and every variant of `enum ComposerRefusal` out of the composer, and
every `return Err(DriverFailure::new(` out of `validate_prompt`, and **refuses to send anything**
unless its probes cover both sets exactly in both directions. Its whitespace sweep is the shipped
trim predicate's own domain — 30 characters — with the expected refusal for each computed from a
transcription of the two predicates, so a wrong transcription is a red probe rather than a probe
that agrees with itself.

**47 probes, 47 refused by the daemon with the predicted refusal, over both `pmux ask` and a
hand-framed request on the daemon's own socket.** Live: statelessness across `/clear`, the absent
tool surface, no subagent and **0 `isSidechain` rows**, NFD delivered, trailing whitespace trimmed,
U+200B kept, and **15/15 at the pool cap**. The ledger is byte-identical before and after —
`439e4853…f167153`, 1,200,199 bytes — because `pmux ask` reserves no ordinal.

### 10.4 Four things this found, and every one is the house bug class

1. **One prompt limit, stated six times, tied nowhere.** `bin/pmux/src/cli.rs` refuses an oversized
   prompt with *"CLI limit"* before the daemon's *"service limit"* can fire, and the two `1024 *
   1024`s — plus a third in `bin/claude-p/src/main.rs` and three more in test files — are six
   independent literals. `bin/pmux/tests/prompt_limit.rs` now **scans `crates/` and `bin/` for every
   declaration** and requires them to agree; it was proved red three ways.
2. **`target/release/pmuxd` was stale, and the first pass of the live suite measured it.**
   `cargo build --locked --release --workspace` changed the daemon's digest and left `pmux`
   byte-identical; a second build reproduced the new digest. `tools/gate-a/run_gate.py` refuses a
   stale release directory before its first cell using cargo's own depinfo — **nothing gives an
   ad-hoc live probe that protection**, and this is the second measurement in this repository taken
   against a binary nobody had checked. The 23 turns are counted in the receipt; their results are
   discarded.
3. **`{python}` is the driver's own `sys.executable`, and nothing checks it can import what the
   cells import.** The first six-phase run of this certification was launched with `python3` — the
   3.13 framework build, which has no `ruff` module — and produced **two product-shaped red cells**,
   `gate_a/python_ruff` and `gate_a/python_ruff_format`, twenty-six cells in, both saying *"No module
   named ruff"*. `ruff check --no-cache .` on the PATH `ruff` had passed minutes earlier, which is
   §8.0's shape exactly: a green sentence about a neighbouring command. The driver refuses an
   *unresolved* placeholder before the first cell and has nothing to say about a *resolved* one that
   cannot do the job. Re-run under `~/.pyenv/versions/3.12.4/bin/python` (ruff 0.12.4), which is what
   `tools/gate-a/README.md`'s invocation means by `python3` and does not say.
4. **The residue audit and the mutation gate cannot run at the same time.** With the campaign
   running, `scripts/gate-a-residue.sh` found `/tmp/pmux-observed-escape-…`, `/tmp/pmux-sidecar-loss-…`,
   `/tmp/pmux-performance-…` and `/tmp/pmux-soak-…` — every one a `pseudomux-service` fixture whose
   cleanup never ran because `cargo-mutants` killed the process testing a mutant. A clean run of
   `lifecycle_faults` at this head leaves none of them. `residue/gate_a_residue` is a Gate A cell and
   it scans a **shared** `/tmp`, so a Gate A run overlapping a mutation campaign is red for a reason
   that is not the commit. Sequencing, not a defect — but it is the kind of sequencing that has to be
   written down once rather than rediscovered.

### 10.5 Gate A and `gate_b`, and why their result is not in this file

Both were run in pinned worktrees at the commit this section lands as, after the mutation campaign
had ended and `/tmp` had been swept. **Their verdict is deliberately not written here**, and the
reason is §8.4's: a pinned receipt is bound to a commit and a tree hash, so any commit that carries
a Gate A number is a commit that receipt no longer describes. Writing "61/62" into the tree being
graded would make this paragraph the third quotation in this repository's history of a receipt for
some other commit.

The artefacts are `.context/gate-a/pinned-receipt-<label>-<commit>.json` — which is where
`scripts/gate-in-worktree.sh` now writes with no `--receipt` at all, and what
`--print-receipt-path` answers, so the spelling here is a reader's copy of one the runner owns —
and the thing that reads them is
`scripts/path-b-done.sh --gate-a-receipt`, which checks `describes_commit`, `tree_sha`, the inner
receipt's digest, its `manifest.sha256`, `source_unchanged`, freshness, and that the executed cells
cover all 70. **Run the script.** Its criterion 4 is the record; this section is not.

The one cell that may be red is derived and stays exactly one: a cell may be red only if **both**
§1's criterion-4 section and an **open** row of `docs/current-state.md` §9.4 name it, which is
`gate_f/linux_docker_self_tests`, granted by row **C6**.

### 10.6 What this certification does not establish

* **`gate_a/rust_fmt`'s lesson, one level up: nothing here re-measures Path A**, and Path A remains
  parked by owner decision.
* **The bug-class ledger did not get its thirty-fourth instance, and the reason is a cost.**
  §10.2.1's and §10.4's findings belong in `docs/current-state.md` §9's numbered series, and adding a
  heading there forces every site carrying *"has now found N times"* to move — including
  `crates/service/src/pool/mod.rs` and `crates/protocol/src/v1.rs`, both inside the mutation gate's
  `FULL_GLOBS`. That edit drifts the survivor register the moment it lands and costs the 3 h 16 m
  campaign again. **Deferred deliberately, and named here so it is a decision and not an omission.**
* **The pinned-worktree runner still has no disk pre-flight, and this session is why it wants one.**
  §8.6 recorded it as debt; here it cost a run. The second six-phase attempt died at cell 39 with
  *"No space left on device"* writing its own receipt, and reported ten consecutive product-shaped
  red cells before it did. Three worktrees — one mutation campaign and two Gate A runs, each with its
  own `target/` — took the volume from 28 GB free to zero. Freed by removing the finished worktrees
  and the main tree's 18 GB `target/debug`; not fixed.
* **The floor's drifter set is inherited.** The four target names come from `23e81db`'s derivation,
  not from anything this run measured; what is new is which mutants fall in them.
* **`performance_diagnostics::records_release_diagnostics_without_host_speed_thresholds` failed once
  under load** — *"owned resources remained live: terminals=1, transcripts=1"* — during a workspace
  run that shared the machine with the mutation campaign, and passed alone and on a re-run with the
  machine idle. It is a teardown race under contention, in the same family as the flake §10 of the
  adversarial document recorded, and it is not fixed.
* **`tools/phase0`'s suite failed once the same way** (one failure, one error, 261 tests) under the
  same contention, and passed twice afterwards. Neither failure was reproduced and neither is
  diagnosed.
* **Criterion 4 is a receipt per commit.** The verdict below is a verdict about one commit. The next
  commit takes it back to NOT MET with no cell having changed, and `--commit` does not recover it
  (§8.4).
* **`scripts/path_b_done.py` still has no unit tests**, and the pinned-worktree runner still has no
  disk pre-flight. Both stand from §8.6.
* **One host, one architecture, one Claude Code version.** macOS 15.7.7 / arm64, Claude Code 2.1.227,
  inside the promoted range `2.1.220..=2.1.227`. Nothing here is evidence about Linux, about x86_64,
  or about a version outside that range.

### 10.7 What remains for pmux overall, in the owner's order

Unchanged from §4.1 in order and in substance; re-costed against what this session measured.

1. **Linux.** `gate_f/linux_docker_self_tests` is still the one admissible red cell, still red on
   `test_linux_manifest_is_the_exact_ordered_candidate_projection` alone, still debt row **C6**.
   Re-projecting `tools/linux-docker/gate-a-manifest.json` from `phase-manifest.json` is the first
   act of picking Gate C back up, and it is the only thing standing between this repository and a
   Gate A with no admissible red cell at all.
2. **Non-Path-B docs.** Unchanged: `docs/archive/sandbox-spike.md` and `docs/archive/linux-handoff.md` carry
   unchecked line citations into `docs/path-b.md`; close that scanning gap first, then promote this
   file into `docs/path-b.md` §0.0 so its own citations start being graded. This session added a
   reason to want that: §10 and §14 are the two longest un-graded prose blocks either document has.
3. **The three unfiled `.context/rmux-issue-drafts/`.** Unchanged, and still cheap: rmux 0.10.0
   exists, this tree vendors 0.9.0, and both reported defects were still readable in 0.10.0's
   published source. The one line in the drafts that must be re-checked before sending is their
   "affected versions".
4. **Path A.** Parked by owner decision, untouched again. §4 item 6 is its list.

---

## 11. The third certification of 2026-08-12, and the two reds only a gate run could see

§10 called itself final and it was not; that word is gone from its heading, because a document that
grows a section is a document whose previous section did not get to be the last one. This is the
third end-to-end run, at the commit this section lands as, and it exists to settle one thing §10
could not: whether the answer is **reproducible by somebody who is not the person who ran it**.
§11.5 is that question, answered plainly, and the answer is not a clean yes.

It found two red cells on the way, in §11.1 and §11.1.1, and they have one shape between them:
**each was invisible to every check the commit that introduced it did run.** One needed
`cargo test --workspace`; the other needed Gate A itself, run pinned, which is the only context that
reproduces it at all.

### 11.1 `cargo test --workspace` had been red for four commits, and nothing said so

`crates/rmux/tests/vendor_server_patch.rs` walks the workspace and refuses any file the right to
spell one of the fourteen patch-owned regression names, so that no gate lane can go back to naming
them one at a time. Until `41abf4b` the files it excepted were two, written down:
`vendor/rmux-server/src/pane_io/tests.rs` and `vendor/rmux-server/PMUX-PATCH.md`.

`227063f` added `docs/rmux-upstream-state.md`, which records which vendored file the upstream repro
was copied from and quotes the libtest line naming it. `41a25a0` added
`docs/upstream-issues/02-rmux-server-attach-eof-drops-buffered-frames.md`, whose repro **is** one of
those regressions — its source, the `cargo test` line a maintainer runs, and the measured failure
output all spell the name. Both are upstream-facing documents; neither is a lane and neither can
become one. **Neither commit ran the gate.**

So `cargo test --workspace` was red at `227063f`, `41a25a0`, `d871596` and `4bc35ae`, and Gate A's
`gate_a/rust_tests` and `gate_a/rmux_server_vendor_patch` cells would have been red at all four.
That is §0's lesson arriving a second time from the other end: §0 was five reviewers who never ran
the gate, and this was four commits that never ran it. **A drafting session that lands prose can
redden a test cell**, and nothing in this repository's habits assumed prose could.

The repair is the derivation half of the house bug class, not an edit to the literal. The excepted
set is now two **boundaries** compared by prefix — the crate the patch patches, which owns the
names outright and no longer has to name the two files inside it, and the reports that quote them —
so a third file inside either needs no edit. The refusal message is built from the same derivation
it enforces. MEASURED both ways: the four `vendor_server_patch` tests pass, and a one-line file
planted under `docs/` is still refused, by a message that now reads *"the names belong under
docs/rmux-upstream-state.md, docs/upstream-issues, vendor/rmux-server and nowhere else"*. The
upstream half of that set is not derived and cannot be, for the reason `REGRESSION_LANES` already
gives about its own membership — only the address distinguishes a document that quotes a name from
a lane that restates one — and the comment says so rather than looking derived.

### 11.1.1 Nine self-tests that pass everywhere except inside the cell that runs them

`4bc35ae` added `tools/gate-a/tests/test_pinned_worktree.py`, nine tests over the pinned-worktree
runner's durability predicates, and said in its own words that Gate A had not been run and that
criterion 4 had never been watched go MET through the new default receipt path. That turned out to
be load-bearing, because **nine of those nine fail inside Gate A** and pass everywhere else.

The tests build a synthetic repository under `target/` and drive the runner at it. Their own
docstring explained why `target/`: the runner refuses a receipt anywhere under the temporary
directory, so the scratch must not be there. That reasoning is right and it stops one clause early.
`target/` is durable **relative to where the tree is checked out** — and the one thing that runs this
suite is `gate_f/gate_driver_self_tests`, inside a Gate A that is itself pinned, whose checkout the
runner puts under `$TMPDIR/gate-worktrees`. Inside a gate run the repository, its `target/` and the
synthetic repository below it are all under the host's temporary directory, and the runner refuses
every receipt asked of it — correctly, and for the exact reason the suite exists to check. A
durability predicate cannot inherit its answer from where somebody happened to check the tree out.

MEASURED in a real `git worktree` under `$TMPDIR`, not simulated, and re-measured at §12 by somebody
who trusted none of it: the nine go from **eight failures and one error** to **nine passes**. That
is the whole of what the probe grades, and the number it does *not* support is a suite total —
a bare probe also reddens the four `test_documented_surface` cells, which want built binaries that
no fresh checkout has and that Gate A's `--release-build` produces before the first cell. Sixty-five
of sixty-five is what the main tree says on an idle machine; under a concurrent workspace build
those same four go red there too. Every runner invocation is now handed a temporary root the suite chooses —
`TMP` and `TEMP` popped as well as `TMPDIR` set, because the runner consults all three — and a
`PMUX_WORKTREE_ROOT` outside the tree, since the two constraints pull opposite ways: a receipt may
not be under the temporary directory and a work root may not be inside the repository.
`assert_scratch_outlives_a_run` proves the arrangement of resolved paths in `setUp` rather than
trusting the spelling, so a receipt refused for its location can no longer read, from the
assertion's side, exactly like the predicate failing.

The general lesson is the one §11 opens with. **A test that is only ever run one way has only ever
been run one way**, and the two ways here differ by something no author would think to vary: the
absolute path of the checkout.

### 11.2 The offline gates, at the commit this section lands as

| | |
|---|---|
| `cargo test --workspace --no-fail-fast` | **1226 passed, 0 failed**, 66 test binaries and 6 doc-test targets — against exactly one failing test at `4bc35ae`, §11.1's |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0; the four `vendor/rmux-server` warnings are outside `--workspace` and are not `-D`-graded |
| `ruff check --no-cache .` / `ruff format --check --no-cache .` | All checks passed; 45 files formatted |
| `PMUX_E2E_BIN_DIR=$PWD/target/release bash scripts/gate-a-residue.sh` | passed, 8 candidate executables |

### 11.3 The register's currency: seconds, not three hours

`scripts/register_currency.py` answered **filtered, and empty**: `survivor_register_files_drifted=0`,
`survivor_register_stale_rows=0`, `survivor_register_stale_functions=0`, over 144 rows recorded at
`c94612d` and 1.1 days old. It did not enumerate mutants at all, which is rule 1's own stated
shortcut: identical bytes under `FULL_GLOBS` and an identical pinned tool enumerate identically.

That is the granularity rule working, and it is worth naming why it worked here rather than
recording only that it did. Nothing under `FULL_GLOBS` has changed since `c94612d` — the eleven
commits since it touched `docs/`, `evidence/`, `scripts/`, `tools/`, `bin/pmux/tests/prompt_limit.rs`,
`crates/service/tests/register_currency_self_tests.rs` and, in `41abf4b`,
`crates/rmux/tests/vendor_server_patch.rs`. Rule 2 is the one that could have fired: a KILLED row is
stale when the source of the test that decided it changes. Every `caught_by` in the register names a
target in `pseudomux-service`, `pseudomux-client` or `pseudomux-protocol`, and the derived sources
of those targets are lib modules and `crates/client/tests/*.rs` — none of them touched. There are
**no `undetermined` catchers** (`survivor_register_undetermined_catchers=0`), which is the row kind
that goes stale on any change to any file of any test package and would have fired on `41abf4b`.

The check therefore said so in seconds, which is what it was built to do. Had it demanded the
campaign, that would have been a finding about the granularity rule and is recorded here as the
thing that did **not** happen.

### 11.4 Gate A and `gate_b`, and where their result is

**This paragraph said "Run in pinned worktrees at the commit this section lands as" and no receipt
for that commit has ever existed. §12.1 is the correction and the finding.** What is true, and all
that is: the two gates are to be run through `scripts/gate-in-worktree.sh` with **no `--receipt`
argument at all**, so both land at the runner's own default —
`.context/gate-a/pinned-receipt-<label>-<commit>.json`, the path `--print-receipt-path` answers and
the path criterion 4's `remedy:` block prints. That default is new since §10 and is the whole of
what `4bc35ae` bought: the previous 5/5 was real and its receipt had died with the work root.

**Their verdict is deliberately not written here**, for §10.5's reason, which has not weakened: a
pinned receipt is bound to a commit and a tree hash, so any commit carrying a Gate A number is a
commit that receipt no longer describes. **Run the script.** Its criterion 4 is the record; this
section is not. Two receipts are needed and not one — Gate A is 70 cells across `gate_a`, `gate_c`,
`gate_d`, `gate_e`, `gate_f`, `residue` (62) and `gate_b` (8) — and naming only the first leaves 8
cells ungraded and criterion 4 NOT MET.

### 11.5 Could a third party re-run this and get the same answer? Partly, and here is the line

**Yes for everything the repository carries, and no for the two receipts.**

Reproducible from a clone, by command, with nothing else: §11.2's five offline gates; §11.3's
currency answer, because `evidence/mutation-survivor-register.json` and
`evidence/mutation-enumeration.json` are tracked and the rules are in `scripts/register_currency.py`
and `docs/register-currency.md`; criteria 1, 2, 3 and 5, all of which read tracked files or run
tracked tests.

**Not reproducible from a clone: criterion 4.** The receipts live under `.context/`, and
`.gitignore:20` ignores that directory — necessarily so, because `scripts/gate-in-worktree.sh`
refuses a receipt path inside the repository that `git check-ignore` does not accept, since a
tracked receipt dirties the tree and `scripts/path-b-done.sh` gives no verdict at all from a dirty
one. **So a pinned receipt is by construction never in the repository.** A third party with a clone
and no access to this host has exactly one route to criterion 4, and it is the honest one: run the
two pinned gates themselves, ~40 minutes and ~4 minutes, and the runner will write the receipts at
the same derived paths. What they cannot do is *check this run's* receipts, and what nobody can do
is quote them for another commit.

That is a narrower claim than "reproducible", and it is the true one. The thing `4bc35ae` fixed was
not repository-visibility — it was **durability on the host that ran it**: the receipt and its
evidence now outlive the worktree, so the same person, or anyone on this machine, can re-check the
digests a week later. `$TMPDIR` still holds seven pinned receipts from earlier sessions, six of
which name artefacts that no longer exist; those are what the default path exists against.

### 11.6 Corrections to §10.7's remaining-work list

* **Item 3 is done.** The three drafts are no longer unfiled and no longer in `.context/`:
  `docs/upstream-issues/01-rmux-client-attach-unbounded-slice.md`,
  `02-rmux-server-attach-eof-drops-buffered-frames.md` and
  `03-rmux-snapshot-revision-contract.md` are tracked, each reproduced against pristine upstream
  0.10.0. §11.1 is what filing them cost.
* Items 1, 2 and 4 are unchanged. `gate_f/linux_docker_self_tests` is still the one admissible red
  cell, granted by `docs/current-state.md` §9.4 row **C6** and by §1's criterion-4 section, both.

### 11.7 What this certification does not establish

* **Nobody has watched criterion 4 go MET through the new default receipt path before this run.**
  `4bc35ae` measured its refusals, its remedy and every predicate around it, and did not measure the
  green.
* **§11.1.1's repair was proven in a probe worktree, and NOT by the gate.** The probe is a real
  `git worktree` under `$TMPDIR` with the fixed file copied in — faithful, and worth naming as a
  separate step because the first attempt at reproducing the condition, by pointing `TMPDIR` at an
  ancestor of this tree, fixed eight of the nine and hid the ninth. The ninth was the work-root
  constraint, which only appears when the repository under test is the real one. This bullet said
  *"and then by the gate"*; §12.1 is why that clause is gone.
* **The four commits that carried the red were never gated, and this run does not retroactively
  gate them.** It measures the commit it lands as. `227063f`, `41a25a0`, `d871596` and `4bc35ae`
  remain commits at which `cargo test --workspace` was red.
* **`docs/repo-review.md:186` cites `crates/rmux/tests/vendor_server_patch.rs:576` for a
  `for required in [` that is not there** — a citation that had already rotted before this session,
  in a §3.5 finding that `d276b69` closed. Not fixed here; named so it is a decision.
* **One host, one architecture, one Claude Code version.** macOS 15.7.7 / arm64, Claude Code 2.1.227,
  inside the promoted range `2.1.220..=2.1.227`. No Linux run. Nothing here is evidence about Linux,
  about x86_64, or about a version outside that range.
* **The mutation campaign was not re-run and did not need to be** (§11.3). The 96 % and the 94 %
  floor are `c94612d`'s numbers, defended by a currency check rather than by a fresh campaign.
* **`scripts/path_b_done.py` still has no unit tests**, and the pinned-worktree runner still has no
  disk pre-flight. Both stand from §8.6 and §10.6.

---

## 12. The fourth certification of 2026-08-12, and a run nobody could find afterwards

§11 exists because §10 called itself final and was not. §12 exists for a sharper reason, and it is
the one this round was called to settle: **§11 reported a gate run whose receipts are not on this
host, and §11.1.1's repair was still uncommitted when §11 was written.** The previous certification
asked whether its answer was reproducible by somebody else. This one asked the narrower question
first — *is there anything on disk to reproduce it from* — and for §11's Gate A, the answer was no.

Nothing in §11 was invented. Its offline numbers re-measure exactly (§12.2), its currency reasoning
re-derives exactly (§12.3), and §11.1.1's defect is real and its repair works. What did not exist
was the artefact.

### 12.1 The finding: a past-tense sentence about a run that had not happened

§11.4, as committed, read *"Run in pinned worktrees at the commit this section lands as … so both
landed at the runner's own default"*. Four things were checked, and all four say the same:

| asked | answer |
|---|---|
| `.context/gate-a/pinned-receipt-*-5e9c3ba.json` | absent — no receipt names that commit |
| any receipt under `.context/gate-a/` whose `describes_commit` is `5e9c3ba` or `41abf4b` | none; the newest is `d871596`, and it is the `vendorcheck` probe |
| `$TMPDIR/gate-worktrees/` | two directories, both `b943c9e` — a commit that predates the file §11.1.1 is about |
| the raw gate receipts beside them | `b943c9e`, written before `4bc35ae` added that file at all |

So the sentence was written in the past tense about a run that had not produced a receipt, at a
commit that did not exist when the bytes were typed. **That is the house bug class aimed at a
document's own tense** — a sentence promising a measurement, with no predicate behind it — and it is
the same shape §11.1 found in a test scan and §11.1.1 found in a durability suite, three times in
one session, at three altitudes.

The structural cause is worth stating because it is not carelessness, and §10.5 had already walked
up to it and stopped one step short. §10.5 says no commit can carry its own Gate A *number*. The
stronger statement is the one that would have caught this: **no commit can carry any claim about its
own gate run, including that one happened.** At the instant a section's bytes are written, the
commit those bytes will land as does not exist, so there is no commit for a receipt to describe. A
section that says "this was run" is always writing a cheque against a future the commit itself
changes. §12.4 says what a section may honestly say instead, and says it in the imperative.

### 12.2 The offline gates, re-measured rather than inherited

Run in this tree at the content this section lands as, every one from a cold read of the command:

| | |
|---|---|
| `cargo test --workspace --no-fail-fast` | **1226 passed, 0 failed**, 73 test binaries (66 `Running`, 6 `Doc-tests`) — matching §11.2 exactly |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0; the four `rmux-server` lib warnings are outside `--workspace` and are not `-D`-graded |
| `ruff check --no-cache .` / `ruff format --check --no-cache .` | All checks passed; 45 files already formatted |
| `PMUX_E2E_BIN_DIR=$PWD/target/release bash scripts/gate-a-residue.sh` | passed, 8 candidate executables |
| `tools/gate-a/tests`, main tree, idle machine | 65 of 65 |
| `tools/gate-a/tests/test_pinned_worktree.py`, real `git worktree` under `$TMPDIR` | **8 failures + 1 error at `5e9c3ba`; 9 of 9 with the repair** |

The last row is the one that had never been run by anyone but its author, and it is now the second
person's measurement of it. The two middle numbers of §11.2 are reproduced to the unit, which is
the reason §12 treats §11's *reasoning* as sound and only its *artefacts* as missing.

### 12.3 The register's currency: filtered again, and the reason re-derived not re-quoted

`scripts/register_currency.py` answered **filtered and empty**, in seconds, and it did not enumerate
mutants at all. The reason is rule 1's own shortcut and it survives this commit for a reason worth
writing down: the two files this section's commit touches are `docs/path-b-verdict.md` and
`tools/gate-a/tests/test_pinned_worktree.py`, and **neither is under `FULL_GLOBS` nor is a source of
any Rust test target**. `FULL_GLOBS` is six entries over `crates/service/src` and `crates/protocol/src`;
a Python file under `tools/` is not a Cargo target's source under any of Cargo's layout rules, which
is what rule 2 derives its answer from. There are still no `undetermined` catchers, which is the row
kind that would have staled on any test-package change.

Had it demanded the full campaign for a Python test and a markdown file, that would have been a
finding about the granularity rule and this section would have said so. It did not, and this is the
second consecutive certification at which the rule earned its keep — three hours not spent, twice.

### 12.4 Gate A and `gate_b`: what this section may say, in the only tense that is honest

Not "they were run". This section is bytes in a commit that does not exist yet, and §12.1 is what
that sentence costs when it is written anyway. What a section may honestly carry is the
**imperative and the derivation**, so that the reader produces the artefact rather than trusting a
report of one:

```sh
bash scripts/gate-in-worktree.sh --commit HEAD --label gate-a --release-build \
  --prepare 'cd clients/typescript && npm ci' -- \
  python3 {worktree}/tools/gate-a/run_gate.py \
    --manifest {worktree}/tools/gate-a-candidate/phase-manifest.json \
    --workspace {worktree} --release-dir {worktree}/target/release \
    --validation-root {validation} --receipt {artefacts}/gate-a-receipt.json \
    --phase gate_a --phase gate_c --phase gate_d --phase gate_e --phase gate_f --phase residue
```

`gate_b` is the same invocation with `--label gate-b`, `--phase gate_b`, and the two nightly tools
`--tool nightly_cargo=…` / `--tool nightly_rustc=…` its cells resolve. **No `--receipt` on either**,
so each lands at the runner's own default, which is the one place the convention is written down and
the path `--print-receipt-path` answers.

Two receipts and never one: Gate A is 70 cells, 62 across `gate_a`, `gate_c`, `gate_d`, `gate_e`,
`gate_f` and `residue`, and 8 in `gate_b`. Naming only the first leaves 8 cells ungraded and
criterion 4 NOT MET — and prints the `remedy:` block that names the second.

**The verdict is `scripts/path-b-done.sh`, and it is not in this file.** The one cell that may be
red is derived and stays exactly one: a cell is admissible red only when **both** §1's criterion-4
section and an **open** row of `docs/current-state.md` §9.4 name it, which is
`gate_f/linux_docker_self_tests`, granted by row **C6**.

### 12.5 Could a third party re-run this and get the same answer?

§11.5 answered "partly", and split it correctly: yes for everything the repository carries, no for
the receipts, because `.gitignore` ignores `.context/` and the runner refuses a receipt path git does
not ignore — a tracked receipt dirties the tree and the done-gate gives no verdict at all from a
dirty one. **A pinned receipt is therefore by construction never in the repository.** That analysis
stands and §12 does not weaken it.

What §12 adds is the failure mode §11.5 did not consider, because it was reasoning about a third
party on another host and the receipts were missing on *this* one. The honest formulation has two
levels, not one:

* **Re-runnable**: yes, by anyone with a clone, for all five criteria — criterion 4 by running the
  two commands in §12.4, roughly forty minutes and a few minutes, after which the runner writes the
  receipts at the derived paths. This has always been the real route and it is the only one.
* **Re-checkable**: only on the host that ran it, only while the receipts survive, and **only if
  they were ever written**. §12.1 is a case where a certification claimed the first and delivered
  neither.

The distinction matters because durability is not the same property as existence, and `4bc35ae`
bought durability. A receipt that outlives its worktree is worthless if no run ever produced it, and
nothing in the tree could tell the two states apart — the absence reads exactly like a receipt that
was reaped. That is what makes §12.1 a defect in the document rather than in the runner.

### 12.6 What this certification does not establish

* **This section cannot report its own gate run, by construction, and does not try.** §12.4 is an
  imperative. Whether the run that follows this commit came back green is in the receipts and in
  `scripts/path-b-done.sh`, and a reader who wants that answer runs the script. A future section
  that reports it will be reporting somebody else's commit.
* **§11's Gate A conclusions are neither confirmed nor refuted here.** No receipt for `5e9c3ba` or
  `41abf4b` exists to check, and this commit does not retroactively gate either. What is measured is
  that the artefact is absent, not that the run was fabricated — §11.1.1's defect is real and
  reproduced independently in §12.2.
* **The four commits that carried §11.1's red are still red commits**, and now so is the question of
  what `5e9c3ba` and `41abf4b` measured. Six commits in this range have no pinned receipt.
* **The `test_documented_surface` contention is understood but not fixed.** Four of that file's
  cells go red when a workspace build is rewriting `target/debug` underneath them; they pass alone
  and on an idle machine. Gate A does not hit it because `--release-build` completes first, so it is
  a self-test hazard rather than a gate one, and it is named rather than repaired.
* **`scripts/path_b_done.py` still has no unit tests**, and the pinned-worktree runner still has no
  disk pre-flight. Both stand from §8.6, §10.6 and §11.7. The disk one is not academic: this host
  had 28 GiB free when this section was written, and §10.6 records three concurrent worktrees taking
  it to zero.
* **One host, one architecture, one Claude Code version.** macOS 15.7.7 / arm64, Claude Code 2.1.227,
  inside the promoted range `2.1.220..=2.1.227`. No Linux run.
