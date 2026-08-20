# evidence/

The durable release record. Everything here is either irreplaceable or is the
reviewed output of a gate. Working artifacts, raw run logs, and coordination
documents are deliberately **not** here — they stay workspace-local under
`.context/`, which is gitignored.

## `model-attempt-ledger.ndjson`

The immutable global real-Claude attempt ledger. NDJSON, one canonical JSON
object per line, mode `0600`. Ordinals 5-29 are the legacy records and carry
`schema_version: 1`; every record from ordinal 30 on is a reservation and
carries `schema` = `"pmux.phase0.attempt-reservation.v1"` instead.

- Ordinals 1-4 predate the file and are attested by its first record,
  `{"global_attempt":5,"kind":"approved_prior_baseline"}`
- The ordinal field has TWO spellings and the count needs both. Ordinals 5-29
  spell it `global_attempt`; ordinal 30 onwards spells it
  `global_attempt_ordinal`. The change is at ordinal 30 and is not an anomaly --
  every reservation written since uses the second spelling, so it is the current
  format and the first is the legacy one. A scan that knows only `global_attempt`
  stops at 29 and reports the budget dozens of attempts cheaper than it is. Both
  spellings live in ONE tuple, `phase0_lib.ORDINAL_SPELLINGS`; the budget
  tool is deleted. Count ordinals from the file, not a one-liner.

No SHA-256 is pinned here. This file is a frozen historical ledger, so a
literal digest in prose is stale the moment the next attempt reserves, and a
stale digest that looks authoritative is worse than none. Recompute it when you
need it; the file is its own authority.

### The paths in this file were substituted and the chain was re-sealed, 2026-08-14

**This file used to be the one artifact here that spelled absolute paths, and it
was exempt from the redaction map on the grounds that it could not be rendered.
It has now been rendered, by the machinery that owns the seal, and the exemption
is gone. What follows is the discontinuity that cost, stated here because it is
not recoverable from the file.**

WHAT WAS SUBSTITUTED. Every occurrence of the operator's home directory and
login name — they appeared in `artifact_directory`, in every recorded binary
path, in the Claude binary path, and throughout the `source.revision_identity`
Git-control snapshot, both at the top level of each reservation and inside its
`campaign_contract`. The legacy records that open the file carried none and are
byte-identical.

BY WHAT RULE. `tools/evidence_common/portable_paths.py`, whose needles are asked
of the running machine and never written down, in its root-preserving form
(historical `absolute_placeholders=True`, now deleted). An absolute needle gets an
absolute placeholder spelled with that needle's own root, so a home directory
becomes `/<HOME>` and this checkout becomes `/<REPO>`. That is not cosmetic:
`_validate_reservation_record` reads `artifact_directory` back through
`Path(...).is_absolute()`, `_validate_public_file_identity` reads every recorded
binary path back the same way, and `source_digest._canonical_absolute_path_text`
requires `is_absolute()` **and** `str(Path(value)) == value`. `<HOME>/x` is a
relative path; a substitution using the default placeholder would have turned
almost every reservation into one the validator refuses.

BY WHAT WRITER. Historical `tools/phase0/reseal_ledger.py` (deleted), which substituted then
recomputes, with `phase0_lib`'s own functions and in the appender's own order:
the sibling digests it can first reproduce (`revision_identity_sha256`, then
`campaign_contract_sha256`), then `ledger_prefix_sha256` over the rewritten
prefix, `previous_ledger_sha256` over every rewritten byte in front of the
record, `previous_reservation_sha256` from the record's own prefix boundary, and
`reservation_sha256` last. It refuses to write unless every identifier is gone
and every record still verifies. It is idempotent: run it again and it rewrites
the file to itself.

WHAT IS LOST, AND IT IS NOT RECOVERABLE FROM THIS FILE. A re-sealed chain is a
valid chain over different bytes. Not one of the digests recorded at reservation
time survives: every `reservation_sha256`, every `previous_ledger_sha256`, every
`campaign_contract_sha256` and the whole-file digest are new. Receipts committed
before this date pin the old whole-file digest as evidence that a campaign left
the ledger untouched — `evidence/live-adversarial-suite-2.1.227-macos-aarch64.json`
(`sha256_before`/`sha256_after`), `docs/path-b-adversarial.md`,
`docs/path-b-verdict.md`, `docs/2.1.226-acceptance.md`,
`docs/2.1.227-compatibility.md` and `docs/defect-log.md`. Those receipts are
correct about the run they describe and are deliberately not edited; they can no
longer be checked against the file in front of you, and the value they name is
not derivable from it. If you need the pre-redaction bytes, they are in git
history before this commit and nowhere else.

WHY THE NOTE IS HERE AND NOT IN THE FILE. The ledger schema cannot carry it, and
that was checked by running it rather than reasoned about — see
`python3 tools/phase0/reseal_ledger.py --explain-note` (tool deleted), which printed the six
placements tried and what each validator did with them. A note with no ordinal
is refused by `summarize_attempt_ledger` in every position including the prefix
region; a note with a fresh ordinal spends an irreplaceable attempt; a note with
an ordinal in front forges an attempt this file's own first record attests
predates it. That is a finding about the schema, not a reason to relax the
validator, so the statement lives here.

WHAT THE SEAL STILL MEANS, CORRECTED. Substituting into a sealed record without
re-sealing does not redact it, it forges it — that is still true, it is still
why a blind `--rewrite` used to refuse this file, and a since-removed
redaction test used to prove it by substituting into a copy. Two things said here were broader than what any
predicate tested, and both are withdrawn:

- *"a forged one refuses a live campaign restart."* MEASURED, and it does not.
  `phase0` re-verifies the chain only over records **after** the immutable
  prefix the driver hands it, and the driver derives that prefix as the whole
  file it found. A copy of this ledger with every seal broken by a blind
  substitution was accepted by `reserve_attempt`, which appended the next
  ordinal on top of it without complaint.
- *"`artifact_directory` is re-opened and audited before the next reservation is
  allowed."* `_reconcile_prior_usage_locked` audits post-prefix records of the
  **current** campaign only, so it never opens a committed record's directory.

What used to re-verify this file's chain, on every gate, was
`portable_paths.sealed_records` (deleted with Phase 0). It checked all four bindings a
reservation carries rather than two, because the re-seal recomputes all four and
a binding nothing re-verifies is decoration.

The budget arithmetic is indifferent to all of this — it counts ordinals, and
the redacted file prints the same `consumed`/`ceiling`/`remaining` as the
original, which was checked before and after.

**No record count, no last ordinal, and no remaining-attempt figure is written
in this document, for exactly the same reason.** One was, and it went stale by
38 ordinals against a hard ceiling of 100 while this paragraph sat above it
saying digests must not be pinned. Historical tool (deleted):

    # DELETED. Do not run. python3 tools/phase0/phase0.py budget --ledger evidence/model-attempt-ledger.ndjson

It printed `records`, `first_ordinal`, `last_ordinal`, `predating_the_file`,
`detached`, `consumed`, `ceiling` and `remaining`, every one of them computed
from the file on the call. `consumed` is the ledger's own last ordinal plus the
four detached reservations described below; `ceiling` is the one the records
themselves were reserved against, not a constant in prose. The command refused
rather than reported if the ordinals are not contiguous, if a record spells its
ordinal in none of the recognized ways, or if consumption has passed the
ceiling. `test_run_gate.py::test_the_evidence_readme_states_no_budget_figure_and_its_command_derives_one`
ran in Gate A (`gate_f/gate_driver_self_tests`, now gone) and failed if this
document started stating a figure, or if the command stopped agreeing with the file.

The ceiling is a total across all campaigns, enforced at reservation time
(`tools/phase0/phase0_lib.py`), not a per-run allowance. A restart, a new runner,
a failed call, or a source invalidation never resets it.

### Four detached reservations, all numbered 31 (2026-07-28)

A deleted Phase 0 budget report counted **four attempts this file does not contain**, as its
`detached` field. This is deliberate and self-penalizing; do not "correct" it
away, and do not pass `--detached 0` to make the arithmetic tidier.

On 2026-07-28 a driver script copied this ledger to a private path outside the
source tree on every invocation, ran one campaign against that copy, and never
copied the result back. Each run therefore restarted from the same 26-record
base and reserved the same ordinal 31 in a detached file, while its retry loop
compared record counts against the reset copy and concluded nothing had been
spent. Four campaigns ran where one was authorized. The ordinal 31 that appears
in this file is a later, legitimate reservation, unrelated to those four.

Reconciliation, from the four retained run directories:

- `f0ad703f` rejected at the agent-team guard (`claude_launch.rs:397-408`);
  no Claude process was launched.

- `ca52a109`, `16849243`, `e2260f34` each started a session and launched a real
  Claude, then stopped at `NeedsTrust`/`NeedsInput` because the working
  directory had never been trusted. No prompt was submitted.
- All four report `observed_tokens: 0`. No model tokens were billed.

The four reservations are counted anyway. A reservation consumes its ordinal
whether or not Claude produced a result -- that is the rule this ledger exists
to enforce, and exempting these because they happened to be cheap would make
the budget a measure of luck rather than of attempts. The hash-chained records
are NOT renumbered into this file: forging chain entries to tidy the arithmetic
would cost more integrity than the four ordinals are worth.

Any driver must reserve against THIS path, or copy its result back before the
next run. A driver that reserves against a copy it later discards silently
resets the global budget.

This file is append-only and is the budget authority. Do not edit, reformat, or
regenerate it. Phase 0 (deleted) located it by explicit `--ledger` path.

### Real Claude runs this ledger never saw, and the one it under-counted

Two separate defects, found while sizing a 2.1.226 promotion campaign. Neither
is fixed by a number in this document; the first is fixed in code, and the
second is a policy question that belongs to the owner.

**The reservation guard under-counted by exactly the detached reservations.**
`phase0_lib.reserve_attempt` compared the ledger's own next ordinal against the
ceiling and refused with *"global real-Claude attempt ceiling is exhausted"* —
a message naming the global total over a predicate testing this file's
numbering. `summarize_attempt_ledger`, the count the command above prints, adds
the detached reservations back first, so the two disagreed by exactly
`DETACHED_GLOBAL_ATTEMPTS`: at the ledger's current ordinal the guard would
still have handed out four ordinals past the point the report calls empty.
Both now derive the total through one
`phase0_lib.global_attempts_consumed_through`, and
`test_phase0.py::LedgerTests::test_the_reservation_guard_stops_where_the_budget_report_says_it_does`
pins the two boundaries to each other rather than to a written-down figure.

**`DETACHED_GLOBAL_ATTEMPTS` was documented as "the only such number" and is
not.** `turn-latency-2.1.220-macos-aarch64.json` — committed, in this
directory, described below — is the receipt for a run of
`tools/promotion/measure_turn_latency.py --driver-environment operator` against
the operator's real Claude. Its `path_a` block holds one `pmux turn` sample per
real turn and its `path_b` block one `pmux ask` sample per real turn, 22 in
each including two warm-ups; `measure_path_b` raises rather than record a
sample whose `text` came back empty, so every one of them is a model round
trip. It is stamped `2026-08-06`, a week after this ledger's last record, and
it reserved nothing. The tool is not the only such entry point — the real lanes
in `crates/e2e/tests/pool_concurrency.rs` and
`crates/e2e/tests/cross_cell_contamination.rs` launch real Claude behind
`PMUX_POOL_REAL_CLAUDE` and also reserve nothing.

So "enforced at reservation time" is true only of work that goes through
`tools/phase0`, while the sentence above it calls the ceiling *a total across
all campaigns*. **Whether decision D4's ceiling was meant to cover instrument
runs as well as campaigns is not decided here**, and nothing re-prices it
silently: were those turns counted, consumption would already be past the
ceiling and `phase0.py budget` would refuse instead of reporting. That refusal
would be correct if the answer is yes. Until the owner answers, the honest
statement is that this file is the authority for *reserved* attempts and is not
a complete census of real Claude turns.

**That paragraph is now a number rather than only a warning.**
`phase0.py budget` prints `real_turns_outside_the_ledger`, derived by scanning
the receipts in this directory: `total` is the count of real model round trips
behind committed evidence that reserved no ordinal, `receipts` names each one
and its contribution, and a receipt the scan cannot classify **stops the count**
instead of being read as zero — the one behaviour a budget cannot have. It is
reported *beside* `consumed` and never folded into it, because folding it in
would answer D4 rather than ask it. The figure is a **lower** bound and says so:
the `PMUX_POOL_REAL_CLAUDE` lanes reach a real model, reserve nothing and leave
no receipt, so nothing can count them.

## `promotion-<version>-<os>-<arch>.json`

**The receipt one Claude Code version's promotion rests on, and the only thing
in this repository that has ever driven a minified cell as a promotion check.**
Produced by

    python3 tools/promotion/promote_claude_version.py \
        --release-dir target/release --claude <path to that version> \
        --output evidence/promotion-<version>-<os>-<arch>.json

`--describe` prints the ordered check list, its pass criteria and which
`compatibility::RepromotionTrigger` each check exercises, and runs nothing.
`compatibility.rs::every_repromotion_trigger_is_exercised_by_the_promotion_path`
holds that list to the five triggers, and the tool itself refuses to run if its
checks and the Rust ids disagree.

Three keys are worth reading before reusing it.

- `verdict` is `promotable` only for a run against a real Claude with every
  check passed; a `--driver-environment double` run says `rehearsal` and
  proposes no profile.
- `range_provenance` is **generated from the check results**, not written. That
  string has been wrong in both directions — it claimed a bundle pmux does not
  launch, and then, after the drain was measured at 2.1.226, it went on saying
  the drain had never been measured there. A sentence assembled from results can
  do neither.
- `checks[].detail.per_version_recommendations_not_to_be_shipped` is published
  and never used. `docs/version-drift.md` §P3 records why with this tool's own
  three runs: at 2.1.226, on one host inside half an hour, the per-version fit
  was 250 ms, 500 ms and 500 ms. The 2.1.227 run makes it four, and it fitted
  **250 ms** — below `POST_MARKER_CATCH_WINDOW_FLOOR_MS`, which is the whole
  reason the shipped drain is read from the pooled receipt instead.

`real_claude_turns.count` is what the budget scan above reads.

**Two of this receipt's three sibling runs are not in any receipt.** The path was run three times at 2.1.226 while it was being written; only the last is committed, and the earlier two spent five real turns each that nothing in this directory records. That is the same shortfall the budget scan above reports, seen from the other end: a receipt counts the run it describes, and a run nobody keeps a receipt for is counted by nothing.

The 24 `model_call_attempt` records (ordinals 6-29) were run on 2026-07-19
against Claude Code `2.1.215` (macOS, aarch64, `transparent` terminal profile,
`sdk` input transport) and are bound to two source digests that are no longer
reproducible. Per decision **D5** they are **budget accounting only** — they are
not promotable compatibility evidence, and Gate B coverage restarts from zero
against one new frozen digest. Their enduring analytical value is the observed
transcript drain distribution: min 2,320 ms, max 2,479 ms, mean 2,354 ms over
24 turns.

Ordinals 30-43 (reserved 2026-07-27 and 2026-07-28) are the Gate B
drain-calibration campaigns, run against Claude Code `2.1.220`. This file is the
authority for that version -- `claude.version_output.normalized_version`, on
every reservation record it holds. Nothing else measures it; the receipt below
only copies it into its `provenance` block.
Ordinals 31-43 are the ones that receipt covers.

## `gate-b-drain-calibration.json`

The Gate B drain-calibration receipt: the offline verifier's own `--json` output
over the retained evidence from those campaigns, with absolute host paths
scrubbed so it reads from any checkout.

Its producer is recorded in the file's own `provenance` block:

- `# DELETED.` `python3 tools/phase0/verify_calibration.py --evidence-root <root> --json`, a
  deliberately independent second implementation that recomputes each gap and
  each reported hash straight from published artifacts rather than trusting the
  tool that measured them (`tools/phase0/README.md` § "Gate B drain-calibration
  prompt suite and verifier").
- Post-processing: drop every `attempts[].attempt_dir` (they are absolute host
  paths), re-serialize with `json.dumps(obj, indent=1, sort_keys=True)`, and add
  the `provenance` block, which is the only key the verifier does not emit.
- The evidence root is host-local and untracked, so it does not travel with a
  clone. Re-analysis is free while it exists -- the verifier reads only
  already-published files, spends no ordinal and calls no model -- and
  impossible once it is gone. That is the sense in which this receipt is
  irreplaceable: the ledger records reservations, not results.

What it covers: 17 attempt rows over global ordinals **31 through 43**, 10 of
them successful, spanning all nine graded prompts (`01-baseline-trivial` twice,
the other eight once each; 9 at medium effort, 1 at low). Five rows carry
ordinal 31: one is the ledger's legitimate ordinal 31, and the other four are
the four detached reservations described above. Those four attempt ids are
absent from the ledger by design; each appears in the `campaign.json` of one of
the four retained run directories, so the match is checkable only inside the
evidence root.

What it measures:

- 10 computable late-arrival gaps: min 0 ms, max 1 ms, **zero** attempts with a
  late transcript row once the 20 ms noise band is applied, 4 samples inside the
  band, against a configured `transcript_drain_ms` of 2,000.
- `headroom_ms` is deliberately `null`. With the worst gap inside the noise band
  there is no measured late row to subtract, and publishing `configured - 1`
  would turn a clock artifact into 1,999 ms of apparent proven margin
  (`tools/phase0/verify_calibration.py:782-793`).
- 7 reported hashes recomputed from the captured poem text and matched, **0
  mismatches**, 3 not applicable, 7 with no result (the failed attempts).
- 7 failed attempts, 0 fatal, 0 incomplete, 0 unreadable, and
  `failing_conditions: []`.

What it does not carry: no Claude version of its own -- that binding lives in
the ledger above, and `provenance.claude` copies it from there rather than
deriving it; no transcript or assistant text; no reviewed terminal snapshots;
and no tamper-evidence. Manifest hashes and ownership are `phase0.py audit`'s
job, and the verifier assumes the tree it read has already been trusted.

## `pooled-transcript-drain-macos-aarch64.json`

**The receipt the shipped `transcript_drain_ms: 1000` actually rests on.** It is
a bound POOLED over Claude Code 2.1.207, 2.1.215, 2.1.220 and 2.1.223 — 226
reachable post-answer arrivals in 425 macos/aarch64 transcripts, maximum 438 ms,
×2.0, rounded up to a 250 ms step. `docs/version-drift.md` §3.3–§3.5 is its
prose and its justification: a per-version fit measures noise (permutation
p = 0.730) and under-estimates a tail maximum, so it errs in the direction that
truncates an answer. Regenerated by

    python3 tools/promotion/measure_transcript_drain.py \
        --corpus ~/.claude/projects --corpus ~/.claude-1/projects \
        --version 2.1.207 --version 2.1.215 --version 2.1.220 --version 2.1.223 \
        --bound-ms 1000 --json

Three keys are worth reading before reusing the number.
`per_version_recommendations_not_to_be_shipped` publishes what each version
*would* have been fitted to — 250, 750, 1000, 250 — which is the entire
argument for pooling, kept in the artifact rather than only in a document.
`full_drain_binds_on` is the price, counted from the corpus: the full drain
binds only on the `cli` turns that reached a terminal candidate carrying no
`turn_duration` marker of their own, and the receipt reports that count and its
fraction — so widening the bound is not free, and the number is read from the
file rather than repeated here.
`repromotion_triggers_this_tool_detects` names the two triggers this tool
is the detector for, in the same spelling
`compatibility::RepromotionTrigger` uses.

`compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit`
re-derives the recommendation from this file's own stated margin and step,
requires at least two versions each of which contributed arrivals, requires at
least one per-version fit to be strictly smaller than the pooled bound — so the
word "pooled" cannot become decoration — and requires
`PromotedProfile::drain_provenance` to quote this file's own numbers.

## `promoted-profile-2.1.220-macos-aarch64.json`

The receipt for the **floor** of the promoted range, and the file
`every_promoted_drain_is_the_one_its_receipt_recommends` reads for identity.
`docs/path-b.md` §12.4 is its prose. Regenerated by
`python3 tools/promotion/measure_transcript_drain.py --corpus ~/.claude/projects
--version 2.1.220 --json`, whose corpus is the operator's own host-local
transcripts and therefore does not travel with a clone.

Read `what_would_invalidate_it` before reusing the number. Note that the tool
FAILS on a `(type, subtype)` it has never classified rather than defaulting: a
new row kind that could follow an answer widens the drain or stops the run, and
never silently misses. Note also that its `recommended_transcript_drain_ms`
happens to equal the pooled bound only because 2.1.220 is the version that
contributed the pooled maximum; it is **not** the number pmux ships *because*
it is 2.1.220's fit, and the file above is the one that says so.

## `turn-latency-double-macos-aarch64.json` and `turn-latency-2.1.220-macos-aarch64.json`

What one pmux turn costs, per leg, with a distribution rather than a point
estimate. **These exist because `docs/path-b.md` carried two different numbers
for one quantity — 571 ms and 535.5 ms — for months, with no argv, receipt or
harness behind either.** Neither could be defended and neither could be
regenerated, so both were replaced rather than chosen between. `path-b.md` §10.1
and §9.1.1, and `current-state.md` §6.1.1, are their prose.

Both are produced by `tools/promotion/measure_turn_latency.py`:

    python3 tools/promotion/measure_turn_latency.py \
        --release-dir target/release --claude target/release/pmux-test-claude \
        --turns 60 --drain-ms 250 --output evidence/turn-latency-double-macos-aarch64.json

    python3 tools/promotion/measure_turn_latency.py \
        --release-dir target/release --claude "$(command -v claude)" \
        --driver-environment operator --model sonnet --effort low \
        --turns 20 --drain-ms 1000 \
        --output evidence/turn-latency-2.1.220-macos-aarch64.json

- **`turn-latency-double-macos-aarch64.json`** is pmux's own machinery against
  `pmux-test-claude`, which has no model in it. n=60 measured turns per path plus
  3 discarded warm-ups, one held Path A session so no sample pays a launch, and a
  pool warm floor of one so no `pmux ask` sample pays a mint.
- **`turn-latency-2.1.220-macos-aarch64.json`** is real Claude 2.1.220,
  sonnet/low, n=20, at the promoted profile's own 1000 ms drain. It is also the
  artifact that settles `path-b.md` §13 item 6: its `turn_duration_arrival` block
  reports 20/20 turns carrying Claude's marker and 0/20 with any
  analysis-changing row arriving after it.

Two properties worth preserving if either is ever regenerated by other means.
The tool **FAILS on an unclassified `*_at_ms` field** rather than publishing a
total that silently excludes a leg — that guard is what surfaced
`turn_duration_observed_at_ms` in the first place. And percentiles are
nearest-rank, so every number published is a value that was actually observed.

`host.load_average_1m` is recorded because this host is a workstation and is
never idle; a receipt taken far above the recorded value should be re-taken
rather than compared.

## `mutation-survivor-register.json`

**One written disposition for every mutant that survived the full-scope mutation
gate, and the record the next run is held against.** Produced by hand from a
named `outcomes.json`; read and enforced by `scripts/mutation_register.py`.

A mutation score is one number and it hides the thing that matters: WHICH
mutants survived, and whether anyone has ever looked at them. The standing
complaint this file answers is that survivors are found faster than they are
closed, and that the same rows come back as fresh findings because nothing
recorded that they had already been worked. So every survivor carries exactly
one disposition and a reason:

- **KILLED** — it survived the run this register ratchets from and is caught at
  the recorded head. Kept so that its reappearance is a regression rather than a
  new row.
- **EQUIVALENT** — the mutation provably changes no observable behaviour, with
  the argument written out. Where the clause was also provably dead the entry
  says whether deleting it was considered and why it was or was not done.
- **ACCEPTED** — real, not closed, with the risk it leaves open and a
  `closeable` field naming what closing it costs: `cheap`, `seam`,
  `cost-bound`, `fault-injection`, `compile-time-only`, `log-assertion`,
  `unobservable-on-this-host`.
- **REMOVED** — the code the mutant patched no longer exists, because the clause
  was deleted rather than tested.

**A KILLED row also names the test that killed it**, under `caught_by`, and
`validate` refuses a register where one does not. A KILLED row is a claim about
two pieces of the tree, not one — the code the mutant patches and the test that
decided it — and until this field existed the second half was unwatched: delete
that test and the row is simply false, with the done-gate still reporting
criterion 1 MET. `docs/register-currency.md` §4.1 reproduces that in a throwaway
clone, step by step. The field is `{test, target, run}`, distilled from the
deciding run's own per-mutant logs by

    python3 scripts/mutation_register.py catchers \
        --outcomes <run>/outcomes.json --logs <work>/out/mutants.out/log \
        --run <run> --out <run>/catchers.json

which `scripts/gate-a-mutants.sh` now runs on every campaign, because the 234 MB
`log/` tree does not survive the work directory and nothing else records what
caught anything. `python3 scripts/mutation_register.py record-catchers` is what
writes the distillation onto the rows. A mutant decided by a TIMEOUT names no
test and no target; its row records `undetermined` instead of a guess, and the
currency check reads that as stale on a change to any test source. EQUIVALENT,
ACCEPTED and REMOVED rows carry no `caught_by`: no test decided them, and asking
one to name a catcher would be asking it to name a test that never ran.

No figure from it is written in this document, for the same reason no ledger
count is. Ask the file:

    python3 scripts/mutation_register.py report \
        --register evidence/mutation-survivor-register.json

It prints the head and evidence directory the register was recorded at, the run
totals, the score, and the census by disposition and by closing cost — every one
of them computed from the file on the call.

**The key is not `file:line:column`.** That is how `cargo mutants` names a
mutant and it is the one thing about a mutant guaranteed to rot: adding a test
above a function moves every line below it, and adding a test is exactly how a
survivor gets closed. The key is
`(file, function, genre, replacement, occurrence)`, where `occurrence` orders
the mutants agreeing on the first four by the position the tool reported.
Nothing in that tuple moves when a test is added, and two mutants of the same
shape in one function are still told apart — which is what makes a **new**
survivor inside an already-accepted function visible instead of absorbed by that
function's row. The line and column are recorded under `observed_at` as a
reader's aid and never as identity; `check` counts how many have drifted and
never fails on drift.

`scripts/gate-a-mutants.sh` runs

    python3 scripts/mutation_register.py check \
        --outcomes <run>/outcomes.json \
        --register evidence/mutation-survivor-register.json

on **every** run, at either scope, and refuses when a mutant survived that the
register does not hold, or when one the register calls KILLED or REMOVED
survived again. It does **not** refuse a survivor that has since been caught:
those are printed as `retired_survivor=` so the entry can be pruned, because
refusing them would make closing a survivor break the gate.

**The `full` scope's floor is read out of this file** —
`recorded_at.floor_percent` — rather than written into the gate script beside
it. That is the ratchet: there is exactly one statement of the number in the
tree, it sits next to the survivor list that explains it, and it moves only when
a new measurement is recorded here. `PMUX_MUTANTS_MINIMUM_SCORE` may raise that
floor and is refused if it is below it. The `gate` scope keeps its defended
constant of 94, declared in the script, because this register is a record of a
`full` run and says nothing about that scope.

`floor_percent` is **not** `mutation_score_percent`, and
`recorded_at.floor_derivation` says why in the file itself. This gate's error is
one-directional — `docs/archive/testing-gate-a-census.md`'s "THE SCORE DRIFTS UPWARD" bullet — so the
raw score is an over-estimate, and the floor is that same measurement with every
mutant whose only failing test was a MEASURED drifter counted as missed. The
drift is measured here rather than inherited: five mutants the prior run counted
as caught are missed in the recorded one, no edit between the two touched any of
them, and the prior run's own logs name a real-PTY or real-rmux test as the sole
catcher of all five.

**The register inherits that drift too, and the response is not to weaken it.**
A drifting test can flip one caught mutant to missed, and `check` will then
refuse naming exactly that mutant. That is the useful failure: it names one
mutant to re-test with a file/line filter, where a score dropping a point names
nothing. Re-test the one mutant; do not add a row to make the refusal go away.

## `mutation-enumeration.json`

**Every mutant the register's own campaign enumerated, keyed as the register is
keyed and counted rather than listed.** Written by
`python3 scripts/mutation_register.py census`, which
`scripts/gate-a-mutants.sh` runs at the end of every `full` campaign; read by
`scripts/register_currency.py`.

It answers the one question 144 survivor rows cannot: whether a mutant found at
some later commit is one this measurement has ever seen. A brand-new function's
mutants appear in no row and in no count, which is how they stop being invisible
— and a working-tree edit is visible to it in a way `git diff` between two
commits is not, because the enumeration reads the files on disk. The counts are
`{file: {function: {genre: {replacement: n}}}}`: the register's key with
`occurrence` collapsed to a count, which is what "has this ever been enumerated"
needs and a twentieth of the size of the keys.

`recorded_at` carries the frame the campaign measured under — the globs, the
test packages, the mutation profile, the required cargo-mutants version and the
toolchain channel — all read out of `scripts/gate-a-mutants.sh` and
`rust-toolchain.toml` at recording time rather than passed in by the caller.
Rule 3(b) of the currency check compares those five against the same two files
later; a frame recorded from a caller's flags would be comparing a copy against
itself.

## `mutation-filtered-run-<name>.json`

**The receipt for one filtered `cargo mutants` run, and for the survivor-register
rows it closed.** Written by `python3 scripts/mutation_refilter.py`; read by a
person, and its mutant count is read by rule 3(f) of the currency check.

Each receipt names the commit, the functions it graded, the `-F` expression it
graded them with, the per-mutant timeout and the unmutated baseline that sized
it, and what caught each mutant. `--stale` grades exactly the functions the
currency check calls stale, deriving the set from the same code that printed the
refusal, so the remedy cannot name a different set from the complaint.

Rule 3(f) is why the count matters: filtered runs are admissible only while
their accumulated mutants since the last full-scope campaign are fewer than one
full enumeration. Past that the cheap path has already cost more than the sound
one.

### `mutation-filtered-run-native-seam.json`

The first of them, and the one that predates the tool — produced by hand.

`mutation-survivor-register.json` above documents the idiom this file is the
output of: when a row is worked, re-test THAT MUTANT with a file/name filter
rather than paying three hours for a score. This run took the functions the
`SessionRuntime` test seam was built to reach — `crates/service/src/native.rs`,
filtered by name to the start funnel, the reaper, the fences, the completion
proof, `diagnose`, `shutdown` and the clock — and re-tested every one of them
after `crates/service/src/native/tests/seam.rs` was written.

It holds the exact argv, the tool version, the counts, the caught, missed and
unviable lists in full, and the SHA-256 of the three source files the run
graded, so the claim "these rows are KILLED" is checkable against something
other than a sentence -- including whether the tree it graded is the tree the
commit carries.

**It is not a score and no percentage may be read out of it.** It enumerates a
few dozen of the mutants a full-scope run enumerates and says nothing whatever
about the rest; the register's `recorded_at` remains the only recorded
measurement. Which of its rows the seam commit made stale is no longer a
sentence anybody writes here — `scripts/register_currency.py` computes it, per
row, and criterion 1 of the done-gate refuses on the answer.

### `mutation-filtered-run-agreement-c94612d.json`

**The acceptance test for the currency check, and the only receipt here that names an ancestor
commit on purpose.** `c94612d` is where the full-scope campaign's answer exists, so it is where a
filtered answer can be held against one. It grades the 291 mutants of the 36 functions the rules call
stale across `23e81db..c94612d`, and `docs/register-currency.md` section 9 is the comparison: of
those 291, all but four agree with that campaign as written, and all but one agree with the register
after its own drift audit — in 38 m 27 s against 3 h 16 m 05 s.

It closes no row. Its commit is an ancestor of the register's own head, so rule 3(f) counts nothing
from it — a receipt for a tree the recorded campaign already covers has not spent anything.

## `path-b-defect-register.json`

**One written status for every defect the Path B verdict lists, and the file
criterion 1 of the done-gate reads.** Produced by hand; read and enforced by
`scripts/path_b_done.py`, which `scripts/path-b-done.sh` runs.

Criterion 1 is *"no known unfixed defect in the Path B path"*. Before this file
existed, that question was answered by reading four paragraphs of
`docs/path-b-verdict.md` and writing a sentence — and because those paragraphs
carry their closures as annotations added later, the answer depended on which
annotation the reader believed. It was answered three times, differently. Here
each defect is a row with exactly one status:

- **OPEN** — a live defect with nothing decided about it. Criterion 1 is NOT MET
  while any row is OPEN, and the refusal names the row.
- **CLOSED** — fixed, with the commit named. The commit must exist in this
  repository or the row itself is refused.
- **ACCEPTED** — the behaviour is known and the decision to keep it is written
  in `decision`. A row with no decision is refused, because "accepted" with no
  recorded reason is indistinguishable from forgotten.

**The set of rows is bound to the document, in both directions.** Criterion 1's
own section lists its defects as `(a)`, `(b)`, `(c)`, `(d)`; every letter it
publishes must have a row here carrying that letter, and every lettered row here
must be a letter that section publishes. A fifth defect added to that document
is therefore a refusal rather than a row nobody wrote, which is the one property
that makes this more than a second place to be out of date.

**Every row cites something, and the citation is checked.** `where` must be a
file that exists and `anchor` must occur in it verbatim — the rule the citation
grader holds a Path B document to, applied to the register. The verdict document
records why: a defect register whose own citations do not resolve is this
repository's bug class aimed at the place defects go to be remembered.

**What it does not claim** is written into the file, under
`recorded_at.what_this_does_not_claim`, and is worth repeating: this is not a
claim that no other defect exists. Its completeness is exactly what it can be
held to mechanically. Mutation survivors are not recorded here — they are
dispositioned in `mutation-survivor-register.json`, which criterion 1 reads
beside this one, together with the check that no file the mutation gate mutates
has changed since that register's run.

---

## `screen-veto-cost-2.1.227-macos-aarch64.json`

**The availability cost of the unrecognised-screen veto, measured rather than
argued.** Produced by one run of 24 real turns plus
`crates/service/examples/screen_census.rs`; read by a person, and cited by
`UNRECOGNISED_SCREEN_VETO`'s own doc comment.

`UNRECOGNISED_SCREEN_VETO` refuses a turn that sits on a screen no rule matched
while its transcript stands still. Refusing more trades correctness for
availability, and the trade is only defensible with a number: **a cell that
refuses a screen it should have accepted costs a pooled instance.** So the
question this receipt answers is the narrow one — *does the rule refuse turns
that were fine?*

- **24 real Sonnet 5 turns**, 8 each at `low`, `medium` and `high`, through a
  pooled daemon at Claude Code 2.1.227 on macOS/aarch64.
- **4,415 frames** recorded from the production reads themselves
  (`PMUX_SCREEN_CORPUS_DIR`) and replayed through the PRODUCTION classifier, so
  the verdict on every frame is the one the daemon that captured it would have
  given.
- **`turn_monitor.observe` — the read the veto is decided from — produced 0
  unrecognised frames in 2,629 observations.** Longest continuous run: 0 ms.
- Longest legitimate unrecognised run anywhere: **844 ms**, at
  `startup.wait_until_ready`, a cold pane before its first composer.
- **False-refusal rate: 0/24. The veto never fired.**

`per_turn` carries every turn's effort, exit status, duration, stop reason and
token counts, so the claim "24 real turns" is checkable rather than asserted;
`census.by_site` carries the per-site verdict counts and the longest continuous
unrecognised run at each.

**It names no commit, on purpose.** The daemon that produced these numbers was
built from an UNCOMMITTED working tree, so `provenance` says that in as many
words and gives the sha256 of the two binaries that ran plus the commit the tree
became (`93bc7a1`). It first carried a `commit` field holding whatever HEAD
happened to be when the file was written, which named code the run never
executed — the same defect `scripts/gate-in-worktree.sh` exists to stop a gate
receipt committing, found here in a receipt written by hand.

**What it does not claim** is in `not_established`, and the first entry is the
important one: because the veto never fired, this file is strong evidence about
the FALSE-refusal rate and **no evidence at all** that the firing path behaves
correctly against a live Claude. That path is covered by unit tests only. One
host, one version, one pane geometry; and since nothing was refused, the window
has never been narrowed against data — it is a bound set ~35x above the longest
run measured, not a fit.

---

## `live-adversarial-suite-2.1.227-macos-aarch64.json`

**Every Path B refusal guard fired against a live daemon, plus the six live
checks that need a real model.** Written by hand from three JSON outputs of a
`/tmp` harness that is gone; read by a person.

The guards are **derived, not listed**. The harness parses
`COMPOSER_MODE_PREFIXES`, `COMPOSER_REWRITTEN_CHARACTERS`,
`COMPOSER_LINE_CONTINUATION` and every variant of `enum ComposerRefusal` out of
`crates/claude/src/composer.rs`, and every `return Err(DriverFailure::new(` out
of `validate_prompt` in `crates/service/src/driver_io.rs`, and **refuses to send
anything** unless its probes cover both sets exactly. `guards.derived_from`
names the two files; `guards.by_guard` is the per-guard count that came back.

- **47 probes, 47 refused by the daemon**, each with the refusal a transcription
  of the two shipped predicates predicted. Among them a **30-character
  whitespace sweep** — every character `is_trimmed_from_the_end` is defined over
  — sent alone: whichever side of that conjunction a character falls, the daemon
  must refuse it, and it says which of `prompt must not be empty` and
  `unsafe control character` it chose.
- **Both transports.** Every probe goes through `pmux ask` AND through one
  hand-framed `run_stateless` request on the daemon's own socket, because a
  guard that only fires in the client is a guard the daemon does not have.
- **`shadowed_by_the_client` is the one divergence, and it is real**: the
  oversized-prompt probe is refused by `bin/pmux/src/cli.rs`'s own copy of the
  limit, saying *"CLI limit"*, so the daemon's *"service limit"* message is
  **unreachable through `pmux ask`** and was reached here only over the socket.
  `bin/pmux/tests/prompt_limit.rs` is the guard that keeps the copies equal.
- **`live_checks`** carries the six model-dependent results: statelessness
  across `/clear` (`NO-PRIOR-CONTEXT`), agentic induction (`NO-TOOLS`), subagent
  spawning (`CANNOT-SPAWN`, and **0 rows with `isSidechain` true** across every
  live instance's own transcripts), NFD delivered as NFC, trailing whitespace
  trimmed and answered, U+200B kept and answered — and the **15-instance wave at
  the pool cap: 15/15 correct**.
- **`ledger`** records the digest before and after: byte-identical, because
  `pmux ask` reserves no ordinal.

**`turns.records` is one record per real turn and the budget COUNTS it** rather
than reading a stated total (`_live_adversarial_turns`). There are **45**: the
22 the results above stand on, and **23 from a first pass that ran against a
stale `target/release/pmuxd`** — discovered mid-session, replaced, and re-run.
Those 23 turns were real and are counted; their results are discarded and not
reported, because a result from a binary that is not this tree's is a result
about a different daemon.

**What it does not claim** is in `not_established`. The first entry is the one
that matters: the unrecognised-screen veto **never fired here either**, so this
file adds nothing to the firing path `screen-veto-cost-…json` also could not
reach. The wave's 51,123 ms wall time is an upper bound taken while a full-scope
mutation run held four cores, not a latency measurement.

## Linux 2026-08 Path B receipts (`x86_64`)

`linux-minified-post-answer-x86_64.json` is still pinned in
`compatibility.rs` as "NOT a promoted-profile drain receipt" (fast-path
46 ms). The promotion drain is `pooled-transcript-drain-linux-x86_64.json`.

| File | What it is |
| --- | --- |
| `pooled-transcript-drain-linux-x86_64.json` | Linux promotion drain. Path B campaign versions 2.1.227/2.1.232/2.1.233, 191 reachable arrivals, max 118 ms, bound 250 ms. Estimator saturated at the 250 ms quantum (118×2.0=236). |
| `promoted-profile-2.1.227-linux-x86_64.json` | Floor receipt for the linux cell (2.1.227-only, max 46 ms, recommends 250). |
| `promotion-2.1.236-linux-x86_64.json` | Paid ceiling: 2.1.236 `pmux run` grades, emptiness after `/clear`, 5 reachable arrivals max 46 ms against the pooled 250 ms bound. |
| `linux-minified-post-answer-x86_64.json` | Fast-path ground truth on 2.1.227 and 2.1.232 minified cells. Max reachable 46 ms. Not the promotion drain. |
| `linux-minified-system-remainder-2.1.236-x86_64.json` | REPLACE displacer on a TUI minified cell (`pmux run`, not `--print`). Cold billed **199** input / 0 cache; after `/clear` billed **288** / 0 cache (that billed 288 is what the pool pays). Remainder is a chars/4 lower bound on leftover envelope after subtracting displacer+user estimates: 176 cold, 265 after `/clear`. `post_cold_census.clearing=1` so the second turn waited on recycle, not a remint. Public `--debug-file` does not dump API bodies (`api_request_detail_emitted: false`). Process-log `No CLAUDE.md` is not model-visible. |
| `linux-minified-system-body-2.1.236-x86_64.json` | Live `/v1/messages` dump via mitmproxy (`HTTPS_PROXY` + `NODE_EXTRA_CA_CERTS`). Armed Sonnet turn still sends: billing header, `You are Claude Code, Anthropic's official CLI for Claude.`, REPLACE displacer, a user `<system-reminder>` (`userEmail`, `currentDate`), and a `messages[].role=system` `<total_tokens>` reminder. Tools/CLAUDE.md/git/cwd absent. Title gen is a separate Haiku call. Emails redacted to `<USER_EMAIL>`. Billed usage under MITM is **not** the unproxied remainder receipt (199/288). |
| `linux-minified-noclear-cache-x86_64.json` | Path A `pmux start --cell minified`, three turns, never `/clear`. Cache continuity. |
| `linux-pool-leased-sticky-x86_64.json` | Pool `Leased`: same `s{slot}e{epoch}`, T2 `cache_read` equals T1 write. |
| `linux-messages-sticky-eval-x86_64.json` | HTTP Messages sticky eval. Cache hits only above the ~1024-token floor. |
| `linux-pi-agentic-subagent-x86_64.json` | Pi on Messages: agentic tools, sequential reviewer, parallel reviewers. |

Phase 0 has been removed. `model-attempt-ledger.ndjson` is a frozen historical
ledger. Do not reseal it. Do not run `phase0.py budget`. Living pin confirmation
is `tools/dev/operator_eval.py`. Drop-flag promotion is `tools/dev/promote.py`.
