# pmux v1 validation specification

**Status:** Historical freeze census plus living test-ownership notes.
Living verification is `tools/dev/`. `OPEN-*` rows below are freeze-census
status from that census; they are not living release blockers. No external
compatibility or portability claim is made by this file.

`spec.md` is normative for product behavior. This document is normative for
test ownership, public-surface coverage, and living commands in `tools/dev/`. The local
`.context/final-pmux-plan.md` coordinates execution and evidence but is not an
executable correctness authority.

**Living verification is `tools/dev/`** (`check.sh`, `operator_eval.py`, `promote.py`). Gate A below is a historical freeze census, not the required development workflow.

## 1. Correctness authority

Pmux correctness is owned by production Rust and tracked tests. The transcript
parser/engine, actor, protocol DTOs, lifecycle boundaries, and process cleanup
must not have a second semantic implementation in Python, TypeScript, shell, or
`.context` tooling.

- Rust unit and integration tests are the primary executable specification.
- TypeScript and Python tests prove their own transport, validation,
  reconnection, and ergonomic contracts against shared public vectors. They do
  not reimplement service or transcript semantics.
- A deterministic fake Claude may emulate only the external PTY/transcript
  behavior needed to exercise shipped boundaries. Pass/fail semantics remain
  in Rust assertions and production pmux.
- Phase 0, linux-docker, and package-smoke have been removed. Historical freeze
  census commands below are not a living pin. Their transcript or process
  interpretations cannot close a product matrix row.
- Every production defect found during validation receives a minimized tracked
  regression before or with the fix.
- Test counts are supporting metadata. This map is the historical freeze
  census. It does not close a living pin. Living verification is `tools/dev/`.

### Evidence threat-model boundary

The candidate/evidence envelope fails closed on stale, partial, or internally
self-consistent substitution; concurrent pathname replacement; a tracked
command that hangs or floods its bounded output; an observed descendant that
escapes the owned process boundary; and mutation-then-restoration of a declared
source, dependency, tool, configuration, or binary input. This is release
evidence under an ordinary non-hostile same-UID host. It does not claim to
withstand a malicious same-UID actor using `ptrace` or killing the supervisor,
or compromise of the kernel, hardware, or trust roots below the recorded tool
and platform identities.

The inherited vnode marker attributes ordinary non-hostile child trees; this
evidence boundary does not claim resistance to a child that intentionally
closes the reserved marker descriptor before escaping every observed parent,
process-group, and session relationship. The exact leader must carry the marker
before release. Marker loss while that leader remains live beyond the fixed
250 ms exit-only grace fails closed. On Darwin, descriptor-table teardown can
precede `waitpid` reaping; observed marker disappearance is accepted only when
that exact leader becomes reapable and is reaped inside the grace, and the
receipt records the event with a strict Boolean. An observed process escape
always fails closed.

### Status vocabulary

| Status | Meaning |
| --- | --- |
| `COVERED` | The named tracked tests establish the row at its authoritative layer and all required boundary repetitions exist. |
| `OPEN-L0` | A local example/boundary invariant is missing or can pass for the wrong reason. |
| `OPEN-L1` | Generative, model, mutation, or fuzz ownership is missing. |
| `OPEN-L2` | Subsystem, real PTY/rmux, filesystem, or lifecycle composition is missing. |
| `OPEN-L3` | A shipped executable/socket/stdio/process boundary is missing. |
| `OPEN-L4` | Rust/TypeScript/Python shared-client conformance is missing. |
| `OPEN-L5` | Deterministic concurrency, fault, resource, soak, or performance evidence is missing. |
| `EXTERNAL` | Gate B or C evidence is required after the deterministic source freeze. |
| `REJECTED` | Intentionally unsupported in v1 and closed only by a stable rejection test. |
| `OUT-OF-SCOPE` | No v1 behavior or support claim exists for this future platform boundary; the row is closed only by explicit scope and claim-language review, not by inventing a request rejection. |
| `HISTORICAL` | Freeze-census row whose gate was deleted. Not a living release blocker. |

Only `COVERED`, tested `REJECTED`, and explicitly reviewed `OUT-OF-SCOPE` rows
closed the historical freeze matrix. Living verification is `tools/dev/`.
`OPEN-*` and `HISTORICAL` rows in this file are census, not a second pin.
`EXTERNAL` rows were Gate B/C after freeze, not a Linux Claude pin.
`OUT-OF-SCOPE` must never hide an exposed or partially implemented public
behavior.

### `AUTHORED`, and what "rerun pending" actually means

`AUTHORED` is a status word for a row whose named tests exist, assert the row's
contract, and have been observed passing at source level, but have **not** been
executed against a frozen release candidate and recorded in a receipt.
`AUTHORED` is a statement about attestation, not about coverage.

The Section 4 status column fuses those two things, so read its location column
carefully. Where a row says "exact release Gate D/E rerun pending," that means
**receipt** pending, not **coverage** pending: the owning tests exist and assert
the contract. Path A `full_stack.rs` process-boundary tests were deleted
when the public session surface was removed. Living e2e is
`crates/e2e/tests/pool_concurrency.rs`. Rows that still say "exact release
Gate D rerun pending" mean a product receipt is pending, not that the old
Path A suite should be re-run.

## 2. Validation layers

| Layer | Authority |
| --- | --- |
| L0 | Rust unit/table tests for DTO, parsing, identity, policy, local transitions, limits, and redaction. |
| L1 | Rust property/model/mutation/fuzz tests over framed protocol, JSONL, transcript graphs, and actor operation sequences. |
| L2 | Rust subsystem tests with real filesystem behavior, deterministic clocks/faults, and real rmux/PTY where OS behavior matters. |
| L3 | Rust black-box tests of the exact shipped binaries, public UDS/stdio, modes, permissions, signals, exit status, and cleanup. |
| L4 | Shared-vector conformance through Rust, TypeScript, and Python clients plus CLI/MCP/facade mappings. |
| L5 | Deterministic concurrency, backpressure, capacity, fault loops, resource ceilings, soak, and size-scaling/performance evidence. |
| L6 | Frozen-candidate real-Claude macOS promotion and same-source Docker Linux portability evidence. |

### Environment preconditions for every gate below

The gates in Sections 3 and beyond assume three things about the host that no
command in them checks: that nothing else writes to the candidate workspace,
that the machine is otherwise idle, and that a live campaign's directory,
permission mode, ledger path, and effort were settled before the first ordinal
was spent. Each has cost a real run. Establish all of them before running
anything, because most of them fail after the evidence envelope has already
committed something irreversible.

### The candidate must be a standalone clone, not a polled workspace

**HISTORICAL freeze census.** Phase 0 and linux-docker have been deleted. Do
not grep or run the paths below; they are not in the tree. Living verification
is `tools/dev/`.

A live campaign re-observed the frozen candidate at six sites — every call to
`_verify_candidate_unchanged` in the since-deleted `tools/phase0/phase0_lib.py`,
and a `grep -c` of that file used to print `6`. Each of those calls `observe_source_identity`, which is not a
file-content comparison. It takes a Git revision capture before the manifest walk
and another after it, and rejects any inequality between the two with `workspace
revision changed across source capture`. The identity embeds `repository_control`
(`tools/linux-docker/source_digest.py:1335`), a set of `lstat` records that each
carry `ctime_ns` (`:906`) for the workspace root, the worktree control entry,
the Git directory and its parent, the common directory and its parent, `HEAD`,
the index, `packed-refs`, `config`, and the `objects` and `refs` directories
(`_repository_control_snapshot`, `:1008`). Any process that runs `git status`,
refreshes the index, or fetches moves one of those `ctime_ns` values, and the
identity changes without a single tracked file changing. A second, narrower
fence sits inside one capture: the digest program independently rejects a
`repository_control` change observed during its own revision query
(`tools/linux-docker/source_digest.py:1309-1312`), which surfaces from phase0 as
`canonical source/revision capture failed: Git repository control identity
changed during capture`.

This extends to the Git control plane the same principle Section 3 states for
included source directories: changing an included source directory's ctime is
never treated as legitimate validation churn (`docs/testing.md:387-388`).

The campaign-length window is no longer part of this. As of 2026-07-28
`_verify_candidate_unchanged` (`tools/phase0/phase0_lib.py`) gates on a
projection of the identity onto the fields the frozen-candidate claim is
actually made of (`SOURCE_IDENTITY_CLAIM_FIELDS` and
`REVISION_IDENTITY_CLAIM_FIELDS`, applied through `source_identity_claim`), and
records everything outside that projection as an observation instead of failing
on it. What that fix removed was a whole-dict comparison between bind time and
verify time. It did not touch either of the two fences above, because both
compare a capture against itself over a window of roughly a second, and both
still compare the whole revision identity.

Measured on 2026-07-28 in a workspace whose manager polls git roughly every 10 s:
the ~1.3 s before/after capture pair failed about 30% of the time, and the ~11 s
window spanning one live attempt failed essentially always. Only the second of
those two numbers was retired by the projection fix. The first is intact, a
campaign runs that capture pair at least six times per attempt, and at ~30% each
that is not a risk to accept but the expected outcome. In a standalone clone
with its own `.git` and nothing watching it, repeated sampling over 50 s showed
zero drift and zero capture errors.

The expense is that these failures land after the ledger reservation. A
reservation consumes its global ordinal whether or not Claude produced a result
(`evidence/README.md`, *Four detached reservations*), and the ceiling is a total
across all campaigns (`MAX_GLOBAL_ATTEMPT_CEILING = 100` in
`tools/phase0/phase0_lib.py`), never reset by a restart or a failure. So each
drift failure spends an irreplaceable attempt and returns nothing.

The remedy is to clone the frozen candidate to a path nothing watches — no IDE
indexer, no backup agent, no workspace manager, no editor with a Git decoration
provider — and then prove two separate things before spending anything. First,
that the path is quiet:

```bash
# HISTORICAL. source_digest.py is gone. Do not run.
# Zero drift required. One unique line, or the path is not quiet enough.
# for _ in $(seq 30); do
#   PYTHONDONTWRITEBYTECODE=1 python3 \
#     "$CANDIDATE_ROOT/tools/linux-docker/source_digest.py" \
#     "$CANDIDATE_ROOT" --revision | shasum -a 256
#   sleep 2
# done | sort -u
```

`--revision` (`tools/linux-docker/source_digest.py:1983`) prints
`workspace_revision_identity` (`:1352`), which is the value the fences compare,
so digesting its output samples the real fence rather than a proxy. Sixty
seconds covers a 10 s poller six times. More than one unique line means the
campaign will fail, and the only question is whether it fails before or after a
reservation.

Second, that the clone still is the frozen candidate. A quiet path cloned at the
wrong ref, or with a dirty tree, passes the drift check and is then rejected by
the campaign:

```bash
# HISTORICAL. source_digest.py is gone. Do not run.
# Fails closed on any mismatch against the frozen workspace source digest.
# PYTHONDONTWRITEBYTECODE=1 python3 \
#   "$CANDIDATE_ROOT/tools/linux-docker/source_digest.py" \
#   "$CANDIDATE_ROOT" --expected "$FROZEN_WORKSPACE_SOURCE_SHA256"
```

`--expected` (`:1980`) compares `workspace_source_sha256` and raises on
difference (`:2006-2012`). It is mutually exclusive with the revision modes
(`:1990-1994`), so these are two invocations, not one.

If the polling cannot be moved away from — the workspace manager is not yours to
configure, or the poller cannot be disabled for the duration of the run — then
the campaign is not runnable on that path. There is no third option: a live
attempt started there will consume ordinals to produce capture errors. Disable
the poller for the window, or clone elsewhere. This subsection is freeze
history; living confirmation does not run source_digest.

This precondition is falsified by a full campaign window completing inside a
polled workspace with zero `workspace revision changed across source capture`
and zero `Git repository control identity changed during capture` failures. That
would show the poller stopped touching control-file ctimes, not that the fences
are wrong.

### A red process-boundary timing failure is a claim about the machine first

On 2026-07-28 the host carried 16 orphaned `bun` processes from an unrelated
project, reparented to init after their parent died, hot-spinning in a
JavaScriptCore allocate/GC/decommit loop for two days: 415% aggregate CPU
(4.1 cores), RSS sawtoothing by roughly 500 MB on a ~6 s period, and 9.8 GB of
swap in use. Three different tests are commonly confused when this happens, and
they are not the same claim:

- `bin/pmux-launcher/tests/process_blackbox.rs:414`
  (`socket_and_token_validation_fail_before_broker_use_and_are_bounded`) is where
  the contention was measured, and until 2026-08-08 most of what it measured was
  the harness. `assert!(started.elapsed() < Duration::from_secs(2));` spanned
  `launcher_binary()` and `assert_candidate_unchanged()`, which sha256 the whole
  candidate: **350 ms** per iteration on this host against the 2 s bound — 5.7x
  headroom, not the 600x this section used to claim — of which the launcher's own
  refusal was **4 ms**. Under 60 bounded spinners (load average ~60) it failed
  3/3; with the hashing hoisted out of the timed region
  (`timed_refusal`, `:406`) the same test passes 3/3 under the same load and the
  region reads 4 ms. The ordering rule below still stands, but a quiet host was
  never the whole story here. This test is not carried as debt anywhere, and it
  is neither C8 nor C9.
- C9 (debt row `C9` in `docs/current-state.md`) is
  `bin/pmux-hook/tests/process_blackbox.rs::stalled_relay_is_bounded_and_does_not_echo_private_input`,
  whose failing assertion was a wall-clock upper bound, and host contention is
  its confirmed mechanism. It took the first of the three admissible
  dispositions: the upper bound is replaced by a lower bound plus a recorded
  observation, so the test now gates on what the product does and records what
  the host did (`bin/pmux-hook/tests/process_blackbox.rs:259-278`). Load can
  delay an exit but cannot hurry one, which is why the lower direction is sound
  and the upper one never was.
- C8 (debt row `C8` in `docs/current-state.md`) is
  `bin/pmux-rmuxd/tests/process_blackbox.rs:431`
  (`owner_eof_reaps_a_hup_term_ignoring_pane_tree_and_surfaces_lease_loss`), and
  it is not a timing bound at all. Host load explains nothing about it, and no
  wall-clock measurement bears on it.

C8's recorded failure was `private process boundary observation failed / could
not run /bin/ps: No child processes (os error 10)`
(`crates/rmux/src/process_boundary.rs:374`, surfaced with its context at
`bin/pmux-rmuxd/src/main.rs:268`). That `ECHILD` is the signature of an inherited
`SIGCHLD` disposition which auto-reaps children before the observation can ask
about them. C8 took the third admissible disposition on 2026-07-28, and this is
the register the nonclaim belongs in, alongside the boundary in Section 1: pmux
is claimed to be observable by this test on a host whose test process has an
ordinary `SIGCHLD` disposition, with no `SIG_IGN` and no `SA_NOCLDWAIT`
inherited from an ancestor, and nothing whatever is claimed about a test process
that inherits an auto-reaping one. A red cell there is a documented nonclaim
rather than a product signal. It is not a claim that the product's boundary
proof is weaker in that environment: `OwnedProcessBoundary` never claims to
observe a process the kernel has already reaped. The reasoning and the exact
scope are in `docs/current-state.md` under *The disposition taken for C8*.

The headroom is the point, and the bounds must not be relaxed to compensate. The
launcher's 2 s bound exists to prove that argument validation returns without
waiting out the shipped ten-second broker deadline, which is what the next test
pins (`bin/pmux-launcher/tests/process_blackbox.rs:456`,
`stalled_broker_read_uses_the_shipped_ten_second_deadline_and_redacts_token`,
asserting `assert!(elapsed >= Duration::from_secs(9));` at `:471`). Widening 2 s
toward 10 s does not make the test more robust; it makes it assert nothing. The
rule in Section 3 stands unchanged — a flaky command fails the gate
(`docs/testing.md:379-381`) — and the disposition for a load-sensitive assertion
is a quiet host plus a bound that gates only the claim, never a wider bound.

Check the host before starting, and check it again before believing a red timing
test:

```bash
uptime                     # 1-minute load average; compare against core count
sysctl -n vm.swapusage     # sustained non-zero "used" is a red flag
ps -Ao pid,ppid,pcpu,rss,comm | awk '$2 == 1 && $3 > 10'   # reparented CPU burners
```

The ppid-1 filter has legitimate hits such as `WindowServer` and terminal
emulators. The signature to look for is a worker — a language runtime, bundler,
test runner, or package manager — whose parent is gone and which is still
burning CPU. Those never exit on their own.

The ordering rule is therefore that a failing process-boundary timing assertion
is a claim about the machine until the machine is ruled out. Read load, swap,
and the ppid-1 list; fix the host; rerun. Only a failure that survives a quiet
host is evidence about the code. Under D9 this matters because an argument about
whether the timing should be fine is not an observation. The load average is.

### The pool mints a trusted empty cwd

`pmux run` names no working directory. The daemon mints a private pre-trusted
empty cwd per pool instance. A folder-trust screen (`NeedsInputKind::Trust`)
on a minted cell is a remint, not an operator `pmux attach` or a campaign
`--cwd` ritual. Historical session campaigns that died on untrusted
directories are in `evidence/README.md` (*Four detached reservations*).

### The permission mode must not stall a grade that runs a tool

A permission modal raised after submission is detected, and then deliberately
not treated as a failure. `completion_evidence`
(`crates/service/src/driver_io.rs:835`) classifies the snapshot at `:780` and
the stabilized snapshot at `:807`, and both `NeedsInput` branches return a
default `TerminalEvidence` (`:785-790`, `:809-814`) rather than an error — that
is negative liveness evidence, `ready_prompt: false`, `quiet: false`. The turn
therefore never satisfies the drain predicate
(`crates/service/src/v1/actor.rs:2597-2602`), the actor keeps polling, and the
attempt runs out its whole `--turn-timeout-seconds` before failing, having
burned its ordinal. The default is 300 s (`tools/phase0/phase0.py:217`) and the
ceiling is `MAX_TURN_TIMEOUT_SECONDS = 600` (`tools/phase0/phase0_lib.py`);
the 2026-07-28 campaign ran at that ceiling, so the observed loss was ten
minutes, not five, and the ledger records it as ordinal 36. The cost is whatever
value the (now-deleted) Phase 0 driver passed.

A permission prompt is terminal for an unattended run in the only sense that
matters: nobody will ever answer it. Living one-shot does not forward
`--permission-mode`; the pool mint owns the minified cell (tools denied).
Historical campaigns that set the flag are archive-only.
`--scenario claude-p-one-shot` remains removed
(historical `phase0_lib.py::validate_config`). The
observable signature of getting this wrong is a turn that consumes its full
deadline and returns nothing while the transcript shows a tool call and no
result.

The remedy is not a campaign launch flag. `docs/spec.md` §2 refuses a public
session; on `NeedsInputKind::Trust` the pool remints a pre-trusted empty cwd
rather than answering the modal. An automatic answer to a trust prompt would
be a security change, not a robustness fix. Phase 0 is deleted; those launch
flags are not a living surface.
`dangerously-skip-permissions` is not a living campaign option.

### The driver must reserve against the tracked ledger

**HISTORICAL.** Phase 0 is deleted. The ledger is frozen.

`tools/phase0` located the ledger only by explicit `--ledger` path
(`tools/phase0/phase0.py:115`) and enforces the ceiling at reservation time
against the prefix the driver supplies (`--ledger-prefix-records` and
`--ledger-prefix-sha256`, `:98-99`). A stale prefix is internally consistent, so
a driver that copies `evidence/model-attempt-ledger.ndjson` to a private path,
reserves there, and discards the copy resets the global budget silently and
passes every check. On 2026-07-28 four campaigns each re-reserved the same
ordinal into a discarded copy while the retry loop compared record counts
against the same reset base and concluded nothing had been spent
(`evidence/README.md`, *Four detached reservations*, which also records that all
four are counted anyway). Before a live run, confirm `--ledger` resolves to the
tracked file; after it, confirm that file grew. When counting the budget by hand,
count both spellings of the ordinal field (`evidence/README.md`, ledger section):
records from ordinal 30 onward spell it `global_attempt_ordinal`, not
`global_attempt`, or the budget reads fourteen attempts cheaper than it is.

### Effort is bounded by authorization, not by taste

**HISTORICAL.** Phase 0 is deleted. Living `pmux run` effort is the product
table, not this campaign envelope.

`APPROVED_EFFORTS = ("low", "medium")` (since-deleted `tools/phase0/phase0_lib.py`)
was enforced in `validate_config` and mirrored in the CLI's `--effort` choices.
A campaign at `high` was outside the approved envelope and cannot produce
promotable evidence, whatever it observes.

## 3. Deterministic Gate A command manifest (historical freeze census, not executable)

Gate A was one ordered freeze census, not a living executable. A command that was unavailable, skipped without
an applicable documented platform exclusion, flaky, or dependent on an
untracked oracle failed the freeze. Do not run this; living commands are `tools/dev/`.

The candidate envelope supplies one pre-created, canonical, owner-private
`PMUX_GATE_A_VALIDATION_ROOT` outside the canonical workspace. Its
`typescript-dist`, `fuzz`, and `fuzz-evidence` children exist before the
persistent source witness is captured. All generated TypeScript and fuzz output
below stays in those children: changing an included source directory's ctime is
never treated as legitimate validation churn, and replacing any validation
child changes the separately retained validation-root identity and fails
closed.

The envelope also supplies `PMUX_GATE_A_RELEASE_DIR`, the exact fresh release
directory frozen by the prelude, and runs every command below with `umask 077`
and `CARGO_TARGET_DIR=$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/workspace` in
its sanitized base environment. A command may select only one named child of
that same external `cargo-target` directory. No validation command writes to
the canonical workspace `target/` tree or rebuilds a binary in
`PMUX_GATE_A_RELEASE_DIR`. Before Section A, the envelope performs one exact
fresh release build, captures all seven required executable receipts (`pmux`, `pmuxd`, `pmux-mcp`, `pmux-rmuxd`, `pmux-launcher`, `pmux-hook`, plus `pmux-test-claude`) and
identities, and freezes that directory; Sections A through F and residue then
revalidate the same source, tool context, validation root, TypeScript stage,
and release binaries around every command.

### A. Static, build, documentation, and ordinary tests

```bash
# freeze census; living is tools/dev/
cargo +1.88 fmt --all -- --check
cargo +1.88 check --locked --workspace --all-targets --all-features
cargo +1.88 clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo +1.88 doc --locked --workspace --all-features --no-deps
cargo +1.88 test --locked --workspace --all-targets --all-features

# `vendor/rmux-client` is intentionally excluded from the workspace, so it
# receives its own locked, offline static and test lane. Its target directory
# remains under the external validation target tree.
cargo +1.88 fmt --manifest-path vendor/rmux-client/Cargo.toml --all -- --check
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-client" \
  cargo +1.88 check --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-client" \
  cargo +1.88 clippy --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features -- -D warnings
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-client" RUSTDOCFLAGS='-D warnings' \
  cargo +1.88 doc --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-features --no-deps
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-client" \
  cargo +1.88 test --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features \
  -- --test-threads=1

cargo +1.88 test --locked -p pseudomux-rmux --test vendor_patch -- --test-threads=1
cargo +1.88 test --locked -p pseudomux-rmux --test attach_fragmentation -- --test-threads=1

# `vendor/rmux-server` is also excluded and pmux uses it with default features
# disabled. Compile every target and filter the library tests to the whole
# `pane_io::tests` module, which is how this lane comes to
# run all fourteen patch-owned EOF regressions without writing one name down,
# in that exact product feature set; use all features for strict Clippy/rustdoc.
# A MODULE and not a name list because `--exact` against a name nobody wrote
# runs zero tests and exits zero: fourteen such cells per lane meant a
# fifteenth regression compiled in every lane and executed in none.
# `crates/rmux/tests/vendor_server_patch.rs` derives the set from the patched
# source, and refuses every file outside that source and
# `vendor/rmux-server/PMUX-PATCH.md` the right to name one.
# Clippy denies every warning except two named style lints already present
# across immutable upstream files, and provenance prevents those allowances
# from masking a broader source change. The published package is not a closed
# integration-test artifact: Windows targets and source-ledger tests refer to
# repository-only files that crates.io omits. Direct rustfmt therefore covers
# the production module tree and internal regressions, all targets are still
# compiled, and actual pmux sidecar/process tests own the shipped boundary.
rustfmt +1.88 --edition 2021 --check \
  vendor/rmux-server/src/lib.rs vendor/rmux-server/build.rs
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-server" \
  cargo +1.88 check --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --all-targets --no-default-features
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-server" \
  cargo +1.88 clippy --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --all-targets --all-features \
  -- -D warnings \
  -A clippy::collapsible-else-if -A clippy::uninlined-format-args
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-server" RUSTDOCFLAGS='-D warnings' \
  cargo +1.88 doc --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --all-features --no-deps
CARGO_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/vendor-rmux-server" \
  cargo +1.88 test --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --lib --no-default-features \
  pane_io::tests:: \
  -- --test-threads=1
cargo +1.88 test --locked -p pseudomux-rmux \
  --test vendor_server_patch -- --test-threads=1

node clients/typescript/node_modules/typescript/bin/tsc \
  -p clients/typescript/tsconfig.json --noEmit
node clients/typescript/tests/dist-stage.mjs prepare \
  "$PMUX_GATE_A_VALIDATION_ROOT/typescript-dist" --outside-root "$PWD"
node clients/typescript/node_modules/typescript/bin/tsc \
  -p clients/typescript/tsconfig.json \
  --outDir "$PMUX_GATE_A_VALIDATION_ROOT/typescript-dist"
node clients/typescript/tests/dist-stage.mjs verify \
  "$PMUX_GATE_A_VALIDATION_ROOT/typescript-dist" --outside-root "$PWD"
PMUX_TYPESCRIPT_DIST_DIR="$PMUX_GATE_A_VALIDATION_ROOT/typescript-dist" \
  node --test \
    clients/typescript/tests/client.test.mjs \
    clients/typescript/tests/dist-stage.test.mjs \
    clients/typescript/tests/golden-conformance.test.mjs
(cd clients/python && PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -v)
python3 -m ruff check --no-cache clients/python tools/evidence_common tools/dev
python3 -m ruff format --check --no-cache clients/python tools/evidence_common tools/dev
```

The macOS envelope seals the exact TypeScript verifier digest and private tree
at `typescript_stage_verify`, then revalidates it around every later command.
The Linux runner records the equivalent platform-only
`typescript_stage_identity_capture` gate, requires
`typescript_stage_preconsume_unchanged` immediately before Gate D can consume
the stage, and requires `typescript_stage_postconsume_unchanged` after all
consumers and before scoped validation cleanup. A mismatch skips the dependent
shipped-boundary commands and fails the ordered manifest.

The complete 2,751-test upstream server library sweep remains useful
diagnostically, but it is not a release gate. During validation,
`handler::attach_tests::attached_prefix_lifecycle::attached_exit_notifies_after_command_prompt_rename_session`
hit its hard-coded five-second notification timeout once under concurrent
diagnostic load, then passed both in isolation and in a subsequent 2,751/2,751
serialized sweep. That direct-handler test does not call the patched
`forward_attach` EOF branch. Because a rerun cannot turn a flake into Gate A
evidence, deterministic patch regressions, provenance, compilation, and the
actual `pmux-rmuxd` process boundary own this dependency repair.

Package-smoke was the freeze census packaging gate. It has been deleted. Do
not restore it. Living client checks are TypeScript `npm test` and Python
client unittests in `tools/dev/check.sh`. The historical commands were:

```bash
# freeze census; living is tools/dev/
# DELETED. package-smoke is gone. Do not run.
# PYTHONDONTWRITEBYTECODE=1 python3 tools/package-smoke/package_smoke.py typescript
# PYTHONDONTWRITEBYTECODE=1 python3 tools/package-smoke/package_smoke.py python
```

### B. L1 property, model, mutation, and fuzz gates

The following exact invocations were the freeze census. The tracked targets exist.
Do not treat this as a living Gate A remaining-open list; living commands are `tools/dev/`.

```bash
# freeze census; living is tools/dev/
PROPTEST_CASES=4096 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pseudomux-claude --test transcript_properties
PROPTEST_CASES=2048 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pseudomux-service --test actor_model
PROPTEST_CASES=2048 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pseudomux-client --lib protocol_properties
PROPTEST_CASES=2048 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pmuxd --bin pmuxd \
  handler::tests::arbitrary_admitted_payloads_have_bounded_decode_recovery_and_responses \
  -- --exact --test-threads=1

# The two pinned tools, installed BESIDE the workspace and never onto PATH.
# `--root .context/tools/<binary>` is what puts them at the one path every
# reader derives (`.context/tools/<binary>/bin/<binary>`); `--version` is what
# `gate_b/cargo_fuzz_version` and `gate_b/cargo_mutants_version` assert, and both
# scripts refuse anything else. Neither script installs: on a host without them
# `gate_b` refuses by name rather than reaching for whatever `cargo install`
# would resolve today. Run these once per host.
# freeze census; living is tools/dev/
cargo install --root .context/tools/cargo-fuzz    --version 0.13.2 --locked cargo-fuzz
cargo install --root .context/tools/cargo-mutants --version 27.1.0 --locked cargo-mutants

test "$(.context/tools/cargo-mutants/bin/cargo-mutants mutants --version)" = "cargo-mutants 27.1.0"
PMUX_MUTANTS_CARGO="$(rustup which --toolchain 1.88.0 cargo)" \
PMUX_CARGO_MUTANTS_BIN="$PWD/.context/tools/cargo-mutants/bin/cargo-mutants" \
PMUX_MUTANTS_SCOPE=gate \
PMUX_MUTANTS_MINIMUM_SCORE=94 \
PMUX_MUTANTS_JOBS=4 \
PMUX_MUTANTS_WORK_DIR="$PMUX_GATE_A_VALIDATION_ROOT/mutants" \
PMUX_MUTANTS_EVIDENCE_ROOT="$PMUX_GATE_A_VALIDATION_ROOT/mutants-evidence" \
  bash scripts/gate-a-mutants.sh

test "$(.context/tools/cargo-fuzz/bin/cargo-fuzz --version)" = "cargo-fuzz 0.13.2"
nightly_cargo="$(rustup which --toolchain nightly-2026-03-26 cargo)"
nightly_rustc="$(rustup which --toolchain nightly-2026-03-26 rustc)"
nightly_bin="$(dirname "$nightly_cargo")"
PMUX_FUZZ_RUNS=50000 \
PMUX_CARGO_FUZZ_BIN="$PWD/.context/tools/cargo-fuzz/bin/cargo-fuzz" \
PMUX_FUZZ_TARGET_DIR="$PMUX_GATE_A_VALIDATION_ROOT/fuzz" \
PMUX_FUZZ_EVIDENCE_ROOT="$PMUX_GATE_A_VALIDATION_ROOT/fuzz-evidence" \
PMUX_NIGHTLY_BIN_DIR="$nightly_bin" \
PMUX_NIGHTLY_CARGO="$nightly_cargo" \
PMUX_NIGHTLY_RUSTC="$nightly_rustc" \
  bash scripts/gate-a-fuzz.sh
```

#### What the mutation score covers, and what it does not

`gate_b/mutation_score_agent_launch_pool_protocol` is named for the FILES it
mutates, and the name is the whole point: the number it prints is a statement
about `crates/service/src/{agent.rs,claude_launch.rs,pool/**}` and
`crates/protocol/src/**` and about nothing else. The script prints the globs
beside the score on every run, and prints whatever the scope leaves out as a
DERIVED set difference against the full list — so a `full` run correctly prints
no exclusions, and the number cannot be quoted without its scope.

**It does not measure admission.** The cell was called
`mutation_score_service_admission_and_protocol` and its scope value `admission`
until the name was read back against the globs: `crates/service/src/native.rs`
is not in them, and `native.rs` is where `admit_bound_resources`,
`admit_config_root`, `admit_cwd`, `claim_reaches` and `effective_config_root`
are all declared. A number labelled "admission" that mutates no admission guard
is exactly the defect the tool was installed to enumerate — so the label went.

`native.rs` and `driver_io.rs` are OUT of the cell — together they are **886 of
the 1,588 mutants** the full first-party scope enumerates, so `full` is 2.26x
the mutants of `gate` and, since each mutant is one build-and-test cycle,
ESTIMATED at about that multiple of the wall time. (An estimate, and labelled
one: no complete `full` run has been timed. The largest that exists was stopped
at 623 of 1,588.) They are measured out of band by `PMUX_MUTANTS_SCOPE=full`,
which no cell runs.

**MEASURED 2026-08-11, and the estimate above was low in wall time and high in
its multiple.** Two complete `full` runs exist: 1,654 mutants at `1882dee` in
11,854 s and 1,653 at `0b1cff6` in **10,443 s** — 2.9 hours, against `gate`'s
1.45, so 1.98x the wall time for 2.35x the mutants. The enumeration is 1,653 and
not 1,588; that figure and the 886 beside it are from an older head and are not
re-derived here. `full` still runs no cell, and the reason is now wall time
alone rather than wall time and an unknown number: it fits inside
`phase_timeouts_seconds.gate_b` = 14400 s with 1.4x headroom, which is thin
enough that a busy host would blow it.

What holds the admission guards instead is the differential entry-path test,
`native::tests::every_entry_path_that_reaches_admission_answers_the_alias_family_identically`:
it drives every DERIVED entry path through one admission decision, asserts the
answers are identical across all of them, and is proven to discriminate by
removing a guard from one path at a time and watching it redden naming that
path.

Four properties of the number, stated so it is never read as more than it is:

* **It is a LOWER bound.** Only the test targets of `pseudomux-protocol`,
  `pseudomux-client` and `pseudomux-service` are run against each mutant. A
  mutant they miss may still be caught by `bin/pmuxd`'s or `bin/pmux`'s blackbox
  suites, by `crates/e2e`, by the libFuzzer targets in `fuzz/`, or by the Python
  and TypeScript conformance lanes — none of which is consulted.
* **`unviable` is excluded from both sides.** A mutant the compiler rejects was
  never a test of the tests. `timeout` counts as CAUGHT: a mutant that makes the
  suite hang has been detected, just expensively.
* **THE SCORE DRIFTS UPWARD, AND THREE NAMED TESTS ARE WHY.** Any test that
  fails for its own reasons — not only one that times out — is recorded as the
  mutant being caught, so the error is one-directional: it can only make the
  gate PASS. Two runs of THE SAME TREE, one with a Python suite beside it and
  one quiet, disagreed on three mutants, all that way. Three MORE flipped
  between runs that were all quiet, and opening the log of the run that said
  CAUGHT names the cause each time:
  `bounded_soak.rs::repeated_real_rmux_cycles_remain_resource_bounded_and_leave_no_residue`
  (`rmux.sock` retained at cycle 13),
  `driver_io.rs::tests::a_preamble_that_lands_after_the_anchor_still_rebinds`
  (`preamble_not_settled` after 802 ms) and
  `private_runtime.rs::a_terminal_resize_after_creation_is_delivered_and_not_silently_clamped`
  (`private rmux sidecar exited unsuccessfully`). All three spawn real processes
  or hold a wall-clock budget, and the mutation loop runs four full suites in
  parallel — so an idle machine is necessary and not sufficient, and the
  measured score is an over-estimate by up to a few mutants. `current-state.md`
  §9.23 has the table. Take the measurement on an idle machine, keep the floor
  below it, and re-run any individual closure claim rather than reading it.
* **`vendor/` is excluded and the exclusion is asserted**, not inherited. It is
  **643 of the 762 tracked `.rs` files (84.4%) and 311,685 of the 440,778
  tracked Rust lines (70.7%)**, and it is not ours. (Both figures from
  `git ls-files '*.rs'`; this bullet said "75% of the Rust" until the two were
  computed, and 75% is neither of them.) The script refuses if the enumerated
  mutant list ever reaches `vendor/`, because "the workspace excludes it" is a
  fact about a `members` line that a future edit can change silently.

**THE FLOOR IS PER SCOPE, and neither tier is aspirational.** `gate` is **94%**,
defended from what has actually been measured and never from aspiration. It was
85% while `pool/**` had never been measured to completion; the whole scope has
since been measured to completion twice, each time in a single run.

`full` is **93%**, and the script does not hold that number: it reads it from
`evidence/mutation-survivor-register.json`'s `recorded_at.floor_percent`, beside
the disposition of every survivor that explains it, so the tree states the floor
once instead of twice. `PMUX_MUTANTS_MINIMUM_SCORE` may raise either floor and is
refused below it — a caller re-pointing this gate at a number the tree has
already beaten is how a green cell comes to mean nothing.

93 and not the 94 that was measured, for the reason the drift bullet above
gives, now measured at this scope rather than inherited: `floor_percent` is the
same run with every mutant whose only failing test was a MEASURED drifter counted
as missed, which is 17 of 1,086 and lands on 93. From the other direction, five
mutants the `1882dee` run counted as caught are missed at `0b1cff6` with no edit
between them touching any, and that run's own logs name a real-PTY or real-rmux
test as the sole catcher of all five — against exactly five mutants of headroom
at a floor of 94. **One of those five is the fourth drifting test**,
`a_terminal_is_created_at_the_requested_geometry_and_not_the_rmux_default`; the
bullet above names three and the list is four.

**What actually ratchets `full` is not the floor.** Every survivor of that run
carries a written disposition in the register, and
`scripts/mutation_register.py check` runs inside this script on every run at
either scope and refuses one that produced a survivor the register does not
hold, or where a mutant the register calls KILLED or REMOVED survived again. Any
commit that lowers the score has produced a mutant that is missed and was not, so
the register refuses it first and BY NAME — which is why the score floor can
afford the point of drift margin. `evidence/README.md` documents the file, the
key it uses instead of `file:line:column`, and why closing a survivor must not
break the gate.

A run is COMPLETE when `end_time` in `outcomes.json` is non-null AND
`caught + timeout + missed + unviable` equals `total_mutants`, which equals the
`enumerated_mutants` line the script writes into its own metadata. Both checks
are needed: `outcomes.json` counts the mutants that got an OUTCOME, so a run
stopped at 623 of 1,588 writes `total_mutants: 623` and sums perfectly against
itself.

**MEASURED 2026-08-07 at the cell's own settings** (`PMUX_MUTANTS_SCOPE=gate`,
`PMUX_MUTANTS_JOBS=4`, pinned 1.88.0), on an idle machine, both runs complete:

* **BEFORE, at `0d7f2ca` plus the phase-0 and package-smoke work — the gate
  cell's own run, 5,285 s:** 702 enumerated, 102 unviable, 600 decided, 561
  caught, **39 missed — 93.50%**. `pool/**` alone: 233 decided, **19 missed —
  91.85%**, which retires the 84.5% this section used to quote.
* **AFTER, at this commit — same script, same settings, 5,099 s:** 702
  enumerated, 102 unviable, 600 decided, 573 caught, **27 missed —
  95.50%**, exit 0 against the 94% floor. `pool/**` alone: 233 decided,
  **4 missed — 98.28%**, and all four are equivalent mutants with the
  premise each rests on written out as a test.

Per file, AFTER:

| file | decided | caught | missed | score |
| --- | --- | --- | --- | --- |
| `crates/protocol/src/v1.rs` | 190 | 172 | 18 | 90% |
| `crates/protocol/src/v1/launch_environment.rs` | 14 | 14 | 0 | 100% |
| `crates/service/src/agent.rs` | 80 | 77 | 3 | 96% |
| `crates/service/src/claude_launch.rs` | 83 | 81 | 2 | 97% |
| `crates/service/src/pool/class.rs` | 21 | 21 | 0 | 100% |
| `crates/service/src/pool/config.rs` | 49 | 49 | 0 | 100% |
| `crates/service/src/pool/host.rs` | 1 | 1 | 0 | 100% |
| `crates/service/src/pool/instance.rs` | 13 | 13 | 0 | 100% |
| `crates/service/src/pool/machine.rs` | 20 | 20 | 0 | 100% |
| `crates/service/src/pool/mod.rs` | 107 | 103 | 4 | 96% |
| `crates/service/src/pool/refusal.rs` | 22 | 22 | 0 | 100% |

**Every one of the 27 survivors is named with a reason** in `current-state.md`
§9.23 — 16 serde length hints, 4 equivalent pool mutants, 2 `#[cfg(not(unix))]`
twins, and 5 individually argued. There is no unclassified gap in this scope, so
the floor is not holding a place for known work.

That census is from 2026-08-07 and this scope has not been re-measured since.
It is stale in the safe direction: the thirteen `StartSessionRequest::serialize`
length hints the `full` runs report are caught as of `0b1cff6`, because the count
is now compared against the fields it counts rather than trusted. Anything said
about the `gate` number below is a 2026-08-07 measurement; the `full` scope's is
`evidence/mutation-survivor-register.json`.

Why not tighter: the drift above is worth up to about three mutants (0.5%)
and it runs one way, upward, so a floor within 0.5% of the measurement can
redden on a clean tree for reasons that have nothing to do with the tree.
94% sits 1.5 points under 95.50% — nine survivors of room, six of them
beyond the measured drift. A floor AT the measurement reddens on the first
ordinary commit and is then raised or ignored. Why not looser: with 600 decided
mutants each survivor is 0.167%, so 85% would admit 90 survivors — more than
three times today's — before the cell said anything, and 93% would admit the
tree exactly as it stood before this change. **The survivor list, not the
score, is the artifact a reader acts on** — `missed.txt` is copied into the evidence directory on every
run, and every entry in it is named with its reason in `docs/archive/current-state-2026-08.md` §9.23.

**Runtime, MEASURED end to end rather than extrapolated.** The scope is 702
mutants and each one is a rebuild plus a run of three packages' test targets; at
`--jobs 4` on a 10-core M1 Pro the two complete runs took **5,285 s and 5,099 s**
— about 1.45 hours, against an earlier extrapolation from a partial run of 2.3
hours. `run_gate.py` applies `phase_timeouts_seconds.gate_b` = 14400 s **per
cell**, so it fits with **2.7x** headroom, and the first of those two runs is the
gate cell itself doing exactly that. It is still by a wide margin the most
expensive cell in the gate, and that is why `native.rs` and `driver_io.rs` are
out of it: adding them back is 1,588 mutants instead of 702.

**A run that will not fit is chunked or detached, never truncated.** This host
kills background jobs at 3,599 s, which is under the cell's own wall time, and
two earlier attempts died at exactly that. The AFTER run above was detached with
`nohup` and polled. Do not compose a figure out of the pieces of a stopped run:
`outcomes.json` from a partial run sums perfectly against itself, so the
composition looks exact and is not.

`[profile.mutants]` in the root `Cargo.toml` is `dev` with `debug = false` and
nothing else, and `debug-assertions` and `overflow-checks` are on under it: a
score measured with assertions off would count every `debug_assert!` in the tree
as a test that does not exist. Those are two claims and the script makes them
separately. `assert_profile_is_dev_without_debuginfo` refuses any key beyond the
two in the table, and that is ALL it does — `Cargo.toml` declares no
`[profile.dev]`, so a `[profile.dev] debug-assertions = false` added tomorrow
would pass it. `assert_profile_properties_are_live` is the one that measures the
properties, by building `crates/protocol/tests/mutation_profile.rs` under
`--profile mutants` and firing a `debug_assert!` and an integer overflow at it;
`PROFILE_PROPERTIES` is the set it asserts, the refusal text is interpolated
from that array so it cannot name a property nobody probes, and
`test_run_gate.py::test_the_mutation_gate_probes_every_profile_property_it_names`
holds the array, the probe's constants and the probe's assertions to one set.

The two candidate binaries `crates/service`'s process-level tests exec —
`pmux-rmuxd` and `pmux-launcher` — are built once outside the mutation loop and
handed to every mutant through `PMUX_TEST_BIN_DIR`. That is sound rather than a
shortcut: neither package depends on a mutated crate, so no mutant can change
either binary, and the script proves that from `cargo tree` before it starts.
Without them the unmutated baseline fails in `bounded_soak` and no mutant is
tested at all.

The Rust-client property target fixes ChaCha and
`RngSeed::Fixed(0x504d_5558_434c_4e54)` in tracked source. The environment
selects the case and shrink counts only; it does not supply an ambient seed.

The candidate envelope resolves and hashes those direct nightly executables and
their common bin directory before this command; the fuzz driver rejects any
other directory relationship and puts only that exact nightly plugin directory
ahead of the isolated cargo-fuzz directory. Fuzz crashes/hangs are minimized
into the tracked seed corpus and an ordinary regression. Gate evidence records
exact toolchain, cargo-fuzz version, corpus hash, seed, runs, and elapsed time.
Fuzz tooling is installed under `.context` rather than mutating global Cargo
state.

### C. Serialized real-rmux/PTY and lifecycle faults

These tests are credential-free and never invoke real Claude:

```bash
# freeze census; living is tools/dev/
cargo +1.88 build --locked -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook
cargo +1.88 test --locked -p pseudomux-service --test native_service -- --ignored --test-threads=1
cargo +1.88 test --locked -p pseudomux-service --test private_runtime -- --ignored --test-threads=1
cargo +1.88 test --locked -p pseudomux-service --test lifecycle_faults -- --test-threads=1
```

The source-level lifecycle targets are present. Rows that additionally depend
on the shipped companions remain open until Gate D reruns the applicable test
against the explicitly hashed release binary directory.

### D. Shipped-binary and cross-client gates

```bash
# freeze census; living is tools/dev/
# The candidate prelude already built and froze the exact seven binaries (pmux, pmuxd, pmux-mcp, pmux-rmuxd, pmux-launcher, pmux-hook, plus pmux-test-claude).
# Gate D compiles only test harnesses in the external validation target and
# executes shipped boundaries from that unchanged release directory.
PMUX_E2E_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
PMUX_E2E_TYPESCRIPT_DIST_DIR="$PMUX_GATE_A_VALIDATION_ROOT/typescript-dist" \
  cargo +1.88 test --locked -p pseudomux-e2e --all-targets -- --include-ignored --test-threads=1
# Living e2e is pool_concurrency. Path A full_stack / live cross-cell /
# claude-p facade suites were deleted with the session product.
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pmux --all-targets -- --test-threads=1
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pmux-mcp --test stdio_blackbox
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pmux-launcher --test process_blackbox
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pmux-hook --test process_blackbox
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pmux-rmuxd --test process_blackbox
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pmuxd --test process_blackbox
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pseudomux-service --test native_service -- --test-threads=1
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pseudomux-service --test private_runtime -- --ignored --test-threads=1
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pseudomux-service --test lifecycle_faults -- --test-threads=1
```

Missing named packages/targets are current `OPEN-L3` rows, not optional
commands. The E2E harness must fail if a required binary is absent, outside the
exact directory, changes during the run, or differs from its recorded digest.

### E. L5 gates

```bash
# freeze census; living is tools/dev/
cargo +1.88 test --locked -p pseudomux-service --test concurrency_backpressure
cargo +1.88 test --locked -p pseudomux-service --test resource_bounds -- --test-threads=1
cargo +1.88 test --locked -p pseudomux-service --test bounded_soak -- --test-threads=1
cargo +1.88 test --locked -p pseudomux-service --lib replay_scaling_tests -- --test-threads=1
cargo +1.88 test --locked -p pmuxd --bin pmuxd native_framing_and_successful_decode_have_deterministic_linear_work -- --test-threads=1
cargo +1.88 test --locked -p pseudomux-claude --test size_scaling --release -- --nocapture --test-threads=1
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pseudomux-service --test concurrency_backpressure \
  -- --include-ignored --test-threads=1
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pseudomux-service --test resource_bounds \
  -- --include-ignored --test-threads=1
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked -p pseudomux-service --test bounded_soak -- --test-threads=1
PMUX_TEST_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" \
  cargo +1.88 test --locked --release -p pseudomux-service \
  --test performance_diagnostics -- --nocapture --test-threads=1
```

Host-sensitive latency/throughput measurements are recorded as diagnostics,
not brittle pass/fail thresholds. Algorithmic size scaling, protocol/resource
ceilings, absence of leaks, and bounded completion are release invariants.

### F. Tooling and evidence-envelope self-tests

```bash
# freeze census; living is tools/dev/
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/evidence_common/tests -p 'test_portable_paths.py' -v
# DELETED. package-smoke, phase0, linux-docker are gone. Do not run.
# PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/package-smoke/tests -v
# PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/phase0/tests -v
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/dev/tests -v
bash -n scripts/gate-a-fuzz.sh scripts/gate-a-mutants.sh scripts/gate-a-residue.sh scripts/path-b-done.sh scripts/pmuxd-run.sh tools/dev/check.sh tools/screen-corpus/per_binary_tests.sh
shellcheck scripts/gate-a-fuzz.sh scripts/gate-a-mutants.sh scripts/gate-a-residue.sh scripts/path-b-done.sh scripts/pmuxd-run.sh tools/dev/check.sh tools/screen-corpus/per_binary_tests.sh
# DELETED. linux-docker is gone. Do not run.
# PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/linux-docker/tests -v
bash scripts/gate-a-residue.sh --self-test-disappearing-temp-root
PMUX_E2E_BIN_DIR="$PMUX_GATE_A_RELEASE_DIR" bash scripts/gate-a-residue.sh
```

Living `tools/evidence_common` is path redaction (`portable_paths.py`).
Bounded-process, package-smoke, Phase 0, and linux-docker have been removed;
their parser tests cannot close product rows. Living: `tools/dev/tests` owns the
documented surface (`test_documented_surface.py` derives `README.md` from
`pmux --help`, `pmuxd serve --help`, `pmux-mcp`'s `tools/list`, `MAX_POOL_SIZE`
and `MODEL_TABLE`) and the three-command workflow. Tree-wide redaction is
`tools/dev/redaction/test_redaction.py`.
Gate A (`run_gate.py`, the phase manifest, driver tests) has been removed.

### The SIGKILL crash harness (removed)

`tools/crash-harness` measured crash-safety for the stored-agent product.
That product is gone; the harness is deleted. Dated receipts stay.

## 4. Public-surface coverage matrix

This matrix separates structural coverage from candidate execution. A row is
`COVERED` only when its cited tracked tests are complete at every named layer
and current source-level evidence has passed. Where the last missing evidence
is execution through the exact integrated release binaries, the row remains
`OPEN-L3`; where it is the release-bound load/fault/resource pass, it remains
`OPEN-L5`. The location column names that pending Gate D or E run explicitly.

### 4.0 Package and vendored dependency integrity

| ID | Contract | Layer | Authoritative tracked location | Status |
| --- | --- | --- | --- | --- |
| PKG-01 | Historical freeze packaging gate (identity-fenced tarball/wheel). Deleted with package-smoke. Living client checks are TypeScript `npm test` and Python unittests in `tools/dev/check.sh`. Do not restore the freeze gate. | L4 | package-smoke deleted | HISTORICAL |
| DEP-01 | The locked Cargo graph resolves exact `rmux-client =0.9.0` to the local vendored crates.io archive; its official package/VCS/file manifest is fixed offline, every upstream file is unchanged except the one reversible initialized-slice bound, the excluded crate passes its own locked fmt/check/Clippy/rustdoc/test lane, and direct plus managed attach preserve legal `1 \| 4 \| payload` fragmentation | L0/L2 | `crates/rmux/tests/vendor_patch.rs`, `crates/rmux/tests/fixtures/rmux-client-0.9.0.sha256`, `crates/rmux/tests/attach_fragmentation.rs`, standalone `vendor/rmux-client/Cargo.toml` Gate A lane | COVERED |
| DEP-02 | The locked Cargo graph resolves exact `rmux-server =0.9.0` to the local vendored crates.io archive; its package/VCS/complete-tree identity is fixed offline; only the documented attach-EOF production file and fourteen regressions differ; `pmux-rmuxd` disables defaults, requests no explicit features, and resolves an exactly empty server feature set; complete frames buffered before EOF dispatch once across Unlock barriers, truncated tails fail without mutation, and receiver close plus drain provides the single EOF/control linearization boundary; and the excluded crate passes exact no-default all-target compilation and patch tests plus all-feature rustdoc and Clippy with every warning denied except the two named immutable-upstream style-lint categories | L0/L2/L3 | `crates/rmux/tests/vendor_server_patch.rs`, `crates/rmux/tests/fixtures/rmux-server-0.9.0.sha256`, `vendor/rmux-server/src/pane_io/tests.rs`, `bin/pmux-rmuxd/tests/process_blackbox.rs::real_attach_half_close_delivers_the_final_complete_frame_exactly_once`, standalone `vendor/rmux-server/Cargo.toml` Gate A lane; exact release Gate D rerun pending | OPEN-L3 |

### 4.1 Protocol and framing

| ID | Contract | Layer | Authoritative tracked location | Status |
| --- | --- | --- | --- | --- |
| P-01 | All ten request methods; version, request UUID, strict execution DTOs and safe defaults | L0/L4 | `crates/protocol/tests/v1_wire.rs::{every_method_variant_round_trips,request_decoding_requires_version_and_rejects_unknown_fields,minimal_start_request_applies_safe_defaults}`; `crates/protocol/tests/v1_golden.rs::{shared_golden_frames_are_exact_rust_v1_values,every_strict_request_object_pointer_rejects_an_additive_field}`; all-ten-method tests in the Rust/TS/Python golden-conformance suites named by `CL-01` | COVERED |
| P-02 | Every response-result, event, error, enum discriminant and required nested field | L0/L4 | `crates/protocol/tests/v1_golden.rs::{shared_required_field_inventory_exactly_matches_rust_authority,every_golden_result_event_and_error_rejects_the_shared_required_field_inventory}` plus the shared required-field deletion tests in `crates/client/tests/v1_golden.rs`, `clients/typescript/tests/golden-conformance.test.mjs`, and `clients/python/tests/test_golden_conformance.py` | COVERED |
| P-03 | Additive response/event fields but strict known discriminants/identity | L0/L4 | `crates/protocol/tests/v1_golden.rs::every_golden_result_event_and_error_accepts_additions_at_every_object_boundary`; `crates/client/tests/v1_golden.rs::{shared_goldens_accept_additive_fields_at_every_result_event_and_error_object_boundary,shared_negative_identity_schema_sequence_cursor_gap_and_exhaustion_matrix_fails_closed}` and the same named contracts in the TS/Python golden suites | COVERED |
| P-04 | Four-byte big-endian framing, UTF-8 JSON, 8 MiB request/response bound, resynchronization rules | L0/L1/L3 | `crates/protocol/tests/v1_wire.rs::{native_frame_header_admission_has_an_exact_inclusive_8_mib_boundary,native_frame_accumulator_is_fragmentation_invariant_and_preserves_next_frame}`; `bin/pmuxd/src/handler.rs::{production_reader_accepts_every_header_and_payload_fragment_width,production_reader_distinguishes_clean_eof_from_every_truncated_boundary,arbitrary_admitted_payloads_have_bounded_decode_recovery_and_responses}`; `bin/pmuxd/tests/process_blackbox.rs::raw_uds_modes_framing_redaction_signal_and_cleanup_cross_the_real_daemon`; exact release Gate D rerun pending | OPEN-L3 |
| P-05 | Subscription wait/event bounds, byte paging, replay-gap snapshot and cursor semantics | L0/L1/L2/L4 | `crates/service/tests/v1_actor.rs::{bounded_replay_reports_gap_with_current_snapshot,event_page_exact_frame_boundary_never_skips_a_retained_sequence,future_subscription_cursor_fails_before_changing_actor_history,direct_registry_subscription_preflight_rejects_wire_and_service_bounds_before_lookup}`; `crates/service/tests/actor_model.rs::production_actor_subscription_pages_match_cursor_gap_wait_and_frame_invariants`; shared Rust/TS/Python negative replay/cursor tests | COVERED |
| P-06 | Stable typed errors, retryability, details, nil correlation when request ID unrecoverable | L0/L3/L4 | `crates/protocol/tests/v1_conformance_vectors.rs::shared_manifest_matches_the_closed_v1_surface`; shared Rust/TS/Python error-body tests; `bin/pmuxd/src/handler.rs::{typed_decode_error_preserves_request_id_and_connection_recovers,duplicate_envelope_fields_are_rejected_without_dispatch}`; `bin/pmuxd/tests/process_blackbox.rs::raw_uds_modes_framing_redaction_signal_and_cleanup_cross_the_real_daemon`; exact release Gate D rerun pending | OPEN-L3 |
| P-07 | All protocol-owned integers are nonnegative safe JSON integers; opaque JSON integers use the signed safe range; producers and cursor advancement fail before overflow/saturation | L0/L1/L2/L4 | `crates/protocol/tests/v1_golden.rs::safe_integer_wire_field_inventory_enforces_inclusive_bounds`; shared safe-integer and safe-cursor cases in `crates/client/tests/{fake_uds.rs,v1_golden.rs}`, `clients/typescript/tests/{client.test.mjs,golden-conformance.test.mjs}`, and `clients/python/tests/{test_client.py,test_golden_conformance.py}`; `crates/service/tests/v1_actor.rs::{event_sequence_exhaustion_rejects_before_turn_mutation_and_preserves_close_reserve,direct_turn_deadline_domain_is_checked_before_turn_or_terminal_mutation,direct_registration_rejects_invalid_compatibility_and_idle_domains_before_publication,transcript_opaque_numbers_fail_before_any_public_producer_can_serialize_them,worker_timing_producers_check_near_safe_max_and_one_past_at_the_sample}`; `bin/pmux-mcp/src/main.rs::{rpc_ids_are_verbatim_strings_null_or_integral_signed_safe_numbers,outbound_number_preflight_recurses_through_arrays_and_objects}` | COVERED |
| P-08 | Reserved disconnect/heartbeat lease values fail as `unsupported_feature`; the default continue/no-heartbeat value is the only admitted v1 wire behavior | L0/L3/L4 | `crates/protocol/tests/v1_golden.rs::reserved_turn_lease_vectors_decode_as_valid_requests`; shared Rust/TS/Python reserved-lease tests; `crates/service/src/native.rs::tests::unimplemented_disconnect_leases_fail_closed`; exact release Gate D rerun pending | OPEN-L3 |
| P-09 | The shared manifest pins all eighteen nested plain-string enums under `value_enums`, and Rust, TypeScript, and Python each assert exhaustiveness in both directions against it, so no client may carry a variant the manifest omits or omit one it declares; the two client runtime validators source those same arrays instead of local literals, so they are transitively manifest-pinned | L0/L4 | `tests/conformance/v1/manifest.json` `value_enums`; `crates/protocol/tests/v1_conformance_vectors.rs::shared_manifest_value_enums_match_the_rust_string_enums` (asserts the key set and every value list); `clients/typescript/tests/golden-conformance.test.mjs::"shared manifest value enums match the TypeScript unions"`; `clients/python/tests/test_golden_conformance.py::ValueEnumConformanceTest::{test_shared_manifest_value_enums_match_the_python_literals,test_every_python_string_literal_alias_is_pinned}`; validator sourcing at `clients/typescript/src/protocol.ts::V1_VALUE_ENUMS` consumed by `client.ts::requireEnumField` call sites and `clients/python/pmux_client/client.py::_values` | COVERED |

### 4.2 Transcript authority

| ID | Contract | Layer | Authoritative tracked location | Status |
| --- | --- | --- | --- | --- |
| T-01 | Exact UUID plus canonical/Unicode cwd; new collision; unique resume; bounded locator | L0/L1/L2 | `crates/claude/src/locator.rs::{cwd_identity_uses_canonical_unicode_normalization,generated_split_or_mismatched_identity_rows_never_authorize_resume,generated_collision_missing_and_ambiguous_identity_sets_fail_closed}`; `crates/service/tests/native_service.rs::transcript_identity_preflight_precedes_lifecycle_files_version_and_process_side_effects` | COVERED |
| T-02 | Complete-line JSONL framing; partial line cannot complete; monotonic cursor/generation; replacement/truncation fail closed | L0/L1/L2 | `crates/claude/tests/transcript_properties.rs::{cursor_frames_arbitrary_records_for_arbitrary_chunks,cursor_matches_reference_across_append_truncate_and_replace_sequences}`; `crates/service/tests/transcript_filesystem_faults.rs::{complete_line_framing_blocks_a_terminal_row_until_its_newline,unterminated_terminal_row_times_out_and_reaps_instead_of_committing,active_turn_truncation_fails_closed_and_reaps,active_turn_file_generation_replacement_fails_before_replacement_content_can_commit}` | COVERED |
| T-03 | Known row/content shapes and relevant schema drift; malformed JSON/UTF-8/values fail closed | L0/L1 | `crates/claude/tests/transcript_properties.rs::{strict_parser_is_total_over_arbitrary_bounded_bytes,malformed_semantic_mutations_never_complete_a_strict_turn,generated_semantic_conflicts_never_produce_an_authoritative_terminal}`; parser table tests and actor-level malformed JSON/UTF-8 filesystem faults | COVERED |
| T-04 | Exactly one post-arm main typed prompt matching normalized bytes | L0/L1 | `crates/claude/tests/transcript_engine.rs::{multiple_main_typed_prompt_acknowledgements_fail_closed,non_main_typed_rows_cannot_acknowledge_or_conflict_with_the_active_prompt,api_error_overrides_end_turn_and_prompt_matching_is_exact_after_normalization}`; generated graph/prompt-conflict mutations in `transcript_properties.rs` | COVERED |
| T-05 | Main parent graph, append order, branches/cycles, sidechain/team/meta isolation and attachments | L0/L1 | graph/sidechain/attachment table tests in `crates/claude/tests/transcript_engine.rs`; `crates/claude/tests/transcript_properties.rs::{generated_graph_mutations_at_arbitrary_depth_fail_closed,generated_parallel_sidechain_interleaving_preserves_deduplicated_usage}` | COVERED |
| T-06 | Logical message grouping and fragment conflict rules | L0/L1 | `crates/claude/tests/transcript_engine.rs::{fragmented_tool_turn_is_grouped_correlated_and_deduplicated,request_id_groups_fragments_and_row_uuid_is_final_fallback,strict_graph_rejects_interleaved_logical_message_identities}`; generated fragmented-turn property | COVERED |
| T-07 | Ordered, deduplicated tool calls/results; duplicate/orphan/conflict rejection | L0/L1 | `crates/claude/tests/transcript_engine.rs::{parallel_tool_calls_and_results_keep_call_order,exact_duplicate_tool_blocks_are_deduplicated_by_tool_use_id,duplicate_and_orphan_tool_ids_fail_closed}`; generated fragmented-turn and semantic-conflict properties | COVERED |
| T-08 | Per-logical-message usage; main/side/combined separation; conflict and overflow rejection | L0/L1 | `crates/claude/tests/transcript_engine.rs::{usage_aggregation_overflow_fails_closed,strict_mode_rejects_unknown_correlated_content_and_usage_conflicts}`; generated fragmented-turn and parallel-sidechain usage properties | COVERED |
| T-09 | Exact terminal stop matrix, text requirements, API-error precedence and latest eligible leaf | L0/L1 | `crates/claude/tests/transcript_engine.rs::{exact_stop_reason_matrix_is_fail_closed,strict_terminal_success_requires_text_but_refusal_may_be_textless,api_error_overrides_end_turn_and_prompt_matching_is_exact_after_normalization,trailing_structural_or_tool_result_leaf_prevents_earlier_terminal_commit}` plus the generated stop matrix | COVERED |
| T-10 | Completion requires ack+candidate+stable cursor+complete EOF+drain+ready+quiet+no modal/lease loss | L0/L2 | `crates/service/tests/completion_gate.rs::{every_completion_input_independently_blocks_commit_until_satisfied,completion_time_lease_loss_fails_without_a_success_commit}`; `crates/service/tests/v1_actor.rs::{ready_and_quiet_terminal_without_transcript_candidate_never_completes,transcript_terminal_needs_ready_quiet_and_drain_before_completion,post_terminal_evidence_repoll_ingests_late_rows_before_commit}`; the POST-MARKER CATCH WINDOW -- how late a row may arrive and still be read before the commit -- is named and floored at `v1::backend::{POST_MARKER_CATCH_WINDOW_FLOOR_MS,post_marker_catch_window_ms}` (438 ms, MEASURED as the campaign's largest post-answer arrival), enforced at compile time by two `assert!`s inside `driver_io.rs`'s `SCREEN_QUIET_FOR` initialiser (the MINIFIED floor binds first), and bound to the six graduated-band tests by `crates/service/tests/v1_actor.rs::the_bands_catchable_window_is_the_products_own_derivation`, which is what makes those six assertions about a shipped guarantee rather than about a local helper | COVERED |
| T-11 | Final text/blocks/tools/model/usage/provenance come only from terminal main logical message | L0/L2/L3 | `crates/service/tests/v1_actor.rs::actor_maps_fragmented_fixture_without_prior_sidechain_team_or_meta_leaks`; exact release Gate D rerun pending | OPEN-L3 |

### 4.3 Launch, session actor, lifecycle, and security

| ID | Contract | Layer | Authoritative tracked location | Status |
| --- | --- | --- | --- | --- |
| S-01 | Interactive-only argv; forced new/resume identity; full forbidden flags and bounded raw allowlist | L0/L2/L3 | `crates/service/src/claude_launch.rs::{subscription_launch_is_interactive_and_sanitized,resume_uses_resume_instead_of_session_id,print_and_positional_passthrough_are_rejected,every_typed_launch_option_has_one_unambiguous_argv_mapping}`; living pool e2e in `crates/e2e/tests/pool_concurrency.rs`; exact release Gate D rerun pending | OPEN-L3 |
| S-02 | Canonical paths, typed options, environment snapshot/patch, subscription stripping, transparent profile | L0/L2/L3 | canonical path/environment/profile tests in `crates/service/src/claude_launch.rs`; exact release Gate D rerun pending | OPEN-L3 |
| S-03 | Private inline files, one-use broker token, launcher `execve`, no secret argv/log leakage | L0/L2/L3 | `crates/service/src/sensitive_launch.rs::{secrets_are_files_not_process_arguments,artifacts_are_removed_on_drop}`; `crates/service/src/launch_broker.rs::{capability_is_one_use,expired_capability_never_returns_spec}`; `bin/pmux-launcher/tests/process_blackbox.rs`; exact release Gate D rerun pending | OPEN-L3 |
| S-04 | Exact compatibility cell; empty default operator registry; promoted macos/aarch64 2.1.220..=2.1.227; Linux operator `--tested-claude-profile` only (not promoted); auto→sdk; drain bounds; untested visibility | L0/L3 | `crates/service/src/compatibility.rs::{require_tested_matches_the_complete_cell_and_uses_its_drain,allow_untested_is_explicit_and_uses_the_daemon_fallback,invalid_and_duplicate_profiles_are_rejected}`; empty-registry actual-daemon E2E; exact release Gate D rerun pending | OPEN-L3 |
| S-05 | One actor, legal transitions, at most one active turn | L0/L1 | `crates/service/tests/v1_actor.rs::{one_active_turn_and_prompt_hash_idempotency_are_enforced,simultaneous_distinct_turns_accept_exactly_one}`; `crates/service/tests/actor_model.rs::command_sequence_matches_single_owner_actor_model` validates every emitted state transition through the production transition predicate | COVERED |
| S-06 | Turn ID exact replay/conflict; no eviction; capacity before terminal mutation | L0/L1/L2 | idempotency/history/capacity tests in `crates/service/tests/v1_actor.rs`; `crates/service/tests/actor_model.rs::{command_sequence_matches_single_owner_actor_model,turn_capacity_is_checked_before_any_actor_or_backend_mutation}` | COVERED |
| S-07 | Session+generation fence; stale operations never retarget; exact close tombstone replay | L0/L2/L3 | `crates/service/tests/v1_actor.rs::stale_generation_operations_cannot_target_a_resumed_process`; tombstone unit tests; exact release Gate D rerun pending | OPEN-L3 |
| S-08 | Event monotonicity, count+byte retention, paging/gap, long poll, subscriber backpressure | L0/L1/L2/L5 | P-05 actor/model/client tests; `crates/service/tests/concurrency_backpressure.rs::{replay_byte_saturation_preserves_frame_paging_and_gap_exclusivity,every_concurrent_long_poll_subscriber_wakes_for_one_actor_event,actual_daemon_slow_and_disconnected_event_subscribers_leave_one_of_64_slots_live}`; exact release Gate E rerun pending | OPEN-L5 |
| S-09 | One immutable deadline covers admission through serialized commit and replays exact timeout | L0/L1/L2 | all five tests in `crates/service/tests/deadline_idempotency.rs`; deadline/expiry commands in `crates/service/tests/actor_model.rs::lifecycle_commands_match_deadline_attach_modal_expiry_and_close_model` | COVERED |
| S-10 | Stable cursor-correlated editor; one paste; changed stable render; at-most-one Enter; ambiguity reaps | L0/L2/L3 | admission/fence/ambiguity tests in `crates/service/src/driver_io.rs`; exact release Gate D rerun pending | OPEN-L3 |
| S-11 | Typed needs-input; no automatic answers; admission modal fail/reap. Public post-Enter session resumability is not a product. Path A startup/admission/post-Enter full-stack cells were deleted with the session surface. Living Trust recovery is pool remint | L0/L2 | modal classifier/admission tests in `driver_io.rs`; actor modal-phase tests in `v1_actor.rs` (`a_turn_refused_by_a_modal_names_the_modal_and_what_to_do_about_it` publishes remint). Deleted Path A composition is CLOSED like CLI-02 | COVERED |
| S-12 | Internal actor cancellation sends one Ctrl-C; ready+quiet+post-interrupt drain or taint; next-turn isolation on the actor. Public `pmux cancel` / cancel-then-next-turn session composition is not a product (CLI-06). Living recovery is pool remint / Messages reprime | L0/L2 | `crates/service/tests/v1_actor.rs::{cancellation_recovers_or_taints_and_confirmed_close_terminates_the_actor,cancellation_requires_post_interrupt_transcript_stability,cancellation_drains_late_rows_before_an_immediate_fresh_turn}`. Deleted Path A full-stack cell is CLOSED like CLI-02 | COVERED |
| S-13 | Public attach is not a product surface. Private rmux EOF/frame delivery remains an internal sidecar contract | L0/L2 | every one of the fourteen patch-owned regressions in `vendor/rmux-server/src/pane_io/tests.rs`; `bin/pmux-rmuxd/tests/process_blackbox.rs::real_attach_half_close_delivers_the_final_complete_frame_exactly_once` | COVERED |
| S-14 | Close retries until exact process boundary reaped; descendant escape invalidates proof | L0/L2/L3/L5 | `crates/service/tests/v1_actor.rs::unconfirmed_close_stays_retryable_until_process_reaping_is_confirmed`; `crates/service/tests/lifecycle_faults.rs::observed_descendant_escape_keeps_real_close_unconfirmed_across_retry`; exact release Gate D/E reruns pending | OPEN-L5 |
| S-15 | Idle TTL only closes non-running unattached state; explicit close remains deterministic | L0/L1/L2 | idle/attach/active-turn tests in `crates/service/tests/v1_actor.rs`; `crates/service/tests/actor_model.rs::lifecycle_commands_match_deadline_attach_modal_expiry_and_close_model` | COVERED |
| S-16 | Daemon shutdown drains, closes, sidecar exits, unchanged socket removed; owner loss and lease faults bounded | L2/L3/L5 | cancellation-safe shutdown tests in `crates/service/src/native.rs`; `bin/pmuxd/tests/process_blackbox.rs`; `crates/service/tests/{private_runtime.rs,lifecycle_faults.rs,bounded_soak.rs}`; exact release Gate D/E reruns pending | OPEN-L5 |
| S-17 | Hybrid hooks additive/corroborating only, bounded/private, artifacts removed | L0/L2/L3 | `crates/service/tests/hybrid_hooks.rs`; `bin/pmux-hook/tests/process_blackbox.rs`; Hybrid full-stack cell; exact release Gate D rerun pending | OPEN-L3 |
| S-18 | Redaction of prompts, environment values, terminal screens, tokens, capabilities and backend matcher details | L0/L2/L3 | launch/driver/actor redaction tests; CLI/MCP process suites; actual daemon and full-stack log/argv assertions; exact release Gate D rerun pending | OPEN-L3 |
| S-19 | Public **control** is only an explicit absolute owner-only UDS: no path discovery, no client daemon autostart, and no INET listener on the default daemon. `--messages-bind` is an opt-in loopback Anthropic Messages facade (not a general HTTP/TCP control plane); `parse_messages_bind` refuses any non-loopback address; default E2E daemons must not pass the flag | L0/L3 | absolute-path rejection in Rust/TS/Python clients, CLI, and MCP; `bin/pmuxd/src/messages_http.rs::loopback_only`; living pool e2e in `crates/e2e/tests/pool_concurrency.rs`; exact release Gate D rerun pending | OPEN-L3 |
| S-20 | Trust/login/permission/update/quota/unknown modals produce typed `needs_input` or the stricter admission failure without any automatic answer bytes. Path A real-PTY startup/post-Enter/admission full-stack cells were deleted with the session product. Living Trust recovery is pool remint | L0/L2 | classifier/no-write tests in `driver_io.rs` (`prompt_and_modal_classification_are_conservative`, `the_blocking_phrase_table_holds_every_phrase_the_classifier_names`, `completion_evidence_rejects_modal_before_or_during_stability`); actor modal tests in `v1_actor.rs`. Deleted Path A composition is CLOSED like CLI-02 | COVERED |
| S-21 | Daemon restart never reconstructs a public session or reinjects an interrupted prompt. Living recovery is pool remint / Messages reprime, not caller resume of a UUID+generation. Public session resume is not a product (CLI-02) | L0/L2 | `crates/service/tests/v1_actor.rs::{stale_generation_operations_cannot_target_a_resumed_process,oversized_exact_result_becomes_one_replayable_failure_without_reinjection}`; `bin/pmuxd/src/conversation.rs::{rewind_or_compact_reprimes,class_change_reprimes}`; `crates/service/tests/path_b_pool.rs::leased_ttl_recycle_remints_down_to_the_warm_floor` | COVERED |
| S-22 | Native turn input is one complete bounded prompt | L0/L3/L4 | prompt bounds/source-conflict tests in `bin/pmux/tests/process_boundary.rs`; exact release Gate D rerun pending | OPEN-L3 |
| S-23 | `permission_mode` argv is a total wildcard-free mapping: six variants emit the `--permission-mode <value>` pair, `dangerously_skip_permissions` emits that single flag and no value, no other variant ever emits it, and the closed raw allowlist still refuses it as an `extra_args` argument; the wire value round-trips as `dangerously_skip_permissions` | L0 | `crates/service/src/claude_launch.rs::{dangerously_skip_permissions_is_one_flag_and_no_other_mode_emits_it,dangerous_permission_bypass_has_a_stable_snake_case_wire_value,every_typed_launch_option_has_one_unambiguous_argv_mapping}`; the enum→argv table is exhaustive by construction at `claude_launch.rs::permission_mode_argv`, so an unmapped future variant is a compile error. General option mapping through the shipped child argv stays owned by `S-01` | COVERED |
| S-24 | A session launched with the permission bypass sets `dangerous_permission_bypass` and every one of its turn results carries the `dangerous_permission_bypass` warning with its exact message, on the completed path and on the cancelled-turn path; a session without the bypass never carries it | L0 | `crates/service/src/v1/actor.rs::permission_bypass_is_a_per_turn_result_warning_only_for_bypass_sessions` (asserts code, message, and absence); producer `actor.rs::permission_bypass_warnings` reached from `::build_turn_result` and `::finish_cancel`. Transport of `TurnResult.warnings` itself is owned by `P-02`/`CL-01`; the launch→registration→actor propagation of the flag (`claude_launch.rs::resolve_claude_launch` → `native.rs::start_session_owned_with_retention` → `registry.rs::register` → `actor.rs::spawn`) is a required-field copy at three struct literals with no dedicated composition test | COVERED |
| S-25 | The inherited snapshot term is filtered by a closed allowlist before every other step: an exact name absent from `INHERITED_EXACT_KEYS` and not covered by an `INHERITED_PREFIXES` prefix is denied by construction and reported as a removal; matching is case-sensitive in both forms; every name the allowlist declares survives its own filter unrewritten (`TERM`, overwritten by the transparent profile, is the single stated exception); the four nested-Claude markers `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_REMOTE`, `CLAUDE_CODE_CHILD_SESSION` are denied by the allowlist **without** help from the transparent denylist; and the documented order is exactly `allowlist(snapshot) - unset + set - policy_removals + profile_changes`, with `set` bypassing the allowlist as the explicit extension channel | L0 | `crates/service/src/claude_launch.rs::{an_unknown_inherited_name_is_denied_by_construction,the_allowlist_denies_nested_claude_markers_without_help_from_the_denylist,every_allowlisted_name_survives_the_snapshot_filter,allowlist_prefix_and_exact_matching_are_case_sensitive,caller_supplied_set_bypasses_the_allowlist_entirely,documented_environment_order_is_allowlist_then_unset_then_set_then_removals}`; filter `claude_launch.rs::inherited_from_snapshot` (the local alias for `crates/protocol/src/v1/launch_environment.rs::inherits`) over `launch_environment.rs::{INHERITED_EXACT_KEYS,INHERITED_PREFIXES}`, producer `claude_launch.rs::build_environment`; normative text at `docs/spec.md` §4 | COVERED |
| S-26 | The allowlist is auth-policy aware and the two mechanisms are independent: under `inherit` the provider-routing families (`ANTHROPIC_`, `AWS_`, `GOOGLE_`, `GCLOUD_`, `CLOUDSDK_`, `AZURE_`, `VERTEX_REGION_` prefixes plus `CLOUD_ML_REGION`) and the subscription auth names survive the snapshot filter; under `subscription` those same names are denied at the filter **and** removed again by the policy pass, so a name reintroduced through `set` is still stripped and `inherit` never relaxes any other validation | L0/L2/L3 | `crates/service/src/claude_launch.rs::{inherit_retains_provider_routing_and_subscription_denies_it,transparent_profile_strips_parent_behavior_but_inherit_keeps_credentials,environment_patch_order_and_subscription_stripping_are_exact}`; branch in `claude_launch.rs::inherited_from_snapshot` over `launch_environment.rs::{PROVIDER_ROUTING_PREFIXES,PROVIDER_ROUTING_EXACT_KEYS,SUBSCRIPTION_AUTH_KEYS}`, second pass in `claude_launch.rs::build_environment`; child-side attestation shares `S-02`'s full-stack launch assertions, so exact release Gate D rerun pending | OPEN-L3 |
| S-27 | The minified (Path B) cell is a start-time property admitted only on a tested compatibility profile (promoted macos/aarch64 2.1.220..=2.1.227, or an operator `--tested-claude-profile`; Linux is operator, measured 2.1.236, not promoted), and it is refused before a Claude process is spawned; the same one rule also refuses at registration, so the `pub` registry is not a second door. Public `clear_session` is refused (`session_surface_removed`). Internal pool `/clear` still fences on the transcript the pool binds: matching rotates, and every stale fence — including one stale by exactly one rotation — is a non-retryable `id_conflict` carrying `stale_transcript_fence`. Product recovery of a lost Messages pin is reading `x-pmux-cell` (`s{slot}e{epoch}`) from the response or `doctor` `conversation_leases`. The internal fence refusal is Step 0, ahead of the busy guard, and holds mid-turn and under a modal | L0/L3 | `crates/service/tests/minified_cell.rs::{a_session_registered_as_a_minified_cell_can_be_cleared_without_selection,an_untested_profile_cannot_register_a_minified_cell_at_all,a_clear_fenced_one_rotation_behind_is_refused_rather_than_answered,a_clear_fenced_two_rotations_behind_is_refused,a_stale_fence_is_refused_without_disturbing_a_turn_in_flight,a_stale_fence_cannot_claim_a_transcript_that_has_since_served_a_turn,a_stale_fence_is_refused_while_the_session_waits_on_a_modal,an_untested_compatibility_cell_cannot_select_the_minified_cell}`; one shared predicate at `crates/service/src/v1/actor.rs::require_tested_for_minified_cell` called from `native.rs::start_session_owned_with_retention`, `registry.rs::register`, and `actor.rs::select_minified_cell` (the copy in `actor.rs::spawn` was DELETED as unreachable: `register` is the only caller of `spawn_actor` and applies the rule immediately before it) | COVERED |
| S-28 | Assert-empty: a transcript a clear rebinds onto must have every row individually proven inert, and the `/clear` command echo Claude writes is what distinguishes a real clear from any other slash command the fuzzy composer might have executed. The measured 6-row `/clear` preamble and the launch preamble are both ACCEPTED (non-vacuity). **Read the two row counts precisely, because they are not the same kind of number.** SIX is measured: `/clear` writes five rows immediately (`mode`, `file-history-snapshot`, the `<local-command-caveat>` user row, the `<command-name>/clear</command-name>` user row, and the `local_command` system row) and a `last-prompt` metadata row follows once nothing else does. The launch fixture is FIVE rows, but the MEASURED launch preamble is **four** (`mode`, `permission-mode`, `bridge-session`, `file-history-snapshot`); the fifth row in the fixture is a caveat row added deliberately so that some row carries both the session id and the cwd, without which the locator will not corroborate the file at all. Calling it "the measured 5-row launch preamble" stated something the measurement did not establish, and a reader who budgets for exactly five is budgeting against a test artifact, while a successor opened by `/model`, one carrying a prompt/reply, a turn marker, an unexpected system subtype, an unknown row type, an unrecognized user row, a metadata record outside the measured preamble allowlist (`queue-operation` is queued user input), a preamble metadata row stamped with a foreign session id, a `last-prompt` row naming a prompt, or more than 16 rows is REFUSED with a named `reason` and no transcript content in the diagnostic; a refusal reaches `SessionState::Tainted`, which refuses every later turn and every later clear non-retryably. A missing echo or an unterminated trailing row is a bounded not-yet answer, never an immediate refusal, because `resolve_rotation` returns the instant row 0 parses | L0/L3 | `crates/service/src/driver_io.rs::tests::{a_cleared_transcript_carrying_the_measured_preamble_rebinds,the_measured_launch_preamble_is_accepted_and_carries_no_clear_echo,a_launch_with_no_transcript_yet_is_not_a_refusal,a_successor_opened_by_a_different_slash_command_is_refused,a_successor_transcript_that_is_not_empty_is_refused,a_successor_carrying_a_turn_marker_is_refused,a_successor_carrying_an_unexpected_system_subtype_is_refused,a_successor_carrying_an_unknown_row_type_is_refused,a_successor_carrying_an_unrecognized_user_row_is_refused,a_successor_over_the_row_budget_is_refused,a_successor_with_no_clear_echo_is_refused_after_the_settle_wait,a_preamble_that_lands_after_the_anchor_still_rebinds,a_partly_written_trailing_row_is_not_a_settled_preamble,a_launch_preamble_carrying_a_clear_echo_is_refused,a_diagnostic_schema_token_is_bounded_before_it_is_reproduced,a_successor_carrying_metadata_that_is_not_preamble_is_refused,preamble_metadata_must_carry_this_transcripts_own_identity,a_last_prompt_row_that_names_a_prompt_is_refused}`; quarantine mapping at `crates/service/tests/minified_cell.rs::a_clear_whose_result_was_not_empty_taints_the_session`. MEASURED corpus: 61 post-`/clear` transcripts, `<command-name>` counter `{'/clear': 60}`. Predicate `driver_io.rs::prove_transcript_inert`, hooked between `resolve_rotation` and `arm_at_eof`. The composer selection itself is still unproven AT THE SOURCE, but the menu geometry it depends on is no longer unmeasured (`docs/path-b.md` §10 item 7): the selection highlight is COLOUR-ONLY (`fg=idx153` against `idx246`, no glyph and no reverse video, so it was absent from pmux's data until the styled read was widened), the menu ranks by a fuzzy score over NAMES AND DESCRIPTIONS rather than alphabetically (at prefix `/c` the selected entry is `/cd`; `/doctor` is a candidate at `/cl` because its description contains "Claude"), and the composer's own gate is measured from the LAST RENDERED ROW at 2 rows in 85/85 live 2.1.220 screens and 5/5 recovered 2.1.70 fixtures rather than from the grid bottom. `wrong_local_command` remains a post-hoc detection rather than a prevention | COVERED |
| S-29 | A `clear_session` that provably typed nothing does not quarantine the cell. `driver_io::clear_and_rebind` marks the two refusals raised before the command is submitted -- a past deadline, and a clear issued before the session's first turn, where no transcript exists to observe a rotation against -- and the actor returns those without `poison_after_failed_rebind`. The claim is positive, so any refusal that does not make it still taints | L0 | `crates/service/src/driver_io.rs::tests::a_clear_refused_before_submission_says_so_and_abandons_nothing` (both triggers, plus the post-Enter contrast that must NOT carry the mark); actor mapping at `crates/service/tests/minified_cell.rs::a_clear_that_provably_typed_nothing_leaves_the_session_usable`, which proves the next turn still completes and the session is still clearable | COVERED |
| S-30 | The launch half of assert-empty is enforced at the admission boundary, not in the wire path: `TranscriptSource::assert_empty_at_launch` has a REFUSING default and `SessionRegistry::register` demands it of every `SessionCell::Minified` registration before an actor exists. A `SessionIdentity::Resume` -- or a caller-chosen `New` id colliding with an existing transcript -- is refused, and a `Full` cell is never asked to prove a claim it does not make. The refusing POLARITY of the default is itself tested: replacing it with `Ok(())` left the whole workspace green, because every other implementor takes the default and none is registered as minified | L0/L3 | `crates/service/tests/minified_cell.rs::{a_minified_cell_cannot_be_registered_over_a_transcript_that_served_work,a_transcript_source_that_cannot_prove_emptiness_may_not_back_a_minified_cell}` (the second registers a source that deliberately does not override the method, and fails on the flip) | COVERED |
| S-31 | `StartSessionRequest.cell` is omitted from the wire when it is the default and always present when it is not, so a new client cannot brick itself against a `deny_unknown_fields` daemon that predates the field; protocol `SessionSnapshot` still publishes `transcript_session_id` and `cell` as required DTO fields (additive wire / typed refusal). They are not a public recovery API; living pin recovery is `x-pmux-cell` | L0/L4 | `crates/protocol/tests/v1_wire.rs::{a_default_cell_is_omitted_from_the_wire_and_a_chosen_one_never_is,minimal_start_request_applies_safe_defaults}`; snapshot fields in the shared required-field inventory (`tests/conformance/v1/cases.json`, asserted by the Rust/TS/Python `P-02` suites) and a non-default `cell` exercised by the `start_session` golden vector | COVERED |
| S-32 | Config isolation replaces `CLAUDE_CONFIG_DIR` with a pmux-owned root and pins `CLAUDE_SECURESTORAGE_CONFIG_DIR` to the root the same request would have used without isolation, so the isolated child authenticates against exactly the caller's credential store. The pin is delivered byte-for-byte (Claude hashes it to name a keychain item) while the root is delivered canonicalized (it must name the directory pmux seeds and the locator walks); an absent pre-isolation root pins the EMPTY STRING, which selects the default unsuffixed store. Injection is step 6, after the profile denylist, so a future `CLAUDE_`-prefixed denylist entry cannot strip the pin. A caller-supplied `CLAUDE_CONFIG_DIR` or `CLAUDE_SECURESTORAGE_CONFIG_DIR` in `set` is mutually exclusive with isolation; an ambient snapshot value is not, because it IS the pin. The root must exist, be euid-owned mode `0700`, not be the pre-isolation root, not overlap cwd, and not carry a `.config.json` that would shadow the seed | L0 | `crates/service/src/claude_launch.rs::tests::{config_isolation_overrides_a_snapshot_config_dir_and_pins_the_original_store,config_isolation_pins_the_default_store_when_the_caller_had_no_config_dir,the_injected_names_are_delivered_and_are_denylisted_by_nothing_today,the_pin_is_byte_exact_while_the_root_is_canonical,config_isolation_refuses_a_caller_supplied_config_dir_or_pin,config_isolation_refuses_the_root_the_request_would_have_used_anyway,config_isolation_refuses_a_root_that_overlaps_cwd,config_isolation_refuses_a_root_anyone_else_can_read,config_isolation_refuses_a_missing_or_non_directory_root,config_isolation_refuses_a_root_whose_config_json_would_shadow_the_seed,without_isolation_nothing_about_the_launch_environment_changes,documented_environment_order_is_allowlist_then_unset_then_set_then_removals_then_isolation}`; wire shape at `crates/protocol/tests/v1_wire.rs::absent_config_isolation_is_omitted_and_a_named_root_round_trips_strictly` | COVERED |
| S-33 | A private config root is SEEDED before launch and the seed is the only thing that makes a fresh root usable: `hasCompletedOnboarding` and one `projects[<canonical cwd>].hasTrustDialogAccepted` key are required, `bypassPermissionsModeAccepted:false` is stable because `cCm()` early-returns on a falsy value, and every DURABLE preference is written to `<root>/settings.json` rather than `.claude.json` — `env.DISABLE_AUTOUPDATER:"1"` always, and `skipDangerousModePermissionPrompt` for a `--dangerously-skip-permissions` request. `.claude.json`'s `autoUpdates:false` is NOT written: it is the exact value that fires `aCm()`, which migrates the preference into userSettings and then deletes the key, so asserting it made every later start read-modify-write the file and every `VerifyOnly` start refuse. Writes are temp+rename+fsync at mode `0600`, `O_NOFOLLOW` on read, idempotent, foreign-key preserving, `env`-merging, and REFUSED outright while a live session is bound to the same root. `projects/` is never created. The root reaches `effective_config_root`, the collision scan and the transcript source with no locator changes | L0/L5 | `crates/service/src/config_isolation.rs::tests::{a_fresh_root_is_seeded_with_onboarding_trust_and_no_projects_directory,seeding_is_idempotent_and_a_satisfied_root_is_not_rewritten,a_root_in_use_by_a_live_session_is_never_written_to,a_new_cwd_under_a_live_root_is_refused_rather_than_raced,foreign_keys_and_other_projects_survive_a_reseed,a_symlinked_config_file_is_refused_rather_than_followed,a_shadowing_config_json_is_refused_because_the_seed_would_be_inert,a_dangerous_bypass_request_accepts_the_dialog_where_claude_reads_it,a_corrupt_config_file_is_refused_rather_than_overwritten}`. **Stability against pmux is the weaker claim and every one of those tests makes only that one** — they call the seeder twice with no Claude in between. Stability against CLAUDE is `config_isolation.rs::tests::a_real_claude_launch_leaves_the_seed_already_satisfied` (`#[ignore]`, runs the operator's real `claude` once and then re-seeds under `VerifyOnly`, which is the code path a real second start takes, so a key that becomes transient later fails for the same reason it would in production). Propagation at `crates/service/src/native.rs::tests::config_isolation_carries_the_private_root_all_the_way_to_the_transcript_source`; CLI/MCP start-session profile surface removed with the session product | COVERED |
| S-34 | A minified cell refuses a WRITABLE terminal attachment, at the reservation and before any rmux grant is minted, and a session holding one cannot be converted into a minified cell. This is the mutation channel the deleted clear-retry window could not see: the grant sends keystrokes client → rmux socket → PTY, so `reserve_writable_attach`, `release_writable_attach` and attach reconciliation all mutate the session while emitting nothing. Read-only attachment and Full-cell writable attachment are unaffected | L0/L3 | `crates/service/tests/minified_cell.rs::{a_minified_cell_refuses_a_writable_terminal_attachment,a_session_holding_a_writable_attachment_cannot_become_a_minified_cell}` (each anchored against the Full-cell positive on the same fixture); rule at `crates/service/src/v1/actor.rs::refuse_writable_attach_on_minified_cell`, called from `reserve_writable_attach` and mirrored in `select_minified_cell` | COVERED |
| S-35 | `cell: minified` REQUIRES `config_isolation`, and requires that root to be unshared and unused: a start with no root is refused on the request alone, a root already bound to a live session is refused rather than shared, and a root containing anything but the two files pmux seeds is refused by an allowlist rather than by a list of known residue names. The channels this closes are per-ROOT rather than per-session — `history.jsonl` (append-only, not truncated by `/clear`, recall scoped to cwd so it spans every rotation), `paste-cache/` (content-addressed, not project-scoped, mtime-cleaned, outlives transcript pruning), `projects/`, `backups/` | L0 | `crates/service/src/config_isolation.rs::tests::{a_freshly_seeded_root_is_pristine_enough_for_a_minified_cell,a_root_that_has_served_before_cannot_back_a_minified_cell}`; request rule at `crates/service/src/claude_launch.rs::validate_config_isolation`, now with a default-suite test of its own at `claude_launch.rs::tests::a_minified_cell_is_refused_without_a_private_configuration_root` (it previously had only an `#[ignore]`d e2e, so `cargo test` stayed green with the rule deleted); disposition and pristine rules at `crates/service/src/native.rs::{admit_config_root,admit_bound_resources}`; end-to-end the Path B e2e cell now starts with a per-cell private root and its transcripts are asserted there | COVERED |
| S-36 | The root and cwd rules are properties of the INCUMBENT and of the RESOLVED resource, not of the applicant's request shape — and the relation they decide is CONTAINMENT, not identity. The invariant, stated exactly: **no directory a live minified cell binds may be reachable by any other session, in any role, at any depth** — as a configuration root, as a cwd, as an isolation root, as an ancestor of any of those, or through the `HOME` the delivered root is derived from. LEAK 7 was the gap between the message this rule already printed ("no other session may be launched **under it**") and the predicate it decided with (`must_treat_as_same_directory`, an IDENTITY test): `R/sub` is not `R`, so no incumbent was found and `admit_config_root` returned `SeedDisposition::Write` against a live cell's private root. EIGHT shapes were MEASURED as ADMITTED over the real socket against a live minified cell — config root nested in the cell root (absent subdir, and the cell's own `projects/`), config root an ANCESTOR of the cell root, `HOME` redirected so the delivered root landed at `<cell root>/.claude`, cwd IS the cell's configuration root, cwd inside it, cwd inside the cell's workspace, and a minified applicant's own canonicalized, owner-checked, pristine private root nested INSIDE the victim's — and the victim's own root ended up holding the intruder's transcript, `.claude.json` and `settings.json`. The rule is now asked with the containment predicate that already existed (S-39), inode-keyed and in both directions, over the full cross-product of every directory the applicant binds against every directory every live claim binds (`LiveResourceClaim::directories`). SYMMETRIC IN THE CELL: a live MINIFIED claim answers on containment whatever the applicant is, and a MINIFIED APPLICANT gets containment against every live claim including ordinary ones, because a private root nested inside a live ordinary session's workspace is the same leak one second later. ORDINARY-versus-ORDINARY stays IDENTITY and role-matched, deliberately: nesting is the ordinary shape of a filesystem, and widening that arm would refuse a second ordinary session working in a subdirectory AND — through the seed disposition — stop pmux seeding a private root that merely sits under a live session's cwd. Resources are still compared as directories rather than byte strings, so every leak-5 spelling remains one directory; where several sessions reach one resource, the strictest claim answers. Admission is keyed on `effective_config_root`, and the assumption that this covers an isolated start's NAMED root (step 6 of `build_environment` overwrites `CLAUDE_CONFIG_DIR` with the canonicalized isolation root) is now a CHECK rather than an assumption | L0 | `crates/service/src/native.rs::tests::{no_directory_a_live_minified_cell_binds_is_reachable_at_any_depth_in_any_role,containment_binds_a_minified_applicant_to_ordinary_sessions_and_leaves_them_to_each_other,an_ancestry_walk_is_bounded_and_refuses_a_loop_an_unreadable_ancestor_and_an_overlong_name,the_named_isolation_root_and_the_delivered_configuration_root_must_be_one_directory,a_root_a_live_minified_cell_holds_admits_no_other_session_in_any_shape,a_minified_cell_is_refused_a_root_any_live_session_is_already_using,a_minified_cell_is_refused_a_root_anything_has_ever_run_in,a_cwd_is_not_shared_with_a_minified_cell_in_either_direction,a_live_minified_cells_resources_are_refused_under_every_alias_of_the_same_inode,a_root_that_does_not_exist_yet_is_admitted_and_one_that_cannot_be_inspected_is_not,a_published_session_claims_the_root_and_cwd_of_its_own_transcript}` (the last builds a published `SessionMetadata` and drives the whole chain metadata → `live_resource_claims` → incumbent → refusal). The eight-relation test asserts BOTH applicant cells per row and carries a negative control, so it cannot pass by refusing everything. Rules at `native.rs::{admit_bound_resources,require_isolation_root_is_the_effective_root,claim_reaches,incumbent_cell_for,admit_config_root,admit_cwd,strictest_cell}`, containment from `claude_launch::one_directory_contains_the_other` (ONE implementation, two callers), resource identity from `native.rs::effective_config_root` and the resolved launch cwd. COST, MEASURED: an ancestry walk per resource per live cell is O(depth × cells) — 3.41 ms per admission against 15 live minified cells with a 14-component applicant path, against 3.17 µs with no live cells and 1.40 ms for a bare `/usr/bin/true` spawn on the same host. It is not on a latency path that cares: `admit_bound_resources` has exactly one production call site (`start_session_owned_with_retention`), runs once per session START and never per turn or per `/clear`, and the same function then spawns Node for `detect_claude_version`. RESIDUAL, NARROWED BY S-48: that one call site still needs a live `NativeService` and is still only EXECUTED by `#[ignore]`d tests. What is no longer unwatched is its presence and its position — S-48 pins the four calls of the funnel prefix, in order, against `start_session_owned_with_retention`'s own source, so removing `admit_bound_resources` from the funnel or reordering it past the resolution it judges is a default-suite failure rather than a clippy warning. Deleting it outright is still a compile error (the seed disposition it binds is consumed downstream) | COVERED |
| S-37 | Cross-cell contamination: the channel table (`ROOT_CHANNELS`) remains compile-checked. Live Path A `start_session` sweeps were deleted with the session product. Living minified-cell isolation is the service `minified_cell` suite and pool e2e | L0 | `crates/e2e/tests/cross_cell_contamination.rs::every_named_channel_claims_the_paths_beneath_it_and_nothing_else`; `crates/service/tests/minified_cell.rs`; `crates/e2e/tests/pool_concurrency.rs` | COVERED |
| S-38 | A DIRECTORY pmux binds a session to may only be named in a spelling whose meaning does not depend on what exists. `..` is the one such construct: the kernel resolves left-to-right, so `/X/NOPE/../rootA` reports `NotFound` while `NOPE` is missing even though a recursive create — which is what Claude does to its own `CLAUDE_CONFIG_DIR`, and what `mkdir -p` does — creates the intermediate and lands the path on the live `/X/rootA`. MEASURED over the real socket before this: `--env CLAUDE_CONFIG_DIR=/X/NOPE/../rootA` was ADMITTED against a live minified cell holding `/X/rootA`, and the intruder's child wrote its own transcript physically inside that cell's root (LEAK 5b). Two independent refusals: the effective configuration root — for every shape that produces one, `set`, snapshot, or `HOME`-derived — must carry no `..` component at all; and the admission gate stops reading an absence as evidence for any `..` spelling of EITHER bound resource. pmux refuses rather than collapsing `..` lexically, because lexical collapsing is not the kernel's rule when a component is a symlink (`a/b/..` is `b`'s target's parent) and a wrong "different" is the leak while a wrong "same" is a refusal. The identity predicate itself keeps reporting the kernel's answer, because its other caller compares the securestorage PIN, which is a keychain-service input rather than a directory pmux binds | L0 | `crates/service/src/native.rs::tests::{an_effective_config_root_spelled_with_a_parent_component_is_refused,a_dot_dot_through_a_missing_directory_is_refused_as_either_bound_resource}` (the second asserts BOTH resources, both cells, and an EMPTY claim list — with no incumbent the old rule fell through to `SeedDisposition::Write`); the two filesystem facts are pinned as premises by `crates/service/src/claude_launch.rs::tests::an_absence_reported_for_a_dot_dot_spelling_proves_nothing` and the predicate's exact scope by `::only_a_parent_component_makes_a_spelling_depend_on_the_filesystem`. Rules at `native.rs::{effective_config_root,require_establishable_identity}` and `claude_launch.rs::traverses_a_parent_component` | COVERED |
| S-39 | Containment between the configuration root and the cwd is decided on the DIRECTORY, not on a path prefix. `root.starts_with(cwd) || cwd.starts_with(root)` compared whole components rather than bytes, so it was never open to a bare name-prefix collision, but it was open to the same alias family as LEAK 5: both sides are `Path::canonicalize`d and MEASURED `canonicalize` does not collapse the APFS firmlink namespace, so a cwd of `/System/Volumes/Data<W>` and a root of `<W>/inner` are the containment the rule exists to refuse while neither is a component prefix of the other. Asked as an ancestry question on `(st_dev, st_ino)` instead: every ancestor of the candidate descendant is compared to the candidate ancestor as a resource, in both directions | L0 | `crates/service/src/claude_launch.rs::tests::{containment_is_decided_on_the_directory_and_not_on_a_path_prefix,config_isolation_refuses_a_root_inside_a_cwd_spelled_through_the_firmlink_alias}` — the first drives the whole `aliases_of` table and proves each row is the same inode before asking, and asserts a name-prefix sibling (`workspace` vs `workspace-two`) is still NOT containment; the second is the production-reachable macOS case through `resolve_claude_launch`. Rule at `claude_launch.rs::{one_directory_contains_the_other,contains_or_is}`, now `pub(crate)` with a SECOND caller: S-36's live-cell admission asks the same walk rather than a copy of it, so the ancestry rule cannot be fixed in one place and forgotten in the other. TERMINATION: the walk is LEXICAL — `Path::ancestors` strictly shortens the spelling by one component per step — so a symlink or firmlink CYCLE on disk cannot make it loop; the three filesystem hazards are answered by `DirectoryIdentity::of` and every one is fail-CLOSED, since `ELOOP`, `EACCES` and `ENAMETOOLONG` all become `Unresolved` and `must_treat_as_same_directory` reports "treat as the same", which refuses the start (`native.rs::tests::an_ancestry_walk_is_bounded_and_refuses_a_loop_an_unreadable_ancestor_and_an_overlong_name` asserts all three refuse, and asserts the step count is bounded by the spelling the caller sent) | COVERED |
| S-40 | THE SECOND DOOR IS SHUT FOR PATH B. A `cell: minified` start may not carry `CLAUDE_CONFIG_DIR` or `CLAUDE_SECURESTORAGE_CONFIG_DIR` in `environment.set` at all, and the refusal names `config_isolation` as the supported way. `config_isolation` is canonicalized (so it must exist, and no alias, trailing slash or `..` survives), owner-checked, shadow-checked and pristine-checked; the plain env value is the only spelling of that directory nothing canonicalizes, and it is the door every leak in this family has come through. This deletes the spelling surface rather than filtering it. A `cell: full` start may still put those names in `environment.set` — that is an internal launch-path control, not a product door. Public `start_session` / `run_once` are refused; there is no `pmux probe` | L0 | `crates/service/src/claude_launch.rs::tests::a_minified_cell_may_not_reach_its_configuration_root_through_the_environment` — both names, a `..`-through-missing value, and the Full-cell control that the same name on a `cell: full` start still reaches the child. Rule at `claude_launch.rs::validate_config_isolation`, stated on the CELL and before the isolation-conflict loop so it does not inherit its force from it. Nothing in the tree combines `cell: minified` with either name in `set` | COVERED |
| S-41 | PATH B IS REACHABLE WITHOUT A FLAG. `compatibility::PROMOTED_PROFILES` ships one cell -- Claude Code 2.1.220 THROUGH 2.1.227 / macos / aarch64 / transparent / sdk -- so `require_tested_for_minified_cell` admits a mint on a supported host with no `--tested-claude-profile` on `pmuxd` argv. Linux is operator `--tested-claude-profile` only (measured 2.1.236), not promoted. `resolve` searches the OPERATOR's cells first, so an operator profile whose range contains the same version overrides the promoted one rather than colliding with it; promotion widens the door by a BOUNDED range with two closed ends, and one patch past the tested ceiling, one patch below the measured floor, and any other `major.minor` line are all still refused. Two cells whose ranges overlap are refused at boot as ambiguous. The promoted `transcript_drain_ms` is MEASURED and POOLED over every version measured -- max reachable post-answer arrival 438 ms over 226 arrivals in 425 transcripts spanning 2.1.207/2.1.215/2.1.220/2.1.223, x2.0 rounded up to a 250 ms step = 1000 ms (the shipped `PROMOTED_PROFILES` constant), receipt at `evidence/pooled-transcript-drain-macos-aarch64.json`, regenerated by `tools/promotion/measure_transcript_drain.py` -- and unit tests bind the shipped constant to the receipt's own recommendation and re-derive it from the receipt's own margin and step, so the two cannot drift and nobody can re-fit it per version by accident | L0/L3 | `crates/service/src/compatibility.rs::{every_promoted_profile_passes_the_admission_an_operator_profile_must,a_promoted_cell_admits_this_platform_with_no_operator_profile,an_operator_profile_overrides_the_promoted_one_for_the_same_identity,every_promoted_drain_is_the_one_its_receipt_recommends,every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit,versions_are_ordered_numerically_and_unparseable_ones_are_refused,a_tested_range_may_never_span_a_major_or_minor_version,overlapping_ranges_are_refused_and_adjacent_ones_are_not}`; `bin/pmuxd/src/main.rs::an_operator_may_state_a_version_range_and_overlapping_ones_are_refused`; end-to-end on real Claude with the flag absent at `crates/e2e/tests/pool_concurrency.rs::a_promoted_profile_serves_a_real_turn_with_no_operator_flag` | COVERED |
| S-42 | A SIDECHAIN ROW THAT MOVED NO TOKENS STILL REFUSES. `TranscriptAnalysis::sidechain_rows` counts every row of any kind this turn appended on a sidechain -- deliberately wider than `sidechain_indices`, which keeps only assistant rows with a uuid and a reconstructible parent -- and `TurnResult::sidechain_rows` carries it to `Pool::commit`. The `usage.sidechain` half of the guard cannot see a `Task` subagent whose rows report no usage, and that turn used to commit with its isolation claim unmade. `HostTurn::sidechain_rows` stays an `Option` so a host that cannot count can say so, and `None` is now a REFUSAL (`sidechain_rows_not_counted`, non-retryable) rather than `unwrap_or(0)` | L0/L2 | `crates/claude/tests/transcript_engine.rs::{a_sidechain_row_with_no_usage_is_counted_even_though_it_moves_no_tokens,a_turn_with_no_sidechain_counts_no_sidechain_rows}`; `crates/service/tests/path_b_pool.rs::{a_host_that_did_not_count_sidechain_rows_refuses_rather_than_reading_zero,sidechain_tokens_alone_refuse_the_turn_even_with_no_row_count,a_sidechain_row_on_a_toolless_cell_refuses_rather_than_undercounting}` | COVERED |
| S-43 | THE LAUNCH BROKER LAYER EXCHANGES A FRAME. `LaunchBroker::probe` connects to the broker's own endpoint, writes a launcher frame at `LAUNCHER_PROTOCOL_VERSION + 1` and reads back the `unsupported_version` refusal -- exercising accept, framing, length prefix and dispatch. The version mismatch is answered BEFORE the pending map is touched, so the probe cannot consume a one-use capability; the token lookup is the one step not on this path and the layer's detail string says so. The layer folds the probe with the accept-loop liveness read and both disagreement arms are faults | L0/L2 | `crates/service/src/launch_broker.rs::{a_probe_exchanges_a_real_frame_without_spending_a_pending_capability,a_probe_against_a_stopped_broker_is_refused_rather_than_answered,the_probe_version_can_never_be_the_live_one}`; layer arms in `native.rs::each_layer_reports_not_established_rather_than_health_when_it_proved_nothing` | COVERED |
| S-44 | THE MCP FRONT END IS DRIVEN AGAINST A LIVE DAEMON. `run_stateless` was covered by a blackbox test against a scripted native server and by a schema-drift test, in both of which the daemon is the test. The live test spawns the real `pmux-mcp` over real pipes against a real `pmuxd`, completes the MCP handshake, calls the tool, and joins the answer to the CHILD side (`prompts.jsonl` -> `launches.jsonl` on `cwd`) so a front end that fabricated a plausible answer without reaching a pool instance fails; an inadmissible class is asserted to survive as a typed MCP error | L3 | `crates/e2e/tests/pool_concurrency.rs::the_mcp_front_end_runs_a_stateless_turn_against_a_live_daemon` | COVERED |
| S-45 | WHAT `/clear` LEAVES IN THE MODEL'S CONTEXT IS CONSTANT AND IDENTIFIED. Measured on real Claude 2.1.220 sonnet/low, one pool instance, six sequential turns: the cleared context is 326 input tokens after one, two, three and five clears (cold: 171-194 across runs, so the invariant is the cleared context and not the step). The residue is exactly three messages the rotated transcript carries -- a 245-char `<local-command-caveat>` meta user row, a 130-char `<command-name>/clear</command-name>` user row and a 45-char `<local-command-stdout>` system row, 420 characters in total -- read off the instance's own transcripts, with at most one caller prompt per transcript. `input_tokens` ALONE is not the turn's input: a 2709-character prompt reported `input=2 cache_creation=1230`, which is why a long filler prompt appeared to leave the count unmoved | L3 | `crates/e2e/tests/pool_concurrency.rs::the_context_a_cleared_instance_carries_is_constant_across_clears` | COVERED |
| S-46 | ONE PHYSICAL DEADLINE GETS ONE ANSWER. `InputGateBudget::cap` is `min(gate maximum, remaining turn)`, so a fired `tokio::time::timeout` is either the turn ending (`TurnTimeout`) or one operation unproven inside the gate's own bound (an ambiguity), and `InputGateBudget::expiry` is the single place that asks which. The two reads asked; `paste_once` and `enter_once` did not, so the same event reached callers under two codes depending on nothing observable — and on the `/clear` path, where `DEFAULT_CLEAR_TIMEOUT_MS` and `INPUT_GATE_MAX_DURATION` are both 15,000 ms and the deadline is computed first, the remaining turn binds on EVERY clear. The deadline answer out of `enter_once` keeps `mark_enter_attempted`, because `clear_and_rebind` reads that one key to decide whether the bound transcript is suspect | L0 | `crates/service/src/driver_io.rs::tests::{a_turn_deadline_that_expires_inside_a_write_is_reported_as_the_deadline_it_is,a_gate_maximum_that_expires_inside_a_write_is_still_an_ambiguity,a_clear_whose_deadline_expires_inside_a_write_reaches_its_caller_as_a_turn_timeout}` — the second is the discriminator that forbids answering `TurnTimeout` unconditionally, the third is the caller-reaching proof on the path `await_turn_step` does not mask | COVERED |
| S-47 | A PRIVATE TERMINAL RETAINS NO RMUX HANDLE. rmux-sdk binds a handle to its `TransportClient` at construction and the poison latch is write-once, so a retained handle dies permanently on its first aborted request. Reads have minted per operation since layer (c); writes rode one `Pane` captured at `create`, so one aborted write left `paste`, `enter` and `interrupt` failing for the life of the terminal while the wire answered `DaemonLost retryable: true`. `write_pane`/`write_window` now mint per write, inside the spawned task and under the FIFO permit — outside it, a write abandoned on its first poll would take the `connect` with it and never be issued | L2/L5 | `crates/service/tests/private_runtime.rs::private_terminal_write_recovers_after_the_sdk_aborts_its_write_transport` (real `SIGSTOP`ped sidecar; proves the pane by `wait_visible_text` and by the fixture's `SIGINT` trap, not by the return code); the abandonment half is pinned by `private_abandoned_paste_reaches_the_pane_strictly_before_a_following_interrupt` | COVERED |
| S-48 | ADMISSION IS ONE ANSWER, WHATEVER DOOR THE START CAME THROUGH. Leaks 1, 2 and 3 were each the sentence *this path lacks the guard*, and each was found by reproducing one entry path after the guard had been written for another. The living start that reaches `admit_bound_resources` is the pool mint: `Request::RunStateless` through the pool's `config_isolation` root built by `stateless::launch_request_for` from a `MintSpec` no caller can touch. Public `Request::StartSession`, `Request::RunOnce`, and every agent Request are refused on the wire (`session_surface_removed`). `ADMISSION_ROUTES` must still classify every derived door — driven (`pool_start`) or carrying no start WITH THE REASON — so a new door tomorrow fails the test by name. Historical mutations that used to name `caller_start` / `run_once_start` / `agent_start` as driven routes are the reason the derivation exists; those product doors are gone. THE ROUTE LIST IS DERIVED: `derived_admission_routes` closes over `native.rs` from `admit_bound_resources` to find every door, reads protocol v1's own `Request` variant list to classify every wire method, scans the crate for every caller of an externally visible door and for every function that constructs a `StartSessionRequest` literal, and `ADMISSION_ROUTES` must classify each result as DRIVEN or as carrying no start WITH THE REASON — checked in both directions, so a route that appears tomorrow fails the test by name and a driver whose route was renamed fails too. The four lines of the funnel the test copies (`resolve_claude_launch` → `effective_config_root` → `require_isolation_root_is_the_effective_root` → `admit_bound_resources`) are pinned IN ORDER against `start_session_owned_with_retention`'s own source, so the differential cannot decay into a statement about a helper | L0 | `crates/service/src/native.rs::tests::every_entry_path_that_reaches_admission_answers_the_alias_family_identically`. Twenty-six comparisons: two identities (`New`, `Resume`, derived from the `SessionIdentity` variant list), two roles (configuration root, working directory), six spellings of the held directory — identity, trailing slash, `..` through a MISSING component, a terminal symlink, a path inside the live cell's own subtree, and on macOS the APFS firmlink alias — plus an unheld control per identity that every route must ADMIT with `SeedDisposition::Write`, so it cannot pass by refusing everything. Each spelling's premise is asserted before it is used. PROVEN OBSERVABLE by three product mutations, each restored byte-exact: `claim_reaches` reduced to `applicant == Minified` (LEAK 7's shape) made three routes ADMIT a configuration root inside a live cell's `projects/` while the pool's refused; deleting BOTH `..` guards (`effective_config_root`'s spelling rule and `require_establishable_identity`'s absence rule) made the same three admit the `..`-through-missing spelling; and rewriting `resolve_agent_start` to carry the caller's `set` instead of the agent's made the agent route the lone dissenter on the unheld control. Every one names the dissenting routes in the failure | COVERED |
| S-49 | THE DRAIN IS A POOLED BOUND AND THE VERSION KEY IS A RANGE, AND FIVE NAMED CONDITIONS RETRACT IT. `transcript_drain_ms` is the max reachable post-answer arrival POOLED over every version measured (438 ms over 226 arrivals in 425 transcripts spanning 2.1.207/2.1.215/2.1.220/2.1.223), x2.0, rounded up to a 250 ms step -- not a fit to one version, because a per-version fit measures noise (permutation p = 0.730) and is 87% low at n=1, which errs in the direction that TRUNCATES an answer. The Rust check re-derives the recommendation from the receipt's own `recommendation_basis.margin` and `.rounded_up_to_ms` so no constant is repeated across the language boundary, and it refuses a receipt pooled over one version, a named version that contributed no arrival, a receipt where no per-version fit is strictly smaller than the pooled bound, and a `drain_provenance` that does not quote the receipt's own numbers. `--bound-ms` turns the reading into a check: exit 4 above the bound, exit 5 when there was NOTHING to check -- which is the failure mode that matters, since a brand-new Claude Code version has zero `cli` turns and used to read as exit 0. `RepromotionTrigger` makes the five retracting conditions values with a FILE and a SYMBOL each, checked by opening the file; two of the five are detected in Python, and a Rust-only binding would have silently stopped covering them. Trigger 4 is the load-bearing one: `AssertEmptyRefusal::is_a_version_drift_signal` classifies all fourteen `assert_empty` reasons exhaustively, and SEVEN of them -- not the one literal that used to be tested for -- halt the pool, because a preamble that is not the one pmux MEASURED is a fact about the installed Claude that every other instance is about to hit | L0 | `crates/service/src/compatibility.rs::{every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit,every_repromotion_trigger_is_in_all_exactly_once,every_repromotion_trigger_names_a_detector_that_exists,versions_are_ordered_numerically_and_unparseable_ones_are_refused,a_tested_range_may_never_span_a_major_or_minor_version,overlapping_ranges_are_refused_and_adjacent_ones_are_not}`; `crates/service/src/driver_io.rs::tests::{every_assert_empty_refusal_is_in_all_exactly_once_and_round_trips,a_preamble_that_moved_is_a_repromotion_trigger_and_a_leak_is_not,a_successor_carrying_metadata_that_is_not_preamble_is_refused}`; `crates/service/src/native.rs::tests::a_child_that_refused_a_launch_flag_is_named_as_a_repromotion_trigger`; `crates/service/tests/path_b_pool.rs::{every_preamble_mismatch_halts_the_whole_pool_and_not_only_a_mis_selected_command,a_refusal_about_one_instance_does_not_halt_the_pool}` | COVERED |
| S-50 | PATH B RETAINS ITS OWN DRAIN EVIDENCE, AND NONE OF THE CONTENT. 178 of the 186 reachable arrivals behind the shipped drain came out of pmux's own PAID campaign directories, and a new Claude Code version has no `cli` turns to re-analyse -- so every destroyed pool instance's transcripts are now mirrored before `erase_tree`, into `pool-evidence/` beside the socket, pruned to `evidence::RETAINED_ROW_FIELDS`. That set is DERIVED, not judged: it is exactly `measure_transcript_drain.py`'s `FIELDS_READ`, which the tool now PRUNES every row to on the way in, so the constant is load-bearing in the tool rather than decorative and nothing downstream can read a prompt even by accident. MEASURED: mirroring the 189 transcripts behind the 2.1.220 receipt produced 271,497 bytes and reproduced that receipt's `post_answer_arrivals`, `recommended_transcript_drain_ms`, `full_drain_binds_on` and 456-turn count IDENTICALLY. On by default, bounded at 64 MiB with oldest-first pruning by mtime, off with `--pool-no-evidence`, and the running daemon publishes the directory it is using | L0/L1 | `crates/service/src/pool/evidence.rs::tests::{the_retained_fields_are_the_ones_the_measurement_tool_reads,a_mirror_keeps_the_measurement_and_none_of_the_content,a_row_that_is_only_content_is_dropped_entirely,the_retained_tree_never_exceeds_its_budget,an_instance_with_no_transcript_leaves_no_directory_behind}`; `crates/service/tests/path_b_pool.rs::ordinary_path_b_traffic_retains_its_own_drain_evidence_and_no_content`; `bin/pmuxd/src/main.rs::an_operator_may_state_a_version_range_and_overlapping_ones_are_refused` | COVERED |
| S-51 | THE MINIFIED LAUNCH BUNDLE IS PUBLISHED AS DATA AND CHECKED AGAINST THE ARGV A MINT EMITS. Source files stated the flags a Path B cell launches with -- `v1/minified.rs`'s module doc and `measure_transcript_drain.py`'s `ROW_KINDS` premise -- and named `--strict-mcp-config` and `--safe-mode`, neither of which any launch path emitted. MEASURED at 2.1.226 from the child's own `--debug-file`: without `--strict-mcp-config` a pristine minified cell in an empty private root fetches the OPERATOR'S ACCOUNT connector list over HTTP (`[claudeai-mcp] Fetching from https://api.anthropic.com/v1/mcp_servers`, plus a 294-entry registry load, 6 MCP lines); with it, 2 lines and `MCP configs resolved in 0ms`, `state: ready` on both arms. The flag is now driver-owned argv for `cell: minified` and for no other cell (`claude_launch::MINIFIED_CELL_FLAGS`), and `--safe-mode` is still not passed -- nothing it closes is measured open and it moves the TUI rendering every Path B screen constant was calibrated without. The two remaining prose sites publish the bundle as an ordered list that a test parses and compares, element for element in argv order, against the argv `stateless::launch_request_for` produces through the same three steps `start_session_owned_with_retention` drives it through; a workspace scan refuses any other code-tree file the right to name one of the spellings | L0/L1 | `crates/service/src/stateless.rs::tests::the_documented_minified_launch_bundle_is_the_argv_a_mint_emits`; `crates/service/src/claude_launch.rs::tests::a_minified_cell_is_launched_with_out_of_root_mcp_configuration_suppressed` | COVERED |
| S-52 | STOPPING A STARTING DAEMON IS A GRACEFUL SHUTDOWN, AND A FAILED START OWNS WHAT IT MINTED. `shutdown_signal()` was an `async fn` passed in argument position to `serve_until`, so `signal(SignalKind::terminate())` ran only when that future was first polled -- AFTER `NativeService::start` had minted the whole declared warm set. MEASURED against real Claude 2.1.226, `--pool-warm claude-sonnet-5/low=3`, SIGTERM 2.6 s in: **exit 143**, three epoch trees left, a stale socket, and a daemon log holding ONE line -- the raw startup `writeln!` -- because the `tracing_appender::non_blocking` `WorkerGuard` never dropped. The handlers are now installed before anything is minted (exit 0, no tree, socket removed, `pmuxd protocol v1 listening` and `pmuxd stopped` both in the log), and the signal is OBSERVED rather than acted on early: a mid-mint SIGTERM finishes the declared warm set first, MEASURED at 4,560 ms, because main holds no service handle to abandon safely. The recovery chain was the half that was uncharacterised and it was EXPONENTIAL: a failed start erased the one tree it collided with and abandoned every tree it had minted before it, so `L -> (L \ {min L}) union {0..min L - 1}` -- every transition observed -- and three abandoned trees took **seven** refusing restarts, `2^w - 1`. `NativeService::start` now drains on a `start_pool` failure, making it one refusing restart per leftover tree (3 planted -> 3, MEASURED), and the refusal names the situation, the rule and what it did about the tree rather than the rule alone | L0/L3 | `crates/e2e/tests/pool_concurrency.rs::a_sigterm_during_the_warm_mint_shuts_down_gracefully_and_leaves_no_tree`; `crates/service/tests/path_b_pool.rs::{a_refused_epoch_tree_is_erased_by_the_start_that_refused_it,a_partly_minted_warm_set_is_still_the_pools_to_drain}` | COVERED |

### 4.4 CLI command matrix

Each published command (`ping`, `run`, `doctor`) must cover its documented
output modes, stdout/stderr separation, runtime exit `1`, parser misuse exit
`2`, parsed local-semantic validation exit `1`, daemon unavailability,
malformed peer/input, and redaction where applicable. An unhealthy `doctor`
emits its typed report before exiting `1`. Session commands (`start`, `turn`,
`attach`, `probe`, `inspect`, `cancel`, `close`) are not a product and are
owned by the CLOSED rows below.

| ID | Command/commit contract | Current owner | Status |
| --- | --- | --- | --- |
| CLI-01 | `ping` text/json/ndjson | `bin/pmux/tests/process_boundary.rs::ping_covers_text_json_ndjson_and_exact_native_requests`; every-mode failure matrix in `cli_contract_matrix.rs`; exact release Gate D rerun pending | OPEN-L3 |
| CLI-02 | Removed. `pmux start` is not a product command. | — | CLOSED |
| CLI-03 | Removed. `pmux turn` is not a product command. | — | CLOSED |
| CLI-04 | `run` is one stateless `(model, effort, prompt)` call | `process_boundary.rs` run/prompt cases; `cli.rs` run names no resource; exact release Gate D rerun pending | OPEN-L3 |
| CLI-05 | Removed. `pmux inspect` is not a product command. | — | CLOSED |
| CLI-06 | Removed. `pmux cancel` is not a product command. | — | CLOSED |
| CLI-07 | Removed. `pmux close` is not a product command. | — | CLOSED |
| CLI-08 | Removed. `pmux attach` is not a product command. | — | CLOSED |
| CLI-09 | `doctor` is turn-free and reports socket/protocol/path/executable health | `process_boundary.rs::doctor_is_turn_free_and_reports_healthy_and_unhealthy_boundaries`; exact release Gate D rerun pending | OPEN-L3 |
| CLI-10 | Removed. `pmux probe` is not a product command. | — | CLOSED |
| CLI-11 | Prompt source/stdin/file, LF normalization, UTF-8/control/size/source conflicts | `process_boundary.rs::{prompt_sources_normalize_and_accept_the_exact_byte_limit,invalid_prompts_and_source_conflicts_fail_before_daemon_contact}`; `cli_contract_matrix.rs::{parser_misuse_is_exit_two_for_every_command,parsed_local_validation_is_exit_one_for_every_command}`; exact release Gate D rerun pending | OPEN-L3 |
| CLI-12 | Removed. `--profile` / stored-agent CLI is not a product. | — | CLOSED |
| CLI-13 | Removed. `--env-passthrough` was a session-start flag. | — | CLOSED |
| CLI-14 | Removed. `probe` environment audit is not a product. | — | CLOSED |
| AGT-01–AGT-17 | Removed. Stored launch-configuration agents are not a product. Protocol agent Requests stay and are refused on the public wire. | `docs/spec.md` §2; `NativeService::dispatch`; `session_surface_removed` | CLOSED |

### 4.5 Other shipped binaries

| ID | Surface | Required process-boundary contract | Tracked owner and current gap | Status |
| --- | --- | --- | --- | --- |
| BIN-01 | `pmuxd` | Actual raw UDS framing, socket/log modes, startup failures, signals, cleanup | `bin/pmuxd/tests/process_blackbox.rs`; exact release Gate D rerun pending | OPEN-L3 |
| BIN-02 | `pmux-mcp` | Initialize/list and exactly `run_stateless` over stdio; unpublished names are `unknown_tool`; structuredContent once; strict errors/frames/output | all five tests in `bin/pmux-mcp/tests/stdio_blackbox.rs`, including `real_stdio_bounds_oversized_output_and_recovers_on_one_stream`; exact release Gate D rerun pending | OPEN-L3 |
| BIN-03 | — | Removed. `claude-p` is not a product binary. | — | CLOSED |
| BIN-04 | `pmux-launcher` | Exact broker request then PID-replacing exec with cwd/argv/env; bounded redacted failures | `bin/pmux-launcher/tests/process_blackbox.rs`; exact release Gate D rerun pending | OPEN-L3 |
| BIN-05 | `pmux-hook` | Three lifecycle events; bounded stdin/stdio/status; relay privacy and semantic non-authority | `bin/pmux-hook/tests/process_blackbox.rs`; exact release Gate D rerun pending | OPEN-L3 |
| BIN-06 | `pmux-rmuxd` | Readiness/version/socket, complete-frame-before-EOF delivery, owner EOF/lease, child/descendant reap and residue | `bin/pmux-rmuxd/tests/process_blackbox.rs`, including `real_attach_half_close_delivers_the_final_complete_frame_exactly_once`; exact release Gate D rerun pending | OPEN-L3 |
| BIN-07 | Pool e2e | Exact release binaries + pool / Messages occupancy and process boundary | `crates/e2e/tests/pool_concurrency.rs`; `full_stack.rs` is compile-checked client-asset contracts only. Integrated exact release Gate D rerun pending | OPEN-L3 |

### 4.6 Native clients and MCP/facade conformance

| ID | Contract | Tracked owner | Status |
| --- | --- | --- | --- |
| CL-01 | Shared golden requests/results/events/errors/durable UUID vectors consumed by Rust/TS/Python. **Coverage is compared to `manifest.methods` and `manifest.events` BY NAME, in all three languages, and never to a literal.** The method half was fixed first and left the event half a hand-written `14` in the same file and the same commit; MEASURED, appending `"future_event"` to `manifest.events` left all eight Rust golden tests green, and neither client asserted event coverage at all -- both compared the corpus to itself. Both halves now redden in all three languages | `tests/conformance/v1/`; `crates/{protocol,client}/tests/v1_golden.rs` (`manifest_methods`, `manifest_events`); `clients/typescript/tests/golden-conformance.test.mjs::"golden.json carries one complete frame for every manifest event"`; `clients/python/tests/test_golden_conformance.py::test_golden_carries_one_complete_frame_for_every_manifest_event` | COVERED |
| CL-02 | Every Rust typed method sends exact request and requires matching result | `crates/client/tests/v1_golden.rs::every_typed_method_sends_exact_golden_requests_and_accepts_matching_results`; `crates/client/tests/fake_uds.rs::every_typed_method_rejects_contextually_mismatched_results` | COVERED |
| CL-03 | TS methods, AbortSignal, timeouts, reconnect/sequence/gap/malformed/additive fields | `clients/typescript/tests/{client.test.mjs,golden-conformance.test.mjs}` | COVERED |
| CL-04 | Python methods, timeouts, reconnect/sequence/gap/malformed/additive fields | `clients/python/tests/{test_client.py,test_golden_conformance.py}` | COVERED |
| CL-05 | Wrong session/generation/schema/sequence/next cursor/gap snapshot and u64 exhaustion fail closed | `tests/conformance/v1/cases.json::client_negative_matrix`; `crates/client/tests/v1_golden.rs::shared_negative_identity_schema_sequence_cursor_gap_and_exhaustion_matrix_fails_closed`; corresponding TS/Python golden-conformance negative-matrix tests | COVERED |
| CL-06 | MCP `tools/list` is `run_stateless` only; unpublished `tools/call` names are `unknown_tool` | `bin/pmux-mcp/src/tools.rs` unit tests and `bin/pmux-mcp/tests/stdio_blackbox.rs`; exact release Gate D rerun pending | OPEN-L3 |
| CL-07 | Removed. `claude-p` is not a product. | — | CLOSED |
| CL-08 | Removed. Stored-agent profile documents are not a product. | — | CLOSED |
| CL-09 | Removed. Stored-agent profile documents are not a product. | — | CLOSED |

### 4.7 Nonfunctional and platform matrix

| ID | Contract | Status |
| --- | --- | --- |
| NF-01 | Concurrent sessions are isolated; same-session admission conflicts serialize exactly (`concurrency_backpressure.rs::{actual_daemon_concurrent_private_ptys_never_cross_session_input_or_transcripts,thirty_two_session_actors_remain_isolated_under_concurrent_load}`); exact release Gate E rerun pending | OPEN-L5 |
| NF-02 | Slow/disconnected subscribers and exactly 64 public connections remain bounded and do not block unrelated sessions (`concurrency_backpressure.rs::{actual_daemon_slow_and_disconnected_event_subscribers_leave_one_of_64_slots_live,every_concurrent_long_poll_subscriber_wakes_for_one_actor_event,replay_byte_saturation_preserves_frame_paging_and_gap_exclusivity}`); exact release Gate E rerun pending | OPEN-L5 |
| NF-03 | Exact prompt/result/frame/event/history/transcript limits fail before unbounded allocation or mutation (`resource_bounds.rs::{exact_prompt_maximum_and_one_past_are_decided_before_actor_or_terminal_mutation,turn_history_byte_reservation_accepts_the_exact_boundary_and_rejects_one_past,actual_daemon_accepts_exact_native_frame_and_rejects_one_past_without_body_allocation}`, `v1_actor.rs::{oversized_exact_result_becomes_one_replayable_failure_without_reinjection,full_turn_history_rejects_new_ids_before_injection_and_keeps_old_idempotency}`, and transcript cursor/locator boundary tests); exact release Gate E rerun pending | OPEN-L5 |
| NF-04 | Repeated normal/cancel/attach/close/daemon-loss/sidecar-loss loops leave bounded descriptors, memory, files, sockets and processes (`crates/service/tests/{bounded_soak.rs,resource_bounds.rs,lifecycle_faults.rs}`); exact release Gate E rerun pending | OPEN-L5 |
| NF-05 | Transcript/protocol framing and replay scale linearly; size-scaling regression is deterministic (`crates/claude/tests/size_scaling.rs`, `actor.rs::replay_scaling_tests`, and `handler.rs::{native_framing_and_successful_decode_have_deterministic_linear_work,invalid_decode_recovery_is_bounded_to_one_additional_json_pass}`); exact release Gate E scaling run pending | OPEN-L5 |
| NF-06 | Diagnostic release-mode parser/replay/startup/cleanup throughput and latency are recorded without brittle host thresholds (`crates/service/tests/performance_diagnostics.rs`); exact release Gate E run pending | OPEN-L5 |
| PLAT-01 | macOS exact real-Claude Sonnet 5 low/medium compatibility cell on frozen source/binaries | EXTERNAL |
| PLAT-02 | Docker `linux/arm64` deterministic suite on identical source digest | EXTERNAL |
| PLAT-03 | Docker `linux/amd64` deterministic suite on identical source digest | EXTERNAL |
| PLAT-04 | `rmux_standard`, `attached_stream`, and read-only attach are stable typed rejections (`crates/service/src/compatibility.rs::reserved_terminal_cells_fail_with_stable_typed_rejections`; launch validation in `claude_launch.rs`); exact release Gate D rerun pending | OPEN-L3 |
| PLAT-05 | Native credentialed Linux Claude compatibility promotion | OUT-OF-SCOPE |
| PLAT-06 | Native Windows transport, PTY, process, and support claim | OUT-OF-SCOPE |
| PLAT-07 | General HTTP/TCP/network **control-plane** implementation or support claim. The opt-in loopback Messages facade (`--messages-bind`) is the token surface, not this row | OUT-OF-SCOPE |
| PLAT-08 | Generic arbitrary-terminal-program API or public raw PTY/session API | OUT-OF-SCOPE |
| PLAT-09 | Transparent automatic daemon-restart recovery or prompt reinjection | OUT-OF-SCOPE |
| PLAT-10 | Native incremental/streaming prompt-input protocol | OUT-OF-SCOPE |

## 5. Intentionally unsupported v1 behavior

Unsupported boundaries do not all have a request discriminant. They are
classified individually so Gate A cannot invent a typed rejection for a
nonexistent API, confuse a positive fail-closed invariant with a rejection, or
turn a future support non-goal into an implementation claim.

| Boundary | Classification and deterministic closure |
| --- | --- |
| HTTP/TCP/network access | `OUT-OF-SCOPE` (`PLAT-07`) for a general control plane. `S-19` proves the default daemon exposes only its exact owner-only UDS, clients neither discover nor autostart it, and Messages (if enabled) is loopback-only. |
| Daemon discovery/autostart | `OUT-OF-SCOPE`; no request exists to reject. `S-19` requires absolute-path/pre-connect and actual-process proof. |
| `rmux_standard`, `attached_stream`, read-only attach | Typed rejection contract owned by `PLAT-04`; it remains `OPEN-L3` until the exact request/process composition passes Gate D. |
| Non-default disconnect or heartbeat leases | Typed rejection contract owned by `P-08`; the shared vectors are covered, but the row remains `OPEN-L3` until the exact daemon composition proves the fields are not silently ignored. |
| Print/background/input-format/output-format/teammate Claude modes and arbitrary passthrough | Typed rejection contract owned by `S-01`; the source attestation is present, but the row remains `OPEN-L3` until the exact child-boundary run passes. |
| Streaming prompt input | Native incremental streaming is `OUT-OF-SCOPE` (`PLAT-10`). |
| Generic arbitrary terminal programs or a public raw PTY/session API | `OUT-OF-SCOPE` (`PLAT-08`). Deterministic fake executables are deliberately admitted for testing, so executable semantics cannot truthfully be rejected by start validation. |
| Automatic trust/login/permission/update/quota answers | A positive no-write/typed-needs-input invariant, not an unsupported request. `S-20` proves typed classification without automatic answer bytes. Path A real-PTY composition was deleted; living Trust recovery is remint. |
| Transparent restart recovery | `OUT-OF-SCOPE` (`PLAT-09`). `S-21` proves no public-session reconstruction or prompt reinjection. Living recovery is pool remint / Messages reprime, not UUID+generation resume. |
| Windows | `OUT-OF-SCOPE` (`PLAT-06`) with explicit claim-language review; there is no Unix request that can reject a Windows implementation. |
| Built-in broad Claude-version admission | A positive fail-closed compatibility invariant owned by `S-04`: the operator registry starts empty; launch is admitted only by the promoted macos/aarch64 2.1.220..=2.1.227 cell or an exact operator `--tested-claude-profile`. Linux remains operator (measured 2.1.236), not promoted. |

## 6. Residue and isolation contract

Every process/PTY/Docker test records exact owned PIDs plus start identity,
process group/session, socket inode, runtime path, builder, container, and image
before mutation. Cleanup may target only those identities. A pass requires:

- no owned live or zombie process/session member;
- no owned socket, runtime, temporary settings/MCP/system-prompt file, attach
  endpoint, broker endpoint, or test log outside the evidence directory;
- no exact Docker builder/container/image left by that cell;
- no generated interpreter or linter output inside the canonical source tree —
  `scripts/gate-a-residue.sh:119-131` fails on any `__pycache__`, `.ruff_cache`,
  `*.pyc`, or `*.pyo` outside `target/`, `.git/`, and
  `clients/typescript/node_modules`. Set `PYTHONDONTWRITEBYTECODE=1` and pass
  `--no-cache` to ruff, as the manifest commands above already do;
- no changed or killed unrelated user process or Docker object.

An acknowledgement from rmux/Docker is not cleanup proof. The exact observable
boundary is rechecked. Unconfirmed cleanup fails the test and preserves private
diagnostics for review.

## 7. Freeze and external promotion

Not living verification and not a Linux Claude pin. Living close: `tools/dev/check.sh`.
This block is leftover Gate C portability evidence.

Gate A closed only when every deterministic row was `COVERED`, tested
`REJECTED`, or explicitly reviewed `OUT-OF-SCOPE`, all freeze commands passed,
residue was empty, and independent review found no missing/false-positive row.
The canonical source and exact release binaries were then hashed together. Any
canonical source change invalidated the freeze and all later evidence.

Gate B uses the existing immutable live-attempt ledger and the previously
approved 60–100 short real-Claude window. Reservations occur before launch and
bind source, binaries, Claude/rmux versions, macOS/architecture,
terminal/input/lifecycle, and prompt identity. Gate C runs both Docker
architectures against the identical source digest and is portability evidence,
not a credentialed Linux Claude promotion.

After macOS evidence review, the leftover Gate C entry point (not a Linux Claude pin) was:

```text
# DELETED. linux-docker is gone. Do not run.
# tools/linux-docker/run.sh \
#   --source-sha256 "$FROZEN_SOURCE_SHA256" \
#   --base-image docker.io/library/rust:1.88.0-bookworm@sha256:MULTIARCH_DIGEST \
#   --acknowledge-docker \
#   --platform all
```

The supplied digest is the already reviewed freeze; the runner may not choose
or update it. `--base-image` is required and has no default: the runner exits
`2` with `base image must be docker.io/library/rust:1.88.0-bookworm at one exact
lowercase multiarch sha256 digest` when it is omitted. That multiarch digest is
deliberately recorded in no tracked file — the runner is given it, it does not
resolve one — so the operator supplies it; historical `tools/linux-docker/run.sh`
(now deleted) is the exact accepted form. Docker authorization is limited to the unique builders,
containers, and image tags reserved and identity-fenced by that invocation.

The final `.context/final-pmux-validation-report.md` records this file's exact
digest/revision, every command/result, source/binary/toolchain/environment
identity, artifact hashes, attempts, performance/resource observations,
limitations, residue audit, and independent final verdict.
