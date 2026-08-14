# When a survivor-register row stops being true

**Specified in sections 1-7, implemented in section 8, and held against the full campaign in section
9.** This document states the three rules that decide whether
`evidence/mutation-survivor-register.json` still describes the tree in front of it, the fallback for
the mutants that have no function, and the trigger that forces a full-scope run anyway. Every number
in sections 1-7 is measured, and each one names the run it came from. Sections 1-7 are as they were
written before any of it was built; section 8 is what was built, what it cost, and which of the
sentences above turned out to be wrong; section 9 is the acceptance test — the filtered answer
against the full one, mutant for mutant, at the commit where both exist.

---

## 1. What is being replaced, and what it costs

`scripts/path_b_done.py`'s `check_survivor_disposition` decides currency like this: read `FULL_GLOBS`
out of `scripts/gate-a-mutants.sh` (`mutation_globs`), `git diff --name-only` from the register's
`recorded_at.head` to the commit being judged over exactly those paths, and refuse if the list is
non-empty.

**It compares whole files.** A comment moved in `crates/service/src/driver_io.rs` declares all 144
rows stale and demands a re-run of the whole enumeration.

MEASURED, on the certification that just finished:

| | |
|---|---|
| full-scope run at `c94612d`, evidence `run.6k6C2J` | **11,765 s = 3 h 16 m 05 s**, 1,661 enumerated |
| what it was re-verifying (`git diff 23e81db..c94612d` over `FULL_GLOBS`) | **2 files**, 833 insertions, 226 deletions, **48 hunks** |
| rows the whole-file rule called stale | **112 of 144** (72 `driver_io.rs`, 40 `native.rs`) |
| rows whose own function was touched | **10** |
| rate at `PMUX_MUTANTS_JOBS=4` | **6.32 s/mutant** (`run.bbUDg3`, 10,443 s / 1,653) and **7.08 s/mutant** (`run.6k6C2J`, 11,765 s / 1,661) |

A filtered run is not a new idea here: `evidence/mutation-filtered-run-native-seam.json` is the
receipt for one, 37 mutants over fourteen names matched by one `-F` regex in `native.rs`, 4 m 34 s
wall.

---

## 2. What one row claims, and why currency is two predicates

A register row is keyed `(file, function, genre, replacement, occurrence)` — `KEY_FIELDS` in
`scripts/mutation_register.py`, and deliberately not `file:line:column`, because adding a test moves
every line below it and adding a test is how a survivor gets closed. A row carries a disposition:
`KILLED` and `REMOVED` assert the mutant is **not** a survivor at the recorded head, so `check`
refuses a run in which one of them survives again; `EQUIVALENT` and `ACCEPTED` assert it survived and
say why that is tolerable.

A `KILLED` row is therefore a claim about **two** pieces of the tree:

1. the code the mutant patches — if that changed, the mutant may no longer be the mutant, or may no
   longer be caught;
2. **the test that decided it** — if that changed, the row's `KILLED` may simply be false.

Today's check tests neither of those. It tests a whole file, which is coarser than (1) and blind to
(2) — the test lives outside `FULL_GLOBS`, so nothing in criterion 1 can see it move.

---

## 3. Rule 1 — the code the mutant patches

**A row is stale when a changed hunk between the register's head and the judged commit overlaps the
span of the item the mutant lives in.**

The spans come from the recorded campaign's own `outcomes.json`, which carries, per mutant, a
`function` object with a `span` covering the whole function and a `span` covering the mutated
expression. `scripts/mutation_register.py`'s `enumerated` already reads that file and already keys it
the way the register is keyed; the item span is one more field of the object it is already reading,
so this is a field lookup and not a parse of Rust.

### 3.1 The fallback for a mutant with no function, and it must be stated in the code

**MEASURED: 21 of the 1,661 mutants `run.6k6C2J` enumerated carry `"function": null`,** and every one
of them is the initializer of a module-level `const`. There are eight such sites:

| site | mutants |
|---|---|
| `crates/protocol/src/v1.rs:30` — `pub const MAX_NATIVE_FRAME_BYTES: usize = 8 * 1024 * 1024;` | 4 |
| `crates/protocol/src/v1.rs:192` — `const MIN_SAFE_JSON_INTEGER: i64` | 1 |
| `crates/service/src/driver_io.rs:39` — `const MAX_TRANSCRIPT_READ_BYTES: u64` | 4 |
| `crates/service/src/driver_io.rs:41` — `pub const MAX_PROMPT_BYTES: usize` | 2 |
| `crates/service/src/driver_io.rs:161` — `const COMMIT_LOOP_SAMPLING_PERIOD_MS: u64` | 2 |
| `crates/service/src/driver_io.rs:201` — `const MAX_ROTATION_ANCHOR_BYTES: usize` | 2 |
| `crates/service/src/driver_io.rs:224` — `const MAX_ASSERT_EMPTY_BYTES: u64` | 2 |
| `crates/service/src/pool/evidence.rs:96` — `pub const MAX_EVIDENCE_BYTES: u64` | 4 |

Seven of the 144 register rows are of this shape, all `ACCEPTED`, in `driver_io.rs` and
`pool/evidence.rs`.

**The fallback: when `function` is null, the item is the enclosing item, found by widening the
mutant's own span to the declaration that contains it.** All eight measured sites are single-line
`const` declarations, so the mutant's own span already equals the item's span — and the
implementation must widen anyway rather than encode that coincidence, because a `const` whose
initializer wraps over two lines would otherwise be checked against half of itself.

**The rule the fallback exists for is the shape of the refusal, not the arithmetic.** A row the
invalidator cannot place is a row it silently holds current. So: any row whose `(file, function)`
cannot be located in the enumeration — no function, and no span either — is **stale**, and any file
in `FULL_GLOBS` the enumeration does not mention at all escalates under rule 3. A check that
under-invalidates in silence is worse than the coarse one it replaces, because the coarse one is at
least wrong in the safe direction.

---

## 4. Rule 2 — the test that decided it

**A row is stale when the test that killed it changed.** This is the hole, not the granularity, and
it is the most valuable part of this work.

### 4.1 The hole, reproduced

Not argued — constructed, in a throwaway clone at `111a071`, and every step run.

The row: `crates/protocol/src/v1.rs` · `<impl Serialize for StartSessionRequest>::serialize` ·
`BinaryOperator` · `/`, which the register carries as **KILLED** and which `run.6k6C2J` reports as
`CaughtMutant`. The mutation turns `fields += 4 * usize::from(emit_policy)` into `4 /
usize::from(emit_policy)`, so it divides by zero exactly when a start names an agent — the branch the
comment above that arithmetic records as having been reached by no test in the workspace until
`an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies` was written in
`crates/protocol/tests/v1_wire.rs`.

1. **Control, mutant applied by hand, test present.** `cargo test --profile mutants -p
   pseudomux-protocol --test v1_wire` → `FAILED. 66 passed; 1 failed`, `panicked at
   crates/protocol/src/v1.rs:1818:19: attempt to divide by zero`, in exactly that test.
2. **Test deleted, mutant applied by hand.** `cargo test --profile mutants --no-fail-fast -p
   pseudomux-protocol -p pseudomux-client -p pseudomux-service` — the gate's own three test packages
   — → **34 test targets, 853 tests, 0 failures.** The mutant survives. The row's `KILLED` is false.
3. **Test deletion committed, nothing else touched.** `bash scripts/path-b-done.sh --only 1` →

       [1/5] No known unfixed defect in the Path B path -- MET
           survivor_register_head=c94612d
           survivor_register_entries=144
           survivor_register_scope=full
           survivor_register_files_drifted=0

   Criterion 1 is MET, at zero drift, over a register holding a row that is now false. The clone was
   deleted afterwards; nothing in this repository was modified to produce it.

### 4.2 The catching test is recoverable from `log_path`

Each outcome carries `log_path`, and the log's tail names the failing test and the target to re-run
it with:

    failures:
        run_once_uses_the_turn_window_instead_of_the_short_rpc_timeout

    test result: FAILED. 19 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

    error: test failed, to rerun pass `-p pseudomux-client --test fake_uds`

MEASURED over all 1,653 mutant logs preserved beside the full-scope run `run.bbUDg3`:

| | |
|---|---|
| caught + timeout outcomes | 1,086 (1,078 caught, 8 timeout) |
| logs naming at least one failing test | **1,076** |
| logs naming the failing target | **1,078** — every caught mutant, none of the 8 timeouts |
| distinct catching tests | 462 |
| the 2 caught mutants naming no test | both `bounded_soak`, whose `failures:` block was shredded by interleaved concurrent output; the target line survived intact |

So the mechanism is available at test-name granularity for 99.8% of caught mutants and at
test-target granularity for all of them, and the 8 timeout-decided mutants name neither and must
fail closed — a timeout-decided row is stale on a change to any test target.

**Recording the first-named catcher is sound for invalidation, and that is worth stating because it
looks unsound.** `cargo test` without `--no-fail-fast` stops at the first target that fails, so the
log names *a* catcher, not *every* catcher. Invalidating on it can only over-invalidate: if the
recorded test changes and some other test still kills the mutant, the re-run says so and the row
comes back `KILLED`. The direction that would be a defect — a row silently held current — needs the
recorded test to be unchanged, and if it is unchanged it still kills.

### 4.3 The blocker, and it is in the gate

`scripts/gate-a-mutants.sh` copies `outcomes.json`, `missed.txt`, `caught.txt`, `timeout.txt` and
`unviable.txt` out of the work directory and **not** `log/`. The work directory is a caller-supplied
scratch path — `PMUX_MUTANTS_WORK_DIR` — and the certification's is gone, so the per-mutant logs of
the register's own campaign, `run.6k6C2J`, no longer exist. The measurements in §4.2 are from
`run.bbUDg3`, whose logs happen to have been preserved by hand.

Sizes, measured: the log directory is **234 MB** (23 MB gzipped) — not a thing to commit. The
distillation is: **824 KB** for all 1,086 caught-or-timeout rows with every failing test name, and
about **15 KB** for the 96 rows the register calls closed (83 `KILLED` + 13 `REMOVED`), which is what
a rule at row granularity needs. **The gate must distil at run time**, while the logs still exist,
and write the catcher beside the outcome.

### 4.4 What rule 2 protects, and what it does not

At row granularity it protects the register's **96 closed rows**. It does not protect the *score*: 96
is one number over 1,156 decided mutants, and a test change can flip any of the 1,120 caught ones.
Protecting the score needs the same map at enumeration granularity — the 824 KB artifact — with
invalidation at test-target granularity, and a target's sources are derivable from cargo metadata
rather than from a table: a `--test <name>` target is that package's `tests/<name>.rs`, a `--lib`
target is that package's `src`, and a `--lib` test's module path names the file it is in, as in
`driver_io::tests::a_fence_that_is_not_the_proven_frame_never_sends_enter`. Stating the scope is the
point: rule 2 as specified closes the hole for the dispositions and leaves the score's test-currency
to the escalation trigger.

---

## 5. Rule 3 — what forces a full run

A function-scoped re-run is an approximation: changing function A can flip a mutant in B through a
callee, a type or a constant. The full run is sound and the filtered run is fast, so **the filtered
path is admissible only while none of the following holds**, and each of these is derived from the
tree rather than judged:

**(a) A changed hunk in a mutated file lies outside every enumerated item span and outside the file's
own `#[cfg(test)]` regions.** That is a declaration, an import, a type, an attribute or a new item,
and its effect is not bounded by one function.

**(b) The measurement's own frame moved:** `FULL_GLOBS`, `TEST_PACKAGES`, `MUTATION_PROFILE`, the
pinned cargo and rustc, or `REQUIRED_CARGO_MUTANTS_VERSION` — all declared in
`scripts/gate-a-mutants.sh`, and each of them already written into every run's `metadata.txt` as
`scope_glob=`, `test_package=`, `mutation_profile=`, `pinned_cargo=`, `pinned_rustc=` and
`cargo_mutants_version=`, so the comparison is against what the run recorded and not against a second
copy of the script's own literals. `GATE_EXCLUDES` is deliberately not in that list: it is not
recorded in `metadata.txt`, and a full-scope register is a statement about `FULL_GLOBS` in any case.

**(c) A file matching `FULL_GLOBS` exists that the recorded campaign never enumerated** — a new file,
or a rename. It has no rows, so no row can be stale, which is the failure mode.

**(d) A register row's `(file, function)` is absent from the new enumeration.** A renamed or deleted
function makes the row un-re-decidable; `check` already prints this as `register_out_of_scope`.

**(e) A change to a file the mutated crates compile that `FULL_GLOBS` does not cover** —
`crates/service/src/runtime.rs` and the eighteen other non-test files under `crates/service/src` the
globs do not name. A callee outside the scope can flip a mutant inside it, and nothing in the
enumeration can see that.

**(f) The arithmetic backstop, which needs no number:** filtered re-runs are admissible while their
accumulated mutant count since the last full-scope run is below the full enumeration's count. Past
that point the cheap path has already cost more than the sound one, so take the sound one. Both
counts are in the runs' own evidence.

**(g) The age backstop, tied to a number that already exists:** the register's full-scope recording
must be no older than `path_b_done.py`'s `--max-receipt-age-days`, the same window a Gate A receipt
is allowed to be stale for. Criteria 1 and 4 are statements about one commit; giving them two
freshness rules is how the two drift apart.

And the standing rule under all of them: **a filtered run is a tool result, not a gate cell.** That
is the sentence `evidence/mutation-filtered-run-native-seam.json` already carries in its
`argv_note`, and it stays true — a filtered run may keep the register current, and no percentage may
be read out of it.

---

## 6. What the rules would have said about the one window there is

`git diff -U0 23e81db..c94612d` over `FULL_GLOBS`: 48 hunks in 2 files. Each hunk classified by
overlap against the `c94612d` enumeration's item spans and against the two files' `#[cfg(test)]`
boundaries (`driver_io.rs` line 4261, `native.rs` line 4728):

| | hunks | rule |
|---|---|---|
| overlaps an enumerated function span | **26** | rule 1 — 17 distinct functions, **75 mutants**, ≈ **474 s (8 m)** at 6.32 s/mutant |
| inside the file's own `#[cfg(test)]` regions | **18** | rule 2 — a test change, invisible to the check that exists |
| overlaps neither | **4** | rule 3(a) — **escalate** |

The four are: eight new corpus-site `const`s in `driver_io.rs`; the documentation immediately above
`TerminalScreenState`, the enum whose `Unknown` arm was split so a frame matching no rule stops
reading as a caller's composer; a changed `use` in `native.rs`; and `NativeService`'s `runtime` field
retyped from a concrete `PrivateRuntime` to `Arc<dyn SessionRuntime>` — a change to dynamic dispatch
under every method of that type, none of which appear in the diff.

**Overlap is not a partition, and this classification is sizing, not a check.** One of the 26 —
`driver_io.rs`'s `+279,130` — adds two enum variants, a whole new enum and a whole new struct *and*
overlaps four enumerated method spans, so it is counted as function-attributed while carrying
declaration-level change. A sound implementation has to split each hunk at item boundaries before
asking which rule it falls under; counting by overlap understates the declaration-level content and
therefore understates how often rule 3 fires.

**So the honest answer for that window is that rule 3 fires and the full run was right**, and the
value of the rules there is that they say so instead of assuming either way. The 3-to-7-minute case
is the window where nothing declaration-level moved; the tier between — invalidating whole files that
carry a declaration change, rather than the whole enumeration — is **921 mutants ≈ 109 minutes** for
these two files, still short of 196.

---

## 7. What this did not establish, as of the design

*This section is the design's own list, kept as written. Section 8 says which items it closed.*

* **No behaviour changed.** No check reads this document. Every rule above is a specification and
  none of them is enforced by a test.
* **The catcher extraction is measured on `run.bbUDg3`, not on the register's own campaign,** whose
  logs the gate discarded (§4.3). Re-deriving it for `run.6k6C2J` requires re-running that campaign
  or teaching the gate to distil first.
* **A filtered re-run must not inherit a baseline the filter narrowed, and that was measured the
  hard way.** In the reproduction, `cargo mutants` ran its baseline over `pseudomux-protocol` alone
  while running each mutant over all three test packages: a 1.5 s baseline, a 20 s minimum timeout,
  and two mutants reported `Timeout` that a 300 s timeout reports `caught`. A filtered re-run has to
  pin `--timeout` from the full campaign's baseline.
* **A drifter can spend a filtered run's answer, and did.** At the 300 s timeout the same run
  reported the §4.1 mutant `CaughtMutant` — caught by `bounded_soak`'s residue assertion under
  four-way parallel load, with a failure naming a missing `rmux.sock` and no divide-by-zero anywhere.
  The full serial suite of §4.1 step 2 is what showed the mutant actually survives. This is the
  one-directional error the register's own `floor_derivation` names, observed in the act, and it is
  why a filtered re-run may not promote a row to `KILLED` on a single caught outcome whose only
  catcher is one of the four measured drifters.
* **Rule 3(e) is stated, not sized.** Nothing here measures how often a change outside `FULL_GLOBS`
  in the mutated crates would flip a mutant inside it; the rule escalates because it cannot tell,
  not because it has been shown to matter.
* **§4.4's score-level protection is unimplemented and its cost is estimated from one campaign's
  logs**, not from a written artifact.

---

## 8. What was built, what it cost, and where sections 1-7 were wrong

Sections 1-7 are the design, unedited. This section is the implementation, and it is written last so
that the two can be read against each other.

### 8.1 The pieces

| | |
|---|---|
| `scripts/register_currency.py` | the three rules and the seven escalations, in one module; `assess` returns the stale rows, the stale functions, the escalations and the remedy |
| `scripts/mutation_refilter.py` | the filtered run: takes `--stale` or `--function FILE::NAME`, writes a receipt naming the commit and the functions it graded |
| `scripts/mutation_register.py` | `catchers` distils what caught each mutant from the run's own logs; `record-catchers` writes it onto the rows; `census` writes the enumeration census |
| `scripts/gate-a-mutants.sh` | runs both of those at the end of every campaign, while `log/` still exists |
| `scripts/path_b_done.py` | criterion 1 reads the answer instead of comparing whole files |
| `evidence/mutation-enumeration.json` | every mutant the campaign enumerated, keyed as the register is keyed and counted rather than listed |
| `scripts/tests/` | one test per rule over a synthetic repository, every one of them broken on purpose and watched go red |

### 8.2 The two reproductions, run

**The hole of §4.1, now closed.** In a throwaway clone of this tree with the machinery applied,
deleting `an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies` from
`crates/protocol/tests/v1_wire.rs` and committing it turns criterion 1 **NOT MET**:

    survivor_register_files_drifted=0
    survivor_register_stale_rows=13
    survivor_register_stale_functions=1
    because: 13 survivor-register row(s) in 1 function(s) stopped describing this commit
    (crates/protocol/src/v1.rs::<impl Serialize for StartSessionRequest>::serialize).
    rule 2: ... is KILLED by an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies
    (-p pseudomux-protocol --test v1_wire), and crates/protocol/tests/v1_wire.rs changed and the
    test is no longer a function this can find in it. Re-decide exactly those, and nothing else,
    with: ...

That is the same commit, the same deletion and the same command that printed **MET** at zero drift
before this work. The drift count is still zero — no mutated file moved — which is the point: the
refusal comes from rule 2, which the old check had no way to ask.

**The cost of §1, now measured.** In the same clone, one word changed in one comment inside
`crates/service/src/driver_io.rs`'s `active_editor`:

| | old check | new check |
|---|---|---|
| rows called stale | **144** — the whole register | **3** — the rows of that one function |
| what it demands | the full-scope campaign, 3 h 16 m | a filtered run over one function, **35 mutants** |
| wall time of the check itself | under a second | **6.4 s**, enumeration included |
| wall time of the remedy | 11,765 s | **~4 minutes**, of which 51 s is the unmutated baseline |

The remedy was not described; it was run. `mutation_refilter.py --stale` derived the stale set from
the same code that printed the refusal, graded 35 mutants over that one function, and wrote a receipt
naming the commit, the functions, the `-F` expression, the timeout and what caught each mutant.

**One word and not one line, and the reason is a finding.** The first attempt at this reproduction
inserted a whole comment line, which shifted every line below it in an 11,410-line file and rotted
six `driver_io.rs:NNN` citations in the Path B documents. The filtered run then refused to grade
anything, because its unmutated baseline was red — twice — and named
`-p pseudomux-service --test path_b_doc_citations` both times. That is the guard working: a suite
that is already red reports every mutant caught.

**MEASURED, and it corrects a number in §3:** `cargo mutants --list --json` over `FULL_GLOBS` takes
**4.4 s**, not the 86 s this document estimated, and enumerates the same 1,661 mutants. The whole of
criterion 1 is 6.4 s on a commit that drifted and under a second on one that did not, because the
enumeration is skipped entirely when no mutated file moved.

### 8.3 The catchers, recorded

`evidence/mutation-filtered-run-killed-rows.json` is the receipt: 274 mutants over the 35 functions
carrying the register's 83 KILLED rows, **211 caught, 3 missed, 1 timeout, 59 unviable**, at a
`--timeout` of 270 s pinned from a measured 53.9 s unmutated baseline over all three test packages.

**Those are the re-derived numbers and not the first ones.** The first run of these same 274 recorded
239 caught, 0 missed and 35 unviable, and 28 of its outcomes were wrong for the reason section 9.5
gives; the run above is the same functions with the harness fixed. None of the three missed is a
`KILLED` row — two are `EQUIVALENT` and one `ACCEPTED`, all of them survivor rows swept in because
they share a function with a closed one — so no disposition in the register moved. The one timeout
has no row at all.

All 83 KILLED rows name a catcher. **70** are caught by `-p pseudomux-service --lib`, **10** by
`-p pseudomux-client --test fake_uds`, **2** by `-p pseudomux-client --test v1_golden`, **1** by
`-p pseudomux-protocol --test v1_wire`, and **none** is undetermined — so no row of this register
depends on a timeout, and none depends on one of the four measured drifter targets. One of the 212
decided mutants named no failing target at all and is recorded `undetermined`; it is not a register
row.

### 8.4 Where the design was wrong, and what replaced it

* **§4.3 said the distillation was needed for "the 96 rows the register calls closed". It is needed
  for 83.** REMOVED is the other closed disposition and it asserts something no test decided — that
  the mutant is no longer *enumerated*. Asking it to name a catcher would be asking it to name a
  test that never ran, so `validate` requires `caught_by` of KILLED rows only, and the 13 REMOVED
  rows are exempt by a stated rule rather than by an omission.
* **§3 said the spans come from the recorded campaign's own `outcomes.json`. That file no longer
  exists** (§7 says so, one bullet further down), so they come from `cargo mutants --list --json` at
  the commit being judged. The campaign's enumeration is instead recorded as a *census* —
  `evidence/mutation-enumeration.json` — which is what answers "has this mutant ever been
  enumerated before". This tree's census was derived at a commit whose `FULL_GLOBS` files are
  byte-identical to the register's head; the file says so in `recorded_at.head_note`.
* **§4.4 put rule 2 at test-target granularity. It is at the catching test's own span.** A `--lib`
  test lives in the same file as the code it tests — 751 of 1,078 caught mutants in one campaign
  were caught by one — so invalidating on the file would have put the whole-file rule back under
  another name. `test_span` reads the function's own lines out of the file, which is why **adding**
  a test costs nothing: the insertion lands beside a recorded catcher and not inside it, and adding
  a test cannot falsify a KILLED row in any case.
* **§5(e) escalates on any change to a mutated crate's `src` outside `FULL_GLOBS`, and that would
  have escalated on every edit to `crates/service/src/native/tests/seam.rs`** — a file that is
  nothing but tests. A file whose `mod` declaration sits inside a `#[cfg(test)]` region of its parent
  is carved out, and the carve-out is derived by finding that declaration rather than by naming the
  file.
* **§7 said a filtered run must pin `--timeout` from the full campaign's baseline. It measures its
  own**, over all three test packages, because the campaign that recorded this register left no
  baseline behind either. Two consecutive failures of that baseline are a red tree; one is recorded
  and retried, which is not a courtesy — the first attempt at writing it was refused by
  `bounded_soak` losing its private `rmux.sock` at cycle six, with no mutant anywhere in the tree.
  The measurement lands where the campaign's did: 45.6 s and 51.4 s against the full campaign's
  45.8 s, which is a 228 s and a 257 s timeout against a 20 s floor.
* **`cargo mutants -F` does not select exactly what it is given, and the receipt records that rather
  than assuming it away.** `--help` says the regex is matched against the names `--list` shows, and a
  filter built for `active_editor` alone came back with six mutants of
  `<impl TerminalControl for RmuxTerminalControl>::completion_evidence`, every one of them genre
  `StructField` and none of them naming `active_editor` anywhere. Over-selection is the safe
  direction — the extra mutants cost wall time and grade correctly — so every receipt carries
  `functions_reached_beyond_those_named`, and a filter that reaches FEWER functions than it was
  handed is a refusal, because that is the direction in which a receipt claims something it never
  graded.

### 8.5 What is still not established

* **Rule 2 is at row granularity and not at score granularity.** It protects the 83 KILLED rows.
  The score is 96 over 1,156 decided mutants and a test change can flip any of the 1,120 caught
  ones; §4.4's artifact for that is still unwritten.
* **Rule 3(e) is still stated rather than sized.** Nothing here measures how often a change outside
  `FULL_GLOBS` in the mutated crates would flip a mutant inside it. *Section 9 sizes how often it
  FIRES — seven files in the one window — and how many of that window's twenty-one falsified rows
  needed it: none. It still does not size the general question.*
* **Nothing has yet run the escalation path end to end on a real declaration-level change.** The
  escalations are driven by the synthetic-repository suite and by unit tests; the three
  reproductions above are rule 1, rule 2 and the filtered remedy. *Section 9 runs it on a real one:
  `23e81db..c94612d` escalates twelve times, five of them rule 3(a).*
* **`scripts/tests` is not a Gate A cell**, and deliberately: the cell census is published, counted
  and covered by a receipt, and adding a cell makes every existing receipt short of one. It runs
  under `cargo test --locked --workspace --all-targets --all-features`, which is a cell, through
  `crates/service/tests/register_currency_self_tests.rs`.
* **`cfg_test_regions` leans on rustfmt** — a top-level module's closing brace in column one — and
  the lean is checked, not assumed: a region holding an enumerated mutant escalates instead of
  excusing the change. What it cannot check is a file `cargo fmt` has never seen.

---

## 9. The filtered answer against the full one, at the commit where both exist

The acceptance test for all of section 8, and there is exactly one place to run it: take the register
as it was recorded at `23e81db`, apply the new rules across `23e81db..c94612d`, and re-decide only
what they call stale — at `c94612d`, which is the one commit where the full-scope campaign's answer
also exists.

### 9.1 Where the campaign's answer had to be recovered from

`run.6k6C2J`'s work directory is gone: `.context/` is not in the repository and the directory was the
caller's, which is the same fact section 4.3 records about its logs. So its answer is recovered from
the register that campaign WROTE, at the commit it was written. `0732922` holds 20 `ACCEPTED` and 16
`EQUIVALENT` rows; `recorded_at.missed` is 36; and `check` refuses a run that produces a survivor the
register does not hold. Those 36 rows are therefore exactly the mutants that campaign missed, and
each of the other 1,625 it caught, timed out on, or could not compile.

The register of `23e81db` is `git show 06a6cdc:evidence/mutation-survivor-register.json` — 143 rows
over `run.7OrHM9`, whose `outcomes.json` IS retained: 1,653 mutants, 64 missed, measured rather than
recovered. Its enumeration census was rebuilt from that same file.

**Its KILLED rows carry no `caught_by`, because the field did not exist yet, and rule 2 fails closed
on that**: an unresolvable catcher invalidates on every file of every test package. 62 of the
window's 91 stale reasons are that, so the stale set below is larger than the same window would
produce against a register that names its catchers. The cheap case is the one section 8.2 measured —
three rows, one function, 35 mutants.

### 9.2 What the rules said about the window, in 5.4 seconds

| | |
|---|---|
| rows the whole-file rule calls stale | **143** — the whole register, on 2 drifted files |
| rows these rules call stale | **70** in **36** functions |
| why | **29** rule 1, **62** rule 2 |
| escalations | **12** — 7 × rule 3(e), 5 × rule 3(a) |
| wall clock of the check itself | **5.4 s**, the 4.4 s enumeration included |

**They escalate, so the answer these rules give for this window is the full run.** Section 6 reached
that by hand from a hunk classification it called "sizing, not a check"; this is the check, on the
same window, and it agrees. The seven rule 3(e) files are `compatibility.rs`, `lib.rs`, `runtime.rs`,
`source_scan.rs`, `v1/actor.rs`, `v1/backend.rs` and `v1/mod.rs` under `crates/service/src` — every
one compiled into a mutated crate and named by no glob.

### 9.3 Twenty-one rows had stopped being true, and the rules named one

A row is false at `c94612d` when its disposition disagrees with what the campaign found there. Held
against the campaign **as written**, 33 of the 143 rows disagree — but the register's own drift audit
at `111a071` restored 12 of those, on the ground that one run catching a mutant is not a test that
kills it. That leaves **21 rows that genuinely stopped being true**, and every number below is over
those 21:

| | |
|---|---|
| direction: a row claiming a survivor that is now caught | **21** |
| direction: a `KILLED` row surviving again, or a `REMOVED` row enumerated again | **0** |
| named stale by the rules | **1** (rule 1, `NativeService::start_session_owned_with_retention`) |
| falsified by a test `crates/service/src/native/tests/seam.rs` added at `3192498` | **18** |
| falsified by a test in `native.rs`'s own `#[cfg(test)]` module | **3** |
| file they are all in | `crates/service/src/native.rs` |

**So the rules missed twenty of them, and the reason is a rule that does not exist rather than a rule
that is too coarse.** Rule 2 watches the test that decided a `KILLED` row. An `ACCEPTED` or
`EQUIVALENT` row names no test — its claim is that NOTHING catches the mutant — so what falsifies it
is a test being ADDED, and nothing here can see that. The direction is the safe one: the register
over-states the surviving set, `check` reports such rows as `retired` and never fails on them, and
the floor read out of the register is loose rather than tight. It is still under-invalidation, so
`assess` now counts it on every run —
`survivor_register_rows_a_new_test_could_falsify`, **48** at this head — instead of leaving it here.

**Closing it is not cheap for this register, and that is measured, not asserted.** The trigger would
have to be "any test source changed", because no cheaper predicate can know which new test covers
which surviving mutant. That names 27 functions and **210 mutants**, about 22 minutes — except that
two of the 48 survivor rows are module-level items with no function for `cargo mutants -F` to select,
which is already an escalation, so the rule would demand the full scope on every commit that touches
a test. The escalation is what covers this window: the check refused the cheap path, and the full run
is what found all 21.

### 9.4 The filtered run, mutant for mutant

291 mutants over the 36 stale functions, at `c94612d`, `--jobs 4`, `--timeout 243` sized from a
measured 48.5 s unmutated pass of all three test packages —
`evidence/mutation-filtered-run-agreement-c94612d.json`:

| | |
|---|---|
| graded | **291**: 199 caught, 8 missed, 1 timeout, 83 unviable |
| agree with the campaign **as written**, mutant for mutant | **287 of 291** |
| agree with the register **after its own drift audit** | **290 of 291** |
| the campaign's survivors inside the graded set, re-found | **4 of 4** |
| wall clock | **2,307 s = 38 m 27 s** against the campaign's **11,765 s = 3 h 16 m 05 s**, a ratio of **5.1** |

All four disagreements point the same way — the campaign recorded CAUGHT and this run records MISSED
— and none of them is the granularity rule, which decides which mutants to re-test and not what the
answer is. Three are mutants the register's own audit had already reverted from `KILLED` to
`ACCEPTED` for exactly this reason: `<impl TerminalControl for RmuxTerminalControl>::interrupt` read
as `==` and as `>`, and `FileTranscriptSource::prove_transcript_inert`'s `+=` read as `*=`. This run
is the third run of those three and it agrees with the audit rather than with the campaign.

**The fourth has no row, and it is a thirty-seventh survivor.** `ScreenShape::of` with
`snapshot.revision != 0` read as `== 0` is one of the eight mutants `c94612d` enumerates and
`23e81db` did not; the campaign counted it caught, so the register holds nothing for it. Applied by
hand to the `c94612d` tree and run serially over all three test packages, it survives: **31 targets,
854 tests, 0 failures, exit 0**. That is the one-directional error the register's own
`floor_derivation` names — a test failing for its own reasons is recorded as the mutant being caught
— found once more, and it means the campaign's survivor set is 37 and not 36.

### 9.5 What the acceptance test actually caught, and it was in the harness

The first run of these same 291 mutants disagreed with the campaign seven times, three of them in the
direction that matters — mutants the campaign missed and it called caught. Hand-applied patches at
the same commit say all three survive: `FileTranscriptSource::prove_transcript_inert` at both of its
`- 1` slices and `read_rotation_anchor` at its own, each with **472 lib tests passing**.

The cause was in `scripts/mutation_refilter.py` and not in any rule. It exported `CARGO_TARGET_DIR`
into the environment `cargo mutants` inherits. That tool copies the tree once per worker and relies
on each copy owning its `target/`; one shared directory makes four workers fingerprint the same
package path into the same place, and cargo then reports `Fresh pseudomux-service` for a source it
has just rewritten. The `/` mutant's own log says exactly that, and carries the failure of the `+`
sibling at the same position — `range end index 144 out of range for slice of length 143`, which
`chunk.len() / 1` cannot produce and `chunk.len() + 1` produces every time. The two replacements are
the same length, so the second edit did not even change the file's size.

| | |
|---|---|
| mutants graded without their own crate being compiled, first run | **101 of 291**, 99 of them reported `CaughtMutant` |
| the same count in `run.bbUDg3`, a full-scope campaign | **0 of 1,653** — `scripts/gate-a-mutants.sh` sets `CARGO_TARGET_DIR` for its probe and its candidate build and for nothing else |
| outcomes that changed once the variable was withheld | **28 of 291**: 20 caught→unviable, 5 caught→missed, 1 caught→timeout, 1 missed→caught, 1 missed→unviable |

The fix is one line withheld from one environment. The guard is `rebuilt_in` in
`scripts/mutation_register.py`, which reads each per-mutant log for its own crate's `Compiling` line
— the crate derived from the mutated path against the workspace manifest, so there is no list of
names to keep in step — and `refilter` refuses outright rather than writing a receipt, because a
receipt naming the test that caught a mutant the tests never saw is the exact claim this tree exists
to refuse.

**`evidence/mutation-filtered-run-killed-rows.json` was produced by that tool before the fix**, and
it is the only source of `caught_by` for all 83 `KILLED` rows — the data rule 2 reads. It was
re-derived with the fixed tool rather than argued about; section 9.6 is what that changed.

### 9.6 The catchers, re-derived, and thirty-five of eighty-three were wrong

The 35 functions carrying the 83 `KILLED` rows were re-graded with the fixed tool — 274 mutants,
1,982 s — and `record-catchers` rewrote every row's `caught_by` from that run's logs:

| | |
|---|---|
| `KILLED` rows whose recorded catcher CHANGED | **35 of 83** |
| dispositions that changed | **0** — no row moved, and no `KILLED` row's mutant survived |
| outcomes that differ from the first run of the same 274 | **28**: 24 caught→unviable, 3 caught→missed, 1 caught→timeout |
| rows still naming no catcher, or naming a timeout | **0** |

The 35 are the rows whose mutants were graded against another mutant's binary, so what was recorded
was that other mutant's catcher. `<impl Serialize for StartSessionRequest>::serialize` is the clearest
case: 5 of its 13 rows named
`an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies` and 1 does, the other 4
being caught by `pseudomux-client`'s `fake_uds` and `v1_golden` targets.

**Section 8.2's reproduction was re-run against the corrected catchers and is unchanged**: deleting
`an_agent_start_emits_the_agent_and_omits_every_path_the_agent_supplies` still turns criterion 1 NOT
MET with **13 rows in 1 function** at `survivor_register_files_drifted=0`. Only one row now names
that test, but rule 2 stales the FUNCTION and every row of a stale function is stale, so the count is
the same for a different reason — which is the sort of coincidence worth writing down rather than
leaving for the next reader to re-derive.

### 9.7 What section 9 did not establish

* **The campaign's answer for the 1,625 mutants it did not miss is a single bit**, recovered from
  "the register holds no row for it". Caught and unviable are not told apart, so a mutant this run
  reports unviable and the campaign caught would read as agreement here.
* **The 21 falsified rows are the window's, not a rate.** One window of eighteen commits, whose one
  test-adding commit happened to close twenty-one survivors, is not a measurement of how often a
  survivor row goes stale.
* **Nothing re-ran `run.6k6C2J`.** The thirty-seventh survivor of section 9.4 is hand-reproduced at
  `c94612d` and is not a re-derivation of that campaign's score; the register's 96, its floor of 94
  and its 1,661 are untouched by this section.
* **The harness defect's effect on `evidence/mutation-filtered-run-native-seam.json` is not
  measured.** That receipt predates the tool — it was produced by hand — and it records no catchers,
  so nothing in the register reads it.
