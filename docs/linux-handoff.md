# linux-handoff.md

**This file was written against an older tree; Gate A, Phase 0, and linux-docker have been removed.** Living verification is `tools/dev/`. Do not treat counts below of Gate A cells as a present-tense fact.

**Audience:** historical (2026-08-13). For a living Linux pin, `tools/dev/`. Do not treat the body as the current finish. Nothing below
assumes you saw the macOS work happen.

**Written:** 2026-08-13, on `macOS-15.7.7-arm64` (Darwin 24.6.0, aarch64, 10 cores), branch
`N0xMare/plan-pmux-architecture`, tree clean. **Every measurement below was taken against
`0d83b7a`**, which was HEAD at **158 commits** (`git rev-list --count HEAD`; 157 since `main`) and
is the last commit before this file. This file and its corrections are docs-only successors to it,
so your `git rev-list --count HEAD` will read higher and nothing measured here moves — except the
two totals that count *this file*, which say so where they appear (§1, §8.1).

**Three labels are used and they are not interchangeable.** **MEASURED** — a command was run on this
host for this document and its output is quoted or counted here. **INFERRED** — derived from
something measured, by an argument stated at the point of use. **UNVERIFIED** — not established; do
not upgrade it because it sounds right.

**Every `path:line` here was re-read at `0d83b7a` before this file was committed.** Line numbers rot.
`crates/service/tests/path_b_doc_citations.rs` grades citations in seven documents and **this is not
one of them**, so nothing in the build will tell you when a number below goes stale. Run
`git rev-parse HEAD`, and when a citation does not land on what it names, trust the name.

**Where to go for what this file does not repeat.**

| You want | Read |
|---|---|
| The caller surface — `pmux run`, models, effort, pool sizing | root `README.md` |
| Path B as designed and as shipped | `docs/path-b.md` — its §0.0 is the reading order |
| What a hostile caller can do to a pooled instance | `docs/path-b-adversarial.md` |
| Architecture, protocol, normative product behaviour | Living `docs/spec.md` (product contract). The 2,224-line figure was this file's 2026-08-13 snapshot of the old spec. |
| Gate state, the debt table, the bug-class ledger | Living `docs/current-state.md`. The 3,618-line essay is `docs/archive/current-state-2026-08.md`. |
| The test-ownership matrix and the gate rules | `docs/testing.md` |
| How big this repository is and where the lines are | `docs/code-census.md` |
| Checking the tree / pinning Claude / dropping the operator flag | `tools/dev/README.md` |
| Leftover Gate C freeze notes (not the starting sequence) | `docs/gate-c-linux-handoff.md` — read §7.3 below first, its §3.2 has rotted |

---

## 1. What pmux is, and what Path B is

pmux drives **real Claude Code CLI instances** inside rmux panes. There is no API contract with
Claude Code: pmux types into a terminal and reads a rendering, so nearly every correctness claim in
the product is a claim about somebody else's undocumented UI, observed through a PTY. That single
fact generates most of §4 and all of §6.

**Path B is the product.** It is a stateless engine: `(model, effort, prompt) -> output tokens`.
Instances are pooled to 15, and an instance is recycled between callers with `/clear` rather than
being restarted. A Path B cell is `SessionCell::Minified` — no tools, no MCP, no writable attach,
no caller-nameable resources. Path A (batteries-included, tool-capable) is
refused on the public wire. **Read Path A's gaps as out of scope, not as
debt you inherited.**

Three facts about the shape of the tree, all MEASURED
(`git ls-files | wc -l`, `git ls-files '*.rs' | grep -v '^vendor/' | xargs wc -l`):

* **983 tracked files**, and **586,447 lines** at `0d83b7a`. The file count is stable; the line
  count is not a fact about the product — this document's own edits move it (it read 586,656 at the
  commit that verified this file), so run the command rather than quoting the number.
* **129 non-vendor `.rs` files, 145,342 lines.** `vendor/` is 661 files and 315,530 lines — 53.9% of
  the repository — carrying 1,351 authored lines of patch. You will never read it.
* **Python is the second language at 65,823 lines** (`git ls-files '*.py' | grep -v '^vendor/' |
  xargs wc -l`), and on the Linux lane specifically the Python is most of what you are picking up:
  `tools/linux-docker/` alone is **16,443 lines across 17 tracked files**. Any sentence of the form
  "pmux is N lines of Rust" understates the maintained surface by a quarter.

Two size traps `docs/code-census.md` names and you will hit in the first hour. MEASURED with the
tree's own span scanner (`scripts/register_currency.py:232` `cfg_test_regions`):
`crates/service/src/driver_io.rs` is **11,410 lines of which 7,150 are one top-level `#[cfg(test)]`
module**, and `crates/service/src/native.rs` is **10,068 of which 5,341 are** — neither file's size
tells you anything about its complexity. And `crates/e2e/src/bin/pmux-test-claude.rs` is a **test
double** that nonetheless builds as a real binary. At `0d83b7a` it sat beside seven product
binaries (`claude-p` still counted), so a newcomer counting binaries got eight. Living
`FLOOR_BINARIES` is seven names (`pmux`, `pmuxd`, `pmux-mcp`, `pmux-rmuxd`, `pmux-launcher`,
`pmux-hook`, `pmux-test-claude`).

---

## 2. What is proven, and the exact scope boundary

### 2.1 The certification is a command, not a paragraph

HISTORICAL. Living tree check is tools/dev/check.sh. Criterion 4 is MET iff run_gate.py is gone. Do not invent a linux drain.

```text
# DELETED as a living command. Do not run.
# PYTHONDONTWRITEBYTECODE=1 bash scripts/path-b-done.sh \
#   --gate-a-receipt .context/gate-a/pinned-receipt-gate-a-<commit>.json \
#   --gate-a-receipt .context/gate-a/pinned-receipt-gate-b-<commit>.json
```

`scripts/path-b-done.sh` (93 lines) plus `scripts/path_b_done.py` (1,410 lines) read the ordinal and
title of every `###` heading under §1 of `docs/path-b-verdict.md`, bind each to a function that
measures it, and **refuse before measuring anything** if the set implemented is not the set the
document publishes, in either direction. Exit 0 = every criterion MET; 1 = at least one NOT MET, each
named; 2 = could not decide; 3 = a partial `--only` run, which is never a verdict.

**MEASURED, and state it this way or not at all: 5/5 holds at `f4622a9`, and 4/5 at every commit
after it.** With both pinned receipts and no `--commit`, the script judges whatever commit it is
run at, prints `criteria=5 (read from docs/path-b-verdict.md)` and `working_tree=clean`, and exits 1
at **4/5**, refusing criterion 4 with *"pinned-receipt-gate-a-f4622a9.json describes commit f4622a9
and this gate is judging `<that commit>`"*, then *"70 manifest cell(s) were graded by no receipt
named here"*, then a three-line `remedy:` naming the receipt path, the seven ungraded phases, and
the exact `scripts/gate-in-worktree.sh` invocation that would produce them. Measured that way twice:
at `0d83b7a` and again at the commit that verified this document.

**That is the gate working, not a regression.** Every commit after `f4622a9` on this branch is
docs-only (`0d83b7a` is `git show --stat` → 1 file, `docs/code-census.md`, +608; this file is the
next), and the gate refuses to attribute `f4622a9`'s run to any of them. **Do not write "the gate
exits 0" without naming a commit** — that sentence has no truth value on its own, which is the whole
reason the receipt names the commit in three fields. Judged where the receipts point
(`--commit f4622a9 --only 4`) criterion 4 reads `manifest_cells=70`, `cells_executed=70`,
`red_and_deliberate=gate_f/linux_docker_self_tests`.

The five criteria and what each printed. Criteria 1, 2, 3 and 5 are measurements of the **tree**;
criterion 4's values are from the `--commit f4622a9 --only 4` run, because it is the only criterion
that grades a commit.

| # | criterion | evidence |
|---|---|---|
| 1 | No known unfixed defect in the Path B path | `defect_register_entries=4`, `open=0`, `closed=3`, `accepted=1`; `survivor_register_head=c94612d`, `entries=144`, `scope=full`, `files_drifted=0`, `stale_rows=0`, `undetermined_catchers=0` |
| 2 | The adversarial suite passes | `adversarial_commands_derived=8`, all 8 passed |
| 3 | A promoted profile for the installed version, from machinery that exercises minified cells | `host=macos/aarch64`, `claude_version_installed=2.1.227`, `promoted_profiles=1`, `promoted_range=2.1.220..=2.1.227`, `promotion_checks_required=9` |
| 4 | Gate A green except the deliberate Linux cell | `manifest_cells=70`, `cells_executed=70`, `deliberately_red_cells=linux_docker_self_tests` |
| 5 | Path B doc claims reconciled to measurement | `citation_rules_passed=4`, `citation_rules_failed=0` |

### 2.2 The scope boundary, stated exactly

The certification covers **one OS, one arch, one Claude Code range, one host shape**:

* **macOS / aarch64.** `crates/service/src/compatibility.rs:484` is the whole promoted set, and it
  is one entry:

  ```rust
  484  pub const PROMOTED_PROFILES: &[PromotedProfile] = &[PromotedProfile {
  485      claude_version_floor: "2.1.220",
  486      claude_version_tested_through: "2.1.227",
  487      os: "macos",
  ```

  with `input_transport: InputTransport::Sdk` (`:490`) and `transcript_drain_ms: 1_000` (`:491`).
* **Claude Code `2.1.220..=2.1.227`.** MEASURED here today: `claude --version` → `2.1.227 (Claude
  Code)`, `which claude` → `<HOME>/.local/bin/claude`. The backing evidence is
  `evidence/promotion-2.1.227-macos-aarch64.json`: `verdict: "promotable"`, `failed_check: null`,
  `real_claude_turns.count = 5`, `measured_at 2026-08-11T14:40:33Z`,
  `host {os: darwin, arch: arm64, cpu_count: 10}`.
* **This host.** Every one of the 70 Gate A cells ran on Darwin (`host.os` in both pinned receipts).
  Nothing has ever been executed on Linux by any gate in this repository. §8.2 is the one Linux run
  that exists on this machine, and why it closes nothing.

**On a Linux host the product refuses itself, today, before you change anything.**
`admissible_here()` (`crates/service/src/compatibility.rs:682`) filters
`profile.os == std::env::consts::OS && profile.arch == std::env::consts::ARCH` over `candidates()`
(`:694`), so on Linux the promoted set matches nothing, and
`require_tested_for_minified_cell` (`crates/service/src/v1/actor.rs:155`) returns
`UnsupportedClaudeVersion` — *"the minified cell requires a tested compatibility profile"* (`:163`) —
for every `pmux run` until an operator overrides it. The living override is
starting the daemon with `--tested-claude-profile '<JSON>'`, which admits a
profile as tested. That is not a promotion. This refusal is not a bug to route
around. It is the definition of the finish line, and §9 is how you reach it.

### 2.3 What "Gate A is 69/70" means

**HISTORICAL / STALE.** At `0d83b7a` this was MEASURED from `tools/gate-a-candidate/phase-manifest.json`: **70 cells in 7 phases** — `gate_a` 28,
`gate_b` 8, `gate_c` 4, `gate_d` 10, `gate_e` 10, `gate_f` 9, `residue` 1. The census file is gone; those 70-cell counts are `0d83b7a` history. Phase timeouts are
`{gate_a: 3600, gate_b: 14400, gate_c: 3600, gate_d: 7200, gate_e: 14400, gate_f: 3600,
residue: 600}` seconds and `max_command_output_bytes` is 16,777,216.

The two pinned receipts at `f4622a9` split those 70:

| receipt | phases | cells | result | wall |
|---|---|---:|---|---:|
| `pinned-receipt-gate-a-f4622a9.json` | gate_a, gate_c, gate_d, gate_e, gate_f, residue | 62 | 61 passed, 1 failed | 2,449 s |
| `pinned-receipt-gate-b-f4622a9.json` | gate_b | 8 | 8 passed, 0 failed | 6,469 s |

Of gate-b's 6,371 s in cells, **6,197 s is one cell**, `mutation_score_agent_launch_pool_protocol`.
Budget for that when you plan a Linux run.

The one red cell is `gate_f/linux_docker_self_tests`, and **the permission for it to be red is
derived, not declared**: the done-gate reads the grant out of debt row **C6** in
`docs/current-state.md` and prints `granted_by=linux_docker_self_tests is granted by
docs/current-state.md row C6`. Close C6 and the cell stops being allowed to fail. §7.3.

---

## 3. What ports for free

This is the good news, and it is substantial. Everything in this section is platform-neutral
apparatus that exists, is used, and costs you nothing to inherit.

### 3.1 The done-gate — and it already knows the word "linux"

`scripts/path_b_done.py:87` is `RUST_OS = {"darwin": "macos", "linux": "linux"}` with `RUST_ARCH`
beside it, and an unmapped host refuses rather than guesses. **Cost on Linux: zero.** Criterion 3
will fail by construction and the refusal is already written: at `scripts/path_b_done.py:749`,
`if not covering:`, it prints *"no promoted profile covers Claude Code `<v>` on linux/`<arch>`; the
ranges are macos/aarch64 2.1.220..=2.1.227"*, and then requires
`evidence/promotion-<version>-linux-<arch>.json` to exist. **That pair is the entire definition of
"Path B is done on Linux"** — see §9.

### 3.2 The survivor register

`evidence/mutation-survivor-register.json` — MEASURED: **144 entries**, keyed by
`["file","function","genre","replacement","occurrence"]` (its own `key_fields`), dispositions
`KILLED` 83, `ACCEPTED` 30, `EQUIVALENT` 18, `REMOVED` 13; by file `driver_io.rs` 72, `native.rs` 40,
`protocol/v1.rs` 15, `pool/mod.rs` 9, `agent.rs` 3, `pool/evidence.rs` 3, `claude_launch.rs` 2.
Its `recorded_at` block:

```
scope=full  head=c94612d  date=2026-08-12  cargo_mutants_version=27.1.0
enumerated=1661  unviable=505  decided=1156  caught=1120  missed=36
mutation_score_percent=96  floor_percent=94
caught_only_by_a_measured_drifter=19  caught_only_by_a_timeout=7
```

**The floor is derived from that same run, not chosen.** `floor_derivation` in the file states the
rule: count as missed every mutant whose only failing target was a measured drifter (`bounded_soak`,
`private_runtime`, `lifecycle_faults`, `performance_diagnostics`) or which was caught only by a
timeout — 19 + 7 takes 96 to 94. `scripts/gate-a-mutants.sh:152` (`register_floor_percent()`) reads
`floor_percent` out of that file and refuses if `recorded_at.scope != "full"`, so the script holds
exactly one statement of the number.

**Cost on Linux: one full-scope campaign, and the result is informative either way.** Every row
records `caught_by.target` and `.test`, which are Cargo target names and therefore portable. The
register is scoped to a *tree*, not a platform: a Linux run either reproduces it (evidence the
platforms agree) or diverges, and each divergence is a platform-specific behaviour worth a row.
Budget: the gate scope alone was 6,197 s at `--jobs 4`; `docs/testing.md` records two complete
full-scope runs at 11,854 s and 10,443 s.

**One thing in the mutation lane is stale and you should not inherit it.** The gate-scope floor is a
defended constant, `SCOPE_FLOOR=94` at `scripts/gate-a-mutants.sh:173`, whose comment defends it
against *"a measured 95.50%"* — a 2026-08-07 census over 702 mutants. The gate-scope receipt at
`f4622a9` measures **97% over 740**. `scripts/gate-a-mutants.sh:129` says the two excluded globs are
*"886 of the 1,588 mutants `full` enumerates"*; the register says `enumerated=1661` and the gate run
says 740, i.e. 921 of 1,661. Both numbers are stale in the safe direction (the floor is lower than
the measurement), and `docs/testing.md` already flags the 1,588/886 pair as from an older head.

### 3.3 The `SessionRuntime` seam — the highest-leverage thing in this list

`crates/service/src/runtime.rs:151`, `pub trait SessionRuntime: Send + Sync + 'static` — 8 methods,
taken from `NativeService`'s call sites rather than from the type. `PrivateRuntime` implements it for
real (`impl` at `:202`); `ScriptedRuntime` (`:401`, `impl` at `:507`) is the double that answers what
a test scripted and **refuses everything else by name**, so a guard sitting above it can still fail.

Why it exists, from `crates/service/src/native/tests/seam.rs`'s own header: a full-scope mutation run
left a block of `native.rs` survivors whose shared cause was **not weak assertions but
unreachability** — a `PrivateRuntime` cannot exist without a real `pmux-rmuxd` sidecar, a real
launcher socket and a completed rmux handshake, and the only three tests that build one are all
`#[ignore]`d. That file is 1,496 lines and 16 test functions (MEASURED:
`grep -c '#\[test\]\|#\[tokio::test\]'`), and it needs no live rmux at all.

**Cost on Linux: zero, and it is the layer that still works on day one.** Every seam test runs under
`cargo test` with no PTY, no sidecar and no Claude. On a host where the real runtime is the
least-proven component, that matters more than it does here. It is also the pattern to copy: when a
Linux mutant survives because nothing can construct the thing, the answer is a trait at the process
boundary, not an `#[ignore]`.

### 3.4 The pinned-worktree runner

`scripts/gate-in-worktree.sh` (692 lines). `git worktree add --detach` at an explicit commit, run the
gate there, hash every artefact into a receipt, remove the worktree. The receipt schema is
`pmux.pinned-worktree-run.v1` (`scripts/gate-in-worktree.sh:111`) and it names the commit it graded
in three fields plus its last printed line, precisely because a gate receipt written by
`tools/gate-a/run_gate.py` names no commit and this repository keeps finding exactly that defect.

**Cost on Linux: zero** — it is bash plus git plus whatever gate you hand it. `--commit`,
`--print-receipt-path`, `--release-build` and repeatable `--prepare` are all platform-neutral. It is
also what removes the serialization you will otherwise hit first: two cargo processes in two target
directories do not queue behind one lock.

### 3.5 The function-scoped currency check

`scripts/register_currency.py` (1,012 lines) decides when a survivor-register row has stopped being
true, by three rules each with a derived input: rule 1, the mutant's item span from
`cargo mutants --list --json` intersected with `git diff -U0` hunks; rule 2, *the test that decided
it* — a `--test NAME` target's sources are `tests/NAME.rs` plus the subdirectories of `tests/`, which
is Cargo's own layout rule; rule 3, the escalations that force a full run anyway. Every decision errs
one way: a row it cannot place is stale, a hunk it cannot attribute escalates.

It replaced a check whose `git diff --name-only` over globs declared all 144 rows stale on a moved
comment and demanded 3 h 16 m. Measured value at HEAD, from criterion 1: `files_drifted=0`,
`stale_rows=0`, `stale_functions=0`, `undetermined_catchers=0` against
`filtered_mutants_since_full_scope=311`. **Cost on Linux: zero**, and it is the difference between a
three-hour and a seconds-long answer on every commit.

Its `cfg_test_regions` (`scripts/register_currency.py:232`) is also the tree's own `#[cfg(test)]`-span
scanner and the right thing to reuse. Three independent analysts hand-rolled brace matchers for
`docs/code-census.md` and two of them desynced on a `'{'` char literal; the scan in §4.1 below uses
this function rather than a fourth matcher.

### 3.6 The derived-set discipline

**A set-of-things-to-check is derived from the tree, never hand-written.** Existing instances you
inherit:

* `crates/service/src/source_scan.rs:53` `declared_functions()` — one scanner, two consumers
  (`crates/service/src/native.rs:9824`'s differential entry-path test, and the rendering register at
  `crates/service/src/driver_io.rs:4553` and `:4679`). Its header says why it is one and not two:
  *"A scanner stated twice is the same bug one level up."*
* `scripts/gate-a-residue.sh:186` `FLOOR_BINARIES` — **seven** names kept as a **lower bound** only
  (verified against the array in that file; at `0d83b7a` this was eight, when `claude-p` still
  counted); the scanned set is `find`-derived. The comment records the defect it replaced: the
  receipt printed `candidate_executables=8` meaning *"our literal has eight entries"* and would
  have printed 8 against a directory of twenty.
* `scripts/gate-a-mutants.sh:175` derives the gate scope by subtracting `GATE_EXCLUDES` from
  `FULL_GLOBS` in a loop, and `:186` refuses if the arithmetic does not close. Not `:194` — that is
  the *full* scope's `SCOPE_GLOBS=("${FULL_GLOBS[@]}")`, which subtracts nothing; this document
  cited it until the line was re-read, which is the failure mode of every `path:line` here.
* `tools/linux-docker/tests/test_runner.py:296` — the Linux manifest's expected size is
  `len(candidate_ids) + len(CONTAINER_ONLY_GATES)`. Its comment records the two literals it replaced:
  a phase-count dict and `len(observed) == 97`.

---

## 4. What must be re-derived, not ported — the work list

Everything in §3 is apparatus. Everything here is a **measurement about a running program on a
platform**, and every one of them was taken on macOS. This section is the bulk of the work. Treat it
as a work list with commands, not as a warning.

### 4.1 The 48 version-keyed `MEASURED` sites in production

**The rule, stated so you can re-run it:** a source line containing `MEASURED` with a `2.1.x` literal
within ±6 lines, over `git ls-files '*.rs' | grep -v '^vendor/'` (**129 files, 145,342 lines**),
bucketed by path, with `#[cfg(test)]` spans excluded using `scripts/register_currency.py:232`
`cfg_test_regions` rather than a fresh brace matcher.

MEASURED at `0d83b7a`:

| bucket | lines |
|---|---:|
| `crates\|bin */src`, **outside** `#[cfg(test)]` — production | **48** |
| `crates\|bin */src`, inside a top-level `#[cfg(test)] mod` | 19 |
| `crates/e2e/src` (the `pmux-test-claude` double) | 2 |
| `*/tests/` targets | 12 |
| **total version-keyed `MEASURED` lines** | **81** |

The 48, by file — **this is the work list**:

| file | sites |
|---|---:|
| `crates/claude/src/composer.rs` | 14 |
| `crates/service/src/driver_io.rs` | 14 |
| `crates/service/src/claude_launch.rs` | 6 |
| `crates/claude/src/engine.rs` | 4 |
| `crates/service/src/v1/backend.rs` | 2 |
| `bin/pmux/src/cli.rs`, `bin/pmuxd/src/main.rs`, `crates/client/src/prompt.rs`, `crates/rmux/src/backend.rs`, `crates/service/src/config_isolation.rs`, `crates/service/src/native.rs`, `crates/service/src/screen_corpus.rs`, `crates/service/src/v1/minified.rs` | 1 each |

Versions named in the ±6 window of those 48: `2.1.220` 14, `2.1.220`+`2.1.70` 3, `2.1.226` 21,
`2.1.226`+`2.1.70` 1, `2.1.226`+`2.1.227` 2, `2.1.227` 7. Production also carries **107** `MEASURED`
lines with no version in the window, so production holds **155** `MEASURED` lines in total.

**State the unit AND the denominator, or the number means nothing — and this is the trap, not an
aside.** `docs/2.1.227-compatibility.md` §2 published **44** on 2026-08-11 for what reads as the
same predicate. It is not the same denominator: §2 never states one, and that document's §11
records the scan as `git ls-files 'crates/*/src/**.rs'`, which excludes `bin/*/src` entirely — a
count published in one section whose scope is only written down six sections later. MEASURED both
ways at `0d83b7a`:

* over **that** denominator, today's count is **46** — a real drift of **+2 since 2026-08-11, both
  at 2.1.227**, with every other version bucket unchanged;
* over the wider `crates|bin */src` denominator this section uses, **48** — the two extra are
  `bin/pmux/src/cli.rs` (2.1.227) and `bin/pmuxd/src/main.rs` (2.1.226), which were always there and
  which the published scan could not see.

So 44 → 48 is *not* four new sites; it is two new sites and two the earlier glob excluded. The
double's count (2) reproduces exactly under both. A count of contiguous comment *blocks* rather
than lines gives **36** in production, in 15 files, over 1,385 comment lines. 46, 48 and 36 are one
tree measured three ways, and none of them is wrong.

**None of the 48 is transferable by argument.** Each is an observation of Claude Code's rendering or
argv behaviour under a macOS build. What a Linux build of 2.1.227 paints into a pane has never been
looked at.

### 4.2 The `cfg` sites: 30 `target_os`, 22 `cfg(not(unix))`

MEASURED over the same 129 files:

* **`target_os` — 30 lines, all 30 in code, 0 in comments.** Values: `"macos"` 22, `"linux"` 17.
  Distribution: `crates/e2e/tests/full_stack.rs` 12, `crates/service/tests/process_support/mod.rs` 6,
  `crates/rmux/src/process_boundary.rs` 4, `crates/e2e/src/bin/pmux-test-claude.rs` 3,
  `crates/service/src/claude_launch.rs` 2, `crates/service/src/native.rs` 2,
  `crates/e2e/tests/cross_cell_contamination.rs` 1.
* **`cfg(not(unix))` — 24 grep lines, of which 22 are attributes** and 2 are inside doc comments
  (`crates/service/src/claude_launch.rs:1077` and `:4145`). The 22 live in `service/src/native.rs` 5,
  `bin/pmuxd/src/main.rs` 4, `service/src/claude_launch.rs` 4, `bin/pmux-launcher/src/main.rs` 2,
  `bin/pmuxd/src/bounded_log.rs` 2, and one each in
  `service/src/{agent,config_isolation,driver_io,private_dir,sensitive_launch}.rs`.

Reproduce both with `git ls-files '*.rs' | grep -v '^vendor/' | xargs grep -n 'target_os'` and the
same for `cfg(not(unix))`, and subtract comment lines by hand — 22 is the attribute count.

### 4.3 The one shipped `cfg(target_os = "linux")` path, and how it is weaker

Of the 17 `"linux"` mentions, exactly **one** is in first-party production code that ships:
`crates/rmux/src/process_boundary.rs:521`. The rest are tests and the test double.

The three arms of `process_start_identity` — the process **birth token**, on which every
`SIGKILL` decision at the process boundary rests:

| arm | line | source | resolution |
|---|---|---|---|
| macOS | `:490` attribute, `:491` fn | `libc::proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec` + `pbi_start_tvusec` | **seconds + microseconds** |
| Linux | `:521` attribute, `:522` fn, `:523` `let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;` | field 22 (`starttime`) of `proc(5)`, taken at `:528` as `nth(19)` after the `") "` that closes `comm` | **clock ticks**, `fine: 0` at `:531` |
| everything else | `:537` attribute, `:538` fn | — | `None`, unconditionally |

**It is weaker in two measurable ways.**

1. The struct (`:477`) carries `coarse` (`:479`) and `fine` (`:481`); the Linux arm always sets
   `fine: 0`, so its token is one field where macOS's is two. INFERRED — from `proc(5)`, not measured
   on a Linux host: `starttime` is in `sysconf(_SC_CLK_TCK)` units, conventionally 100 Hz, giving
   **10 ms granularity against macOS's 1 µs**.
2. macOS returns `None` only when `proc_pidinfo` reports a short read. The Linux arm returns `None`
   on **any** failure to open, read or parse `/proc/<pid>/stat` — which in a container is a live
   case: `hidepid=`, a PID namespace, a `/proc` not mounted.

**And `None` is permissive.** `is_recycled` (`crates/rmux/src/process_boundary.rs:363`) is
`matches!((recorded, observed), (Some(recorded), Some(observed)) if recorded != observed)`, so
`member_identity_still_proven` (`:436`) answers *proven* whenever either token is `None` and the
`SIGKILL` decision falls back to the pre-fix `getsid`-only proof. That is debt row **C2**, and §7.1
is where its disposition expires.

**MEASURED: no gate has ever compiled that arm.** All 70 manifest cells ran on Darwin, and
`#[cfg(target_os = "linux")]` is not compiled for a Darwin target.

**But a default-run unit test is already waiting for it, and it is your cheapest first Linux
measurement of anything.** `crates/rmux/src/process_boundary.rs:781`,
`a_member_whose_birth_token_changed_is_never_signalled`, takes the token of the *test's own live
process* (`:786`) and then asserts, under `#[cfg(any(target_os = "linux", target_os = "macos"))]`
at `:787`, that *"supported platforms must expose a birth token"* (`:790`). Its neighbour at `:761`
does the same for `getsid` and the live process table. Neither needs a sidecar, a PTY, a candidate
binary, `--ignored` or a credential — they are inside the crate's own `#[cfg(test)]` module (opened
at `:542`) and run under plain `cargo test -p pseudomux-rmux`. **Run that before anything else on a
Linux host.** If `/proc` is readable it goes green and the `/proc` parse is confirmed on real data;
if the container hides `/proc`, it goes red immediately and names the reason, which is a far better
day one than discovering it through a `SIGKILL` that did not happen.

**And the conservative Linux arm C2 asks for is already written — in another crate, in a test.**
`crates/e2e/tests/full_stack.rs:4741` is a **second, independent** three-arm implementation of the
same birth token, used by the e2e suite to check the product's answer. It splits on the same `") "`
and takes the same `nth(19)` — but at `crates/e2e/tests/full_stack.rs:4744` it maps only
`ErrorKind::NotFound` to "no token" and **propagates every other error**, where production's
`crates/rmux/src/process_boundary.rs:523` is `.ok()?` and collapses `EACCES`, `EPERM`, a `hidepid=`
mount and a missing `/proc` into the same permissive `None`. Its unsupported arm at
`crates/e2e/tests/full_stack.rs:4820` returns an `Unsupported` **error** where production's
`crates/rmux/src/process_boundary.rs:537` returns `None`. So the tree already holds both
dispositions of C2, in two crates, pointing opposite ways, and the **stricter one is the one that is
not shipped**. Port them together or you will fix one and leave the other — and that one platform
rule is written twice at all is §6.1's class, waiting for exactly the platform that would make the
two disagree.

### 4.4 Two platform assumptions in the same file that are behind no `cfg` at all

* **`/bin/ps`, hard-coded, with BSD-style argv.** `crates/rmux/src/process_boundary.rs:371` is
  `Command::new("/bin/ps").args(["-axo", "pid=,ppid="])`, and `bin/pmux-rmuxd/src/main.rs:370` is the
  same call. UNVERIFIED on Linux: that `/bin/ps` exists at that path (Debian bookworm usrmerges it;
  a distroless or Alpine image does not have it at all) and that procps accepts that argv
  identically. There is no `cfg` and no fallback — a missing `ps` surfaces as
  `could not run /bin/ps`.
* **`libc::getsid` for session membership.** Portable in principle; unexercised here.

### 4.5 Every screen-shape claim, and the instrument that already exists to re-take them

pmux's input gate, completion gate and modal classifier are geometry and phrase claims about the
Claude Code TUI. `crates/service/src/screen_corpus.rs` exists exactly because two of them were wrong
and neither was findable by reading the code — the composer gate measured the cursor's distance from
the physical bottom of the grid (Ink does not always paint to the bottom), and the `/clear` menu
renders its selection in **foreground colour alone**, which was not hard to read in pmux's data, it
was absent from it.

**Recording is opt-in and does not perturb the production path.** Set `PMUX_SCREEN_CORPUS_DIR`; the
disabled path is a single relaxed atomic load, and when enabled, frames go to a bounded channel
drained by one dedicated OS thread — never the tokio runtime the 25 ms poll shares — and a full
channel **drops** the frame rather than blocking the poll. `dropped_frames` reports the loss.

The corpus stamp is the reason this ports: `CorpusStamp` (`crates/service/src/screen_corpus.rs:82`)
carries `claude_version`, `os`, `arch`, `rows`, `cols` and a label, because *the invariants are
claims about a version of Claude Code, and a frame with no version attached cannot refute or confirm
any of them.* **A Linux corpus recorded next to the macOS one is the cheapest way to find out which
of §4.1's 48 sites are actually platform-sensitive** — record, then compare, rather than re-deriving
48 claims by hand.

### 4.6 The alias family: firmlinks die, the class does not

macOS's APFS firmlink namespace is why containment here is answered on `(st_dev, st_ino)` rather than
on path prefixes. `crates/service/src/claude_launch.rs:528`: *"macOS `canonicalize` does not collapse
the APFS firmlink namespace, so a cwd of `/System/Volumes/Data/private/tmp/W` and a root of
`/private/tmp/W/inner` are the SAME containment the rule exists to refuse and neither is a component
prefix of the other."* The walk that replaced the prefix test is `one_directory_contains_the_other`
(`:616`) and its directed form `directory_lies_within` (`:643`), and both resolve each ancestor with
`must_treat_as_same_directory`, *"which follows symlinks and answers on `(st_dev, st_ino)`"*
(`:633`).

**The specific alias is macOS-only; the decision is not.** Because the answer is taken from the
kernel rather than from a string, the same code should hold on Linux over bind mounts, overlayfs
lower/upper spellings, `/proc/self/root` and a container's mount namespace — but that is INFERRED
from the mechanism, not measured. The two `native.rs` alias tables that enumerate spellings
(around `crates/service/src/native.rs:9747` and `:6614`) name a firmlink path that **does not exist
on Linux**, so those rows need a Linux equivalent or an explicit nonclaim. Do not delete them into
silence.

**And note how they will fail — they will not.** Both tables are inside `native.rs`'s `#[cfg(test)]`
module and both firmlink arms are additionally behind `#[cfg(target_os = "macos")]`, so on Linux
they do not go red, they **vanish**, and the suite stays green having tested one alias fewer. An arm
that disappears with the platform is not a passing test. Expect this shape wherever §4.2's 22
`"macos"` `target_os` lines sit inside test code, and read a green Linux suite as a smaller suite
until you have counted it.

### 4.7 The 51 ignored tests, and what a green `cargo test` on Linux does and does not buy

**`docs/gate-c-linux-handoff.md` §4's Step 1 is "establish that the tree builds and the
deterministic suite is green on Linux". Know what that sentence is worth before you spend a day
on it.**

MEASURED at `0d83b7a`: `cargo test --workspace` reports 1,226 passed and **51 ignored**, and every
one of the 51 names its own reason. Read them out of the run rather than from me —
`cargo test --workspace 2>&1 | grep '\.\.\. ignored, '` — and they partition:

| what the ignore reason says the test needs | tests |
|---|---:|
| a live `pmux-rmuxd` sidecar, a real PTY, or the exact candidate binaries — **credential-free, no real Claude** | **44** |
| a real `claude` executable and/or real model turns | 7 |

**44 of the 51 are free, and running them is the highest-value hour on this lane.** They are what
Gate A's `gate_c` phase already runs on macOS, with no environment and no credentials — build the
companions first, exactly as the manifest's first `gate_c` cell does:

```bash
cargo build --locked -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook
cargo test --locked -p pseudomux-service --test native_service   -- --ignored --test-threads=1
cargo test --locked -p pseudomux-service --test private_runtime  -- --ignored --test-threads=1
cargo test --locked -p pseudomux-service --test lifecycle_faults --            --test-threads=1
```

`gate_d`'s three `release_*` cells are the same three targets against the release binaries with
`PMUX_TEST_BIN_DIR` pointed at them, and the other seven `gate_d` cells are the per-binary
blackboxes. That is the whole live-runtime surface, and none of it costs a model turn.

**What is NOT behind the `#[ignore]`, so do not overcorrect.** MEASURED: `cargo test --workspace`
already runs `crates/rmux/src/process_boundary.rs`'s own unit tests (§4.3), the 25 tests of
`bin/pmux/tests/process_boundary.rs`, and **two** `lifecycle_faults` tests that launch a real
sidecar and real descendants —
`repeated_exact_sidecar_loss_reaps_active_descendants_and_runtime_artifacts` and
`observed_descendant_escape_keeps_real_close_unconfirmed_across_retry` — in 6.5 s. So the default
suite is not free of the operating system; it is thin on it. The honest sentence for your Step 1
report is *"the deterministic suite is green and it exercised N of the process-boundary tests,
leaving 51 unrun"*, with N counted, not *"the suite is green"*.

**And expect the count itself to move.** §4.6's firmlink arms vanish on Linux rather than failing,
so "1,226 passed" is not the number you will see, and a smaller number is not a regression. Diff
the *names*, not the totals: `cargo test --workspace -- --list` on both hosts is the comparison
that means something.

---

## 5. The authentication hinge

**This is the cheapest high-value measurement available to you, it costs zero live model attempts,
and the whole platform answer turns on it. Do it first.**

### 5.1 What macOS does, read out of the installed bundle

All of this is MEASURED by reading `~/.local/share/claude/versions/2.1.227` directly (272 MB,
`file` → `Mach-O 64-bit executable arm64`), not from any document in this repository.

**The keychain service name is a function of the config directory.** Verbatim from the bundle:

```js
function gee(){ let e=process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR;
  if(e!==void 0) return (e||join(homedir(),".claude")).normalize("NFC");
  return vn() }
function UY(e=""){ let t=process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR,
  r = t!==void 0 ? !t : !process.env.CLAUDE_CONFIG_DIR,
  n = t!==void 0 ? t.normalize("NFC") : vn(),
  o = r ? "" : `-${createHash("sha256").update(n).digest("hex").substring(0,8)}`;
  return `Claude Code${ua().OAUTH_FILE_SUFFIX}${e}${o}` }
function j8(){ let e; try{ e=process.env.USER||userInfo().username }catch{ e="claude-code-user" }
  if(!yky.test(e)) return "claude-code-user"; return e }
```

So the item is `security find-generic-password -a "<j8()>" -w -s "Claude Code-credentials<suffix>"`,
where the suffix is empty when neither `CLAUDE_SECURESTORAGE_CONFIG_DIR` nor `CLAUDE_CONFIG_DIR` is
set (or the former is set to the empty string), and otherwise `-` plus the first 8 hex characters of
`sha256(NFC(dir))`. **The suffix is the OAuth environment, and only one of its values is
production** — MEASURED, the name `OAUTH_FILE_SUFFIX` occurs **five** times in the binary, of which
**three are value assignments**: `""` (the production OAuth environment, beside
`platform.claude.com`), `"-local-oauth"` and `"-custom-oauth"`. The other two are a key table and
the single read site inside `UY`. Count the assignments, not the occurrences; the raw grep is 5 and
means something else. **The account is `$USER` and its fallback matters in a container:** `j8()` takes
`process.env.USER || userInfo().username`, validates it against `/^[a-zA-Z0-9._-]+$/`, and answers
`claude-code-user` if either step fails. MEASURED counts in that binary: `find-generic-password`
**12**, `CLAUDE_SECURESTORAGE_CONFIG_DIR` **13**, `.credentials.json` **18**.

**pmux exploits exactly this, and the code says so.** `crates/service/src/config_isolation.rs:8`:
*"because Claude namespaces the macOS keychain SERVICE NAME by `sha256(config_dir)[0..8]`, so a fresh
root looks up an empty item and reports 'Not logged in'"*. The other half is delivered at
`crates/service/src/claude_launch.rs:1000`,
`variables.insert("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), pin);` — a pin that is deliberately **not
canonicalized**, because Claude hashes the string and normalizing it would hash to a different
service name.

MEASURED on this host today: `security find-generic-password -a "$USER" -s "Claude Code-credentials"`
exits **0** (the item is present) and `$HOME/.claude/.credentials.json` is **absent**.

### 5.2 What the bundle says happens where there is no `security(1)`

**MEASURED, and it is the single most useful fact in this section: the credential store is not a
platform switch, it is a composite.** Verbatim from the same binary:

```js
function dSu(e,t){ let r={ name:`${e.name}-with-${t.name}-fallback`,
  read(){ let n=e.read(); if(n!==null&&n!==void 0) return n; return t.read()||{} }, … } }
tbo={ name:"plaintext", read(){ let {storagePath:e}=Pin(); … } }
function Pin(){ let e=gee(); return { storageDir:e, storagePath:join(e,".credentials.json") } }
```

The primary is `{name:"keychain", …}` (the `security` calls above); the fallback is `plaintext`,
reading and writing `<gee()>/.credentials.json`, and the composite's telemetry names the branch
`plaintext_fallback_used` (MEASURED: 2 occurrences, alongside 2 of *"Storing credentials in
plaintext"*).

**MEASURED: the strings `libsecret`, `gnome-keyring` and `keyring` occur 0 times in the 2.1.227
bundle.** There is no Linux keyring integration to find in this build.

**INFERRED, with the argument stated:** on Linux there is no `security(1)`, the call throws, the
`catch` returns null, and the composite falls through to `<gee()>/.credentials.json` — i.e.
`~/.claude/.credentials.json` when nothing is pinned. Since `gee()` reads
`CLAUDE_SECURESTORAGE_CONFIG_DIR` first and maps the empty string to `join(homedir(),".claude")`,
pmux's existing pin should keep an isolated cell pointed at the operator's own store on Linux exactly
as it does on macOS — but at a **file** rather than a keychain item.

### 5.3 What is unmeasured, and the experiment

**Do not close any of these by argument.**

1. **The bundle read above is the macOS build.** `file` says Mach-O arm64. The Linux release is a
   different executable, and that the same `keychain-with-plaintext-fallback` composite ships there
   is an inference from one platform's bytes. **Re-read `find-generic-password`, the composite
   constructor, `gee` and the service-name function out of a Linux 2.1.227 before writing a sentence
   about them.** `strings` plus `grep` on the installed binary is the whole method; it took minutes
   here.
2. **Whether the fallback read is clean or noisy on a host with no `security`.** Read precisely,
   because a summary of this gets it backwards: the **synchronous** `read()` catches the throw and
   then, *if a previous read had cached anything*, logs `[keychain] read failed; serving stale
   cache` at `warn` and returns the stale value — it returns null silently only when the cache is
   empty, which on a Linux host is the state at process start. The **async** path logs `[keychain]
   readAsync failed; not caching a null`, sets a last-read failure timestamp, and suppresses retries
   for 1,000 ms (`shs`) against a 30,000 ms cache window (`Z_o`). So the first read on a Linux host
   should be silent and the composite should fall straight through — INFERRED from those bytes.
   Whether it surfaces to a pooled cell as a delay or a log line is **UNVERIFIED**: the `security`
   spawn also carries a 2,000 ms timeout (`Ymr`), and nobody has measured what a missing binary
   costs versus a hung one.
3. **Write-back.** An OAuth refresh inside a cell rewrites the operator's credential. On macOS that
   is a keychain item; on Linux it would be a 0600 file the operator's own session is also
   read-modify-writing. Nothing measures that race, on either platform.
4. **How a Linux host is logged in at all** — whether `claude setup-token` or `/login` in a
   container-less Linux session produces the same `.credentials.json`, and whether headless login is
   possible without a browser.
5. **The spelling of the config directory in the non-`SECURESTORAGE` branch.**
   `crates/service/src/config_isolation.rs:79` states, against the 2.1.220 bundle, that the sibling
   *config* file is `${CLAUDE_CONFIG_DIR || homedir()}/.claude.json`. The credential *directory*'s
   spelling in that branch is INFERRED from it, not read.

**Why this is first.** `docs/sandbox-spike.md` reached the same conclusion from the other direction:
if Linux Claude Code keeps its credential in a file inside the config root, a per-cell credential
file becomes bind-mountable and an entire class of isolation design that is structurally blocked on
macOS opens up. Answering item 1 is an afternoon with `strings`. Answering it wrongly by argument
poisons everything downstream.

### 5.4 What is *not* an authentication question

**The Docker lane does not need any of this.** `tools/linux-docker/README.md` states, and its cells
enforce, that the container runs with **no network, no credentials and no real `claude`** — *"A
Docker result is deterministic portability evidence, not credentialed native-Linux Claude support."*
Gate C and the credential question are two different projects. Conflating them is the easiest
mistake available on this lane.

---

## 6. The bug classes you will meet

### 6.1 The house bug class

**A guard, comment, document, test name or receipt whose message promises more than its predicate
tests; or a check whose set-of-things-to-check is hand-written where it could be derived.**

`docs/current-state.md` §9 carries a numbered ledger of instances that runs to **instance
thirty-three** (§9.29). **MEASURED, and the measurement is itself the lesson:** a bare
`grep -c "THE BUG CLASS, instance"` over that file returns **16**, but only **15** of those lines
are headings (`grep -c '^### .*THE BUG CLASS, instance'`) — the sixteenth is prose in §9.28 quoting
the phrase. That is not a coincidence you get to enjoy from outside. §9.28 records
`test_run_gate.py::test_every_statement_of_the_bug_class_counter_spells_the_same_ordinal`, which
reconciles the counters in three source files against this document, and records that its *second
run caught exactly this*: the ordinal came back as an ellipsis because the last match in the file
was prose quoting a heading. The truth is now read only from lines beginning `###`. A tool built to
find the class found it in itself on the second run, and a hand count of the same thing was wrong
one paragraph after being written down here.
Do not read the ledger as history — read it as a prediction. Its §9.25 is titled *"seven claims
written BY the pass that installed the tool to find them"*, which is the shape to expect: **the class
appears inside the instruments built to catch it**, and it will be waiting for you in the Linux lane.

Three shapes, so you can recognise them without the list:

* **A name wider than its predicate.** The `gate_a` cell `rmux_server_vendor_patch_regressions`
  (`tools/gate-a-candidate/phase-manifest.json:293`) runs
  `cargo test … --manifest-path vendor/rmux-server/Cargo.toml --lib --no-default-features
  pane_io::tests::`. The name claims the vendor patch; the predicate runs one module's tests.
* **A count that is a fact about the checker.** `scripts/gate-a-residue.sh:186`'s comment records the
  original: the receipt printed `candidate_executables=8` meaning *"our literal has eight entries"*,
  and would have printed 8 against a directory of twenty. The repair is that the literal became a
  lower bound and the scanned set became `find`-derived.
* **A list that exists three times.** MEASURED, `git grep -n SCAN_SKIPPED_DIRECTORIES`:
  `crates/rmux/tests/vendor_server_patch.rs:71` and `crates/service/src/stateless.rs:779` declare
  eight entries; `crates/service/tests/path_b_doc_citations.rs:131` declares nine, and the ninth is
  `vendor`. Two guards over one boundary fail by drifting apart.

**The test that tells you which one you have: delete the check and see whether the suite notices.**
If nothing goes red, the check was decoration.

### 6.2 The composer-escape family — and every one of these is a screen-shape claim

pmux proves a turn by **equality**: the text pmux typed is normalized and compared to the text Claude
recorded, and `TranscriptEngine::ingest` refuses the turn when they differ. So a prompt pmux admits
and the composer does not record verbatim is not cosmetic — it is a turn that can never be
acknowledged, and on Path B it is a pooled instance destroyed for an input pmux said was legal. That
is why `crates/claude/src/composer.rs` is a crate-level module and not a guard in the daemon: three
entry points previously each carried their own copy of the rule.

Three measured findings, each from Claude Code 2.1.226/2.1.227 on macOS 15.7.7 / aarch64:

* **A leading `!` was host command execution.** The composer reads a leading `!` as a mode switch
  into bash mode, drops it from the buffer, and Enter runs the rest of the prompt **as a shell
  command on the host** — outside the tool surface, outside `--disallowedTools "*"`, outside
  `--permission-mode`, and outside everything a Path B cell's isolation is built from. Reproduced six
  of six on a warm post-`/clear` instance, three of them concurrently at the 15-instance cap, each
  writing a file that was there afterwards. The turn then never acknowledges, so it also runs to the
  caller's 600,000 ms deadline. **It fires through a bracketed paste**, which is the only way pmux
  ever delivers a prompt. Shipped as `COMPOSER_MODE_PREFIXES` (`crates/claude/src/composer.rs:266`),
  `['/', '!']`.
* **A trailing `\` made a turn never send.** `\` immediately before the cursor is Claude Code's
  multiline chord: Enter deletes the backslash and inserts a newline. Measured on the screen and
  measured through pmux twice, with one trailing backslash and with two, **both of which ran to the
  caller's deadline having written no `user` row at all.** It is not an escaping rule — two
  backslashes fail exactly as one does, because what is tested is the character before the cursor.
  Refused rather than normalized, because removing it would change the text.
* **A tab is a rewrite, not a refusal.** U+0009 is recorded as four U+0020, so the prompt is
  admitted, typed, and then refused by an acknowledgement it can never satisfy. U+000B and U+000C are
  the same measurement two versions later and are recorded as the two ASCII characters of their caret
  notation, `^K` and `^L`. All three are `COMPOSER_REWRITTEN_CHARACTERS`
  (`crates/claude/src/composer.rs:285`).

The sweep behind them is a derived alphabet, not a list: every ASCII punctuation character sent as
the first character of an ordinary prompt, `MEASURED_FIRST_CHARACTER_SWEEP`
(`crates/claude/src/composer.rs:822`) at **32 of 32**, with the completeness test deriving the
alphabet from `is_ascii_punctuation` and refusing any character in neither table.

**Every sentence above is a claim about what a macOS build of Claude Code does with a keystroke.**
None of it transfers to Linux by argument. Re-run the sweep, with the corpus recorder on (§4.5), and
expect at least the terminal-level claims (paste bracketing, wrap column, right-trim) to need
re-taking.

### 6.3 `Unrecognised` — the shape that turns a missing rule into a hang

The most instructive defect in the tree: `blocking_screen`
(`crates/service/src/driver_io.rs:4008`) recognised **24 screen shapes** (MEASURED: 24
`NeedsInputKind::` rows in `BLOCKING_SCREEN_ALTERNATIVES` — Quota 6, Login 4, Permission 4,
UnknownModal 4, Trust 3, Update 3) and answered `Option<NeedsInput>`, so `None` — *"no rule
matched"* — reached every caller as the same value as *"this is an ordinary non-modal screen"*. A
real "trust this directory" screen pmux had not been taught was therefore **PROCEED**, and the turn
ran to its 600,000 ms deadline sitting on a modal. Meanwhile the test table beside it
(`prompt_and_modal_classification_are_conservative`, `crates/service/src/driver_io.rs:5426`) held
**one screen per kind — six rows against twenty-four alternatives** — so the *first* phrase of each
arm was the only one any test could see: ten `||`↔`&&` mutants survived the whole suite inside that
one classifier.

Three repairs, and all three are the pattern to copy:

1. **A distinct value.** `TerminalScreenState::Unrecognised(ScreenShape)`
   (`crates/service/src/driver_io.rs:283`) carries the shape and no caller can read it as a negative.
2. **A register of every decision site, checked against a scan of the crate's own source.**
   `RENDERING_SITES` (22 rows) records, for each function that turns a rendering into a decision,
   what it does with a frame it does not recognise — `Distinct`, `ClosedByCaller(who)`,
   `Refuses(gate)`, `DecidesNothing(why)` or `TestOnly`. A function that starts reading a frame
   tomorrow fails the test by name until somebody says what its unrecognised arm does.
3. **The phrase table derived from the classifier.** `BLOCKING_SCREEN_ALTERNATIVES`
   (`crates/service/src/driver_io.rs:10574`) holds one row per independent way a screen reaches a
   kind, each phrase inside a row asserted load-bearing by dropping it; and a second test reads the
   string literals **out of `blocking_screen`'s own body** and fails if the classifier names a phrase
   the table does not.

**Every phrase in that table is English text painted by a specific build of Claude Code.** Re-verify
them on Linux before trusting a green suite: a phrase that changed spelling turns into a hang, not a
wrong answer, and a green suite will not tell you.

---

## 7. The debt rows that expire on Linux, and C6's real divergence

`docs/current-state.md` §9.4 carries rows C1–C11 plus one reclassification; §9.3 carries the deferred
advisory rows including 22–24. Four of them are yours.

### 7.1 C2, C3 and C4 — the process boundary

**C2** (`crates/rmux/src/process_boundary.rs:436-450`, `:534-540`; both ranges re-read at `0d83b7a`
and both still land on the code the row describes). Its disposition reads, verbatim: *"v1 ships macOS-only and Gate C is Linux-only, so **no
supported platform is affected today**; but the fallback should be conservative (refuse to signal on
an unreadable token) rather than permissive."*

**That clause expires the moment Linux is supported, and it expires harder than the row anticipates.**
The row's own mechanism is *"on any target that is not macOS/Linux … or whenever the token read
fails"*, and on Linux the token read is a `/proc` file open, which fails for reasons that have nothing
to do with the process being gone (§4.3). The permissive-`None` path stops being theoretical the day
you run in a PID namespace.

**C3** (`crates/rmux/src/process_boundary.rs:300-304`, `:336`, `:411`, `:436-450`; all four re-read,
all four land). The residual PID-reuse hazard:
`session_id_recycled` needs a **live** process-table row at the leader PID, so once a recycling
stranger-leader exits, its orphaned same-session children become admissible members again. The
disposition is *"requires an adversarially precise coincidence"* — pid-space wrap onto exactly the
leader PID, plus `setsid`, `fork`, `exit`, inside one 25 ms poll gap. **That probability argument is a
macOS argument.** Linux's default `pid_max` is 32,768 against macOS's much larger space, and a
container's PID namespace starts at 1 and wraps far sooner. Nobody has re-derived the coincidence
probability for a Linux PID space. Do that before inheriting the disposition.

**C4** (`crates/rmux/src/process_boundary.rs:338-342`). The comment at `:338-339` says membership
*"also refreshes the token retained for it"* and the code is `.entry(...).or_insert(...)`, which never
overwrites — so a PID first recorded with a `None` token keeps `None` forever. This is the house bug
class in three lines of production code, it pairs with C2, and it is platform-independent — but it is
the first thing you will trip over while reading the Linux arm.

### 7.2 C6 — the divergence is thirteen names, not seven

**Re-derived at `0d83b7a` independently of the test**, from `tools/gate-a-candidate/phase-manifest.json`
against `tools/linux-docker/gate-a-manifest.json` against the `CONTAINER_ONLY_GATES` frozenset at
`tools/linux-docker/tests/test_runner.py:30`:

```
candidate cells: 70        CONTAINER_ONLY_GATES: 15
linux manifest gates: 84   exact projection would be: 85
```

| in the Linux manifest, in neither the candidate nor the container-only set (6) | in the projection, absent from the Linux manifest (7) |
|---|---|
| `candidate_envelope_tests` | `candidate_envelope_self_tests` |
| `evidence_common_tests` | `evidence_common_self_tests` |
| `linux_runner_tests` | `linux_docker_self_tests` |
| `phase0_evidence_tests` | `phase0_self_tests` |
| `python_package_artifact` | `gate_driver_self_tests` |
| `typescript_package_artifact` | `cargo_mutants_version` |
| | `mutation_score_agent_launch_pool_protocol` |

Row C6's own sentence — *"the failure now names the seven drifted cells one by one"* — is narrower
than what the test prints. It prints **thirteen, in two directions**, and the thirteen decompose into
three kinds of work, not one:

* **Four are rename pairs** and consume one name from each column: `candidate_envelope_tests` ↔
  `candidate_envelope_self_tests`, `evidence_common_tests` ↔ `evidence_common_self_tests`,
  `phase0_evidence_tests` ↔ `phase0_self_tests`, and `linux_runner_tests` ↔
  `linux_docker_self_tests`. All four candidate-side names are `gate_f` cells. These are mechanical.
* **Three are cells the Linux lane never acquired**: `gate_driver_self_tests` (`gate_f`), and
  `cargo_mutants_version` and `mutation_score_agent_launch_pool_protocol` (both `gate_b`). The last
  is the 6,197 s cell of §2.3, so adding it is a scheduling decision as well as a manifest edit.
* **Two are the unsatisfiable `*_package_artifact` pair**, which the Linux manifest carries and the
  candidate does not. §7.3.

**`release_full_stack_e2e` is in BOTH manifests** — candidate phase `gate_a`, Linux phase `D`
(`tools/linux-docker/gate-a-manifest.json:57`). It is in neither drift column. Any claim that it is
missing from one manifest is false. What is true is that the two disagree about its **phase** — the
projection builds `("A", "release_full_stack_e2e")` from the candidate's `gate_a`, the Linux
manifest says `D` — and that the disagreement is **latent**: the projection test's own ordered
assertion, `tools/linux-docker/tests/test_runner.py:393`, sits three lines under the set assertion
at `:388` that fails first, so nothing has ever evaluated it. Do not confuse that with `:843`, which
is in a **different and currently green** test — the one declared at `:798` — that only checks the
relative order of nine names *within* the Linux manifest and never looks at the candidate at all.
`:843` is green with the cell in D. It is also the assertion that goes **red** the moment somebody
repairs C6 by moving the cell to phase A, because its hand-written `required` list at `:831` places
`release_full_stack_e2e` after `typescript_stage_preconsume_unchanged`. Two tests, opposite
directions, one manifest edit: that is why §7.3 calls this a policy decision and not a fix.

**Both blockers C6 names are real, and every line number it gives them has rotted.** All of the
following are in `tools/linux-docker/tests/test_runner.py`. The row cites `:651` and `:638-650` for
the package-framing blocker; the test is at **`:656`**
(`test_package_framing_property_and_shellcheck_gates_are_exact`) and its hand-written `required` list
starts at **`:661`** — and that list does demand `typescript_package_artifact`,
`python_package_artifact` and `candidate_envelope_tests`. The row cites `:808-821`/`:821` for the
ordering blocker; the ordering `required` list starts at **`:831`**, and it does place
`release_full_stack_e2e` between `typescript_stage_preconsume_unchanged` and
`release_binary_unchanged`, i.e. inside phase D.

**Two C6-adjacent claims elsewhere are stale.** `docs/current-state.md` §7.6 puts the projection test
at `tools/linux-docker/tests/test_runner.py:277` (it is `:296`) and reports *"12 pre-existing
host-Git ownership failures"*; deferred row 24 says *"12 red tests carried"*. **MEASURED: zero.** On
this host today `test_docker_ownership.py` is 7/7 green and `test_source_digest.py` is 32/32 green.
**The lane is one failure from clean, not thirteen.**

### 7.3 The two policy decisions inside that one failure

Repairing the projection test means editing two other tests in the same file, and both edits are Gate
C policy decisions rather than drive-by fixes — which is exactly why C6 is still open:

1. **Does the Linux lane still run `typescript_package_artifact` / `python_package_artifact`?** Their
   five `PMUX_PACKAGE_SMOKE_*` anchors are read from the environment by
   `tools/package-smoke/package_smoke.py:1121` onward and produced by nothing in the Linux
   `suite.sh`. Deferred row 38 proposes a self-derived fallback; the alternative is to drop the two
   cells with a written nonclaim.
2. **Does `release_full_stack_e2e` run in phase A or phase D?** The candidate puts it in `gate_a`;
   the container has no release binaries until D, which is where the Linux manifest puts it.

Decide both, in writing, before touching the manifest.

**`docs/gate-c-linux-handoff.md` is leftover C6 freeze notes, not the living first read.**
Living first read is `tools/dev/README.md`. The Gate C file's §4 starting sequence is historical — its
§3.2 is not current. That section describes the candidate as **75 cells** and asserts *"all 75 candidate
cells are present in the Linux manifest"*. MEASURED at `0d83b7a`: the candidate is **70**, and
**seven of its cells are absent** from the Linux manifest — `candidate_envelope_self_tests`,
`cargo_mutants_version`, `evidence_common_self_tests`, `gate_driver_self_tests`,
`linux_docker_self_tests`, `mutation_score_agent_launch_pool_protocol`, `phase0_self_tests`. Its
fifteen container-only names are still exactly right (MEASURED: all 15 are in the Linux manifest and
none is in the candidate), and its worked count literals at `tools/linux-docker/tests/test_runner.py`
are gone — those counts are derived now, which is why the failure names cells rather than numbers.

---

## 8. The Linux docker lane as it exists

### 8.1 What is there

`tools/linux-docker/` — `run.sh` (1,218 lines, host driver), `inside.sh` (171), `suite.sh` (660, with
**105** `run_gate`/`skip_gate` invocation lines), `Dockerfile`, `source_digest.py` (2,063),
`evidence.py` (6,009), `bounded_runner.py` (180), `permissions_probe.py` (650),
`gate-a-manifest.json`, `README.md`, and **5 test files / 111 tests** (4,816 test lines).

Invocation is deliberately un-guessable: `run.sh` demands `--source-sha256`, a digest-qualified
multi-arch `--base-image`, and `--acknowledge-docker`. Compute the first with
`python3 tools/linux-docker/source_digest.py "$PWD"` (add `--json` for the file list) — MEASURED, it
covers **983 files and 193 directories** under algorithm `pmux-source-v2-path-mode-size-content-sha256`.
**Do not pin the value in prose: it is a function of every tracked file, so this very document moves
it.** MEASURED on this host: `docker version` reports server **29.1.5**, and `shellcheck` is present
at `/opt/homebrew/bin/shellcheck`.

**No reviewed multi-arch base-image index digest is recorded anywhere in the repository**, so Gate C
has no runnable invocation until somebody picks one and writes it down. MEASURED:
`tools/linux-docker/run.sh:49` shows the shape as
`--base-image docker.io/library/rust:1.88.0-bookworm@sha256:MULTIARCH_DIGEST`, a literal
placeholder; and `git grep -l 'sha256:[0-9a-f]\{64\}'` over the tree, excluding `vendor/` and
lockfiles, returns only `tests/conformance/v1/golden.json` and
`tools/linux-docker/python-requirements.txt` — neither is a base image. **Picking that digest is the
first thing that has to happen inside Gate C**, and it fixes the Rust toolchain the container
compiles with. Pick a `1.88.0` image: `rust-toolchain.toml` pins the workspace to `channel =
"1.88.0"` with `rustfmt` and `clippy`, the placeholder at `tools/linux-docker/run.sh:49` names
`rust:1.88.0-bookworm`, and the one archived Linux run (§8.2) recorded `rustc 1.88.0` — three
statements of one version, which is itself a set worth deriving one day.

### 8.2 What has actually run — and this contradicts a document

`docs/current-state.md` §7.6 says the lane is *"Never built, never run"*. **MEASURED: on this host,
`.context/linux-docker/` holds evidence of a completed Linux container run from 2026-07-19.**
`final-6552eb85-20260719T1415Z/arm64/result.json`:

```
{"failure_count": 0, "gate_count": 14, "schema_version": 1, "status": "pass"}
gates: rust_fmt rust_check rust_clippy rustdoc rust_tests python_client phase0
       typescript private_pty pmuxd_owner_death pmuxd_pane_reap fake_claude_e2e
       linux_blackbox evidence_binding
```

and its `system.json`: `platform` `Linux-6.12.65-linuxkit-aarch64-with-glibc2.36`, `machine`
`aarch64`, `kernel` `6.12.65-linuxkit`, uid/gid 10001, `effective_capabilities_hex
0000000000000000`, rustc and cargo **1.88.0**, node `v18.20.4`, python `3.11.2`,
`test_storage_filesystem overlayfs`, `real_claude_invoked: false`, `credential_free: true`,
`tested_binary_sha256` over 5 binaries (`pmux`, `pmux-launcher`, `pmux-mcp`, `pmux-rmuxd`, `pmuxd`),
`workspace_file_count: 112`.

**Do not over-read it.** `.context/` is gitignored, so this evidence exists only on this machine and a
fresh clone sees nothing — which is exactly how §7.6 came to say "never run". It is a **14-gate**
ancestor against today's 84, over a **112-file** source context against today's 983, and it predates
the squashed history (`crates/rmux/src/process_boundary.rs` first appears at `405fccd`, 2026-07-27,
eight days later). Whether it compiled any ancestor of today's `cfg(target_os = "linux")` arm is
**UNVERIFIED** — the archived manifests record a count and a digest, not a file list.

The honest sentence: *a much smaller ancestor of this lane ran green on linux/arm64 once, on this
machine, on 2026-07-19; nothing in the repository records it, and nothing about today's tree follows
from it.*

### 8.3 What currently fails

MEASURED at `0d83b7a`, `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/linux-docker/tests -v`:
**111 tests, 1 failure**, exit 1. Per file, each run alone: `test_bounded_runner` 7 OK,
`test_docker_ownership` 7 OK (it does talk to Docker), `test_evidence` 43 OK, `test_source_digest`
32 OK, `test_runner` 22 with the one failure. The identical result is inside the pinned gate-a
receipt at `f4622a9`.

The failure is `test_linux_manifest_is_the_exact_ordered_candidate_projection`
(`tools/linux-docker/tests/test_runner.py:296`), failing at the **set** assertion (`:388`) before it
ever reaches the ordered one. That one test is the entire content of the
`gate_f/linux_docker_self_tests` red cell, and therefore the entire content of "Gate A is 69/70
rather than 70/70". §7.2 and §7.3 are what it takes to close it.

---

## 9. The finish line

**This §9 is the 2026-08 Path B certification finish, not the living Linux session.**
Living finish: `tools/dev/check.sh` + `tools/dev/operator_eval.py` on 2.1.236.
`tools/dev/promote.py` only if `evidence/pooled-transcript-drain-linux-<arch>.json`
already exists (it does not). Do not run Gate A. Phase 0 and linux-docker are
deleted. Do not invent a linux drain.

**HISTORICAL.** Path B certification treated `scripts/path-b-done.sh` exit 0 as the Linux finish
against receipts that named the commit. The table's **70 cells** are `0d83b7a` history, not a
command to run. Living finish: `tools/dev/check.sh` + `tools/dev/operator_eval.py` on 2.1.236.

| # | criterion | mechanised today? | what a Linux pass needs |
|---|---|---|---|
| 1 | No known unfixed defect in the Path B path | **Yes, for what it reads.** Reads `evidence/path-b-defect-register.json` (schema `pmux.path-b-defect-register.v1`, statuses `OPEN`/`CLOSED`/`ACCEPTED`), reconciles its lettered rows against `docs/path-b-verdict.md` §1 in both directions, checks every row's own `where`/`anchor` citation resolves, then runs `scripts/register_currency.py` over the survivor register | One full-scope `cargo mutants` campaign on Linux (§3.2) with every survivor dispositioned — **plus a judgement the gate does not make: see the note below** |
| 2 | The adversarial suite passes | **Yes, fully.** Derives 8 commands rather than reading a list | The suite to be re-run on Linux. Its live half needs credentials (§5) |
| 3 | A promoted profile for the installed version, from machinery that exercises minified cells | **Yes, and it already refuses correctly** (`scripts/path_b_done.py:749`) | Two things and no more: a `PROMOTED_PROFILES` entry (`crates/service/src/compatibility.rs:484`) with `os: "linux"` and the right arch, and `evidence/promotion-<version>-linux-<arch>.json` produced by one run of `tools/promotion/promote_claude_version.py` |
| 4 | Gate A green except the deliberate Linux cell | **HISTORICAL** (`0d83b7a` 70-cell receipts; runner gone) | Do not run the 70 cells. Living criterion 4 is MET iff `run_gate.py` is gone and `tools/dev/check.sh` exists. Do not invent a linux drain. |
| 5 | Path B doc claims reconciled to measurement | **Yes, fully** — 4 citation rules over the workspace | Nothing platform-specific. It will grade whatever you write |

**Criterion 3 is the whole platform gate, and it is smaller than it looks.**
`tools/promotion/promote_claude_version.py` declares **9** checks (MEASURED: `grep -c 'Check('`), and
the done-gate requires the evidence file to carry all nine:

| check | costs real turns |
|---|---|
| `version_identity` | no |
| `launch_bundle_parses` | no |
| `minified_cell_is_admitted` | no |
| `grades_answer` | **yes** |
| `context_did_not_survive_recycling` | no |
| `no_tool_surface` | no |
| `pool_never_halted` | no |
| `drain_within_the_pooled_bound` | no |
| `nothing_survived` | no |

**Exactly one of the nine costs live model attempts.** The macOS 2.1.227 promotion spent 5.

**The budget is small and it is shared.** MEASURED,
`python3 tools/phase0/phase0.py budget --ledger evidence/model-attempt-ledger.ndjson`:
`ceiling 100`, `consumed 85`, **`remaining 15`**, plus 123 real turns behind committed receipts that
reserved no ordinal. A Linux promotion is affordable; a Linux promotion plus improvisation is not.
Run the tool, do not re-invent the session.

**What "done" does not mean — and this is the house bug class aimed at the done-gate itself.**
Criterion 1 is titled *"No known unfixed defect in the Path B path"*, and its predicate is: every row
of `evidence/path-b-defect-register.json` is `CLOSED` or `ACCEPTED`, the register and
`docs/path-b-verdict.md` §1 agree on the lettered set, every citation resolves, and no survivor row
is stale. **It does not read `docs/current-state.md` §9.4 at all.** `docs/current-state.md` is read
by exactly one criterion — criterion 4, to derive the grant for the deliberately-red cell
(`DEBT_DOCUMENT` at `scripts/path_b_done.py:72`, used at `:893` and `:912`). So C2, C3 and C4 (§7.1)
can all still say *"no supported platform is affected today"* on a Linux host and criterion 1 will
still print MET.

Concretely, three things a 5/5 on Linux would **not** establish:

* that the Linux arm of `process_start_identity` is correct — no criterion looks at it;
* that the screen-shape claims of §4.1 and §6 were re-taken — criterion 3 exercises **one** minified
  cell and five graded turns, not the sweep;
* that the C-rows whose disposition is a macOS argument were re-argued. That is a judgement, it is
  yours, and §7.1 is the list. Write the new disposition into the row; do not let a green gate stand
  in for it.

---

## 10. The method that actually worked

Short, and each of these paid for itself more than once in this tree.

1. **Reproduce before fixing.** Every closed defect here has a recorded reproduction. A fix without
   one is a guess with a diff attached, and C10 in `docs/current-state.md` §9.4 is the standing
   example of what happens when an intermittent is reasoned about instead of reproduced: 2 in 12
   whole-target sequences, 10/10 green in isolation, and a mechanism that was never a timing
   artifact.
2. **Prove every new test can fail.** Write the test, then break the thing it guards and watch it go
   red. A test that has only ever been green is an assertion about nothing.
3. **Delete the check and see whether the suite notices.** This is the cheapest detector for the
   house bug class (§6.1) and it takes thirty seconds.
4. **Measure rather than read, and design the probe first.** Two of `docs/path-b.md`'s own MEASURED
   claims were false, and its §0.3 is the five-rule answer: *"A probe that changes two things
   establishes nothing about either"*; a negative result needs a positive control; prefer the
   mechanism to the outcome; say which instrument you read; and an absence is evidence only if
   something would have shown a presence. Read that section before designing any Linux probe — it is
   the most reusable page in the repository.
5. **Quote the day and the unit or quote nothing.** `docs/2.1.227-compatibility.md` §2's 44 and this
   file's 48 are the same predicate on two different days over the same tree.
6. **A gate must gate exactly the claim it protects.** From C9's disposition: an upper wall-clock
   bound widened to survive a busy host asserts nothing. Keep the lower bound, record the
   observation, and do not widen a bound past its claim.
7. **Run the experiment before sealing it.** Any first-ever execution happens outside a sealed
   candidate, where a surprise costs a rerun instead of a checkpoint.

**Before every commit** (historical 0d83b7a ritual; living is `tools/dev/check.sh`), and these are cheap:

```bash
cargo clippy --workspace --all-targets -- -D warnings   # exit 0 at 0d83b7a; 4 warnings, all vendor/rmux-server
cargo fmt --all --check                                  # exit 0
ruff check --no-cache                                    # All checks passed!
PYTHONDONTWRITEBYTECODE=1 PMUX_E2E_BIN_DIR="$PWD/target/release" bash scripts/gate-a-residue.sh
```

MEASURED at `0d83b7a`: `cargo test --workspace --no-fail-fast` → **1,226 passed / 0 failed / 51
ignored**, over **72 `test result:` lines — 66 test binaries plus 6 doc-test targets** (say which,
or the number is a fact about your grep). The residue audit exits 0 with `candidate_executables=8`.
**Read §4.7 before you take the 1,226 as good news on Linux.**
Always export `PYTHONDONTWRITEBYTECODE=1` — a stray `__pycache__` in the source tree is a residue
failure.

---

## 11. Open items handed over

**The three rmux upstream drafts, none filed.** `docs/upstream-issues/` holds
`01-rmux-client-attach-unbounded-slice.md`, `02-rmux-server-attach-eof-drops-buffered-frames.md` and
`03-rmux-snapshot-revision-contract.md`. `docs/rmux-upstream-state.md` (2026-08-12) answers file /
revise / drop for each: **file all three, after the revisions its §2 lists, and do not upgrade rmux
for the sake of them — an upgrade retires none.** Drafts 01 and 02 were reproduced against pristine
upstream 0.10.0 crates from `static.crates.io`, and 01's one-line fix was confirmed sufficient there.
That document records upstream as active — 2,568 stars, `pushed_at` 2026-08-09, 487 commits ahead of
the vendored 0.9.0 — read out of the GitHub API on 2026-08-12; **not re-measured here, and the
network was not touched for this file.** This is not on the Linux critical path; it is a half-day of
editing that is already scoped.

**`tools/crash-harness` — the standing decision is "keep it detached", and it is worth re-reading
once.** It is the instrument that measured `docs/spec.md` §4.8.2's crash-safety claims, it is
**outside the workspace** with its own `[workspace]` table and lock, and that is deliberate:
`tools/screen-corpus/per_binary_tests.sh` enumerates every workspace target from `cargo metadata` and
prints *"every one of the N test targets passed in isolation"* — as members these two binaries would
widen that N by five while contributing zero test cases. It is not a gate cell because it is
probabilistic: `update` mode measures how often a crash lands in a window, and a window nothing lands
in is reported as a clean run rather than as a gap. Its first version sampled one phase of the cycle
and found **zero** wedges in 40 trials against a store that wedged 42% of the time. `docs/testing.md`
has the invocation and the calibration warning. The open question is only whether it stays or is
retired; nothing depends on it.

**Two of `docs/current-state.md` §11's three open questions are still unanswered, both Gate B-shaped
and both cheap.** Whether a rate-limited Claude returns `TurnTimeout` or a typed transcript
`ApiError` (one scenario row, one attempt); and wide-character width handling (one CJK and one emoji
prompt, written down). Both are macOS questions today and both acquire a Linux copy the moment Linux
is promoted. The third — no defined per-turn latency target — is a standing decision, not a gap.

**Deferred rows 22–24** schedule the shape of this lane: row 23 would move
`tools/gate-a-candidate/` and `tools/linux-docker/` to `tools/_deferred/` as a `git mv`; row 24 (D6)
would de-scope `tools/linux-docker/source_digest.py`'s host-Git provenance apparatus to
`rev-parse HEAD` plus `status --porcelain`, **as the first act of picking Gate C back up**. Two of
row 24's own numbers have moved: it sizes the file at 2,026 lines (it is **2,063** today, MEASURED
`wc -l`) and states the cost of leaving it as *"1,664 lines and 12 red tests carried"* — and the
**12 red tests are zero** (§7.2). Re-argue the row on the line count, which still holds, rather than
on the test count, which does not.

**Documentation that is stale against measurement, listed so you do not re-derive it:**

| document | claim | measured at `0d83b7a` |
|---|---|---|
| `docs/current-state.md` §7.6 | Gate C "never built, never run" | a 14-gate ancestor ran green on linux/arm64, 2026-07-19, in gitignored `.context/` (§8.2) |
| `docs/current-state.md` §7.6 | projection test at `tools/linux-docker/tests/test_runner.py:277`, "12 pre-existing failures" | `:296`, and zero other failures |
| `docs/current-state.md` §9.4 row C6 | "the seven drifted cells"; `tools/linux-docker/tests/test_runner.py:651`, `:638-650`, `:808-821` | thirteen names in two directions; `:656`, `:661`, `:831` |
| `docs/current-state.md` §9.3 row 24 | "12 red tests carried"; `source_digest.py` at 2,026 lines | zero red tests; 2,063 lines |
| `docs/gate-c-linux-handoff.md` §3.2 | candidate is 75 cells; "all 75 candidate cells are present in the Linux manifest" | 70 cells, seven absent (§7.3). Its 15 container-only names still hold; its §4 starting sequence still holds |
| `scripts/gate-a-mutants.sh:129`, `:173` | "886 of the 1,588 mutants"; a floor defended against 95.50% | 921 of 1,661; the gate scope measures 97% over 740 |

**And one that is not in a document — it is in this one's own working notes.** The fact sheet this
rewrite was built from decomposed C6's thirteen names as "five renames, two new `gate_b` cells, two
unsatisfiable" — which sums to seven from a set of six plus a set of seven, and is wrong. Re-derived
from the two manifests it is **four rename pairs, three new cells, two unsatisfiable** (§7.2).
Deriving beats inheriting even when the source is one day old and was itself careful.

**And six more that were in this file, found by re-running every claim in it against the tree.**
They are listed because the shapes repeat and you will produce them too, not as an apology:
a citation to `scripts/gate-a-mutants.sh:194` for a subtraction that happens at
`scripts/gate-a-mutants.sh:175` and is refused at `scripts/gate-a-mutants.sh:186` (§3.6); an
"unreachable ordered assertion" pinned to `tools/linux-docker/tests/test_runner.py:843`, which is in
a different and green test, rather than to `tools/linux-docker/tests/test_runner.py:393` in the
failing one (§7.2); **"16 headings"** from a `grep -c` that
counts 15 headings and one line of prose (§6.1); **"`OAUTH_FILE_SUFFIX` occurs exactly three
times"** when the name occurs five times and is *assigned* three (§5.1); **"44 → 48, all four
additions at 2.1.226/2.1.227"** across two different denominators, when the real drift is two
(§4.1); and **"73 test binaries"** for 72 `test result:` lines, 6 of which are doc-tests (§10).
Every one of the six is the same defect: a sentence whose noun is narrower or wider than the
command that produced its number. Four of the six were inherited from a careful source and survived
a careful write. **Re-run the command; do not re-read the sentence.**

---

## 12. If you read only one thing

Every claim in this repository about Claude Code's behaviour is an observation of a rendering taken on
one operating system, at one architecture, against one range of versions, on one host. The apparatus
that made those observations trustworthy — the done-gate, the survivor register, the seam, the
worktree runner, the currency check — is platform-neutral and is yours for free. **The observations
are not.** Re-take them, mark what you have not taken as UNVERIFIED rather than assuming it carried,
and start with §5: what a Linux Claude Code does with a credential is one afternoon of `strings`, and
every other decision on this lane is downstream of the answer.
