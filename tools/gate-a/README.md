# Gate A driver

`run_gate.py` runs the ordered cell manifest at `tools/gate-a-candidate/phase-manifest.json`
(70 cells: `gate_a` 28, `gate_b` 8, `gate_c` 4, `gate_d` 10, `gate_e` 10, `gate_f` 9, `residue` 1)
and writes one JSON receipt.

## What it does

- Substitutes `{workspace}`, `{release}`, `{validation}` and every tool placeholder the manifest
  uses (`{cargo}`, `{rustfmt}`, `{node}`, `{python}`, `{bash}`, `{shellcheck}`, `{cargo_fuzz}`,
  `{cargo_mutants}`, `{nightly_cargo}`, `{nightly_rustc}`, `{nightly_bin}`). Every cell is
  expanded **before the first cell runs**, so an unresolvable placeholder aborts with exit 2
  having executed nothing — and the refusal names **every** unresolved placeholder in the
  selected phases, not the first. **`gate_b` uses seven of them** — `{bash}`, `{cargo}`,
  `{cargo_fuzz}`, `{cargo_mutants}`, `{nightly_bin}`, `{nightly_cargo}` and `{nightly_rustc}` —
  and `phase_timeouts_seconds.gate_b` is 14400 s applied **per cell**, not per phase, so a
  one-placeholder-per-run refusal cost one gate attempt per missing `--tool`. (This said "five",
  which was neither the count it uses nor the count it must be told about.)
- Resolves the gate's **pinned** tools — `{cargo_fuzz}` and `{cargo_mutants}` — from under the
  workspace before looking on `PATH`. The path is **derived** by `workspace_tool_path`, which
  produces `.context/tools/<binary>/bin/<binary>`, over `workspace_tool_root`, which produces the
  `.context/tools/<binary>` that `docs/testing.md`'s `cargo install --root` is given — two
  spellings, one rule, the install root a prefix of the lookup by construction. Every reader in
  the tree computes it: this driver, `tools/gate-a-candidate/candidate_envelope.py`,
  `scripts/gate-a-fuzz.sh` and `scripts/gate-a-mutants.sh`. Both versions are asserted as gate
  cells (`cargo_fuzz_version` → `cargo-fuzz 0.13.2`, `cargo_mutants_version` →
  `cargo-mutants 27.1.0`) and refused again by each script, so preferring the workspace copy
  keeps every reader pointed at one binary. It used to be a literal in three places, one of
  which did not know it, and `--phase gate_b` was unrunnable — six cells, no receipt — on a host
  carrying the pinned binary in its workspace;
  `test_every_reader_of_the_workspace_tool_root_derives_the_same_path` now refuses any second
  spelling anywhere in the tree.
- Applies each cell's `env` **exactly as written**, merged over a small allowlist base
  (`PATH HOME TMPDIR USER LOGNAME SHELL LANG LC_ALL CARGO_HOME RUSTUP_HOME SSL_CERT_FILE`).
  Every manifest cell that invokes `{python}` carries `PYTHONDONTWRITEBYTECODE=1`; dropping
  per-cell env would create a source-tree bytecode residue that `scripts/gate-a-residue.sh`
  then fails on, so a test derives that set from the cells rather than pinning a count.
- Bounds each cell by the phase's `phase_timeouts_seconds`, applied as a **per-cell** ceiling (a
  phase-wide budget would let one cell consume all of it anyway), and cleans up with
  `SIGTERM`/`SIGKILL` on the child's own process group (`start_new_session=True`).
- Streams stdout/stderr: sha256 over every byte, a 4 KiB head and 4 KiB tail excerpt, and a total
  byte count. Nothing is buffered whole. Exceeding `max_command_output_bytes` stops the cell and
  records `output_limit`.
- Records `stdout_equals` and `stdout_sha256_line` as **assertions** (compared by digest, so no
  buffering): a mismatch marks the cell failed and the run continues.
- **Default is continue-on-failure.** Every cell runs, every outcome is recorded, failures are
  summarised at the end. `--stop-on-failure` opts out. Exit 0 = all passed, 1 = some cell failed,
  2 = the driver could not start (malformed manifest, unresolved placeholder, missing directory).
- Hashes the canonical source tree before and after the whole run and records both digests plus
  `source_unchanged`.
- Refuses, before the first cell, if the validation root or any of its `typescript-dist`, `fuzz`,
  `fuzz-evidence` children is not owner-private, or if any executable in the release directory
  ships no cargo depinfo beside it, **or if any release executable is older than a source cargo's
  own `<binary>.d` says it was built from**. The release directory is cargo's own `target/release`:
  an eight-file copy of it cannot satisfy `crates/e2e/tests/pool_concurrency.rs:237`, and a
  group-readable `typescript-dist` cannot satisfy `clients/typescript/tests/dist-stage.mjs`.
  All three used to surface as product-shaped cell failures minutes into the run.
  **No cell in the manifest builds `{release}`**, so the whole release lane is a function of an
  out-of-band `cargo build --locked --release --workspace` — and until the staleness check landed,
  nothing in the driver could tell you which commit it had been run at. Measured 2026-08-07: one
  stale `target/release` produced three red cells in two phases, and none of the three named a
  stale binary. `gate_d/mcp_process` said the server advertises nine tools where the source defines
  thirteen (reads as "the agent resource was never wired to MCP"); `gate_d/cli_process` said
  `unexpected argument '--agent-version'` (reads as "the F7 fix is wrong"); and
  `gate_a/release_full_stack_e2e` spent 388 s to reach `pool_concurrency.rs`'s own staleness guard,
  which is the only thing in the gate that could see the cause and which covers five binaries
  rather than all eight. The rule here is that guard hoisted verbatim — the dependency set is read
  from cargo, never guessed, and every stale binary is named by the one refusal.
- Emits one receipt: schema version, timestamps, host (os/arch/kernel), tool identities
  (rustc/cargo/node/python/ruff paths + versions), source digest before/after, the release
  directory with a sha256 per binary, and the per-cell array. Written mode 0600; the receipt is
  validated against the driver's own schema before it is written.

## What it deliberately does not claim

Under decision **D9** and the evidence threat-model boundary (`docs/testing.md:32-53`), this driver
makes **no claim against a malicious same-UID actor**: no substitution detection, no marker vnodes,
no descendant-escape supervision, no host-Git provenance. It is a recorder, not a fortress. It also
does not gate: it never decides that Gate A "passed" — it reports what happened and a human reads
the receipt.

The source digest is local (`pmux-gate-a-source-v1-…`), **not** the `pmux-source-v2-…` digest from
`tools/linux-docker/source_digest.py`. Importing that module drags in the host-Git apparatus that
decision **D6** de-scopes (and exec-loads `tools/evidence_common/bounded_process.py` at import
time), and advisory row 23 relocates that whole lane. The two digests are not comparable.

## Invocation

```sh
python3 tools/gate-a/run_gate.py \
  --manifest tools/gate-a-candidate/phase-manifest.json \
  --workspace "$PWD" \
  --release-dir "$PWD/target/release" \
  --validation-root "$PWD/../gate-a-validation" \
  --receipt "$PWD/../gate-a-receipt.json" \
  --phase gate_a
```

`--phase` repeats and may be omitted to run every phase in manifest order. `--tool NAME=PATH`
repeats and supplies or overrides a tool placeholder (`--tool nightly_cargo=…` and
`--tool nightly_rustc=…` are required by `gate_b`; `{nightly_bin}` is derived from the former).
`--cell-timeout-seconds` overrides the manifest timeout for every cell. `--continue-on-failure` is
the default and may be stated explicitly.

`gate_b`, run end to end on this host. **The `6/6 in 138 s` that stood here was a receipt for a
phase that had six cells**, printed two lines under this file's own statement that `gate_b` has
eight; the last real `gate_b` receipt spent 5,285 s on the mutation cell alone. The census above
is now checked against the manifest by
`tools/gate-a/tests/test_run_gate.py::test_the_driver_readme_publishes_the_cell_census_the_manifest_has`,
and no pass rate is published here without a receipt to name:

```sh
nightly_bin="$(dirname "$(rustup which --toolchain nightly-2026-03-26 cargo)")"
python3 tools/gate-a/run_gate.py \
  --manifest tools/gate-a-candidate/phase-manifest.json \
  --workspace "$PWD" --release-dir "$PWD/target/release" \
  --validation-root /private/tmp/gate-b/validation \
  --receipt /private/tmp/gate-b/receipt.json --phase gate_b \
  --tool nightly_cargo="$nightly_bin/cargo" --tool nightly_rustc="$nightly_bin/rustc"
```

## Running it against a commit instead of against your editor

This driver hashes the source before and after the run and reports
`source_unchanged`, so a Gate A run owns the tree for its whole ~35 minutes, and
`scripts/gate-a-mutants.sh` owns it for ~3 hours. `scripts/gate-in-worktree.sh`
moves the run somewhere else — `git worktree add --detach` at an explicit
commit, the gate there, the worktree removed afterwards — so editing continues
in the main tree meanwhile. Nothing about what any gate measures changes; only
where it runs.

```sh
bash scripts/gate-in-worktree.sh --commit HEAD --label gate-a --release-build -- \
  python3 {worktree}/tools/gate-a/run_gate.py \
    --manifest {worktree}/tools/gate-a-candidate/phase-manifest.json \
    --workspace {worktree} --release-dir {worktree}/target/release \
    --validation-root {validation} \
    --receipt {artefacts}/gate-a-receipt.json \
    --phase gate_a --phase gate_c --phase gate_d --phase gate_e --phase gate_f --phase residue
```

`{worktree}`, `{artefacts}`, `{validation}` and `{commit}` are substituted; an
unexpanded placeholder aborts before the worktree is used, the same rule this
driver applies to the manifest. `--release-build` runs
`cargo build --locked --release --workspace` inside the worktree first, and
`--prepare '<shell command>'` runs the other precondition the manifest declares
and no cell performs — `cd clients/typescript && npm ci`, which
`docs/testing.md` requires to exist already. A fresh checkout has neither, and
without the npm one four `gate_a` typescript cells are red about the checkout
rather than about the commit. The worktree gets its own `target/`, so two cargo
processes never queue behind one directory lock, which is the blocking the whole
thing exists to remove.

**Where you put the work root is not a free choice**, and the script refuses the
wrong ones before the checkout rather than 50 minutes into a run.
`tools/linux-docker/evidence.py` opens every absolute path component of the file
it reads with `O_NOFOLLOW` and refuses any component carrying setuid, setgid or
the sticky bit — so a worktree under `/tmp` (a symlink, to a directory at mode
1777) reddens `gate_f/candidate_envelope_self_tests` and
`gate_f/linux_docker_self_tests` with *"JSON evidence parent has unsupported
special mode bits"*. The default root is `$TMPDIR/gate-worktrees`, which
resolves to a private per-user directory with a clean chain. That rule also
settles the residue audit by subtraction: it scans one level under `/tmp` for a
leaked test root, and no worktree can be under `/tmp` at all.

**Its receipt names the commit it graded.** A receipt from this driver names
none, and this repository keeps finding the consequence: a 61/62 receipt from
one commit quoted as current seven commits later, a mutation score from
thirty-six commits back briefed as this tree's. `describes_commit`, `tree_sha`,
`describes_head` and a `reader_warning` sentence say which commit and whether it
was HEAD, and every artefact is hashed into it, so the Gate A receipt inside is
identified by content rather than by a path. `scripts/path-b-done.sh` reads
either shape for its criterion 4: a pinned receipt is accepted only for the
commit it names, and a bare one only against a clean tree at that commit whose
recomputed source digest equals the one it recorded.

**Where the receipt goes is not a free choice either**, and for a measured
reason: a certification passed `--receipt` an ephemeral path, both gates ran,
the receipt died with the work root, and criterion 4 then said
`cells_executed=0` over 62 cells that had really run. So there is no
`--receipt` above. It defaults to
`.context/gate-a/pinned-receipt-<label>-<commit>.json`, and a path that cannot
outlive its run is refused before the checkout: under the directory
`tempfile.gettempdir()` names — asked twice, with and without this
environment's `TMPDIR`, `TMP` and `TEMP`, so `/tmp` is refused even when the
shell points elsewhere — or under `--work-root`, whose checkout the script
removes. A receipt inside the repository must be at a path `git check-ignore`
accepts, because one that dirties the tree makes `scripts/path-b-done.sh`
refuse to reach a verdict at all. `--print-receipt-path` prints where a run
with these arguments would write and exits, which is how criterion 4 names the
file it wants without keeping a second copy of the convention.

**And the evidence goes with it.** The two logs and everything under
`{artefacts}` are copied to `<receipt>.evidence/`, each copy compared to its
original by digest, and the copies are what the receipt hashes — so the work
directory can be deleted once the run ends. If that copy fails the receipt
still lands, with `evidence_durable` false and `evidence_fault` naming the
failure, rather than hashing files nobody will be able to re-read.

Tests: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/gate-a/tests -v`.
Three modules, discovered together by the one `gate_f` cell:

- **`tests/test_run_gate.py`** — the driver itself. Every cell it executes is synthetic
  (`sys.executable`, `/bin/sh`); no test invokes a real gate cell and no test runs cargo.
- **`tests/test_documented_surface.py`** — `README.md` against the artefacts it describes. It runs
  real product binaries, but only `--help` and one `tools/list` exchange on stdin: no daemon, no
  socket, no Claude, no tokens. It needs a built workspace, which the release-directory
  precondition above already requires.
- **`tests/test_pinned_worktree.py`** — `scripts/gate-in-worktree.sh`'s durability predicates, each
  one watched refusing: a receipt under the temporary directory, under the platform default beneath
  a `TMPDIR` pointing elsewhere, under the work root, and at a path the repository would track. It
  builds a one-commit repository of its own under `target/` and adds a real worktree to it, because
  driving the runner against this tree would check commits out beside the one being edited. It lives
  here rather than under `scripts/tests`, which `crates/service/tests/register_currency_self_tests.rs`
  makes a `pseudomux-service` test target and therefore a per-mutant cost.

The derivations — the tests that ask a tree, a binary or a constant rather than restating it — are
the two list-based shell cells against every `.sh` in the tree; `typescript_tests` against every
`*.test.mjs` the package globs; `gate_f` against every `unittest discover` command
`docs/testing.md` §F names; the manifest's cell counts against the shape this file publishes; the
bug-class counter in every `crates/`/`bin/` Rust file against the last `THE BUG CLASS, instance …`
heading in `docs/current-state.md`; `evidence/README.md`'s absent budget figure against
`phase0.py budget`; and the whole of `test_documented_surface.py` — the README's command table
against `pmux --help`, its Path B flag list against `pmuxd serve --help`, its pool cap against
`MAX_POOL_SIZE`, its model table against `MODEL_TABLE`, its MCP tool list against the server's own
`tools/list`, and its quickstart daemon against the flags the Path B refusal names. The bug-class
counter lives here rather than beside the counter it guards because every `pseudomux-service` test
target runs once per mutant inside a tree `cargo-mutants` copies, and these tests never do.

The suite is 65 tests, ~15 s (MEASURED 2026-08-12, Python 3.13.0, warm `target/`; the gate's own
`{python}` was 3.12.4 at the last measurement). Unlike the cell counts above, nothing derives that
pair, so read it as a description and not as a pin — it has been wrong three times, saying "35
tests, ~8 s" over 38, "43 tests, ~42 s" over 45, and "51 tests, ~33 s" over 65.
