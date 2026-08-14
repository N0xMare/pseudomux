# Whole-repo review — HEAD `62426c8`

**This is the third version of this document, and the first written on a complete adjudicated set.**
The version at `9ae486e` reported 108 findings with a 0% kill rate and was rejected as
rubber-stamping. The version at `62426c8` was written when the probe phase failed to launch and
arrived empty; its author refused to rank on missing data, re-measured 31 claims by hand instead, and
said so. **That hand-measured work is carried forward here, not discarded** — every one of its
findings was re-checked against the full probe set, and the four places where the two disagree are
resolved in the open, with the winner named.

This version has what that one lacked: all 108 findings re-probed with a mandatory reproduction
artifact, and every critical/high survivor sent to an independent adjudicator asked whether it matters
and whether the severity is honest. **Two of the previous version's four MUST FIX rows were
downgraded by that adjudicator, and I verified the downgrade myself** (§4 S1). Its two refutations
survive (§6).

---

## 1. Verdict

**Yes, this repo is in good shape, and the shape of what is left is unusually consistent: the product
code holds and the layer of guards, receipts and documents around it is where the defects live.**
Ninety-eight distinct defects survived adjudication; **five are high and none is critical**, and of
those five, three are traps that fire on a routine future edit rather than errors a user hits today.
Only two things are wrong for a user right now: `pmux doctor` reports `healthy` on a host whose Claude
version the very next command refuses, and every piped `claude-p` invocation arms a turn with a
trailing newline the composer cannot hold. Both are cheap. The Path B pool's two shutdown races,
which the previous version ranked as the top must-fix items, were reproduced again and then narrowed
by the adjudicator to a scheduler window inside an already-in-progress daemon stop — real, one-line
fixes, but not the shipped-correctness emergency they were ranked as. Everything else is the house
bug class at scale: a hand-written set that has already stopped covering its subject (nine sites), a
message that promises more than its predicate tests (dozens), and roughly half of the docs'
line-number citations no longer resolving. The single highest-leverage open item is still the
citation lint debt row 36 proposes; nothing else pays back as much per line.

---

## 2. Method, and what it cannot tell you

Twelve finders produced **108 findings**. The first verification pass returned 104 CONFIRMED / 0
REFUTED, with two verifiers confirming 22 of 22 blind, and was rejected — a review that refutes
nothing is a transcription of its finders. The re-probe ran in batches of six with a mandatory
reproduction artifact per finding; every critical/high survivor then went to an independent
adjudicator asked two questions: does it matter, and is the severity honest.

| outcome | n | share |
|---|---|---|
| **OVERSTATED** — claim, number or consequence measured false | 16 | 15% |
| **DOWNGRADED** by the adjudicator — real, severity was not | 11 | 10% |
| CONFIRMED, severity unchallenged | 69 | 64% |
| **UPHELD** at critical/high after adjudication | 5 | 5% |
| already fixed this session, verified closed | 7 | 6% |

**Kill-or-correct rate: 27 of 108, 25%** — 27 of the 101 not already fixed, 27%. Against the pass this
replaces, that is 0% → 25%.

**No finding was refuted in its entirety.** Eight of the sixteen OVERSTATED had their stated *failure
mechanism* disproved by measurement while the underlying fact survived — the client-frame decode
(#7), the `wait_ms` literal (#8), `response_result_name` (#13), the agent-start corpus gap (#21), the
paste-injection Cf copy (#33), the vendored-server sweep (#56), `NAMED_EFFORTS` (#62) and the Path B
door list (#63). The other eight were severity or scope corrections. All sixteen are in §6 with the
reason.

**Duplicates merged: 10.** 108 findings are **98 distinct defects**. The e2e environment oracle was
found twice (#5, #102), `Surface::ALL` twice (#23, #50), the MCP `clear_session` gap twice (#25,
#48), the `/clear` cost twice (#81 ⊃ #94), the stale gate-driver size twice (#75, #93), the bug-class
counter twice (#71, #95); the README was found four times and the ledger budget twice, and both were
already fixed.

Six things this method cannot do, stated because they bound everything below.

- **It spends no tokens.** Every claim whose last step is a live Claude turn stops at the code path
  (§7). The host's Claude Code is 2.1.223 against a sole promoted 2.1.220, so even a free-looking
  `ask` is refused before the model call.
- **It compiles one target.** `aarch64-apple-darwin` only. Roughly 30 `target_os`/`cfg(not(unix))`
  blocks are never compiled here, and §4 S2's mutation-receipt defect is a direct consequence.
- **It ran no gate end to end.** No Gate A pass, no cargo-mutants campaign, no Linux Docker lane.
- **The adjudicator saw only critical/high.** The 69 plain CONFIRMED mediums and lows carry their
  prober's severity, not a second opinion. Where a prober's own reasoning undercut its severity I
  have said so in place.
- **It re-measured citations, not reasoning.** Every `path:line` published below was re-resolved at
  this HEAD; several of the previous version's had rotted in the four commits since, which is itself
  §5's subject.
- **The tree was never modified.** Reproductions ran in `/tmp` copies or scratch probe crates, all
  removed. `git status --short` is clean apart from this file.

---

## 3. Must fix

Adjudicated high, UPHELD. None is critical. Ordered by severity, then cheapness.

### 3.1 `claude-p` does not drop the trailing newline `pmux` measured as fatal, and a test pins it

`bin/claude-p/src/main.rs:176` — **HIGH, cheap.**

The facade's entire prompt normalization is `let prompt = prompt.replace("\r\n", "\n").replace('\r', "\n");`.
`pmux` did one more thing there — a `strip_suffix('\n')` under a comment stating the failure as
measured — and **`48aee00` deleted it**: both share `normalize_cli_prompt`
(`bin/pmux/src/cli.rs:1884`) now, and the comment above it retracts the one-newline rule by name. Nothing downstream compensates: `validate_prompt`
(`driver_io.rs:429`) calls `normalize_prompt` (`engine.rs:1147`), whose own doc says whitespace *"is
never trimmed"*, and the actor arms verbatim at `v1/actor.rs:2916`.

**Reproduced against the real engine**, not by reading: arming `"Reply with exactly: ok\n"` against a
typed row of `"Reply with exactly: ok"` returns `Err(UnexpectedTypedPrompt { .. })`. Commit `3e1a699`
records the identical mechanism observed live against 2.1.220 — *"the next run with this fix returned
outcome=completed"* — and that commit touched `bin/pmux/src/cli.rs` and nothing else.

`echo q | claude-p` is the canonical invocation and the reason the facade exists. It is **pinned in
the failing shape** at `bin/claude-p/tests/facade_blackbox.rs:1139-1143`, which asserts the trailing
`\n` reaches the daemon, against a canned server that cannot observe the death it enshrines.

**Fix:** move `pmux`'s exactly-one-newline rule into `crates/client` and call it from both; invert the
blackbox assertion. Few lines. **UNVERIFIED:** the death itself on a live 2.1.220 host (§7).

### 3.2 The error-code list is the one manifest pin still hand-written, and both clients hard-fail on a code they do not know

`crates/protocol/tests/v1_conformance_vectors.rs:223` (`let all_error_codes = [`), asserted at `:259` — **HIGH, cheap.**

Methods, results and events are compile-enforced through `wire_tags!` (`:436`), whose doc comment at
`:118-134` records that this exact hole once shipped `Request::RunStateless` invisible to both
clients. Error codes were left as a 34-element literal. `error_code_name` (`:79`) is exhaustive, so
appending an `ErrorCode` forces one arm 140 lines away from the array — and nothing else.

**Reproduced on a clean `git archive` copy:** with a 35th variant added, `cargo test -p
pseudomux-protocol` is green across all five test binaries and `manifest.error_codes` stays at 34.
The consequence is deterministic, not hypothetical: `clients/python/pmux_client/client.py:1146` and
`clients/typescript/src/client.ts:366` both throw on an unrecognized code, and both pin only against
the manifest (`clients/python/tests/test_client.py:491`,
`clients/typescript/tests/client.test.mjs:292`). So the new daemon error frame is rejected by both
shipped clients, masking the real error. `crates/service/src/pool/refusal.rs:18-24` shows a code
append is actively contemplated, with the safe migration order recorded only as prose.

**Fix:** apply the existing `wire_tags!` macro to `ErrorCode` and delete `error_code_name`. The macro
is in the same file and proven on the other three lists; the manifest content is already in sync, so
no regeneration is needed.

### 3.3 `pmux doctor` reports `healthy` on a host no promoted profile admits, holding both operands

`bin/pmux/src/cli.rs:280-285`, `bin/pmux/src/main.rs:1004` — **HIGH, moderate.**

The help text promises *"validate the socket, the daemon's health tree, the working directory, and the
Claude executable"* and *"Exits 0 only when every check it lists both ran and passed."* The predicate
for the executable is `resolve_executable(Some(claude))`, which canonicalizes and checks
`is_file()`/`0o111` and never runs `--version`.

**Reproduced twice independently, and end to end.** A live daemon returned `"status": "healthy"`, exit
0, carrying `"claude_executable": ".../2.1.223"` and `promoted_profiles[0].claude_version: "2.1.220"`
in one JSON; the very next `pmux run` on the same daemon and cwd exited 1 with
`UnsupportedClaudeVersion: Claude Code 2.1.223 has no tested pmux compatibility profile`. The README
work reproduced the same sequence for `ask`. This is the ordinary first-run state on the supported
platform: Claude Code auto-updated past the promoted cell.

The daemon is honest about it — `native.rs:2938-2940` says in its own words that *"A pool whose Claude
is one patch version off every promoted cell passes this layer and fails every mint"* — so the
over-reach is the CLI's alone, while the daemon already computes the operand
(`detect_claude_version`, `native.rs:4253`, called at `:1567`).

**Fix:** compare the resolved version against the `promoted_profiles` already in the fetched
diagnosis; emit `fail` (or `unproven` if unreadable) naming the installed version, the admitted set,
and `--compatibility allow-untested` / `--tested-claude-profile`. Moderate: doctor must spawn
`claude --version` or ask the daemon, and the exit-code contract change touches Gate A cells.

### 3.4 The conformance manifest structurally cannot pin any internally-tagged wire union

`crates/protocol/tests/v1_conformance_vectors.rs:460`, asserted at `:724` — **HIGH, not cheap.**

`wire_values!` (`:436`) requires `.as_str().expect("v1 value enums serialize as plain strings")`, so
the `value_enums` section can never represent a tagged union. Six wire unions are therefore pinned in
no language: `SessionIdentity`, `ConfigSource`, `SystemPromptPolicy`, `LifecycleMode`,
`RetentionPolicy`, `MessageBlock`. Both clients hand-write their shapes and their golden tests compare
only `value_enums` (`clients/typescript/tests/golden-conformance.test.mjs:570`,
`clients/python/tests/test_golden_conformance.py:578`).

**Reproduced on a clean copy:** adding a `SessionIdentity::Adopt` variant leaves every protocol test
binary green (1/3/8/3/66, 0 failed). Same hole shape as §3.2, and the `wire_tags!` doc records it
having already shipped twice on this surface.

**Fix:** a `tagged_unions` manifest section derived through `wire_tags!` (whose wildcard-free match
makes an unlisted variant a compile error), asserted in Rust and compared by both clients exactly as
`value_enums` already is. Not cheap: one manifest section plus three test files across three
languages, though every mechanism exists.

### 3.5 The fourteen patch-owned regression names are six hand-written copies derived from nothing

`crates/rmux/tests/vendor_server_patch.rs:576` (`for required in [`) — **HIGH, not cheap.**

The gate is one-directional: `assert!(patch_document.contains(required))` over names hardcoded in the
test, never reading `vendor/rmux-server/src/pane_io/tests.rs` for its inventory. Six independent
copies: this test, `PMUX-PATCH.md`'s bulleted list and its count word *"fourteen"*, `docs/testing.md`,
`tools/gate-a-candidate/phase-manifest.json` (14 cells), `tools/linux-docker/suite.sh` (14), and
`tools/linux-docker/gate-a-manifest.json` (14 gate names).

**The derivation exists and nothing runs it.** Re-run twice, using the block delimiters the test
already owns in `reconstruct_upstream_pane_io_tests` (`:472-500`): the removed blocks contain exactly
14 `#[tokio::test]` fns plus 2 plain helpers, matching all six copies — currently in sync, derived by
nobody. **The trap is mechanical:** every macOS-manifest and `suite.sh` invocation against
`vendor/rmux-server/Cargo.toml` uses `--exact`, and the only whole-crate cell is `cargo check
--all-targets`. A fifteenth regression would compile in every lane and execute in none, while the
`patched_pane_io_tests_sha256` fixture forces only a hash bump that points at nothing.

**Fix:** extract the added `async fn` identifiers from the two removed blocks, assert the derived set
equals `PMUX-PATCH.md`'s list and that the count word spells its cardinality; then replace the 28
`--exact` cells across the two manifests with a module filter plus an executed-count assertion. The
derivation is ~20 lines; rewriting the manifests is the work.

---

## 4. Should fix

Downgraded items and confirmed mediums with a real cost. Ordered by severity, then cheapness.

### S1 — The two Path B pool races. **Downgraded from the previous version's top two MUST FIX rows; the adjudicator wins, and here is why.**

Both were reproduced again, and both fixes are one line each.

**`crates/service/src/pool/mod.rs:1069`** — `let instance = &state.instances[&slot];`, reached after
`self.host.clear(&handle).await` released the lock. A probe crate driving only the public `Pool` API
panicked at exactly `1069:52` with `no entry found for key`. **Fix:** the fallible form the same
function opens with, eight lines above — `let Some(instance) = state.instances.get(&slot) else { return; };`.

**`crates/service/src/pool/mod.rs:863`** — the handle write after `host.mint` has **no `else`**, and
`destroy`'s `None => true` arm at `:1199` then erases the epoch root on the assumption that no process
was launched. A probe parking `host.mint` across `pool.shutdown()` measured `mints=1 destroys=0
leaked=0 trees=[]`. **Fix:** an `else` that destroys or leaks the handle it cannot record.

**Why they are medium and not high.** The previous version argued the window is *"real and not
narrow"*, because `run_stateless` reaches `pool.run` at `stateless.rs:450` with no `start_guard`. That
is true of *admission* and not of the *launch*. I read the fence myself: `NativeService::shutdown`
opens by taking `start_guard` (`native.rs:2381`) before setting `shutdown_started`, and
`start_session_owned` holds that same guard for its whole body (`native.rs:1433`) — and
`NativeInstanceHost::mint` (`stateless.rs:235-248`) *is* `start_session_pool` → `start_session_owned`.
So shutdown blocks until an in-flight launch completes. What remains is the tail of `Pool::mint` after
`host.mint` returns: `if let Some(pid) = handle.pid` — which is always `None` for Path B by design
(`stateless.rs:258`, *"The pid is not observable from a `SessionHandle`, and it is not invented"*) —
then one lock acquire, racing a shutdown that must still drain the idle reaper first. For `:1069`, the
production `clear` resolves as `Err` once shutdown force-closes the session, and the `Err` branch is
fully guarded. When either does fire: one caught task panic on stderr during an already-in-progress
stop, or one `Internal` refusal to a caller who is in refusal territory anyway. **The mechanism is
the previous version's; the scope is the adjudicator's.**

### S2 — Mutation-gate integrity: the score is measurably over-stated, and the mechanism that over-states it is still live

Four findings, one story. Cheap in three parts, expensive in one.

- **The run of record contains a provably wrong verdict.** `.context/gate-a-mutants/dead-code-pass/caught.txt:318`
  scores `crates/service/src/claude_launch.rs:964:5` CAUGHT. That line is inside `#[cfg(not(unix))] fn
  resource_key` — never compiled on macOS, so the mutant binary is byte-identical to baseline. Its
  structural twin at `:1216` is `MissedMutant` **in the same run**, and `964` is `MissedMutant` in all
  eight other archived runs. Corrected: 577/600 = **96.17%**, not the published 96.33%. Floor is 94; no
  gate decision changes. *(The probe corrected the finder's arithmetic here — it is not 95.9%.)*
- **The likely mechanism is still live.** `grep -c start_paused crates/service/src/*.rs` is **0**
  across all 17 files. Eight `RotationFixture` tests drive real 60–1500 ms timeouts through real OS
  threads; measured 24 of 24 concurrent runs red on exactly that set, 2 of 8 red at 8×, 12 of 12 green
  at 4×, with an observed `waited_ms` of 1578 against a 1_500 ms bound. `scripts/gate-a-mutants.sh`
  runs this binary once per mutant. The join to `:964` is inference (§7). `native.rs:7058` is the same
  construction with a 20× margin and did not reproduce.
- **The cure is derivable.** Have `scripts/gate-a-mutants.sh` refuse CAUGHT for any mutant whose span
  falls inside a block gated on a `cfg` this host does not satisfy. ~20 lines; it would have refused
  this receipt.
- **The priority product's integration half is out of scope entirely.** `scripts/gate-a-mutants.sh:112`
  `FULL_GLOBS` is six entries covering 14 files / 24,576 code lines against 60 files / 52,717 —
  `crates/service/src/stateless.rs` (698 lines, *"The integration: the stateless pool over a real
  `NativeService`"*) is absent while `pool/**` is fully covered. `mutated_packages` at `:342` is a
  second hand-written list that must agree with `FULL_GLOBS` and is derived from nothing. Cheap to
  add, expensive in wall time, and it may cost a one-time dip toward the floor.

### S3 — Hand-written sets that have **already** stopped covering their subject

Nine sites, each green today only because the thing it misses has not been exercised. All re-resolved
at this HEAD.

| site | promises | measured |
|---|---|---|
| `bin/pmux/tests/cli_contract_matrix.rs:40` `const ALL: [Self; 11]` | five `*_for_every_command` tests | **11 of 13** `Command` variants (`cli.rs:141`). `ask` and `agent` have no coverage for secret-free rendering, malformed-frame rejection, exit-2 or exit-1. Probed live: both behave correctly today. Guard is ~10 lines against `Cli::command().get_subcommands()`; wiring the surfaces in is the work |
| `crates/service/tests/paste_injection.rs:431` | docstring `:419`: *"whitespace AND every Unicode format character (category Cf)"* | **108 of the guard's 170** code points, used as a `continue` filter (`:434`), so a corpus case outside it is silently never asserted. Measured: with U+061C added to the corpus and removed from the guard, the whole suite is green |
| `crates/service/src/driver_io.rs:546` `is_ignorable_prompt_prefix` | *"every Unicode format character (general category Cf)"* | exactly Cf **today** (170 = 170, verified against `unicodedata` 15.1) and a frozen snapshot with nothing re-deriving it. Deleting the U+0890..0891 arm — itself a recent Unicode addition — leaves the suite green |
| `crates/e2e/src/lib.rs:39` `TEST_TRANSPARENT_EXACT_KEYS` | `:50-58`: *"deliberately an INDEPENDENT oracle … so the full-stack lane proves the shipped child never sees the name"* | **10 of the policy's 11** (`launch_environment.rs:98`). Exactly `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`. Prefix and subscription mirrors match exactly. Backstop: `reject_team_markers_reaching_child` (`claude_launch.rs:1151`) fails closed in the shipped daemon |
| `crates/e2e/src/bin/pmux-test-claude.rs:39` `FORBIDDEN_FLAGS` | end-to-end proof no driver-owned flag reaches the child | **8 of the daemon's 11** (`claude_launch.rs:32-50`). Only `--no-session-persistence` is a real omission — whose documented consequence is that the child stops writing the transcript; `--session-id`/`--resume` are consumed by `parse_identity` by design. *(Corrects the previous version's row, which named all three.)* |
| `clients/python/pmux_client/client.py:832` | request-identity validation | **6 methods**. `clear_session`, `run_stateless` and all four agent methods fall through unvalidated — measured directly. `clear_session` carries three canonical UUIDs including the `expected_transcript_session_id` fence. The *result* side at `clients/python/pmux_client/client.py:922-926` does include it, so only the request-side literal went stale. `client.ts:1069` is a `switch` with no `default` and no `never` assertion |
| `bin/pmux-mcp/src/tools.rs:385` `map_tool_call`, pinned by a 13-name literal at `bin/pmux-mcp/src/tools.rs:1288-1301` | the MCP surface | **13 of `Request`'s 16**. `ClearSession` and `Diagnose` are unreachable while `bin/pmux-mcp/src/tools.rs:844` advertises `"cell": {"enum": ["full","minified"]}` — measured from the running binary. Path B over MCP is unaffected (`run_stateless` clears internally); a minified Path A session can only be recycled by close-and-restart |
| `tools/gate-a/tests/test_run_gate.py:812` `for area in ("crates", "bin")` | *"every statement of the bug-class counter"* | **4 statements in 3 Rust files**, under a docstring saying *"ONE number restated in four Rust files"*. `docs/linux-handoff.md` and `docs/current-state.md` both spell the ordinal and are outside the scan — proved by rewriting the handoff's copy and watching the test stay green, then rewriting `v1.rs`'s and watching it fail |
| `tools/gate-a/run_gate.py:62-65` | `:265`: *"Hash the canonical source tree"* | **CLOSED.** It omitted `.gitignore`, `.dockerignore`, `LICENSE-*` and `evidence/` and hashed 10 gitignored `.DS_Store`; `source_files` now derives the set from `.gitignore` and matches `git ls-files --cached --others --exclude-standard` exactly, 953 files, asserted by `test_the_source_digest_is_exactly_what_the_repository_calls_source`. Proved against the real `source_digest`: rewriting `.dockerignore`, `LICENSE-MIT` and `evidence/README.md` between two calls leaves the digest byte-identical and `source_unchanged: true`. `.dockerignore` is the allowlist deciding what enters the Linux evidence image |

`tools/promotion/` (1,066 lines producing the receipt a runtime refusal message cites) has no
self-tests and no `gate_f` cell, and the cell list it would join is itself parsed out of
`docs/testing.md` prose rather than derived from `tools/*/tests` on disk. `tools/crash-harness/` is a
Rust crate that is neither a workspace member nor excluded, so no `--workspace` invocation reaches it;
it depends on three first-party crates by path and compiles today in 17 s, which is the only reason
it is a risk and not a defect.

### S4 — Latent hand-written sets, in sync today

Same class, no current gap; each is one edit away from one. `crates/protocol/tests/v1_launch_environment.rs:35`
manifests the seven allowlist tables by hand under a module doc that calls an empty prefix *"a
security defect rather than a cosmetic one"*. `crates/service/tests/actor_model.rs:1782` builds its
capture corpus from five literal `include_str!`s where the sibling corpus next door
(`screen_corpus_replay.rs`) reads the directory and asserts non-empty — and `native.rs:8447` names the
`include_str!` list as *"the defect this test exists to prevent"*. `scripts/gate-a-fuzz.sh` restates
the three fuzz-target names four times, plus the Dockerfile and `fuzz/README.md`; a fourth `[[bin]]`
added to a `/tmp` copy left all 43 Gate A tests green. `pool/machine.rs:624` `ALL_STATES` and
`ALL_TRANSITIONS` under a comment conceding *"The matrix above is only exhaustive if these lists
are"*: a tenth state added to the enum and to all four wildcard-free matches leaves 13/13 tests green.
`pool/class.rs:512` `NAMED_EFFORTS`; `hybrid_hooks.rs:41` `ALL: [Self; 3]` against `pmux-hook`'s
`EventArg` (`main.rs:35`) with a third copy in the test file; `pool/refusal.rs:962`'s census anchored
to column-zero `pub fn` under a doc claiming *"Every `pub fn` in this file"*; `stateless.rs:641`'s
copy of `CONFIG_ROOT_ENV_DOORS`.

### S5 — Analysis and diagnostic fidelity in `native.rs` / `driver_io.rs`

Five confirmed mediums, all cheap, none reachable by a user.

`native.rs:2699` `session_is_still_offered` is a hand-listed four-state `matches!` under a doc saying
*"This is deliberately not a hand-listed set"*; moving `Draining` into the actor's non-retryable
column leaves 391 + 58 + 9 tests green while `pmux doctor` keeps reporting `TerminalPresent`.
`native.rs:8447` `declared_functions` promises *"outside a `mod tests` block"* and cuts on the literal
`\nmod tests {`, which `v1/actor.rs` never contains — 24 test fns are scanned as production, proven by
planting a bait fn and watching the admission-route table demand it be classified. `native.rs:3422`
`close_unpublished_terminal` is a `#[cfg(test)]` reimplementation of production startup cleanup and the
only thing two `failed_start_terminal_*` tests exercise; mutating production's real tail — code,
message and retryable flag — left all 391 lib tests green. `driver_io.rs:362` `ScreenGeometry` says
*"this type recomputes nothing"* and re-derives `last_rendered_row` with a second copy of
`active_editor`'s scan; changing only `active_editor` publishes a geometry production would have
rejected, with the corpus replay green. `driver_io.rs:255`'s cursor-less Ready fallback is
`#[cfg(test)]`, so an external crate linking the library classifies the unit tests' own Ready fixture
as `Unknown` — three functions and four assertions exist for a branch no shipped path contains.

### S6 — The `/clear` cost: refuted in one module, still stated in four

`pool/config.rs:51-61` carries the correction — the *"~30 ms"* is transcript rotation, not the
caller-visible clear, which MEASURED **703–756 ms, median 730**. Applied at `config.rs:51`,
`pool/machine.rs:154`, `pool/refusal.rs:630`. Not applied at `native.rs:1884` (the doc comment on the
public `clear_session` entry point), `v1/actor.rs:898`, `tests/path_b_pool.rs:583`, and
`docs/path-b.md` §3.4 — the section two other documents name as the source of the
confusion, and whose sentence also cites a `§10.2` that has never existed. `driver_io.rs:611` is
defensible: it documents the `ControlCommand` itself. Understated 23–57×, and the number already
shipped one defect — `ADMISSION_WAIT_CEILING_MS` was first written as 500 ms on its strength, and
`config.rs:63-66` records the 8-caller wave that served 7 of 24. Cheap: one site owns the
measurement, the rest link it.

### S7 — Cross-language conformance below the two high rows

`tests/conformance/v1/README.md:9` states *"Rust compares the error-code list through an exhaustive
match"* — false, and it is the document binding authors are told to trust; a new code ships with the
whole protocol crate green (measured). TypeScript has **no** completeness check that every string
union is in `V1_VALUE_ENUMS`; Python has exactly that check at `test_golden_conformance.py:581`, and
the same unpinned union is invisible in TS and caught immediately in Python. The corpus has no vector
for the agent-supplied start path, nor for `SessionIdentity::Resume` (the one identity mode where
`session_id` is required, and the branch both clients special-case) or `SystemPromptPolicy::Replace` —
zero occurrences of `resume` or `"replace"` anywhere under `tests/conformance/`.

### S8 — Duplicated predicates, and the copies that have already diverged

`pmux` and `claude-p` each hold their own `resolve_executable`, environment-patch builder and prompt
reader. Two have diverged: `claude-p --claude-bin` accepts a mode-644 file and ships a full
`RunOnceRequest` carrying the whole environment snapshot where `pmux` refuses locally in one line
(measured against a logging fake daemon), and `claude-p`'s slash guard (`main.rs:192`,
`prompt.trim_start()`) is the exact predicate `driver_io.rs` documents as bypassable — a BOM-prefixed
`/clear` was framed and sent while `pmux` refused it locally. `map_location_error` and the
terminal-error mapper each exist twice with different groupings and different strings
(`native.rs:4326`/`driver_io.rs:3740`, `native.rs:3653`/`driver_io.rs:3710`) in a file that documents
at length why the startup path was converged; `diagnostic_u64` is triplicated byte-for-byte
(`native.rs:4381`, `driver_io.rs:3471`, `v1/actor.rs:2614`). `crates/client` re-implements native frame
header decoding rather than calling `admit_native_frame_header`, so the path every `pmux` invocation
runs is unfuzzed — though it does derive its ceiling from the same constant (`lib.rs:45`, `:111`) and
is behaviourally identical at every boundary I tested. The derived help-text guard exists in 1 of 7
binaries: `claude-p` ships **17 options with no description** and `pmux-launcher --help` demands a
required `--token` its Options block never lists.

### S9 — Build and dependency configuration

There is **no `[profile.release]`**: the only profile table is `[profile.mutants]` at `Cargo.toml:96`
and there is no `.cargo/`. Measured: `pmuxd` 11,929,184 → **6,201,008 bytes (−48.0%)** under
`LTO=fat CGU=1 STRIP=symbols`, with a strip-only control at −19.9%, so most of the win is LTO; the
fat-LTO build took 1m47s cold, which is the tradeoff to record. Release binaries are gate inputs and
the Linux lane hashes them into an image. **`fuzz/Cargo.lock` has drifted from the root lock in 25 of
39 shared packages** — `serde_json` 1.0.149 vs 1.0.151, `serde` 1.0.228 vs 1.0.229, `uuid` 1.21.0 vs
1.24.0, `thiserror` 2.0.18 vs 2.0.19, `syn` 3.0.0 vs 3.0.2 — while both lanes run `--locked` and
`fuzz/Cargo.toml:38` states deterministic pinning as the reason the separate lock exists; the fuzzed
`serde_json` is not the shipped one. The `fuzz` package also inherits none of the workspace lints,
including `unsafe_code = "warn"`, which all 13 production members carry — verified out of tree that
`clippy -D warnings` does not flag `from_utf8_unchecked` without the table and hard-errors with it.
`proptest = "=1.8.0"` is hand-pinned identically in four member manifests instead of
`[workspace.dependencies]`, unlike every other third-party dependency.

### S10 — Smaller correctness and diagnostic items

The cold-swap slot loss (`pool/mod.rs:367-368`): `destroy` and `reclaim` are two lock acquisitions
with nothing reserving the slot between them, and the loser gets a **non-retryable** `Internal` at
`:800`. Widening that gap by 20 ms turns it on 300/300; unwidened, 0/300 — mechanism proven, natural
rate unmeasured. A byte-budget refusal reports `reason: "row_budget_exceeded"`
(`driver_io.rs:2247`) — reproduced with a six-row, 73,561-byte transcript, and the byte branch has no
test. `pmux`'s most common first-run failure prints its cause twice and names neither the socket nor a
next step: `pmux: I/O error: No such file or directory (os error 2): No such file or directory (os
error 2)`, because `#[error("I/O error: {0}")]` (`client/src/lib.rs:1420`) interpolates the source and
`main.rs`'s `{error:#}` appends it again; the contract test asserts only `contains("I/O error")`.
`render_agent_descriptor` prints a hand-written field subset — `auth_policy`, `compatibility`,
`terminal`, `lifecycle` and `retention` never appear in `pmux agent get`'s text output while the JSON
beside it carries all eleven `AgentSpec` keys, under a doc comment saying the two cannot disagree
(reproduced against a fake daemon). `normalize_claude_version` accepts empty minor and patch (`"1.."`,
`"Claude Code 2.1. (beta)"`) under a refusal saying *"did not contain a semantic version"* — but
`require-tested` still refuses the mint, so the damage is a malformed string in metadata under
`allow-untested`. `wait_for_turn` (`native.rs:1813`) re-checks the mailbox only after a sleep capped at 25 ms
(`:1864`), a literal that has carried no derivation since the initial commit. The Gate A perf receipt hand-transcribes ten product constants under a `source`
field claiming *"these are private consts"* — three of them are `pub` and I imported them from an
external crate.

---

## 5. Polish, hygiene and staleness — rates, not rows

**Line-number citations rot at roughly one in two; paths do not.** Three independent measurements:
hand-graded 46% (n=48), hand-graded 38% (n=13), and a deliberately over-counting same-line-identifier
proxy that I re-ran at this HEAD and that misses **78 of 107 gradable citations (72%)** — the proxy
mis-attributes multi-citation lines, so the truth is between. Zero point past end of file, so nothing
mechanical currently finds any of it. Verified examples: `docs/current-state.md:153` cites
`driver_io.rs:1440` for `blocking_screen` (it is at `:3403`; `:1328` is `"field": "terminal",`) and
`:1808-1843` for the leak invariant (it is at `:4271`); `current-state.md:47` cites `v1.rs:1288-1292`
for `CompletionAuthority` (`:2760`); `current-state.md:1038` cites `run_gate.py:914` for the umask
(`:821`); `docs/agent-resource.md:79` cites `native.rs:3439` for `admit_bound_resources` (`:3758`).
`crates/e2e/tests/matrix_citations.rs`, debt row 36, marked SAFE at +40 lines, remains the
highest-leverage open item in the repo. Two same-shape instances inside the same corpus: debt row R1
(`current-state.md:1477`) cites five lines and all five are wrong, in the row directly above the one a
previous pass de-cited for exactly this reason; and §6.2's own *"a line number is a claim nothing
checks"* lesson was applied to one table while §4's table 433 lines earlier and debt row 31's
NEEDS-CARE deletion instructions still carry line coordinates that resolve to nothing.

**The correction table is itself uncorrected.** `docs/linux-handoff.md:87` says the `required` literal
cited as `test_runner.py:638-650` is *"Actually at `:637-649`"*. Measured, `required = [` is at `:639`
and its `]` at `:650`; `:637` is a closing paren and `:638` the `names =` binding. The table's other
two rows are right, and §6.6 sends the next Gate C owner to it. Low, and ironic.

**One number, two documents, in lockstep.** `docs/testing.md:722-723` and `docs/current-state.md:2478`
carry *"573 caught, **27 missed — 95.50%**"* in near-identical prose; `score.py` over the cited
receipt gives **578/22/96.33%**, `missed.txt` has 22 lines with **zero** matching `cfg` against a
claimed *"2 `#[cfg(not(unix))]` twins"*, and three per-file rows are wrong identically in both.
`testing.md:751` does arithmetic on the stale figure (*"nine survivors of room"*; measured, ~14).
Confirmed one run stale, not fabricated: the previous archived receipt scores exactly 573/27/95.50%.
`docs/linux-handoff.md:166` carries the right value, so the corpus contradicts itself. Two documents
going stale in lockstep is proof they are a copy, not two observations.

**The Gate A verdict is stated five ways in one file.** Derived truth: the manifest sums to **83**
(41/8/4/10/10/9/1) and `.context/gate-a-mutants/dead-code-pass/stdout.log:85` reads `FAIL 82/83 cells
passed, 1 failed, 83 executed failed: linux_docker_self_tests`. `docs/current-state.md` says *"80/81 …
78/81"* (`:679`), *"no longer 75 cells; it is 81"* (`:685`), *"now 83 cells, and no whole-manifest
receipt attests them"* (`:772`, also false), *"75 cells"* (`:927`), and *"an otherwise 80/81 receipt"*
(`:1494`). `82/83` appears nowhere in the file. Medium only because `docs/linux-handoff.md:61-65`
carries a corrective banner naming 82/83 and C6.

**Every count is one `wc -l` from truth, and the drift is growing.** The Gate A driver is stated as
*"533 lines, 26 self-tests … 629 lines"* at `docs/current-state.md:936` and
`docs/gate-c-linux-handoff.md:107`; measured today **851 / 45 / 1,407**, and the test file has more
than doubled past the figure the previous review already flagged. `tools/gate-a/README.md:105` reports
`gate_b` *"passing 6/6 in 138 s"* in a file whose own header says `gate_b` is 8 cells, two of them the
88-minute mutation run — an operator budgets ~40× low. Also stale: `crates/claude/src` 2,822 → 3,565;
`client/src/lib.rs` 1,287 → 1,519; `gate-c-linux-handoff.md:586`'s `target_os` census says *"25
today"* against a measured **30**, with five of its nine cited sites rotted and `docs/linux-handoff.md`
carrying the corrected numbers for the same table; `gate-c-linux-handoff.md:673` says `evidence/`
*"holds only"* three files against six tracked; `current-state.md:1470` says a phrase appears in *"50
rows"* of `testing.md` against a measured 44; `bin/claude-p/src/main.rs:539` publishes a *"68-of-78"*
allowlist ratio that is a property of the caller's shell (measured here: 44 of 54; the stable
invariant is *"admits ten names"*), in a paragraph duplicated verbatim eleven lines below.

**Dead references and broken links.** Thirteen `§X.Y` cross-references resolve to no heading, eleven of
them naming §10.1–§10.8 of `docs/path-b.md`, which **have never existed in any commit** — checked against all
eight commits that ever touched the file. Four broken relative links: `fuzz/README.md:30` →
`../TESTING.md` (now `docs/testing.md`), in the sentence whose whole purpose is routing the reader to
the canonical fuzz invocation; `docs/spec.md:172`, `:2056`, `:2072` resolve from the wrong base.
`.dockerignore:117` re-admits `!tools/phase0/fixtures/*.jsonl`, a directory that has never existed,
while phase0's real fixtures under `tools/phase0/tests/` are not covered by the rule. All three
upstream issue drafts and `docs/linux-handoff.md:727` name 0.9.1 as current upstream; **0.10.0 is
published, unyanked, and all three defects survive into it byte-identically** — the strongest possible
version of each draft's claim, unclaimed. Draft 01's quoted decoder elides the `DATA_TAG` guard and
its repro table blames the wrong call site (and the finder's proposed replacement is also wrong: 0x0D
is `RENDER_TAG`, a valid tag, so the stream desynchronises silently rather than erroring).

**Small stuff, batched and verified.** Four operator-visible strings carry interior whitespace runs
from a lost line continuation — `native.rs:2234` (26 spaces), `:2306` (14, confirmed reaching `pmux
doctor` output on every invocation), `stateless.rs:130` (18), `:137` (22); no test reads any of them.
The README's agent-profile quickstart creates `agents.json` and then tells the reader to export
`profiles.json`. Both client package READMEs mention neither `run_stateless`/`runStateless` nor
`clear_session`/`clearSession` — grep returns nothing at all. `scripts/pmuxd-run.sh` accepts `--help`
silently, exits 0, and forwards it to the daemon's log file. `claude-p`'s `--socket` reads
`PSEUDOMUX_SOCKET` where every other client reads `PMUX_SOCKET`; a clean environment carrying only
`PMUX_SOCKET` gets a bare clap usage error naming neither. `bin/pmuxd/src/main.rs:128-129` hand-writes
*"the owner-set cap of 15"* where `pool/config.rs:21` `MAX_POOL_SIZE` is the predicate — now pinned by
`test_documented_surface.py`, still not derived. `TranscriptLocationError::ScanLimit` renders
*"exceeded 10000 project directories"* while the predicate reads a configurable field, so its own test
prints 10000 for a limit of 1. `ADMISSION_WAIT_CEILING_MS`'s justification claims *"half again on
top"* of a worst case that arithmetic makes 1.47×, not 1.5×. `forbidden_flag_count` (`full_stack.rs:4411`)
is 0 by construction and the assertion cannot fail — inert, not misleading (§6).

**Structure.** `docs/current-state.md` is 321 KB; §9 "Design debt" is ~55% of it and §9.23 alone is 360
lines. What a newcomer needs is ~8%. The corpus has no index; `docs/linux-handoff.md:17-25` is the
only routing table, it routes to six of ten documents, and that one — the genuinely well-maintained
entry point, which declares its HEAD, measures its own citations and keeps a hygiene ledger — is named
for a lane that has not started, so nobody working on macOS opens it.
`docs/instrument-fix-plan.md` is finished work (*"all 8 applied"*) retained as a live-looking plan;
its one open item's citation points 34 lines away from the guard it describes, in four places.

---

## 6. Overturned, refuted, and closed

Do not re-report these. This section costs the next reviewer the least and saves them the most.

**Refuted mechanisms — the fact survived, the stated failure did not.** Eight of the sixteen
OVERSTATED findings:

| claim | what measurement showed |
|---|---|
| `crates/client` re-decodes frame headers, so it may admit frames the protocol rejects | It derives its ceiling from `v1::MAX_NATIVE_FRAME_BYTES` (`lib.rs:45`, `:111`) and its inline decode is identical at 0 / 1 / 8 MiB±1 / `u32::MAX`. A duplicated two-line decode and a doc implying a call-graph relation, not an admission gap. **Low.** |
| `EventStreamOptions::default()`'s `wait_ms: 30_000` drifts silently from the daemon ceiling | Lowering `MAX_SUBSCRIBE_WAIT_MS` fails **three** existing tests in `crates/client/tests/fake_uds.rs`. Magic literal, not silent drift. **Low.** |
| A `#[serde(rename)]` slips past `response_result_name` | It does not: renaming `ResponseResult::Agent`'s tag reddens two protocol and three client golden tests. What is unpinned is the 16 diagnostic strings themselves. **Low.** |
| The agent-vs-inline start exclusivity is *"pinned in no language"* | Rust pins it in six dedicated tests in `v1_wire.rs`. The corpus gap is real and is client coverage only. **Low.** |
| A hostile `\u{202e}/clear` corpus case would pass unnoticed | `REFUSED_SLASH_FORMS` (`driver_io.rs:6954`) already lists it at `:6376` and reddens on narrowing. The masked-skip defect is real for the other 62 code points. **Medium.** |
| Gate A's omission of the vendored `--lib` sweep is undocumented | It is documented at `docs/testing.md:560-568`. What is defective is that passage's *"2,751/2,751 serialized sweep"* receipt: measured **2,749/2,751**, with `attach_tests::lifecycle::attached_live_input_preserves_split_utf8_sequences` failing 3/3 isolated **and on pristine crates.io 0.9.0** — upstream, not patch-induced, and named nowhere in this repo. **Medium.** |
| A sixth `EffortLevel` compiles clean and silently drops out | It is a compile error at `claude_launch.rs:1270` and in `every_variant!`, and the runtime answer is an explicit refusal naming the admitted set. The test-invisibility is real. **Low.** |
| A sixth `CONFIG_ROOT_ENV_DOORS` entry is unchecked for the Path B mint | The sibling test asserts `request.environment.set.is_empty()` (`stateless.rs:611`), which subsumes any door list. Proved by opening a sixth door: the hand-copied test passed, the sibling failed. **Low.** |

**Overturned severities.** The mutation receipt's wrong CAUGHT verdict was filed critical; the
phenomenon is already on the record at `docs/linux-handoff.md:174` as *the stated reason the floor is
94 rather than the measurement*, and the corrected score is 96.17% against a floor of 94 — **medium**
(§4 S2). The `~30 ms` restatements were filed high; no shipped constant is mis-sized at HEAD, so it is
documentation staleness with a repeat-defect argument — **medium** (§4 S6). MCP's missing
`clear_session` was filed high on the theory that MCP callers cannot recycle Path B context; MCP
exposes `run_stateless`, which clears internally — **medium** (§4 S3). The fuzz-target name
duplication was filed high; all six copies agree with the manifest today — **medium** (§4 S4). The
corpus-wide citation rot and the two-document mutation staleness were filed high; both are
documentation-integrity defects that no user of the shipped product can reach — **medium** (§5). The
`§0.5` correction table's own two-line miscount was filed high; it is a doc-only miscount whose row
also names the symbol, in a document that instructs the reader to grep the symbol — **low** (§5). The
`native.rs:7058` wall-clock bound was filed medium as a flake; 60 runs under 24 saturating threads were
60/60 green, and the assertion is **not** redundant — it is the only thing separating the 5 ms deadline
path from the fake's 60 s stall, so it must be tightened to virtual time, not deleted — **low**.

**The previous version's two refutations both survive.**

- **`README.md`'s *Promoted compatibility cells* did not conflate the compatibility gate with the Path B enable switch.** Read
  whole at `26784e3`, its subject is `PROMOTED_PROFILES` and the flag it means is
  `--tested-claude-profile`; `docs/testing.md` row S-41 states exactly that claim. The probe filed it
  as fixed rather than wrong, and `a759d6a` did reword it to remove the ambiguity — so the sentence is
  gone either way. **Do not re-litigate.**
- **`forbidden_flag_count` is redundant, not a widened attestation.** The probe independently
  confirmed the value is 0 in every run in which it is evaluated *and* that a violating launch writes
  no record at all: the double exits non-zero and the lane goes red loudly. The assertion is inert
  dead weight (delete it or move `record_launch` above the early return), not a hole. **Low, §5.**

**Corrected counts carried forward.** `TEST_TRANSPARENT_EXACT_KEYS` covers **10 of 11**, not the
finder's 11 of 12. `FORBIDDEN_FLAGS` omits **one** flag that matters, not three. The mutation
arithmetic is **96.17%**, not 95.9%. The bug-class counter is **4 statements in 3 Rust files** plus 2
Markdown files, not "five sites". The two `native.rs` whitespace runs are **26 and 14**, not 14 and
22. The perf receipt transcribes **ten** constants, not nine. `~46%` of citations rot by hand-grading,
not 12% of section references being unattributed — 71% of unresolved `§X.Y` references name their
target file in adjacent prose, and all three examples chosen to illustrate *"none names its target"*
name it.

**Closed this session — verified, not assumed.**

| was | now |
|---|---|
| `evidence/README.md` published *"53 remain"* against its own recount's 15 | `2b85a1d`. No count, ordinal or remaining figure is written; the figure is derived on the call, and a Gate A cell runs the README's own command and compares. The stale mirrors in `current-state.md` and `instrument-fix-plan.md` went with it |
| The mutation gate's `debug-assertions` guard read `[profile.mutants]` and nothing else | `867009d`. `crates/protocol/tests/mutation_profile.rs` fires a `debug_assert!` and an overflow under `--profile mutants` and reports live; the refusal message is interpolated from the probed array. Re-ran the probe: passes, reports exactly `debug-assertions` and `overflow-checks` |
| `README.md`'s *The command surface* named ten of thirteen subcommands and zero of Path B; its MCP list named 8 of 13 tools; its banner claimed no built-in supported cell; its quickstart's daemon refused every `ask` | `a759d6a`. All four. `tools/gate-a/tests/test_documented_surface.py` derives the table from `pmux --help` cross-checked against `pub enum Command`, the flags from `pmuxd serve --help`, the cap from `MAX_POOL_SIZE`, the models from `MODEL_TABLE`, and the MCP list from a real `tools/list` exchange. **The twin at `docs/current-state.md:159` is still live — §4 S3** |

**Checked and found sound — do not re-hunt.** Vendor provenance is byte-exact and could not be fooled;
`rustfmt src/lib.rs` really does cover the vendor's whole module tree (derived over 560 files); wire
methods, results and events are compile-enforced by `wire_tags!` and agree in all three languages; the
agent-resource API matches field-for-field in both clients; no layering violation; no dead dependency
and no duplicate crate in a production binary (`cargo tree -d` shows one `proptest`); no O(n²) at the
15-instance cap; the fresh-handle-per-snapshot cost is 0.03–0.10 ms and closed; `.dockerignore` and its
Docker twin are byte-identical and fail closed; `evidence/gate-b-drain-calibration.json` and every
count made about it verify; exit-code discipline is consistent across all seven binaries; the
bug-class counter re-verified green, all sites spelling `thirty-three`.

**Deliberate debt — unchanged.** `gate_f/linux_docker_self_tests` red (debt row C6); `vendor/` at
11 MB; both vendor patches (neither defect fixed upstream through 0.10.0); `evidence/` outside
`SOURCE_ROOT_DIRS`; the two Path B latency levers; `claude-p`'s tool-capable `cell: Full`;
`getrandom` and `syn` duplicates.

---

## 7. Unproven

Named individually, with what it would take to settle each. None is a finding; each is a claim someone
will otherwise re-argue from the same non-evidence.

- **That a trailing-newline `claude-p` prompt actually dies (§3.1).** The chain resolves in source with
  no normalization that could rescue it, and `3e1a699` records the identical comparison measured fatal
  live against 2.1.220 — but no `claude-p` turn was run. **Settle it:** one `echo hi | claude-p`
  against a 2.1.220 host. One turn of tokens.
- **That the eight load-sensitive `driver_io` tests are what turned `claude_launch.rs:964` CAUGHT
  (§4 S2).** The run's per-mutant `log/` was not retained, so the failing test name is unrecoverable.
  Measured: the outcome disagreement across nine archived runs, and that those eight fail at ≥8×. The
  join is inference. **Settle it:** re-run that one mutant at `jobs=4` under load with `--log` kept.
- **That the eleven mutants counted CAUGHT via `timeout.txt` are real catches.** Not re-run; they may
  be load artifacts of the same kind. **Settle it:** re-run those eleven serially.
- **The natural hit rate of the cold-swap slot loss (§4 S10).** Proven by widening the gap 20 ms
  (300/300); never observed unwidened (0/300). **Settle it:** a lock-step probe with a second worker
  taking the freed slot, the shape that reproduced both §4 S1 races.
- **The blast radius of `Pool::mint`'s missing `else` in production (§4 S1).** The probe drives a
  double whose `mint` returns `Ok` after being parked across shutdown; the production host's own
  failure path (`finish_failed_start`) may make the `Ok`-after-removal case narrower still. **Settle
  it:** a probe using `NativeInstanceHost`, not a double.
- **Whether the ~30 ms figure is wrong at `driver_io.rs:611`.** It documents the `ControlCommand`
  itself, where the rotation reading is defensible. **Settle it:** the author's intent, not a
  measurement.
- **The 82/83 receipt and its sixteen archived siblings.** Read from
  `.context/gate-a-mutants/dead-code-pass/stdout.log:85`; the other run directories were not audited.
- **Whether the 46 first-party source files outside `FULL_GLOBS` would survive mutation.** Unknown in
  both directions; the score is honestly labelled as scoped, and `scope_does_not_cover=` is printed.

---

## 8. Not covered

Named, not counted, so the reader can see the shape of the hole rather than its size.

**Not executed.** Gate A end to end. Any cargo-mutants campaign (the preflight was run and the run
killed; no score is claimed from this session). The Linux Docker lane — no Docker on this host, so
`tools/linux-docker/{run.sh,suite.sh,inside.sh}` were read and never run.
`tools/screen-corpus/per_binary_tests.sh`. Both `tools/promotion` scripts. Any live Claude turn: the
`require-tested` refusal of `pmux run`, the runtime pool-exhaustion refusal, and §3.1's death are
established by code path and by the repo's own recorded measurements, not by observation. The
credential-free full-stack lane past `DaemonLost` — `pmux-test-claude` needs the e2e harness's
`PMUX_TEST_*` attestation environment, which is not an operator workflow.

**Not compiled.** Every `#[cfg(target_os = "linux")]`, `#[cfg(not(unix))]` and `#[cfg(windows)]` block
— 30 `target_os` sites. Only `aarch64-apple-darwin` is installed. This review can say they are never
compiled, never mutated and never run here; it cannot say whether they compile. §4 S2's wrong CAUGHT
verdict is a direct consequence.

**Read but not audited to completion.** `crates/service/src/agent.rs`, `launch_broker.rs`,
`config_isolation.rs`, `compatibility.rs`. `bin/pmuxd/src/handler.rs`, `bounded_log.rs`, and
`bin/pmux-rmuxd` beyond argument handling. `docs/spec.md`'s normative content — its repo-shaped paths
resolve and none of its numeric claims were sampled, so its staleness rate is *unknown*, not low.
`docs/agent-resource.md`'s design argument. `docs/testing.md`'s 134-row §4 matrix. The ~4,900 lines of
in-file test code in `native.rs`/`driver_io.rs`. `pmux attach` and the Smithers transport.
`crates/service/src/v1/actor.rs` beyond the six sites cited above.

**Deliberately not measured, because the review is read-only.** The cost of the blanket
`tokio = { features = ["full"] }` every member inherits, including the thin clients. Whether reverting
either vendor patch reproduces the regressions it claims. `PMUX-PATCH.md`'s mutation claims. The
per-file mutation survivor taxonomy in `docs/testing.md` beyond its count and its `cfg` claim.

**Left to the owner's judgement.** Whether the omissions in §4 S3 are deliberate. No comment, doc, test
or debt row states a rationale for any of: `clear_session` and `diagnose` being absent from MCP; the
five `AgentSpec` policy fields that never render in `pmux agent get`; `paste_injection.rs`'s narrower
Cf copy; `claude-p`'s `PSEUDOMUX_SOCKET` (a test asserts the divergence is intentional but no document
says why the spec's word *"currently"* never converged); or `pmux-launcher`'s `--token` being
`hide = true` while clap still prints it in the usage line. Absence of a rationale is not proof there
is none.
