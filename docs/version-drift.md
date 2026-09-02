# Version drift: what "Claude Code updated" should cost

**Status: §1–§4 are the measurements. §5 was the proposal, and each item now says whether it is
implemented.** The ledger is untouched and nothing here spent an ordinal.

| §5 | state | where it lives now |
| --- | --- | --- |
| **P1** — pooled conservative bound, not a per-version fit | **implemented** | `tools/promotion/measure_transcript_drain.py` (repeatable `--version`, `--bound-ms`), `evidence/pooled-transcript-drain-macos-aarch64.json`, `compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit` |
| **P2** — range key plus five re-promotion triggers | **implemented** | `crates/service/src/compatibility.rs` (`ClaudeVersion`, `VersionRange`, `RepromotionTrigger`), `driver_io.rs` (`AssertEmptyRefusal`), `native.rs` (`LAUNCH_BUNDLE_REJECTED_MARKER`) |
| **P3** — split promotion into a free part and a paid part | **implemented** | living drop-flag door is `tools/dev/promote.py`; checks live in `tools/promotion/promote_claude_version.py` (free checks 1, 2, 3, 8; paid check 4). Pin confirmation is `tools/dev/operator_eval.py`, not a promotion. |
| **P4** — retain Path B evidence going forward | **implemented** | `crates/service/src/pool/evidence.rs`, `PoolConfig::evidence_dir`, `--pool-evidence-dir` / `--pool-no-evidence`, `measure_transcript_drain.py`'s `FIELDS_READ` |

The owner asked: **is our process for every Claude Code update optimal, robust and efficient?**

The measured answer is **no, and not for the reason we thought.** The process is not merely
expensive — at 13 ledger ordinals a promotion against 15 remaining it can run exactly once more,
ever — it is **unsafe when the corpus is thin**, and the free-evidence plan that was supposed to
rescue it does not have the evidence it was believed to have. Section 2 is the correction; it is
the most important section in this file.

---

## 1. STEP 1 — the tool is unblocked, and `system/api_error` is classified

### 1.1 The refusal, reproduced

```
$ python3 tools/promotion/measure_transcript_drain.py --corpus ~/.claude-1/projects --version 2.1.220
unclassified post-answer row kind(s); add them to ROW_KINDS with a reason: {"system/api_error": 9}
$ echo $?
2
```

Identical for `--version 2.1.223`. The same root is the one the owner's briefing quoted. A second,
larger refusal exists on `~/.claude/projects` for 2.1.201 and below and is **not** resolved here —
see §1.4.

### 1.2 What those rows actually are

98 `system/api_error` rows exist across both roots; 87 of them fall after a turn's final assistant
row in **file** order. Read whole, they are HTTP-client retry records:

| field | what it holds |
| --- | --- |
| `error.message` | `Connection error.` ×67, `Request timed out.` ×9, `529 … overloaded_error` ×9, `401 …` ×2 |
| `retryAttempt` / `maxRetries` | 1–10 of 10 — attempt 1 on 47 rows, attempt 10 on 2 |
| `entrypoint` | `sdk-ts` on all 87 |
| `retryInMs` | the backoff the client is about to sleep |

So a minified cell **can** hit one: nothing in `--disallowedTools "*" --strict-mcp-config`
removes the CLI's own HTTP client. That is not the question the table asks, though.
`ROW_KINDS` asks whether a minified cell can produce the row **after the answer** — which is why
`("user", None)` is already classified unreachable even though minified cells obviously write user
rows.

### 1.3 The decision, and the measurement it rests on

**An `api_error` row is not a post-answer arrival at all.** Its `timestamp` is the moment the HTTP
call failed, not the moment the row was appended. Comparing each of the 87 against its own turn's
final assistant row:

```
n = 87   strictly after the final assistant timestamp: 0
nearest  -1,288 ms      median  -185,807 ms      farthest  -24,379,049 ms
```

Every one is stamped **before** the answer — the nearest by 1.3 seconds, the median by three
minutes. The answer they precede is the row the successful retry produced. A record of a failure
that predates the answer cannot delay an answer that is already written.

They land *after* the answer in file order because the JSONL append order does not follow the
`timestamp` field, and that reordering is a queue-bearing-session property:

| files | consecutive row pairs | timestamp inversions | `api_error` among them |
| --- | --- | --- | --- |
| containing no `queue-operation` row | 143,609 | 95 (0.066 %) | **0** |
| containing a `queue-operation` row | 28,729 | 377 (1.312 %) | 43 |

All 98 `api_error` rows in this corpus live in files that also hold `queue-operation` rows. A
minified cell has no queue. So the entry is:

`tools/promotion/measure_transcript_drain.py:152` — `("system", "api_error")`, `reachable: False`,
`retrospective: True`, with the reason above written out in full.

The case a retry *does* produce a further answer is already guarded, and by the right entry:
`("assistant", None)` is classified reachable with *"A SEMANTIC ROW AFTER THE ANSWER … observing
one retracts any measured value taken without it"*. None was observed.

### 1.4 The exclusion is a checked premise, not a claim

An argument in a comment is the house bug class. `retrospective` is therefore a column the tool
**tests**: `post_answer_arrivals` now also returns each row's offset from the terminal candidate
(`measure_transcript_drain.py:421`), and `main` fails the run — new exit code **3**, distinct from
the unclassified-kind exit 2 — if any row of a `retrospective` kind is ever stamped after the
candidate. The premise is counted at `measure_transcript_drain.py:529` (`since_candidate > 0` on a `retrospective` row) and returned as
`EXIT_RETROSPECTIVE_PREMISE_BROKEN` at `measure_transcript_drain.py:298`.

Proven able to fail: flipping the predicate `since_candidate > 0` at
`measure_transcript_drain.py:529` to
`< 0` turns 2.1.223 red with `{"system/api_error": 9}` and exit 3; restoring it returns exit 0. The
receipt also publishes `timestamp_is_retrospective` per bucket
(`measure_transcript_drain.py:565`), because a negative `min_ms`
in a table of arrivals otherwise has no explanation.

**The refusal was not weakened for anything else.** `pr-link/None` (101), `system/compact_boundary`
(2) and `system/model_refusal_fallback` (10) remain unclassified and 2.1.201 and below remain
refused. `model_refusal_fallback` deserves its own look and not a hasty one: it carries
`direction: retry`, `retractedMessageUuids` and a `fallbackModel`, i.e. it **retracts** assistant
messages already written. That is the invalidating condition `compatibility.rs:469-471` names — *"A
semantic row … arriving after a turn's final assistant row"*, and *"One is enough to retract this value"*.
It should be classified deliberately, not in passing.

### 1.5 The promoted receipt is unchanged

Re-running the tool over the 2.1.220 corpus reproduces `evidence/promoted-profile-2.1.220-macos-aarch64.json`
in every measured field — 189 files, 456 turns, 189 reachable arrivals, max 438 ms, recommendation
1000 ms. The only diffs are `files_scanned` 1195 → 1146 (host transcripts have since been cleaned
up) and the new `timestamp_is_retrospective` key. The committed receipt was left alone;
`compatibility::tests::every_promoted_drain_is_the_one_its_receipt_recommends` still passes.

---

## 2. The correction: the free corpus is a tenth of what the briefing says

The briefing's table gives a `turn_duration` column summing to **2,344** "completed turns across
nine versions", with 1,348 at 2.1.201 and 814 at 2.1.220.

**The corpus holds 219 `system/turn_duration` rows in total, across both roots.** Counted three
ways — by row-version, by file-level admission, and by raw text match — the answer is 219. I could
not reproduce 1,348 / 814 / 138 from any metric I tried (turn_duration rows, turns, post-answer
arrivals, assistant rows, prompt rows, `stop_reason` tallies, `durationMs`-bearing rows), under
either row-level or file-level version attribution. The briefing's file counts reproduce to within
1–2 files; its turn column does not reproduce at all. I am reporting this as unreconciled rather
than explaining it away.

### 2.1 Why the number is small: the marker is a `cli` feature

Every one of the 219 `turn_duration` rows carries `entrypoint: cli`. **Zero** appear on `sdk-ts` or
`sdk-cli`.

| version | `sdk-ts` rows | `cli` rows | `sdk-cli` rows | `turn_duration` rows |
| --- | --- | --- | --- | --- |
| 2.1.156 | 7,575 | 0 | 0 | 0 |
| 2.1.170 | 15,265 | 0 | 0 | 0 |
| 2.1.197 | 7,155 | 0 | 0 | 0 |
| 2.1.201 | 72,915 | 2 | 111 | 0 |
| 2.1.207 | 0 | 18 | 12 | 3 |
| 2.1.215 | 0 | 324 | 0 | 36 |
| 2.1.216 | 0 | 3 | 0 | 0 |
| 2.1.220 | 43,125 | 1,403 | 197 | 179 |
| 2.1.223 | 21,126 | 6 | 0 | 1 |

`cli` is **1.04 %** of the 169,237 versioned rows that name an entrypoint. The apparent finding
"`turn_duration` did not exist before 2.1.207" is not a version fact — it is entirely an entrypoint
confound. The corpus is overwhelmingly Claude Agent SDK sessions, and the SDK entrypoint does not
write the marker.

A pmux Path B cell is a `cli` cell. Its own probe directories confirm it: `pmux-drain-cwd`
(504 rows, 89 markers), `clearprobe-cwd` (420, 60), `pmux-gate-b-cwd` (136, 11), `pmux-phase12-cwd`
(99, 11) — all `entrypoint: cli`, all 2.1.220.

### 2.2 The "free" 2.1.220 corpus is the paid campaign's residue

171 of the 179 markers at 2.1.220, and **178 of the 186 reachable post-answer arrivals**, come from
pmux's own campaign directories. Only 8 arrivals at 2.1.220 come from anywhere else on this host.

This is the load-bearing consequence: **the 2.1.220 profile could be built "for free" only because
a Gate B campaign had just been run at 2.1.220.** The transcripts were free; the turns that wrote
them were not. Re-analysis of transcripts is not a substitute for a campaign at a new version,
because at a new version there are no `cli` turns to re-analyse.

---

## 3. STEP 2 — the measurements

Turns are attributed to the version stamped on **their own terminal candidate**. The shipped tool
admits a *file* if any row names the version and then reads every row in it; one 18,118-row file on
this host spans 2.1.156/170/197/201 and is admitted four times over, which is where the identical
unclassified-kind counts for those three versions come from.

### 3.1 Reachable post-answer arrivals, per version

| version | files w/ turns | turns | reachable arrivals | median | p90 | p95 | p99 | **max** | > 1000 ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| ≤ 2.1.201 | — | 6,091 | **0** | — | — | — | — | — | — |
| 2.1.207 | 3 | 6 | 3 | 24 | 41 | 41 | 41 | **41** | 0 |
| 2.1.215 | 31 | 77 | 36 | 55 | 171 | 315 | 338 | **338** | 0 |
| 2.1.220 | 211 | 1,015 | 186 | 41 | 115 | 202 | 344 | **438** | 0 |
| 2.1.223 | 2 | 207 | **1** | 57 | 57 | 57 | 57 | **57** | 0 |

Everything at 2.1.201 and below yields zero reachable arrivals — not a small sample, **no sample**.

### 3.2 What today's process would promote for 2.1.223 — the unsafe part

Run the shipped tool, unmodified except for §1's classification:

| invocation | turns | reachable | max | **recommends** |
| --- | --- | --- | --- | --- |
| `--corpus ~/.claude/projects --version 2.1.220` | 456 | 189 | 438 ms | **1000 ms** |
| `--corpus ~/.claude/projects --version 2.1.215` | 159 | 41 | 338 ms | **750 ms** |
| `--corpus ~/.claude/projects --version 2.1.207` | 6 | 3 | 41 ms | **250 ms** |
| `--corpus ~/.claude-1/projects --version 2.1.223` | 786 | **1** | 57 ms | **250 ms** |

**Promoting 2.1.223 from its free corpus today would ship a 250 ms drain.** That is 188 ms below an
arrival already observed one version earlier, equal to `TURN_DURATION_DRAIN_FLOOR_MS`
(`crates/service/src/v1/backend.rs:305`), and below `POST_MARKER_CATCH_WINDOW_FLOOR_MS = 438`
(`POST_MARKER_CATCH_WINDOW_FLOOR_MS`, `crates/service/src/v1/backend.rs:375`) — the constant that exists precisely to keep a 438 ms
arrival catchable. The per-version
fit does not merely cost ordinals; on a thin corpus it produces a number that truncates answers.

### 3.3 Q1 — the drain does not move between versions; the estimate moves with sample size

Only 2.1.215 (n=36) and 2.1.220 (n=186) carry enough arrivals to compare.

- **Between versions:** max 338 vs 438 → a spread of **100 ms**.
- **Within one version:** split each version's sessions into random halves, 400 splits, and take
  `|max(A) − max(B)|`: 2.1.215 median 23, p95 **216**, max 271; 2.1.220 median 94, p95 **176**, max
  198.

The between-version spread is **smaller than the within-version p95** for both. Two further tests:

- **Sample-size control.** Draw n = 36 (2.1.215's sample size) from 2.1.220's 186 arrivals, 20,000
  times: the max distribution is p5 = 141, median = **295**, p95 = 438. 2.1.215's observed max of
  338 sits at the **65th percentile** of that distribution — exactly where a 36-arrival draw from
  2.1.220 lands.
- **Permutation test** on the difference in maxima, labels shuffled 20,000 times: **p = 0.730**.

The medians differ by 13 ms at p = 0.046, which is weak and, more to the point, fully confounded:
2.1.215's arrivals are 100 % ordinary interactive sessions while 2.1.220's are 96 % pmux campaign
turns. Workload is not separable from version in this corpus, and workload is the larger effect
(2.1.220 campaign median 43 / max 438 vs non-campaign median 31 / max 106).

**Finding: the per-version pin is measuring noise.** The apparent 2.1.215 → 2.1.220 drift is a
sample-size artifact.

### 3.4 Q2 — 1000 ms would have been safe everywhere there is evidence

**0 of 226** reachable arrivals across all four versions exceed 1000 ms. Nothing exceeds 500 ms
either; the pooled maximum is the same 438 ms, at 2.1.220.

For 2.1.201 and earlier the honest answer is **unestablished, not safe** — those versions have no
`cli` turns at all.

### 3.5 The estimator, and the direction small samples fail in

**Conservative estimator, stated:** the maximum reachable post-answer arrival, pooled over every
observed version, multiplied by `RECOMMENDATION_MARGIN = 2.0` and rounded up to
`RECOMMENDATION_STEP_MS = 250`. Not a percentile — a percentile over 226 samples discards the tail,
and the tail is the whole subject.

The tail is light. Resampling without replacement from the pooled 226:

| n | 4 | 8 | 16 | 32 | 64 | 128 | 200 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| E[max] ms | 131 | 181 | 242 | 301 | 350 | 394 | 427 |

`E[max] = 76.4·ln(n) + 27.2`, **R² = 0.9975** — the logarithmic growth of an exponential tail. Mean
excess over a threshold is flat-to-declining (104 ms at u = 100, 93 at u = 200, 50 at u = 300),
which is the same conclusion: light-tailed, not heavy.

That gives a price for the headroom nobody had costed:

| bound | is the *expected* maximum at |
| --- | --- |
| 500 ms | 486 arrivals |
| 750 ms | 12,788 arrivals |
| **1000 ms (shipped)** | **336,786 arrivals** |
| 1250 ms | 8.9 M arrivals |
| 2000 ms (`DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS`, `compatibility.rs:12`) | 1.6 × 10¹¹ arrivals |

So 1000 ms is not "2.28× the max" in any useful sense; it is roughly a **one-truncation-per-337,000-
unmarked-turns** bound. That is a defensible product number, and it is the number already shipped.

And the direction of small-sample error, quantified: the fit puts E[max] at n = 36 at **301 ms**
against the 438 ms actually present in the population — a bound fitted from a 36-arrival version
is **31 % low**. At n = 1, which is 2.1.223 today, it is **87 % low**. Small samples under-estimate
a tail maximum, and a fitted per-version bound is therefore wrong in exactly the direction that
truncates answers.

### 3.6 Q3 — which fields are version-sensitive

The profile has seven fields (`TestedCompatibilityProfile`, `compatibility.rs:547`):
`claude_version`, `claude_version_tested_through`, `os`, `arch`, `terminal_profile`,
`input_transport`, `transcript_drain_ms`. **It had six when this section was written**; the seventh
is the range ceiling P2 below added, and the count is corrected here rather than left as the kind of
stale total this document exists to catch. The promoted-side struct `PromotedProfile`
(`compatibility.rs:330`) has nine — the same seven with the floor renamed and two provenance
sentences.

**Transcripts can answer one of them.** `terminal_profile` and `input_transport` are structural
choices, not measurements; `os`/`arch` are the host. So the corpus speaks to `transcript_drain_ms`
and to nothing else in the profile.

What the corpus *does* say about drift:

- **Row-kind vocabulary moves.** `stop_hook_summary` first appears at 2.1.220 (7 rows, all pmux's
  own hook); `model_refusal_fallback` only at 2.1.201; `local_command` at 2.1.201/215/220.
- **Top-level keys are added at nearly every version**: 2.1.207 `+durationMs, messageCount,
  session_id`; 2.1.215 `+effort, interruptedMessageId`; 2.1.220 `+hookCount, hookErrors, hookInfos,
  stopReason, toolUseID, …`; 2.1.223 `+source`. **Caveat:** first appearance in this corpus is a
  lower bound on introduction and is confounded with workload — a key appears when a feature is
  *used*, not when it ships.
- **The `turn_duration` payload is invariant** across 2.1.207/215/220/223: identical key sets bar
  `slug`, and `pendingWorkflowCount` absent on all 219 rows. That independently corroborates
  `crates/claude/src/parser.rs:279-280` — *"written by 2.1.177 and is absent on 2.1.207+"*.

**The asymmetry worth acting on.** `transcript_drain_ms` is the only version-keyed quantity in the
profile, and §3.3 shows it barely moves. Meanwhile pmux carries a dozen constants measured on
2.1.220 and keyed to **no version at all**:

- `driver_io.rs:173-174` — *"MEASURED on 2.1.220: rule/composer/rule/footer at rows 4-7, cursor at
  (5,2), rows 8-23 of length zero"*
- `driver_io.rs:215` — the 5-row post-`/clear` preamble, *"MEASURED on Claude Code 2.1.220 over 61
  post-`/clear` transcripts"*
- `crates/rmux/src/backend.rs:172-173` — *"MEASURED on Claude Code 2.1.220, that menu marks its
  selected row with a foreground colour change"*
- `crates/service/src/claude_launch.rs:1345-1347` — the `--effort` level vocabulary, *"MEASURED against
  Claude Code 2.1.220 (aarch64 macOS), 2026-08-04, three ways that agree"*

**The version gate protects the quantity that is stable and does not protect the ones a UI change
breaks.** These are screen and launch properties; transcripts cannot answer them, and no amount of
corpus re-analysis will.

---

## 4. What Gate B proves that the corpus does not

From `evidence/gate-b-drain-calibration.json`: **13 distinct ordinals (31–43)**, 17 attempts, 10
successful, 7 failed. All 7 failures were harness failures — `pmux run exited with 1` ×6, `frozen
source changed during the campaign` ×1 — not Claude's.

Its drain yield: `late_row_attempts` **0** of 10, maximum observed gap **1 ms**, at a configured
drain of 2000 ms. So the campaign contributed **10 drain samples** where the corpus at the same
version contributed **189**. *Gate B is not how you learn the drain, and 13 ordinals is not what the
drain costs.*

What Gate B alone proves, and the corpus cannot:

1. **The minified launch bundle is still accepted at this version** — `--disallowedTools "*"`,
   `--strict-mcp-config`, the env bundle. A renamed or removed flag is a total
   failure and is invisible in JSONL.
2. **The answer pmux returns is byte-identical to what Claude produced** — the hash oracle over 9
   grades including long and unicode payloads. No transcript proves what crossed pmux's own reader.
3. **The whole stack completes at this version** — pane, prompt injection, screen correlation,
   `/clear` rebind, completion gate.

---

## 5. STEP 3 — the proposed protocol

This was the proposal. Each heading below records whether it is implemented.

### P1. Stop fitting the drain per version. Keep 1000 ms. — **IMPLEMENTED**

The pooled estimator of §3.5 — max over *every* observed version, × 2.0, rounded up to 250 —
returns **exactly 1000 ms**, the number already shipped. Widening the key from one version string
to a range therefore requires changing **no number**. This is the whole trade: the drain becomes a
conservative bound instead of a fit, and a conservative bound is only wrong in the direction of
latency.

Latency cost, priced: the full drain binds only on turns with no `turn_duration` marker, which is
**166 of 385 `cli` turns (43 %)** in this corpus; a marked turn already owes 250 ms
(`TURN_DURATION_DRAIN_FLOOR_MS`, `crates/service/src/v1/backend.rs:305`). Going beyond 1000 ms is *not* free — 1250 ms would cost 250 ms on 43 % of turns
to buy 4.5 M arrivals instead of 337 K. I do not recommend it; I recommend recording the price.

**What was built.**

- `--version` on `measure_transcript_drain.py` is now **repeatable**, and the recommendation is
  taken over the pooled maximum of every version named. The receipt publishes
  `per_version_recommendations_not_to_be_shipped` — `{2.1.207: 250, 2.1.215: 750, 2.1.220: 1000,
  2.1.223: 250}` — beside the pooled `1000`, because the difference between those numbers *is* the
  argument and a receipt that hid it would be asking to be re-fitted.
- **The price is measured, not asserted.** `full_drain_binds_on` in the receipt reproduces
  §5's figure exactly from the corpus: 385 `cli` turns that reached a terminal candidate, **166**
  of them with no `turn_duration` marker, fraction **0.431**. A turn with no assistant row is
  excluded — it never reached a candidate, so no drain was ever charged to it.
- `evidence/pooled-transcript-drain-macos-aarch64.json` is the committed receipt: 425 files, 1,336
  turns, 226 reachable arrivals, max **438 ms**, recommendation **1000 ms**.
- `compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit`
  re-derives the recommendation from the receipt's **own** `recommendation_basis.margin` and
  `.rounded_up_to_ms`, so no Rust constant repeats `RECOMMENDATION_MARGIN` or
  `RECOMMENDATION_STEP_MS`. It refuses a receipt pooled over fewer than two versions, refuses a
  named version that contributed no arrival, refuses a receipt in which no per-version fit is
  strictly below the pooled bound, and refuses a `drain_provenance` that does not quote the
  receipt's own 438 / 1000 / 385 / 166. Proven able to fail three ways: moving the shipped drain to
  1250 ms, replacing `166 of 385 cli turns` in the provenance with prose, and rewriting the receipt
  to name one version each turn it red.
- **`--bound-ms` makes trigger 2 a check rather than a reading.** Given the drain already shipped,
  the tool exits **4** when a reachable arrival exceeds it. And it exits **5**, distinctly, when
  there was *nothing to check* — which is the failure mode this tool is most likely to have, since
  a brand-new Claude Code version has no `cli` turns yet (§2.1) and "passed" and "found nothing"
  were previously the same exit code. Reproduced: `--bound-ms 400` against 2.1.220 exits 4 naming
  the 438 ms arrival; `--version 2.1.226 --bound-ms 1000` exits 5 today, on this host, because the
  corpus holds **zero** 2.1.226 rows.

### P2. Key the profile to a range, with a stated re-promotion rule. — **IMPLEMENTED**

Replace the exact-string match (cited against the tree before this was implemented;
`validate_exact_version` no longer exists, and `compatibility.rs:32` records in its own words that it
is "the whole of what `validate_exact_version` used to do") with a
tested-floor plus tested-through ceiling, or a named compatibility class. **What forces
re-promotion — and each of these is already detectable:**

1. `measure_transcript_drain.py` reports an unclassified row kind, or a `retrospective` kind stamped
   after the candidate (exit 2 / exit 3).
2. Any reachable arrival above the bound.
3. The minified launch bundle is rejected by the child.
4. The `/clear` screen or preamble does not match (`MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR`, `driver_io.rs:165-183`; `MAX_ASSERT_EMPTY_ROWS`, `driver_io.rs:215-220`).
5. A major or minor version change, rather than a patch — a conservative default, not a measurement.

**What was built.**

`validate_exact_version` is gone. `ClaudeVersion` parses the three components and keeps them —
a validator that throws its parse away is how a version comes to be compared as a string three
lines later, and `2.1.99` above `2.1.207` is the one bug a range key is guaranteed to have if it
ever is. `VersionRange { floor, tested_through }` is inclusive at both ends and
`TestedCompatibilityProfile::matches` asks it for containment.

**The shipped range is `2.1.220..=2.1.258`** (`2.1.220..=2.1.226` from 2026-08-09 to 2026-08-11;
`2.1.220..=2.1.227` from 2026-08-11; `2.1.220..=2.1.238` from 2026-08-21;
`2.1.220..=2.1.258` from 2026-09-01).
The floor is where the evidence starts: 2.1.220 has
the drain receipt, the Gate B campaign and the screen/preamble measurements, and §3.1 shows 2.1.201
and earlier at *zero* reachable `cli` arrivals — unestablished, not safe. The ceiling is where the
evidence stops. 2.1.226 and 2.1.227 were measured as an A/B of the launch bundle, the post-`/clear`
frame at 2 rendered rows below the cursor, the 5-row preamble in rows and order, the
local-command menu's foreground-only selection, the `--effort` vocabulary, and a pool mint
reaching `state: ready` — `docs/2.1.226-compatibility.md` and `docs/2.1.227-compatibility.md`.
2.1.238 is a different measurement: `promote_claude_version.py` grades plus drain on minified
cells (`evidence/promotion-2.1.238-macos-aarch64.json`), and the `/clear` menu is proven in
`driver_io.rs` as a new geometry (candidates above the composer, indent 2, unselected rows also
uniform; selection is composer-command colour match, not “unique uniform row”). The same
menu-above geometry was measured on linux at 2.1.257, whose promotion widened the linux range to
`2.1.227..=2.1.257` (`evidence/promotion-2.1.257-linux-x86_64.json`, corpus
`crates/service/tests/corpus/claude-2.1.257-clear-menu.ndjson`). The
2.1.226/2.1.227 A/B was not re-run at 238. **2.1.258 widened the macos range again on 2026-09-01**
(`evidence/promotion-2.1.258-macos-aarch64.json`): no new transcript row kind over 2.1.257's, the
same menu-above geometry recorded as
`crates/service/tests/corpus/claude-2.1.258-clear-menu.ndjson` and replayed through the unchanged
proof, and a launch-bundle A/B against 2.1.251 that adds only the `--system-prompt-snapshot`
option 2.1.257 already introduced and rewords the background `--resume` help.
`evidence/promotion-2.1.238-macos-aarch64.json` is the prior ceiling and stays historical. The
drain is **not** measurable from the free corpus at a fresh version — it held zero 2.1.226 rows on
the day 2.1.226 was promoted and zero 2.1.227 rows on the day 2.1.227 was, and
`measure_transcript_drain.py` exits 5 rather than passing — and that is precisely what the pooled
bound of P1 is for. What answers it instead is the daemon's OWN evidence mirror, written by the
promotion run: 5 reachable arrivals at 2.1.258, max **42 ms** (median 25, min 19); 2.1.238 was 5
arrivals, max 54 ms, and 2.1.227 was 5 arrivals, max 52 ms. `range_provenance` on the profile says
all of that, and the daemon publishes it.

Four properties the range key has that the string key did not, each with a test that fails without
it:

- **It never spans a minor.** `VersionRange::new` refuses a floor and a ceiling on different
  `major.minor` lines, and because the bounds share a line, ordered containment refuses another line
  *for free* — there is no second `same_line` clause in `admits` to forget to update. That is
  trigger 5, as a predicate rather than a policy sentence.
- **It does not open backward.** One patch below the floor is refused, asserted in the same loop as
  one patch above the ceiling.
- **Every patch inside it is admitted.** Asserted over the whole range, not its endpoints: a
  containment predicate that admits its endpoints and nothing between them passes a two-endpoint
  test and refuses most of the range in production.
- **Overlapping cells are refused at boot.** Once the key is a range, "duplicate" means overlapping,
  not equal. `2.1.220..=2.1.226` and `2.1.223..=2.1.230` are exactly as ambiguous as two identical
  cells. Adjacent ranges are admitted, because two measurements that partition a line are a
  legitimate thing to hold.

An operator's `--tested-claude-profile` gains an OPTIONAL `claude_version_tested_through`. Absent
means `claude_version`, i.e. an exact match, which is what every profile written before this field
existed meant and still means.

**The five triggers are values, not sentences.** `compatibility::RepromotionTrigger` has five
variants; each carries the FILE and the SYMBOL that detects it, and
`every_repromotion_trigger_names_a_detector_that_exists` opens each file and fails when the symbol is
not in it. `every_repromotion_trigger_is_in_all_exactly_once` uses a wildcard-free `match`, so a
sixth trigger stops the crate compiling until somebody says where it is detected. The daemon
publishes all five, with what to do about each, in the configuration layer of the health tree.

| # | trigger | detector | new? |
| --- | --- | --- | --- |
| 1 | `unclassified_transcript_row_kind` | `measure_transcript_drain.py` `TRIGGER_UNCLASSIFIED_ROW_KIND`, exits 2 / 3 | existed, now named |
| 2 | `reachable_arrival_above_the_bound` | `measure_transcript_drain.py` `TRIGGER_ARRIVAL_ABOVE_THE_BOUND`, exits 4 / 5 | **new check** (P1) |
| 3 | `launch_bundle_rejected` | `native.rs` `LAUNCH_BUNDLE_REJECTED_MARKER` | **new** |
| 4 | `clear_screen_or_preamble_mismatch` | `driver_io.rs` `is_a_version_drift_signal` | **new** |
| 5 | `major_or_minor_version_change` | `compatibility.rs` `same_line` | **new** |

**Trigger 3.** `startup_screen_diagnostics` gains one structural boolean,
`child_rejected_a_launch_flag`, and names the trigger when it is true. The marker is
`unknown option`, MEASURED on this host at 2.1.223 and 2.1.226 byte-identically: `claude
--pmux-probe-sentinel doctor` prints `error: unknown option '--pmux-probe-sentinel'` on stderr,
exits 1 with empty stdout, and the commander exits *before* the subcommand runs — so the probe that
established it executed nothing and spent no ordinal. Until now a child that refused a flag and a
Claude that was merely slow produced the identical `NeedsInput` refusal. The screen text is still
never reproduced: the boolean is a fact, not an excerpt, and the test asserts the flag name does not
appear in the refusal.

**Trigger 4, which is the one that matters most.** It half-existed: `clear_selected_wrong_local_command`
tested `reason == "wrong_local_command"`, and the pool halted on it because — its own doc —
*"it means pmux's model of the composer no longer matches the installed Claude, and every other
instance is typing `/clear` into the same composer."* **That sentence is true of six other refusal
reasons and none of them was tested for.** A cleared preamble carrying a metadata record type pmux
has never seen, a `system` row whose subtype is not `local_command`, a line the parser cannot parse,
a row kind it does not recognise, a third `user` row, or more rows than the preamble has ever had —
each is Claude writing a post-`/clear` preamble that is not the one `MAX_ASSERT_EMPTY_ROWS` (`driver_io.rs:215-220`) was MEASURED
against, and each is a fact about the *installed Claude* that every other pool instance is about to
hit. Each quarantined one instance while the pool minted the next one into the identical drift.

The thirteen string literals are now `AssertEmptyRefusal`, a fourteen-variant enum whose
`is_a_version_drift_signal` is a wildcard-free `match`, so a new refusal reason cannot be added
without answering the question. Seven are drift and halt the pool, carrying WHICH reason so the
operator is sent to the part of the preamble that moved; seven are not, and each of those has a
stated reason rather than a default — a byte budget checked before any parse can fire on a large
leaked file, a `clear_command_missing` is a deadline expiring and is indistinguishable from a slow
clear, a `preamble_not_settled` is a stalled writer, an `unexpected_clear_echo` is an identity fact,
and a prompt / a turn marker / a semantic row are content, which is a leak and a leak is one
instance.

One wire value changed deliberately: the BYTE-budget site reported `row_budget_exceeded` while
publishing `bytes` and `byte_budget` — a refusal whose reason names a different quantity from the
one it measured. It is now `byte_budget_exceeded`, and it is the member of the pair that is *not*
drift.

Reproduced at HEAD's classification, by narrowing `is_a_version_drift_signal` back to
`WrongLocalCommand` alone: `driver_io::tests::a_preamble_that_moved_is_a_repromotion_trigger_and_a_leak_is_not`
fails with `["wrong_local_command"]`,
`path_b_pool.rs::every_preamble_mismatch_halts_the_whole_pool_and_not_only_a_mis_selected_command`
fails with the same list, and
`driver_io::tests::a_successor_carrying_metadata_that_is_not_preamble_is_refused` — which drives a
whole rotation, preamble read and all — fails on the trigger it should have carried. Restoring the
classification returns all three to green.

### P3. Split promotion into a free part and a paid part. — **IMPLEMENTED**

Living drop-flag door is `tools/dev/promote.py`. Pin confirmation is
`tools/dev/operator_eval.py`. The checks themselves live in
`tools/promotion/promote_claude_version.py`, as one ordered runnable path.
`--describe` prints the check list, its pass criteria, and which of the five re-promotion triggers
each check exercises, without running anything.

**Free, 0 ordinals — checks 1, 2, 3 and 8.** The version-identity rule, a non-executing parse probe
of every flag in the minified launch bundle, a declared warm `SessionCell::Minified` instance
reaching `idle`, and `measure_transcript_drain.py` over the daemon's own evidence mirror. The
corpus part fails closed on an unknown row kind and on a broken `retrospective` premise, which is
exactly the schema-drift detector §3.6 shows is needed; its honest limit is §2, and the promotion
path treats **exit 5 — "there was nothing to check" — as a FAILURE**, because a promotion that
measured no arrival at the version it promotes has measured nothing. P4's mirror is what stops that
from being the normal case.

**Paid — check 4, and it is not a campaign.** Four grades on one instance plus one per additional
effort: five turns at the default `--effort low --effort high`. The earlier draft of this paragraph
proposed *"trivial, unicode-hash, long-output"*, and **the middle one cannot be run on the cell that
needs promoting** — a `--disallowedTools "*"` cell cannot execute `shasum`, which is the same reason
Historical Phase 0 prompts `03-09` (now deleted) were unusable here. The shipped suite replaces the hash with an exact
**unicode echo**: a nonce plus non-ASCII text returned byte for byte, which proves the same delivery
property with an oracle a toolless cell can satisfy. Every grade is `nonce + a result the prompt
makes computable`, checked by equality, and instance reuse is proven by **pid identity**.

**What it must never do is fit the drain.** The bound it asserts against is read from
`evidence/pooled-transcript-drain-<os>-<arch>.json`'s own `recommended_transcript_drain_ms` — the
number `compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit`
already pins to the shipped profile — and `--bound-ms` is deliberately not an option. §3.2 said a
thin per-version corpus fits noise; **three runs of this path at 2.1.226 on one host within half an
hour fitted 250 ms, 500 ms and 500 ms**, with the maximum reachable arrival moving 29 → 192 →
223 ms as the load average moved 4.2 → 6.8 → 5.5. All three are far inside the pooled 1000 ms
bound; any of them, shipped, would be a promotion built on the last five turns anyone happened to
run — and the smallest is below `POST_MARKER_CATCH_WINDOW_FLOOR_MS = 438`.

### P4. Make the free part actually free, going forward. — **IMPLEMENTED**

pmux cells write `cli`-entrypoint transcripts with markers. Today that evidence is a by-product of
campaign directories under `~/pmux-drain-campaigns` and `/private/tmp`. If ordinary Path B traffic
under `allow_untested` is retained the same way, the corpus for version N+1 accumulates *before*
promotion is needed, and P3's free part stops being empty at exactly the moment it matters. This is
the only proposal here that changes the shape of the problem rather than its price.

**What was built.** Every destroyed pool instance's transcripts are mirrored into a corpus before
the tree is erased. The four things the owner asked to have stated:

- **Where it writes.** `<socket parent>/pool-evidence/`, beside `logs/` and derived
  through the same `daemon_sibling_dir` those two are, owner-only at every level pmux creates. It is
  deliberately outside the pool parent, held to the rule `--pool-retain-dir` already had, because
  it is written from a config root the next line erases. `--pool-evidence-dir` moves it,
  `--pool-no-evidence` turns it off, and the running daemon publishes the answer in
  `configuration.path_b.evidence_dir` — so "on by default" is a claim the daemon answers rather than
  one this document makes.
- **When.** In `Pool::destroy`, after the process is proven reaped and before `erase_tree`. That is
  the only window in which the file exists and nothing is writing to it, and it is off the turn
  path, so it costs no turn latency. A failure is logged and never fatal: a teardown that refused to
  erase a config root because a mirror could not be written would trade a guarantee for a
  convenience.
- **What it retains — and this is the part that is derived rather than judged.** Not the transcript:
  a mirror pruned to `evidence::RETAINED_ROW_FIELDS`, which is **exactly** the set
  `measure_transcript_drain.py` reads, published there as `FIELDS_READ` — `entrypoint`, `isMeta`,
  `isSidechain`, `promptId`, `subtype`, `timestamp`, `type`, `version`. Eight keys, none of which
  can hold a prompt or a completion. Choosing eight fields that "look safe" is a judgement nobody
  can check; taking the eight the only consumer reads is a fact, and
  `evidence::tests::the_retained_fields_are_the_ones_the_measurement_tool_reads` reads the Python
  file to establish it. The tool now **prunes every row to `FIELDS_READ` on the way in**, so that
  constant is load-bearing in the tool itself rather than decorative, and nothing downstream can
  read a prompt even by accident.
- **Bounded.** 64 MiB, enforced after each write by deleting oldest-first (by mtime — a session uuid
  carries no order, so a name-ordered prune would delete an arbitrary file rather than the least
  useful one). The number is measured, not picked: mirroring the 189 transcripts behind the 2.1.220
  receipt through this field set produced **271,497 bytes**, 1,437 bytes per transcript, so 64 MiB
  is on the order of **46,000 transcripts** — two orders of magnitude past the 425 behind the
  shipped bound.

**The mirror is not an approximation.** Running `measure_transcript_drain.py` over mirrors of those
189 transcripts reproduced `post_answer_arrivals`, `recommended_transcript_drain_ms`,
`full_drain_binds_on`, `partition_balances` and the 456-turn count **identically** to the receipt
taken from the originals. The tool cannot tell the difference, because it never reads a field the
mirror dropped.

Proven able to fail: making `Pool::destroy` skip the retention call reddens
`path_b_pool.rs::ordinary_path_b_traffic_retains_its_own_drain_evidence_and_no_content`; adding
`message` to `RETAINED_ROW_FIELDS` reddens both the cross-language field check and the redaction
assertion, the latter printing the caller's prompt back out of the mirror. The off switch is
asserted against the directory that already holds one mirror, so it cannot pass with the feature
deleted.

### Cost in ordinals

| protocol | per new patch version | nine versions |
| --- | --- | --- |
| **today** (Gate B drain calibration) | 13 | 117 — exceeds the 100 ceiling; budget is 85 consumed / 15 remaining |
| **P1 + P2** (range key, conservative bound) | **0** inside the range | 0 |
| **P3** at a range boundary | **5**, measured — `promote_claude_version.py` at its default `--effort low --effort high`, three times at 2.1.226 and once at 2.1.227 | 5 per boundary crossed |

The 5 is the tool's own turn count, not an estimate: four grades on one instance plus one per
additional effort. It reserves no ledger ordinal, and the receipt says so in
`real_claude_turns` — which is the key a since-deleted Phase 0 budget report
used to read to report how far the ledger under-counts real exposure.

**What that census said when measured, not a living command.** Phase 0 is
deleted; do not run `phase0.py budget`. The figures below are the historical
report against `evidence/model-attempt-ledger.ndjson`:

| figure | value |
| --- | ---: |
| `consumed` / `remaining` | **85 / 15** |
| `real_turns_outside_the_ledger.total` | **54** |
| — `turn-latency-2.1.220-macos-aarch64.json` | 44 |
| — `promotion-2.1.226-macos-aarch64.json` | 5 |
| — `promotion-2.1.227-macos-aarch64.json` | 5 |

So the ledger's 85 is not the count of real Claude turns this repository has caused; **139 is the
lower bound**, and 54 of them are outside the ceiling that governs the other 85. The 54 is **never
folded into `consumed`** — whether the 100-turn ceiling covers instrument runs as well as campaigns
is decision **D4**, and it is the owner's. The figure exists so that question is asked against a
number instead of a feeling.

It is a **lower bound and the tool says so in its own output**: the `PMUX_POOL_REAL_CLAUDE` e2e lanes
reach a real model, reserve no ordinal and leave no receipt, so nothing can count them. An
unclassifiable receipt stops the count rather than being skipped.

---

## 6. What should still refuse

A refusal is acceptable; a wrong answer is not. Nothing below is proposed for weakening.

- **An unclassified row kind** — kept, and now joined by a broken `retrospective` premise (exit 3).
- **A version below the tested floor** — refuse. P2 widens the door forward from a measured floor;
  it does not open it backward to versions with no evidence, and §3.1 shows 2.1.201 and earlier
  have none.
- **A reachable arrival above the bound** — refuse and re-promote.
- **A rejected launch bundle, or a `/clear` screen that does not match** — refuse. These are the
  failures the version key should be protecting against and currently is not (§3.6).
- **Never default a drain for an unmeasured version.** `DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS`
  stays behind an explicit `allow_untested`, and `RequireTested` keeps refusing.

The goal is fewer *unnecessary* refusals. Under P1+P2 the refusals that disappear are the ones where
pmux already holds a bound proven adequate across four versions and 226 arrivals; the refusals that
remain are the ones where it holds nothing.

---

## 7. What this did **not** establish

- **The briefing's turn table does not reconcile.** 2,344 claimed; 219 `turn_duration` rows found.
  I could not construct the claimed figure from this corpus by any metric I tried. Everything above
  is computed from what is on disk today.
- **Nothing was measured about 2.1.223's launch bundle or screen.** Whether 2.1.223 still accepts
  the minified flags is unknown and untested; it costs ordinals and I spent none.
- **The tail extrapolation in §3.5 assumes a stationary tail shape.** It is a model, fitted to 226
  samples that are 82 % one version and 79 % one workload. It prices headroom; it does not prove a
  bound.
- **Version and workload are confounded** and cannot be separated in this corpus (§3.3). A cleaner
  answer needs `cli` turns of both kinds at two versions.
- **The write-time lag of an `api_error` row is unmeasurable from JSONL.** §1.3 establishes those
  rows are stamped before the answer; it does not establish how long after the answer the bytes
  landed. The classification rests on the event ordering and on the queue argument, not on a
  write-time measurement.
- **Nothing here covers Linux or any non-aarch64 host.**
- **A defect found and not fixed:** `read_transcript`'s docstring
  (`tools/promotion/measure_transcript_drain.py:386-391`) says a file whose rows disagree on version is
  *"reported by the caller, not silently mixed in"* — and nothing in `main` reports it. Three such
  files exist here, one of 18,118 rows spanning four versions, and it is silently mixed into all
  four. That is the house bug class, it is why §3 re-attributes turns by their own candidate, and it
  is left for a separate change so the measurements above and the classification in §1 stay
  independently reviewable.

---

## Reproducing this

```sh
export PYTHONDONTWRITEBYTECODE=1
python3 tools/promotion/measure_transcript_drain.py --corpus ~/.claude/projects   --version 2.1.220
python3 tools/promotion/measure_transcript_drain.py --corpus ~/.claude/projects   --version 2.1.215
python3 tools/promotion/measure_transcript_drain.py --corpus ~/.claude-1/projects --version 2.1.223

# The pooled bound pmux actually ships, and the check that it still holds.
python3 tools/promotion/measure_transcript_drain.py \
    --corpus ~/.claude/projects --corpus ~/.claude-1/projects \
    --version 2.1.207 --version 2.1.215 --version 2.1.220 --version 2.1.223 \
    --bound-ms 1000 --json > evidence/pooled-transcript-drain-macos-aarch64.json
```

The per-version, per-workload and resampling figures in §3.3–§3.5 were computed by a scratch script
that imports this tool's own `ROW_KINDS`, `turns` and `post_answer_arrivals` and differs from it in
one respect only: it attributes a turn to the version on its own terminal candidate. The corpus is
host-local operator transcripts and is not committed.
