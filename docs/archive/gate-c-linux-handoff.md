# Gate C / Linux handoff

**Audience:** historical (2026-07). Living Linux verification is `tools/dev/`
(`check.sh`, `operator_eval.py`, `promote.py`). Do not run Gate A. Phase 0,
linux-docker, and package-smoke have been deleted; they are not a Claude pin.

**Status of this document:** leftover Gate C freeze notes, written 2026-07-28,
immediately after the macOS Gate A and Gate B work concluded, and re-verified
line by line on 2026-07-29 during the validated pre-push review round (branch
`N0xMare/plan-pmux-architecture`, remote `git@github.com:N0xMare/pseudomux.git`).
Run `git rev-parse HEAD` first and record it. Where a cited document contradicts
the tree, this file says which one is right.

One history note you need before touching any commit id: a pre-push reword of one
commit message renumbered **every commit id** from this file's introduction onward.
Trees are unchanged; only ids and messages moved. Ids you may meet in older
coordination notes map as: `5326287` → `e279c46` (the commit that introduced this
file), `df33615` → `34ceb41`, `2279ea9` → `b492dac`, `4ac84ab` → `9b7b959`,
`20df39f` → `a5218b2`. The old ids resolve nowhere in the pushed history.

**CITATION FRESHNESS, read this before chasing a line number.** Every citation below
was re-verified against the working tree of the 2026-07-29 review round (a descendant
of `a5218b2`). That round itself landed further docs and harness fixes, so line
numbers — especially into the files below, all changed at least once since this
document was first written — may shift again by the pushed tip:

```
  bin/pmux-hook/tests/process_blackbox.rs
  clients/python/pmux_client/client.py
  clients/python/pmux_client/protocol.py
  clients/python/tests/test_client.py
  clients/typescript/src/client.ts
  clients/typescript/src/protocol.ts
  clients/typescript/tests/client.test.mjs
  crates/client/src/lib.rs
  crates/client/tests/v1_golden.rs
  crates/e2e/tests/full_stack.rs
  crates/protocol/src/v1.rs
  crates/protocol/tests/v1_golden.rs
  crates/protocol/tests/v1_wire.rs
  crates/service/src/driver_io.rs
  crates/service/src/native.rs
  crates/service/src/v1/actor.rs
  crates/service/src/v1/backend.rs
  crates/service/tests/actor_model.rs
  crates/service/tests/deadline_idempotency.rs
  crates/service/tests/support/mod.rs
  crates/service/tests/transcript_filesystem_faults.rs
  crates/service/tests/v1_actor.rs
  docs/current-state.md
  docs/instrument-fix-plan.md
  docs/spec.md
  docs/testing.md
  evidence/README.md
  tools/evidence_common/bounded_process.py
  tools/evidence_common/tests/test_bounded_process.py
  tools/linux-docker/tests/test_docker_ownership.py
  tools/phase0/phase0.py
  tools/phase0/phase0_lib.py
  tools/phase0/tests/test_phase0.py
  tools/phase0/tests/test_verify_calibration.py
  tools/phase0/verify_calibration.py
```

Quoted strings are durable; line numbers are not. If a cited line does not say
what this document claims, `rg` the quoted text rather than assuming the claim
is wrong -- and if the quote is absent too, then the claim really is stale and
should be treated as such.


**What you are being handed:** a complete, unexecuted Linux/Docker portability harness
(`tools/linux-docker/`, 16,276 lines including tests — recount in §7), two hard blockers
in front of it (D6 and C6), and a finite budget of live-model attempts that cannot be
replenished. Gate C has never been built and never been run — not once, on any machine.

---

## 1. What is done, and what does not transfer

### 1.1 What pmux is, in one paragraph

pmux drives the real interactive Claude Code TUI inside an rmux 0.9.0 PTY sidecar and
treats Claude's own JSONL transcript as the sole semantic authority for turn completion.
`CompletionAuthority` (`crates/protocol/src/v1.rs:1379-1383`) is a single-variant enum —
`Transcript` — so "the screen became the authority" is unrepresentable in the wire
protocol. The screen is a liveness gate only. Completion is proved by a bounded drain:
`crates/service/src/v1/backend.rs:206-210` (the expression at `:209`),
`self.at_eof && !self.has_partial_line && self.stable_for_ms >= required_stable_ms`.
That is a bounded proof of *absence*, and it exists because Claude writes the transcript
incrementally with no end-of-stream marker.

(The drain rule has been cited in coordination notes as `crates/service/src/backend.rs:195`.
That file does not exist; `crates/service/src/` has no `backend.rs`. The path is
`crates/service/src/v1/backend.rs`.)

### 1.2 Gate A — DONE, macOS only

> **STALE COUNT, kept because the receipt it describes is real.** "Counted from the file" was
> true when it was written and is not now: the manifest is **83 cells** (`gate_a` 41, `gate_b`
> **8**, `gate_c` 4, `gate_d` 10, `gate_e` 10, `gate_f` 9, `residue` 1). `gate_b` grew by
> `cargo_mutants_version` and `mutation_score_agent_launch_pool_protocol`; see
> `docs/current-state.md` §7.1, whose verdict of record is **80/81**. The 75/75 receipt below
> attests the 75 cells that existed then and nothing since.

The 75-cell ordered manifest `tools/gate-a-candidate/phase-manifest.json` (`gate_a` 41,
`gate_b` 6, `gate_c` 4, `gate_d` 10, `gate_e` 10, `gate_f` 3, `residue` 1 — counted from
the file) executed end to end with **75 planned / 75 executed / 75 passed / 0 failed**,
repeatedly, on `macOS-15.7.7-arm64-arm-64bit`. The driver is `tools/gate-a/run_gate.py`
(533 lines, 26 self-tests in `tools/gate-a/tests/test_run_gate.py`, 629 lines).

Receipts as they exist on disk today (fields read from the JSON):

| File | Summary | `source_unchanged` | Window (UTC) |
|---|---|---|---|
| `.context/gate-a/receipt-20260728-final.json` — **the receipt of record** | 75/75/75/0 | `true` | 2026-07-29 02:48:10 → 03:01:07 |
| `.context/gate-a/receipt-20260727-agent-profiles.json` — superseded | 75/75/75/0 | `true` | 2026-07-27 19:42:00 → 19:52:48 |
| `.context/gate-a/receipt-20260727-env-allowlist.json` — superseded | 75/75/75/0 | `true` | 2026-07-27 22:44:26 → 23:00:38 |
| `.context/gate-a/receipt-20260727.json` — superseded, invalid for this tree | 75/75/75/0 | `true` | 2026-07-27 14:50:18 → 15:01:03 |

(The receipt of record is named with the operator's local date, 20260728; its UTC window
is on the 29th.)

**What the receipt of record attests, exactly.** Its sha256 is
`32c9ccc669ff3a33c29730962e07033ce5d545e15d597cbbf464f0eb8f2278eb`. It records
`source_digest_before == source_digest_after ==`
`47ab4fb474578fb64eb35f82ea11a7485b750cdb71174a620145b885dafbdc39`, which is the Gate A
source digest of the tree at commit **`b492dac`** — recomputed independently during the
review round (`git archive b492dac` into a clean directory, then `run_gate.py`'s own
`source_digest()`; exact match). It does **not** attest the pushed tip: the commits after
`b492dac` repaired documentation, harness and client-SDK defects found by the pre-push
review, and `docs/`, `tools/` and `clients/` are all inside the driver's
the source set derived from `.gitignore` (`run_gate.py:62-65`), so those edits move the digest too. Run
`git diff --stat b492dac..HEAD` to see exactly what changed after the attested tree, and
re-run Gate A yourself if you need an attestation of the tip.

**The receipts did not travel, and that is durable.** They live in `.context/gate-a/`,
and `.context/` is gitignored (`.gitignore:20`). If you cloned this tree onto the Linux
box, you do not have them. Do not treat their absence as evidence of anything; request
them or regenerate. (Earlier drafts of `docs/current-state.md` cited these receipts under
`evidence/gate-a/`, a directory that never existed; the review round repaired those
citations. If any note still says `evidence/gate-a/`, read it as `.context/gate-a/`.)
`evidence/` itself contains exactly **three** tracked files: `README.md`,
`model-attempt-ledger.ndjson`, and `gate-b-drain-calibration.json` — the third is the
Gate B receipt, §1.3.

### 1.3 Gate B — DONE, macOS only, live Claude 2.1.220

Nine grades, one live turn each, `effort=medium`, run in an isolated standalone clone.
All nine succeeded. `gap` is the late-arrival gap in ms against a configured
`transcript_drain_ms` of 2000 (observed drain ~2350 ms); `out` is assistant output tokens.

```
01 trivial            gap 0  out 4
02 poem no tool       gap 0  out 130
03 poem+hash          gap 0  out 525
04 poem+hash variant  gap 1  out 492
05 reverse transform  gap 0  out 2052
06 triple transform   gap 1  out 2385
07 long poem          gap 1  out 2182
08 unicode poem       gap 0  out 1048
09 long unicode       gap 0  out 1443
```

Output spans 4 → 2385 tokens. The gap never exceeds 1 ms.

**The Gate B receipt travels with your clone.** It is the tracked file
`evidence/gate-b-drain-calibration.json`, and `tools/phase0/verify_calibration.py`
verifies it offline — no Docker, no Claude, no network. The review round added a
`provenance` block to it (candidate binary hashes, `normalized_version: "2.1.220"`); the
per-attempt Claude-version binding remains the attempt ledger
(`normalized_version` on ordinals 30-43 of `evidence/model-attempt-ledger.ndjson`), and
the receipt's own provenance note says so.

Seven of the nine prompts request a SHA-256 of the poem the model just wrote
(`tools/phase0/prompts/03-*` through `09-*`; `01` and `02` do not). Seven hashes were
independently reproduced from the poem text pmux's own result captured; zero mismatches.
That is a checksum over the whole capture pipeline.

**What the two "unicode" grades did and did not test.** All nine files in
`tools/phase0/prompts/` are pure ASCII English instructions — verified by scanning the
directory for any byte outside `\x00-\x7F`; there are none. Grades 08 and 09 *ask Claude
to write* CJK and emoji. So the campaign exercised the non-ASCII **response** path —
transcript read, JSONL parse, block concatenation, hashing — and did **not** exercise the
non-ASCII **input** path: bracketed paste of non-ASCII text into the composer. Any
standing limitation about non-ASCII *prompts* remains undischarged.

**Honest limits, and these are load-bearing:** n=1 per grade; one machine; one Claude
version; `effort=high` is outside authorization (`APPROVED_EFFORTS = ("low", "medium")` at
`tools/phase0/phase0_lib.py:97`, enforced at `:1368-1369`). **This does not license cutting
`transcript_drain_ms`.** What it establishes is narrower and still valuable: response
structure does not *drive* late arrival.

### 1.4 What genuinely transfers to Linux

These are properties of the protocol and of Claude, not of the host:

- **The transcript semantics.** `crates/claude/src/engine.rs` is pure parsing over bytes.
  The `.concat()` at `engine.rs:843` joins final text blocks with an **empty** separator,
  not `"\n"` — remember this if you reimplement a hash oracle.
- **Prompt identity.** `engine.rs:126` compares the normalized typed prompt for equality
  against the armed prompt. Platform-independent.
- **`CompletionAuthority::Transcript`** and the whole v1 wire surface, including the 17
  `value_enums` pinned in `tests/conformance/v1/manifest.json` (count read from the file)
  with both-direction exhaustiveness assertions in Rust, TypeScript and Python.
- **The drain rule** as an algorithm. Its *calibration* does not transfer; see §6.
- **The hash-oracle result.** A hash Claude computed over text pmux captured is a statement
  about pipeline fidelity, and fidelity is not host-dependent.

### 1.5 What does NOT transfer, specifically

- **PID birth tokens.** `crates/rmux/src/process_boundary.rs` reads a birth token to fence
  PID reuse. macOS (`:490-517`) uses `proc_pidinfo(PROC_PIDTBSDINFO)` and gets
  `pbi_start_tvsec` + `pbi_start_tvusec` — **microsecond** resolution. Linux (`:521-533`)
  reads field 22 (`starttime`) of `/proc/<pid>/stat` and sets `fine: 0` — **clock-tick**
  resolution. The Linux path has never executed. Two processes born inside the same tick
  produce identical tokens, so the fence is coarser. It stays conservative (`is_recycled`
  at `:363-368` requires *both* tokens `Some` and *different*), but it is not the fence
  macOS was validated with.
- **The process table.** `process_boundary.rs:370-372` and `bin/pmux-rmuxd/src/main.rs:369-372`
  both shell out to `/bin/ps -axo pid=,ppid=` and parse with `split_whitespace`, rejecting
  any row with a third field (`process_boundary.rs:391-400`). That is BSD `ps` syntax. The
  Dockerfile installs `procps` (`tools/linux-docker/Dockerfile:42`), but this is the single
  most likely first-run Linux failure and it is cheap to check by hand.
- **The process boundary generally.** Session leadership, `setsid`, orphan reparenting to
  pid 1 vs. to a subreaper, and `SIGCHLD` disposition inheritance all differ.
  Debt row **C8** (`docs/current-state.md` §9.4) records a macOS
  `ECHILD` out of `/bin/ps` traced to an inherited signal disposition — **closed as an
  explicitly unsupported boundary, not repaired** (§5.6). Expect the Linux analogue to be
  different, not absent.
- **The PTY.** rmux 0.9.0 is vendored at `vendor/rmux-client` and `vendor/rmux-server`,
  pinned `=0.9.0` at `Cargo.toml:52-54`. PTY allocation, window-size propagation and the
  master/slave lifecycle are libc-level and differ.
- **Path and filesystem semantics.** macOS is case-insensitive by default and has different
  `st_ctime_ns` behavior; `TMPDIR` is per-user on macOS and `/tmp` on Linux; socket path
  length limits differ.
- **`docs/spec.md:33-35`** is the authoritative claim language: *"macOS and Linux are
  intended release targets and have Unix implementation paths; any production support claim
  belongs to a reviewed external matrix and the operator's explicit profile configuration."*
  Linux is an *intended* target, not a validated one. Gate C would change that sentence's
  evidential standing only for deterministic portability, never for credentialed
  native-Linux Claude support (`tools/linux-docker/README.md:10-14`).

### 1.6 What Gate C is, and what it is not

From `tools/linux-docker/README.md:10-14`: the runner builds one Linux image per requested
architecture, then executes **without network access, source/config mounts, provider
credentials, or a real `claude` executable**. A Docker result is *deterministic portability
evidence*, not credentialed native-Linux Claude support. `docs/testing.md` says the same
(`rg -n 'Gate C runs both Docker'`): Gate C runs both Docker architectures against the
identical source digest and is portability evidence, not a Linux Claude promotion.

So: **Gate C does not need the attempt ledger and does not spend attempts.** It is the one
gate you can iterate on freely. That is also why §5's ledger traps matter — you will be
operating in a repository where a *different* gate's budget is stored, and every way to
destroy it is accidental.

---

## 2. D6 — the host-Git de-scope

### 2.1 What depends on host Git today

`tools/linux-docker/source_digest.py` is 2,026 lines and does two unrelated jobs.

**Job 1, portable source identity (keep).** `workspace_source_manifest` (`:715`),
`workspace_source_guard` (`:722`), `workspace_source_digest` (`:733`) hash the declared
source context by canonical relative path, mode, size and content SHA-256. This needs no
Git at all. It is what the container runs: `Dockerfile:114-115` invokes
`python3 tools/linux-docker/source_digest.py /workspace --expected "$PMUX_EXPECTED_SOURCE_SHA256"`
after `COPY . /workspace` (`:84`), and `Dockerfile.dockerignore:35-36` excludes `.git` and
`**/.git`, so **there is no Git **repository** inside the container and never was.**

**Job 2, host Git provenance (remove).** `_git_command_argv` (`:740`) through the end of
`validate_workspace_revision_capture` (`:1780`, ending before `validate_expected_digest` at
`:1954`) — 1,214 lines — is a Git-receipt apparatus: bounded Git queries with retained
causal receipts, a Git-executable identity, split-index rejection
(`raise SourceIdentityError("Git split-index mode is unsupported for revision provenance")`
at `:1141`, repeated in the validator at `:1559`; tested at
`tools/linux-docker/tests/test_source_digest.py:789`), and `_repository_control_snapshot`
(`:1008-1163`). Note `_shared_index_identities` (`:974-1006`) *binds* split-index backing
files; it does not reject.

`_repository_control_snapshot` is the problem. It binds `git_dir`, `objects`, `refs`,
`HEAD`, the index, `commondir`, `config` and friends, each through `_control_node_identity`
(`:886`), which records `"ctime_ns": str(metadata.st_ctime_ns)` at **`:906`**.

`workspace_revision_capture` (`:1204`) re-snapshots at `:1309` and raises
`"Git repository control identity changed during capture"` (`:1311`) if anything differs,
then embeds the whole `repository_control` blob into the returned identity at `:1335`
(identity dict `:1318-1336`).

`docs/current-state.md` row 24 (§9.3) records the removal as
**−1,664** lines: 1,214 from `source_digest.py` plus ~450 test lines. The ~45 `run.sh`
lines the same row names (`run.sh:200-201` and the second capture at `:1089-1090`) are not
inside that total.

### 2.2 Why this is urgent — what already broke, what got fixed, and what remains

The host-Git apparatus is **not confined to the Gate C lane.**
`tools/phase0/phase0_lib.py` exec-loads `tools/linux-docker/source_digest.py` as an exact
hash-pinned authority, and `_candidate_source_digest_authority` (`:973`) *requires*
five names (`:990-996`):

```
"workspace_source_manifest",
"workspace_revision_capture",
"validate_workspace_revision_capture",
"validate_workspace_revision_identity",
"_revalidate_bounded_process_authority",
```

`observe_source_identity` (`:1002`) then builds the campaign's source identity with
`"revision_identity": revision_identity` at `:1073` — so the `ctime_ns` of `git_dir`,
`objects` and the workspace root is *inside the recorded source identity* of a live Claude
campaign.

Measured on 2026-07-28, before any fix, in a workspace something polled with `git` about
every 10 seconds:

- a **1.3 s** capture window failed ~**30%** of the time;
- an **11 s** campaign window failed **essentially always**;
- the failures landed **after reservation**, so each one consumed an irreplaceable ordinal;
- in a **standalone clone** (its own `.git`, nothing polling it): **zero** drift over 50 s.

**Half of that failure class has since been fixed, and half remains. Know which is which.**

*Fixed — "gate the claim, not the environment" (instrument fix, `docs/instrument-fix-plan.md`
§1.1; the trade is recorded honestly in its §4, point 6).* `_verify_candidate_unchanged`
(`phase0_lib.py:5302`) **no longer compares the whole identity dict**. It gates only
`SOURCE_IDENTITY_CLAIM_FIELDS` (`:1093-1099`: `digest`, `file_count`, `algorithm_sha256`,
`implementation`, `phase0_evidence_authorities`), records non-claim drift as an
observation, and raises `"frozen source changed during the campaign"` only on a claim
change. The comment at `:5221-5223` says why: the whole-dict comparison it replaces
"destroyed a paid, successful Claude turn". It is called at six sites — `:5008`; `:5298`
(`before_daemon_launch`); `:5571` (`attempt_N_before_reservation`, inside `_reserve` at
`:5655` and before `reserve_attempt` at `:5670`, so that one is free); **`:5905`
(`attempt_1_after_pmux_start`, under a live reservation)**; `:6011`
(`attempt_N_after_public_command`); `:6151`. A `ctime_ns` bump *between* observations no
longer fails a campaign.

*Not fixed — the D6 case.* Every one of those observations still runs the full host-Git
capture, `workspace_revision_capture`, **twice**, and the capture aborts on drift *during
its own window*: `"Git repository control identity changed during capture"`
(`source_digest.py:1309-1311`) and `"workspace revision changed across source capture"`
(`phase0_lib.py:1053`). That is the ~1.3 s window that failed ~30% of the time under
polling; it opens at all six verify sites, and at `:5812`/`:6011` an abort still spends
the ordinal. The same window is why `tools/phase0/tests/test_phase0.py` is non-hermetic
against the live `.git`: any concurrent git command — even `git status` — can kill a
capture mid-flight.

**Read that as a class of failure, not an incident.** A gate whose pass/fail depends on
whether an unrelated process ran `git status` in the wrong 1.3 seconds is not a gate. And
because two of the verify sites are *inside* the reserved window, its false negatives are
paid for in the one currency that cannot be earned back. (All four Gate A receipts on disk
record `source_unchanged: true`, so nothing in the receipt set is an instance of this; the
evidence is the phase0 measurements above.)

There is already a precedent in the tree, written by someone who hit this before.
`tools/gate-a/run_gate.py:42-44`:

> `# Local digest by choice: importing tools/linux-docker/source_digest.py drags in`
> `# the host-Git apparatus that decision D6 de-scopes (and exec-loads`
> `# bounded_process.py at import time), and advisory row 23 relocates that lane.`

The Gate A driver reimplemented a local content digest (`source_digest` at
`run_gate.py:264`, over the set `source_files` derives from `GITIGNORE` and `SOURCE_SKIP`,
`run_gate.py:62-65`)
rather than depend on it. Gate A is the gate that passes 75/75.

### 2.3 What D6 should become

Replace the 1,214-line apparatus with roughly 25 lines: `git rev-parse HEAD` and
`git status --porcelain`, recorded as provenance annotation, **not** as an identity that
anything compares for equality mid-run.

Concretely, of the fields in the current identity dict (`source_digest.py:1318-1336`), the
ones that survive on merit are `head`, `head_ref`, `branch`, `detached` and
`status_porcelain_v1_z_sha256`. The field that must go is `repository_control`. The
receipts, the bounded-process authority plumbing, the split-index rejection and the Git
query specs go with it.

**Do not do this as a local edit to `source_digest.py` alone.** `phase0_lib.py:1008-1014`
refuses to load a module missing any of those **five** names, and `_validate_source_identity`
(`phase0_lib.py:2287`) validates the persisted identity shape — including a call to
`source_digest.validate_workspace_revision_identity` at `:2231`. D6 is a two-file change
plus tests: `tools/linux-docker/source_digest.py` and `tools/phase0/phase0_lib.py`, with
`tools/linux-docker/tests/test_source_digest.py` (924 lines; 11 of its test methods name
`revision`) shrinking accordingly.

D6's completion criterion is an observation, not an argument: **run a source-identity
observation inside a workspace that something is actively polling with `git`, and have it
neither abort nor fail.**

---

## 3. C6 — the manifest re-projection

### 3.1 What it is

There are two manifests.

- `tools/gate-a-candidate/phase-manifest.json` — the ordered candidate manifest, **75
  cells**, the thing Gate A actually executed.
- `tools/linux-docker/gate-a-manifest.json` — 110 lines, `schema_version: 1`,
  `platforms: ["linux/arm64", "linux/amd64"]`, `gates`: a flat ordered list of **97**
  `{phase, name}` entries. Phase counts, tallied from the file: `P` 3, `A` 43, `B` 6, `C` 4,
  `D` 16, `E` 11, `F` 11, `Z` 3.

The Linux manifest is meant to be an exact ordered *projection* of the candidate manifest
plus the container-only cells. `tools/linux-docker/tests/test_runner.py:277`
(`test_linux_manifest_is_the_exact_ordered_candidate_projection`) asserts that relationship:
the candidate phase counts (`:284-295`), then a hardcoded 97-entry ordered `expected` list
(`:297`), then `len(observed) == 97` and uniqueness (`:350-351`).

### 3.2 The drift, exactly

`test_runner.py:286-293` asserts the candidate is:

```
gate_a 42, gate_b 6, gate_c 4, gate_d 11, gate_e 10, gate_f 8, residue 1   # = 82
```

The file on disk is:

```
gate_a 41, gate_b 6, gate_c 4, gate_d 10, gate_e 10, gate_f 3, residue 1   # = 75
```

That mismatch alone makes the test red, and it is red for the *right* reason — it detected
the drift. The candidate manifest was trimmed 82 → 75 on 2026-07-26 and the Linux manifest
was not re-projected; recorded as a **known regression introduced during the freeze and
deliberately not repaired** (`docs/current-state.md` row C6, §9.4).

Diffing the two manifests by cell name: **all 75 candidate cells are present in the Linux
manifest**; 22 Linux names are absent from the candidate. Of those 22, **15 are legitimately
container-only**:

```
system_identity, image_release_binary_identity, cross_uid_uds_report,
typescript_stage_identity_capture, release_candidate_binding, release_build,
release_repro_stage, release_repro_binary_equivalence,
typescript_stage_preconsume_unchanged, release_binary_unchanged,
typescript_stage_postconsume_unchanged, validation_output_cleanup,
container_source_after, container_source_stability, artifact_privacy
```

and **7 are the drift** — cells removed from the candidate that the container would still
run as a release gate:

```
evidence_common_tests, package_smoke_self_tests, phase0_evidence_tests,
candidate_envelope_tests, linux_runner_tests      # the 5 removed gate_f self-tests
typescript_package_artifact, python_package_artifact   # the 2 unsatisfiable cells
```

### 3.3 Why Gate C needs it

1. **The two `*_package_artifact` cells cannot pass.** `suite.sh:452-457` invokes
   `python3 tools/package-smoke/package_smoke.py {typescript,python}`, and
   `tools/package-smoke/package_smoke.py:1109-1113` unconditionally reads five
   `PMUX_PACKAGE_SMOKE_*` environment anchors that have **no producer anywhere in the
   repository** (`docs/current-state.md` row 38, §9.3). Those
   two cells fail 100% of the time under any driver. Ship them into the container and
   Gate C fails for a reason that has nothing to do with Linux.

2. **The five `gate_f` cells are harness self-tests, not release gates.** Running the
   harness's own unit tests as a portability gate conflates "the harness works" with "the
   product ports."

**On the surrounding red-count claim.** Older rows describe this lane as "12 → 13 red cells
(the other 12 are pre-existing `test_docker_ownership` host-Git failures)". The review
round established the real cause and repaired it: the 12 red
`tools/linux-docker/tests/test_docker_ownership.py` tests were a **fixture defect** — its
`setUp` created no Git repository at all, so every method died in `source_digest.py`
before reaching the `run.sh` behaviour it guards. The fixture now builds a real repository
**with one commit** (a bare `git init` is not enough: `rev-parse HEAD` on a commitless
repo emits stderr, which the bounded Git query treats as fatal), and the destructive-safety
behaviour those tests guard was proven correct against the repaired fixture. They were
never "host-Git failures". Still: re-derive the red set by running the suite on your
machine before you use any red count as an acceptance criterion — `test_runner.py:277`
stays red until C6 is done, and that one is red on purpose.

### 3.4 Order matters

Do **D6 first, C6 second.** `docs/current-state.md` row C6 states it: *"under D6
`source_digest.py` is about to lose ~1,664 lines, which rewrites this manifest's inputs
anyway."* Re-project first and D6 invalidates the projection; you regenerate the 97-entry
`expected` list twice.

C6 done properly: regenerate `tools/linux-docker/gate-a-manifest.json` *from*
`tools/gate-a-candidate/phase-manifest.json` mechanically, regenerate the `expected` list
(`test_runner.py:297`) from the same source, and update the phase-count assertion
(`:284-295`). If you find yourself hand-editing 97 tuples, stop and write the generator —
hand maintenance is exactly what produced C6.

---

## 3b. Two setup preconditions Gate A does not state, and each cost a run

Both were found the hard way on 2026-07-28, across four captures of the same
tree. Neither is a product defect and neither has a helpful error message.

**Run `npm ci` in `clients/typescript` before Gate A.** `node_modules/` is
gitignored, so a fresh clone has no `tsc`. Four TypeScript cells fail, and
`release_full_stack_e2e` then fails too because `PMUX_E2E_TYPESCRIPT_DIST_DIR`
was never produced -- so a missing dev dependency reads as an end-to-end product
failure. Installing it does not disturb the source digest; `node_modules` is
excluded (884 files with and without it).

**`$VALIDATION_ROOT/typescript-dist` must EXIST and be EMPTY, and must be
re-emptied between runs.** It has three distinct failure modes and I hit all
three:

- **wrong mode** -> `root mode must be 0700`. `mkdir -p` uses your umask and a
  child does NOT inherit the validation root's mode, so chmod the dist
  directory itself, not just its parent. The error names neither the
  directory nor the expected owner, so it is hard to place.
- pre-populated -> `prepare requires an empty root`
- absent -> `ENOENT: no such file or directory, lstat '.../typescript-dist'`
- left over from a FAILED run -> `prepare requires an empty root` again, because
  `typescript_external_build` creates and populates that directory and will run
  even after `typescript_stage_prepare` has already failed. A failed capture
  therefore poisons the next one.

So between runs, all three properties together:

```sh
mkdir -p "$VALIDATION_ROOT/typescript-dist"
find "$VALIDATION_ROOT/typescript-dist" -mindepth 1 -delete
chmod 700 "$VALIDATION_ROOT/typescript-dist"
```

I hit all four of these modes across six captures of the same tree. Every one
cost a full run, and none of the messages says which directory it means.

**And the rule that matters more than either:** Gate A hashes the whole source
tree, so it must run ALONE, on a frozen tree, with nothing else in flight -- no
concurrent edits, no formatter, no agent, and no `ruff`/`cargo` invocation of
your own. Four captures in this project were invalidated by a concurrent writer,
twice by the operator's own verification commands. A capture whose inputs moved
underneath it proves nothing about the tree it claims to describe, which is what
`source_unchanged: false` in a receipt means.

## 4. Starting sequence — the first six things, in order

### Step 0 — Get out of a polled workspace, and know which tree you are on

Clone the repository to a plain directory with its own `.git` and nothing watching it. Not
a Conductor workspace, not a worktree of one, not anything a file-sync daemon or IDE indexer
touches. This is not optional and §2.2 is why.

```bash
git clone git@github.com:N0xMare/pseudomux.git
git -C pseudomux checkout N0xMare/plan-pmux-architecture
git -C pseudomux rev-parse HEAD          # record it
```

There is no hash constant to compare against here: the pushed tip includes the pre-push
review round and postdates this sentence. What you can check: `git log --oneline
a5218b2..HEAD` is that round's fix set, and `git diff --stat a5218b2..HEAD` names every
file it touched — treat each of those files as line-shifted relative to this document's
citations, and re-locate by quoted string.

**Working looks like:** `stat` the `.git` directory twice, 30 s apart, with nothing else
running, and get identical `ctime_ns` both times. And a recorded `HEAD` that matches the
remote branch tip.

---

### Step 1 — Establish that the tree builds and the deterministic suite is green on Linux

Nothing else in this document means anything if this fails.

```bash
rustup toolchain install 1.88.0            # rust-toolchain.toml pins channel = "1.88.0"
cargo build --locked --workspace --all-targets
cargo test --locked --workspace --all-targets --all-features
```

**Working looks like:** compilation succeeds and the test count is near the macOS figure —
**580 passed / 0 failed / 17 ignored** at the review round, against exactly that command
line. (Older notes carry a **544** figure; that was the 2026-07-27 count, retired in the
review round — the +36 was suite growth on macOS, not platform gating.) Some ignores may
differ by platform; a *large* divergence in passed-count means `cfg(target_os = ...)`
gating you have not accounted for — and the right comparison baseline is a macOS run of
the **same commit**, not a number in prose.

Re-derive the platform-conditional sites yourself with
`rg -n 'target_os' bin/ clients/ crates/ fuzz/ tests/` — there are 25 today. The ones that
compile on Linux are:

```
crates/rmux/src/process_boundary.rs:521, :787
crates/service/tests/process_support/mod.rs:286, :475
crates/e2e/src/bin/pmux-test-claude.rs:803
crates/e2e/tests/full_stack.rs:4043, :4065, :4518, :4598
```

The remainder are `target_os = "macos"` arms and `cfg(not(any(linux, macos)))` fallbacks,
neither of which compiles on Linux.

**If the process-boundary tests fail here, stop and read §6 before doing anything else.**
That is the falsification case, not a bug to grind on.

Sanity-check the `ps` assumption by hand while you are here:

```bash
/bin/ps -axo pid=,ppid= | head
```

**Working looks like:** exactly two whitespace-separated integer columns per row, no header,
no third field. A third field or a header line fails
`crates/rmux/src/process_boundary.rs:391-400` on every single observation.

---

### Step 2 — HISTORICAL. Install the Gate A tool set

**HISTORICAL.** Do not install a deleted Gate A tool set. Living toolchain is `tools/dev/check.sh`. The Debian package list below is history.

`TOOL_EXECUTABLES` (`tools/gate-a/run_gate.py:87-91`) requires `bash`, `cargo`, `cargo-fuzz`, `node`, `python`
(the interpreter running the driver), `rustfmt` and `shellcheck` to resolve; any of them can
be redirected with `--tool NAME=PATH` (`:508`). Cells additionally use `npm` for the
TypeScript client and `shasum`. The Dockerfile's Debian list is the closest thing to a
prerequisites manifest that exists: `ca-certificates, git, libdigest-sha-perl, nodejs, npm,
procps, python3, python3-venv, shellcheck, util-linux` (`tools/linux-docker/Dockerfile:36-46`),
plus `nightly-2026-03-26` and `cargo-fuzz 0.13.2` (`:78-81`) for the fuzz cells, and
`ruff==0.12.4` (`tools/linux-docker/python-requirements.txt`).

One more thing that will surprise you: the driver runs every cell with a sanitized
environment restricted to `CARGO_HOME HOME LANG LC_ALL LOGNAME PATH RUSTUP_HOME SHELL
SSL_CERT_FILE TMPDIR USER` plus a `PATH` default (`ENVIRONMENT_ALLOWLIST`, `run_gate.py:85-86`). Anything else you
export does not reach the cells.

**Working looks like:** each of those executables resolves on `PATH`, `cargo-fuzz --version`
reports 0.13.2 (the manifest cell `cargo_fuzz_version` asserts that string exactly),
`ruff --version` reports 0.12.4.

---

### Step 3 — Historical freeze: Gate A on Linux, before touching Docker (not living confirmation)

Do not paste. `run_gate.py` and `phase-manifest.json` are deleted. Linux pin is `tools/dev/operator_eval.py`.

The 2026-07 driver was platform-neutral by construction (`run_gate.py:42-44`, `:57-61`). Historical
invocation (will fail: those files are gone):

```text
# DELETED. Do not run.
# python3 tools/gate-a/run_gate.py --manifest tools/gate-a-candidate/phase-manifest.json ...
```

Historical notes (the driver is gone). Five arguments were `required=True` (`run_gate.py:917`). Three details that decided whether a 2026-07 freeze run worked on first contact:

- `--release-dir` must **already exist** — it is resolved with `strict=True` at `:448` and
  the driver never builds it. Cells bind the frozen binaries through it, e.g.
  `PMUX_TEST_BIN_DIR: "{release}"` on `gate_d/launcher_process`. The macOS receipt records
  **8 binaries, mode 0500**, in an owner-only directory outside the workspace
  (`docs/current-state.md`, the Gate A capture table's "Release candidate" row). Build
  them, put them there, make the directory owner-only.
- `--validation-root` is created if missing (`:449-450`). It is the external
  `CARGO_TARGET_DIR` root; the envelope contract is the `docs/testing.md` paragraph
  beginning "The envelope also supplies `PMUX_GATE_A_RELEASE_DIR`"
  (`umask 077`, `CARGO_TARGET_DIR=$PMUX_GATE_A_VALIDATION_ROOT/cargo-target/workspace`).
  No validation command may write to the workspace `target/`.
- The driver sets `os.umask(0o077)` itself (`:503`); do not fight it. That same
  `testing.md` paragraph requires it and `clients/typescript/tests/dist-stage.mjs`
  rejects 0644 output.

**Where to put the receipt.** Not in `.context/` — that is gitignored (`.gitignore:20`) and
is exactly why the macOS receipts did not travel. Write it outside the workspace, then, if
it should survive, copy it into `evidence/` and commit it: `evidence/` holds only
`README.md`, `model-attempt-ledger.ndjson` and `gate-b-drain-calibration.json` today, and
it **is** now hashed by the Gate A source digest: `source_files` (`run_gate.py:236-262`) takes
everything `.gitignore` does not exclude, so adding a file there moves the digest.

**Working looks like:** the driver prints per-cell progress and finishes with 75 executed,
0 failed, and `"source_unchanged": true` in the receipt.

**Why before Docker:** this separates "pmux does not work on Linux" from "the Docker envelope
does not work." A failure inside the container is roughly ten times harder to read. A native
Linux Gate A pass is also, by itself, a new and reportable result — the first non-macOS
execution of the 75-cell manifest that has ever happened.

**Expect this step to find things.** It has never been run. Record every failure with its
cell id before fixing anything.

---

### Step 4 — D6: remove the host-Git apparatus

Now, and only now, with a green native Linux baseline to regress against.

Files: `tools/linux-docker/source_digest.py`, `tools/phase0/phase0_lib.py`,
`tools/linux-docker/tests/test_source_digest.py`, `tools/linux-docker/run.sh` (`:200-201`,
`:1089-1090`), and whatever `tools/linux-docker/tests/test_docker_ownership.py` turns out to
need once `source_digest`'s interface changes.

Delete `_repository_control_snapshot` (`source_digest.py:1008-1163`) and everything that
exists only to serve it. Keep `head`, `head_ref`, `branch`, `detached`,
`status_porcelain_v1_z_sha256` as annotation. Update the required-interface tuple at
`phase0_lib.py:1008-1014` and `_validate_source_identity` (`:2287`, the
`validate_workspace_revision_identity` call at `:2231`) in the same change.

**Working looks like:**
- `tools/linux-docker/tests` and `tools/phase0/tests` are no worse than the red set you
  recorded in step 3, and every remaining red cell has a name you can explain;
- `source_digest.py` is down by roughly 1,200 lines;
- and the acceptance observation: **a source-identity capture that overlaps a concurrent
  git command no longer aborts.** Force it without spending an attempt by running
  `git status` in a loop against the workspace while calling `observe_source_identity`
  directly. (Post-instrument-fix, drift *between* captures is already survivable — §2.2 —
  so the thing D6 must kill is the abort *during* a capture.)

Always run Python here with `PYTHONDONTWRITEBYTECODE=1` and ruff with `--no-cache` (§5.6).

---

### Step 5 — C6: re-project the Linux manifest

Regenerate `tools/linux-docker/gate-a-manifest.json` from
`tools/gate-a-candidate/phase-manifest.json`, and regenerate the `expected` list and the
phase-count assertion inside `test_runner.py:277-351`. Drop the seven drifted cells named in
§3.2. Write the projection as a script, not by hand.

**Working looks like:** `test_linux_manifest_is_the_exact_ordered_candidate_projection`
passes, and the tooling-only gates are green:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/linux-docker/tests -v
python3 -m ruff check         --no-cache tools/evidence_common tools/package-smoke tools/linux-docker
python3 -m ruff format --check --no-cache tools/evidence_common tools/package-smoke tools/linux-docker
bash -n    tools/linux-docker/run.sh tools/linux-docker/inside.sh tools/linux-docker/suite.sh
shellcheck tools/linux-docker/run.sh tools/linux-docker/inside.sh tools/linux-docker/suite.sh
```

(`tools/linux-docker/README.md:211-229` is the full block, which also covers the `scripts/`
shell gates and the `tools/evidence_common` and `tools/package-smoke` suites. None of it
calls Docker or Claude.)

---

### Step 6 — Build and run Gate C

You need one input that **exists nowhere in this repository**: a reviewed multi-architecture
base-image index digest. `run.sh` refuses to choose or update it
(`tools/linux-docker/README.md:48-51`); it must be an index containing exact `arm64` and
`amd64` manifests, and the runner records the raw index and verifies both platform mappings.

```bash
SOURCE_SHA256="$(tools/linux-docker/source_digest.py "$PWD")"

tools/linux-docker/run.sh \
  --source-sha256 "$SOURCE_SHA256" \
  --base-image docker.io/library/rust:1.88.0-bookworm@sha256:<multiarch-index> \
  --acknowledge-docker \
  --platform arm64 \
  --output "$PWD/.context/linux-docker/manual-arm64"
```

That shape is `tools/linux-docker/README.md:39-46`. Start with **one** platform and an explicit empty output
directory; `--platform all` doubles the failure surface on a first run for no diagnostic
benefit, and on a single-architecture host it additionally needs binfmt_misc/QEMU emulation
registered and a buildx builder able to produce the foreign platform. Name that dependency in
your evidence if you never complete both architectures.

**A repaired documentation trap, for the record:** until the review round,
`docs/testing.md` published "the exact Gate C entry point" *without* `--base-image`, an
invocation that exits 2. It now includes the flag (`rg -n 'exact Gate C entry point'
docs/testing.md` and read the block under it). If the block you find lacks
`--base-image`, your tree predates the repair — `tools/linux-docker/README.md:48` is the authority either
way: both `--source-sha256` and a digest-qualified `--base-image` are mandatory.

**Working looks like, in stages:**
1. the image builds and `Dockerfile:114-115` confirms the frozen digest immediately after
   `COPY . /workspace` (`:84`) — a mismatch here means your host tree is not the tree you
   hashed;
2. `inside.sh:12-23` accepts the container: Linux kernel (`:12-15`), started as root
   (`:16-19`), well-formed 64-hex `PMUX_FROZEN_SOURCE_SHA256` (`:20-23`);
3. the container starts under the intended isolation — `--network none`, `--cap-drop ALL`
   with only `CHOWN`, `DAC_READ_SEARCH`, `KILL`, `SETGID`, `SETUID` re-added, and
   `--security-opt no-new-privileges` (`run.sh:990-998`);
4. the ordered suite runs and the host binding report requires identical host/container
   source manifests, an unchanged complete binary manifest, the requested architecture, a
   credential-free system report and a passing suite result (`tools/linux-docker/README.md:96-99`);
5. cleanup removes only the exact reserved builder/container/image identities
   (`tools/linux-docker/README.md:186-202`).

**Environment note carried forward:** on the macOS host, Gate C was additionally blocked by a
wedged Docker daemon — `com.docker.backend` running and the socket present, but `/_ping`
returning HTTP 000 and `/version` timing out (`docs/current-state.md`, roadmap row
"blocked — Docker API wedged"). Confirm `docker version` and `docker buildx ls` respond
*before* you invest in a run.

**Rootless Docker will not work as-is.** `run.sh:993-997` re-adds `CHOWN`,
`DAC_READ_SEARCH`, `KILL`, `SETGID` and `SETUID` for the root-supervised cross-uid probe, and
`permissions_probe.py` (650 lines) proves that one uid can reach the pmuxd socket and another
cannot. Plan for rootful Docker or expect to re-scope that probe — and if you re-scope it,
say so in the evidence rather than dropping it silently.

---

## 5. Traps — stated here so nobody has to rediscover them

### 5.1 The attempt ledger is irreplaceable, append-only, and capped

`evidence/model-attempt-ledger.ndjson`, mode 0600, NDJSON, `schema_version: 1`. It is the
global real-Claude attempt budget. The ceiling is `MAX_GLOBAL_ATTEMPT_CEILING = 100`
(`tools/phase0/phase0_lib.py:44`), enforced at reservation time (`reserve_attempt`, `:3239`;
validation at `:1378-1386`). It is a total across all campaigns, machines and runs. A restart,
a new runner, a failed call or a source invalidation never resets it.

How much is left is not written here, and the figure that used to be — "47 of 100 consumed,
53 remain" — was already wrong by 38 ordinals when it was read back. Ordinals 1-4 predate the
file and are attested by its first record; the four detached reservations of §5.3 are consumed
too. Ask the file, which counts all of that for you:

    python3 tools/phase0/phase0.py budget --ledger evidence/model-attempt-ledger.ndjson

**A reservation consumes its ordinal whether or not Claude ever produced a result.** That is
the rule the ledger exists to enforce.

Gate C does not spend attempts. You are unlikely to need this budget at all. Which is
precisely why the risk is accidental destruction, not deliberate overspend.

### 5.2 The two ordinal field spellings

Records **5-29** spell the field `"global_attempt"`. Ordinal **30 onwards** spells it
`"global_attempt_ordinal"`. A naive `grep '"global_attempt"'` stops at 29 and reads the budget
**dozens of attempts cheap**. Both spellings live in one tuple,
`phase0_lib.ORDINAL_SPELLINGS`, which is what `phase0.py budget` counts through.

`evidence/README.md` was stale on exactly this point once, and was repaired in the review
round. The rule survives the repair: the file itself is the authority (its own words:
*"the file is its own authority"* — no digest is pinned in prose because a fresh
reservation stales it instantly). **Recount from the ledger before you trust any prose
figure, including the ones in this document.** Both spellings, both ranges.

### 5.3 Reserve against the real ledger path, never a copy

On 2026-07-28 a driver script copied the ledger to a private path on every invocation, ran one
campaign against the copy, and never copied the result back. Four campaigns each restarted from
the same base and re-reserved **the same ordinal 31** in a discarded file, while the retry loop
compared record counts against the reset copy and concluded nothing had been spent. Four
campaigns ran where one was authorized.

All four are counted anyway. One was rejected at the agent-team guard; three each launched a
real Claude and stopped at `NeedsTrust`/`NeedsInput` because the working directory had never
been trusted; all four report `observed_tokens: 0`. They are not renumbered into the file —
forging hash-chained records to tidy the arithmetic would cost more integrity than four
ordinals are worth (`evidence/README.md`, the "Four detached reservations" section).

`tools/phase0/phase0.py` locates the ledger by explicit `--ledger` path, `required=True` at
`:115` (and again for the `audit` subcommand at `:63`). Point it at
`evidence/model-attempt-ledger.ndjson` or copy the result back. Never edit, reformat or
regenerate the file (`evidence/README.md`: "Do not edit, reformat, or regenerate it").

### 5.4 Validation cannot run inside a polled workspace

See §2.2 in full, including what changed: since the instrument fix,
`_verify_candidate_unchanged` (`phase0_lib.py:5302`) gates only the claim fields, so
`ctime_ns` drift *between* observations no longer fails a campaign. What survives is the
abort *during* a capture — `workspace_revision_capture` runs twice per observation and
raises on mid-capture drift (`source_digest.py:1309-1311`; `phase0_lib.py:1053`) — a
~1.3 s window per observation that failed ~30% of the time under ten-second polling, open
at all six verify sites including the two under a live reservation (`:5812`, `:6011`).
Use a standalone clone until D6 lands, and after D6 lands, prove it landed by re-running
in a polled one on purpose.

### 5.5 Live-turn traps (only relevant if you ever run Gate B on Linux)

- **A permission prompt burns the whole turn deadline.** A mid-turn permission prompt is
  classified correctly, but `completion_evidence` returns a **default** `TerminalEvidence`
  for any `NeedsInput` screen (`crates/service/src/driver_io.rs:838-849`, and again after the
  stability wait at `:807-815`) — `ready_prompt` and `quiet` both false. So the turn keeps
  polling until its deadline (`--turn-timeout-seconds`, default 300,
  `tools/phase0/phase0.py:217`) and only then fails, having already spent the ordinal.
  Observed live on 2026-07-28.
- **Folder trust is never auto-answered, and must stay that way.** `docs/spec.md:1109`
  classes `needs_trust` / `needs_login` / `needs_permission` / `needs_update` / `needs_input`
  as *Operator input*, caller action: *"Obtain explicit authorized human action outside the
  turn."* Whatever you do about the bullet above, `NeedsTrust` keeps its current behaviour.
  Trust the working directory **before** the campaign starts. Three of the four detached
  reservations in §5.3 died exactly here.
- **`effort=high` is outside authorization.** `APPROVED_EFFORTS = ("low", "medium")`
  (`phase0_lib.py:97`), enforced at `:1368-1369`. Raising it is a scope decision, not a config
  change.
- **Grade by prompt content hash, never by position.** `prompt_suite_index` is assigned from
  CLI argv order (`phase0_lib.py:5676`, `prompt_suite_index=index + 1`), so a resumed campaign
  renumbers from 1 and every attempt silently acquires another prompt's label. That once
  produced a published by-grade table shifted by two grades, in which two prompts that had
  succeeded read `attempts=0`. `tools/phase0/verify_calibration.py` now grades by the
  reservation's prompt `sha256` and falls back to the index only with a note
  (`analyze_attempt` at `:444`; `grade_source` at `:489-517`). Any new tabulation must do
  the same.

### 5.6 Gate A must run alone, on a frozen tree

- **Alone.** The general rule first: a flaky gate command *fails the gate* — a command that
  is "unavailable, skipped without an applicable documented platform exclusion, flaky, or
  dependent on an untracked oracle fails the gate" (`docs/testing.md` §3, its opening
  paragraph). Two process-boundary tests were once carried as open flakes, **C8 and C9.
  Both are now dispositioned — do not reopen them as excuses, and do not let them excuse a
  new Linux failure:**
  - **C8** (`docs/current-state.md` row C8, §9.4) is
    `bin/pmux-rmuxd/tests/process_blackbox.rs:431::owner_eof_reaps_a_hup_term_ignoring_pane_tree_and_surfaces_lease_loss`.
    Its recorded failure was `could not run /bin/ps: No child processes` — an `ECHILD`
    (`crates/rmux/src/process_boundary.rs:374`, surfaced with its context at
    `bin/pmux-rmuxd/src/main.rs:268`), the signature of an inherited `SIGCHLD` disposition
    that auto-reaps children before the observation can be made. **That was never a timing
    bound.** **CLOSED 2026-07-28 by disposition 3: an explicitly unsupported boundary.**
    Nothing was repaired; the nonclaim is written out under the §9.4 table in
    `current-state.md` and registered in `docs/testing.md`. If you hit `ECHILD` there, you
    are outside the supported boundary, not looking at a defect — and on Linux, where the
    `SIGCHLD`/reaping rules differ anyway, treat any process-boundary surprise as §6
    material, not as "the known flake".
  - **C9** (`docs/current-state.md` row C9, §9.4) is
    `bin/pmux-hook/tests/process_blackbox.rs::stalled_relay_is_bounded_and_does_not_echo_private_input`
    (now defined at `:280`). It **was made deterministic — disposition 1, landed.** The
    old wall-clock *upper* bound (`elapsed < PROCESS_TIMEOUT`) is gone; `PROCESS_TIMEOUT`
    no longer exists anywhere in that file. Elapsed time is printed as an observation only
    (`:306-307`) and the surviving assertion is a **lower** bound (`:321-324`): load can
    delay an exit but cannot hurry one, so the lower direction is not load-sensitive and
    still catches a hook that reports a timeout it never waited out. A later review also
    fixed a pre-connect regression in this test that **hung** the gate command instead of
    failing it (commit `b492dac`). C9 is closed; a failure here now is a real finding.
  - **A third test is where the 2026-07-28 headroom measurement comes from, and it is
    neither C8 nor C9.** `bin/pmux-launcher/tests/process_blackbox.rs:414`
    (`socket_and_token_validation_fail_before_broker_use_and_are_bounded`) asserts
    `elapsed < PRE_BROKER_REFUSAL_BOUND` (2 s) at `:440`. A two-day flaky streak was
    traced to orphaned processes from an unrelated project saturating the machine; after
    killing them, **that** test passed 10/10. The "about 3.3 ms against its 2 s bound"
    this list used to report was the launcher's refusal, not the timed region: until
    2026-08-08 that region also spanned two sha256 passes over the candidate, 350 ms of
    the 2 s for the 4.3 MB debug build and 3/3 red under 60 bounded spinners. Hoisting
    them out (`timed_refusal`, `:406`) leaves 4 ms in the region and 3/3 green under the
    same load. The upper bound is still live; it is now load-sensitive only in the
    launcher's own runtime.
  - **Do not relax any bound to "fix" a flake, and check what the stopwatch spans before
    blaming the host.** The launcher's 2 s bound exists to prove validation returns
    *before any broker use* — the third case now points at a live socket nobody answers
    and the test asks the listener afterwards whether anything ever connected (`:449`) —
    and the broker socket read and write timeouts are 10 s
    (`bin/pmux-launcher/src/main.rs:48-49`). Widen it toward 10 s and the assertion stops
    asserting anything. If a bound trips, look at the machine — check load and orphaned
    processes before every gate run — and then look at whether the region between
    `Instant::now()` and `elapsed()` holds anything but the product.
- **On a frozen tree.** Gate A hashes the canonical source before and after; a change
  invalidates the run and every downstream claim. Do not edit while it runs. (§3b is the
  full set of setup preconditions, each learned the expensive way.)
- **No generated output in the source tree.** `scripts/gate-a-residue.sh:119-131` fails on any
  `__pycache__`, `.ruff_cache`, `*.pyc` or `*.pyo` outside `target/`, `.git/` and
  `clients/typescript/node_modules`. Always set `PYTHONDONTWRITEBYTECODE=1` and pass
  `--no-cache` to ruff — including for your own ad-hoc debugging runs. A previous Gate A
  attempt scored 73/75 with residue findings left by agents poking at the tree. **That is the
  gate working**, and it will catch you too.

### 5.7 Documentation traps, one repaired and one durable

- Repaired: `docs/testing.md`'s published Gate C entry point omitted the mandatory
  `--base-image` until the review round; it now carries the flag. If the block you find
  lacks it, your tree predates the repair (§4 step 6). `tools/linux-docker/README.md:48`
  is the authority either way.
- Durable: the Gate A receipts live in gitignored `.context/gate-a/` and **did not travel
  with the clone** (§1.2). Older notes citing them under `evidence/gate-a/` describe a
  directory that never existed; the review round repaired those citations in
  `docs/current-state.md`.

### 5.8 The stopping rule you are operating under

**D9** (`docs/current-state.md` §9.1, "D9 — design admissibility and freeze"). A change is
admissible only if it (a) fixes a defect reachable by a non-adversarial caller through the
public v1 surface or by an ordinary accident, (b) deletes with no observable v1 behavior
change, or (c) is a documentation edit that makes an existing sentence true. Everything
else is recorded, not implemented, in the format the rule names: `file:line · one-line
defect · one-line cost of leaving it · SAFE/NEEDS-CARE/RISKY` — appended to
`current-state.md` §9.3 (the numbered advisory rows) or §9.4 (the C-rows).
After freeze, only an **observation** reopens a decision; an argument is not an observation.

D6 is (a). C6 is (a). The review round's repair of the `testing.md` Gate C invocation
(§5.7) was a (c). Almost everything else you will be tempted to do on this lane is
neither; write it down and move on.

---

## 6. What would falsify the macOS conclusions — stop and report, do not push through

Gates A and B are evidence about **macOS and only macOS**. Gate C's job is to find out whether
they transfer. Some failures are ordinary porting work. Some are findings that invalidate a
conclusion, and those must be reported rather than worked around, because a workaround
silently converts "we do not know" into "it passed."

**Stop and report if:**

1. **The drain assumption breaks.** Gate B on macOS measured a late-arrival gap that never
   exceeded 1 ms against a 2000 ms configured drain. If a Linux run — even a deterministic one
   against the in-repo fake, `crates/e2e/src/bin/pmux-test-claude.rs` — shows late transcript
   arrival after `at_eof && !has_partial_line` has held for the required stable window
   (`crates/service/src/v1/backend.rs:206-210`), the drain's *calibration* does not transfer.
   Write-visibility semantics differ between APFS and ext4/overlayfs, and the container adds an
   overlay layer macOS never had. **Do not "fix" this by raising `transcript_drain_ms`.** A
   larger drain is a per-turn tax on every caller forever, and the macOS data licenses no change
   to that number in either direction. Report the measured gap distribution.

2. **The process-boundary guarantee behaves differently.** Specifically: PID-reuse fencing being
   weaker than assumed because the Linux birth token is clock-tick granular with `fine: 0`
   (`process_boundary.rs:521-533`) where macOS is microsecond-granular (`:490-517`); or
   `member_identity_still_proven` (`:436-450`) returning "proven" on unreadable tokens more often
   on Linux than on macOS. `docs/current-state.md` row C2 (§9.4)
   already records that an unreadable token is *permissive* for the signal decision, and says
   this was acceptable only because "v1 ships macOS-only and Gate C is Linux-only, so no
   supported platform is affected today."
   **The moment Gate C runs, that sentence stops being true.** If you see the permissive fallback
   taken on Linux, escalate; do not adjust the test. The conservative alternative — refuse to
   signal on an unreadable token — was deliberately not taken pre-freeze because it risks turning
   a working cleanup path into a permanent unconfirmed-close on any transient read failure.

3. **`/bin/ps -axo pid=,ppid=` parses differently.** Any header row, third column, or non-integer
   field fails `parse_process_record` (`process_boundary.rs:391-400`) on *every* observation,
   which makes the process boundary unobservable rather than wrong. If procps output does not
   match, report it — the fix (a portable process-table reader, or `/proc` directly on Linux) is
   a product change with its own review, not a test tweak.

4. **The release build is not reproducible across architectures in the way the suite requires.**
   `run.sh` stages a fresh reproduction build and requires it to match the frozen candidate's
   exact names, modes, sizes, hashes and bytes (`tools/linux-docker/README.md:171-174`). If Linux
   produces non-deterministic bytes for *unchanged* crates, the reproduction check is asserting
   something that is not true on this platform; report it and narrow the claim rather than
   loosening the comparison.

5. **The container cannot satisfy the isolation contract.** If `--network none`, the capability
   set at `run.sh:993-997`, the cross-uid separation, or the exact-identity cleanup cannot be
   achieved on the target Docker installation, the result is not the Gate C described in
   `tools/linux-docker/README.md`. Say which contract term was relaxed. `tools/linux-docker/README.md:200-202` is
   unambiguous: *"A missing, replaced, or unconfirmed object is a failed cleanup, not permission
   to remove something else."*

6. **Deterministic test counts diverge substantially from the macOS run of the same commit**
   (580 passed / 0 failed / 17 ignored at the review round; a retired 544 figure survives
   in older notes) in a way the `cfg(target_os)` sites in §4 step 1 do not explain.
   Silently-skipped tests are the failure mode that makes a green Linux run worthless.

**Conversely, do not stop for:** ordinary compile errors, missing Debian packages, path or
`TMPDIR` differences, Docker daemon configuration, or shellcheck findings. Those are the work.

---

## 7. Inventory — what exists in `tools/linux-docker/`

Counted at the review round with `git ls-files tools/linux-docker | xargs wc -l`:

| File | Lines | Role |
|---|---:|---|
| `evidence.py` | 6,009 | Evidence capture, binary/system/binding/cleanup records, atomic publication |
| `source_digest.py` | 2,026 | Portable source identity (keep) + host-Git provenance (**D6 removes 1,214**) |
| `run.sh` | 1,218 | Host-side orchestrator: build, resource ledger, container lifecycle, cleanup |
| `suite.sh` | 738 | The ordered in-container gate suite |
| `permissions_probe.py` | 650 | Root-supervised cross-uid socket/session/runtime probe |
| `README.md` | 234 | The contract this lane is judged against; read `:10-14`, `:39-51`, `:186-202` |
| `Dockerfile` | 213 | Networked build; digest check at `:114-115`; `procps` at `:42`; nightly-2026-03-26 and cargo-fuzz 0.13.2 at `:78-81` |
| `bounded_runner.py` | 180 | Shared bounded subprocess core |
| `inside.sh` | 171 | Container entrypoint; requires Linux kernel + root + frozen digest (`:12-23`) |
| `Dockerfile.dockerignore` | 117 | Build context exclusions (`.git` at `:35-36`) |
| `gate-a-manifest.json` | 110 | The Linux projection — **C6 re-projects this** |
| `python-requirements.txt` | 11 | Pinned Python tooling (ruff 0.12.4) |
| `tests/test_evidence.py` | 1,960 | |
| `tests/test_source_digest.py` | 924 | 11 revision-related tests that D6 shrinks |
| `tests/test_runner.py` | 850 | Includes `:277`, currently red — the C6 detector |
| `tests/test_docker_ownership.py` | 661 | 7 tests, Docker-driving; fixture repaired in the review round (§3.3) |
| `tests/test_bounded_runner.py` | 204 | |
| **total (17 files)** | **16,276** | |

Note `docs/current-state.md` row 23 (§9.3): this whole lane,
together with `tools/gate-a-candidate/`, is scheduled to move to `tools/_deferred/` — *"a
`git mv`, not a deletion; do it once the receipt exists."* The receipt exists. The
**18,744-line** size that row and older notes have carried **has never matched the tree**;
the real figure at the review round is **22,994** = 16,276 (`tools/linux-docker`, table
above) + 6,718
(`tools/gate-a-candidate`: `candidate_envelope.py` 4,279 + `phase-manifest.json` 1,377 +
`tests/test_candidate_envelope.py` 1,062). Recount with
`git ls-files tools/linux-docker tools/gate-a-candidate | xargs wc -l` before quoting any
figure. If someone does the move before you start, every path in this document shifts by
one directory level and nothing else changes.

---

## 8. If you read only one thing

Living Linux work is `tools/dev/check.sh` (tree) and `tools/dev/operator_eval.py`
(Claude pin). Do not run `tools/gate-a/run_gate.py` or `tools/linux-docker/run.sh`
to pin Claude or end a Linux session. The rest of this file is leftover C6
freeze notes. Get out of a polled workspace. Do not spend a live-model attempt
for any reason on this leftover Docker lane. And when something disagrees with
what macOS proved, that disagreement is the deliverable — write it down and
report it rather than making it go away.
