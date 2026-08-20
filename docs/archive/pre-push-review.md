# Pre-push review — `d851b63`, before merge to `main`

**Date:** 2026-08-13. **Head reviewed:** `d851b63e6a22ad6c53171c2ae1fad6a0efb25a09`, tree clean
before and after. **Host:** `aarch64-apple-darwin`, Claude Code `2.1.227`.
**Input:** 24 findings from four parallel read-only reviewers, one self-flagged as blocking.

Every claim below that says MEASURED was re-run here. Findings I could not establish are marked
UNVERIFIED rather than repeated.

---

## 0. Verdict

# READY

**Nothing here blocks the push.** The one finding flagged as blocking is real as a fact and wrong as
a severity call, and the reasoning offered for its urgency is mechanically incorrect — see §1.1 and
§1.3, both of which I refute by command.

What I did establish, at HEAD:

| gate | result |
|---|---|
| `cargo test --workspace --no-fail-fast` | **exit 0** — 72 `test result:` lines, **1,226 passed, 0 failed, 51 ignored** |
| `python3 -m unittest discover -s tools/linux-docker/tests` | 111 tests, **1 failure** — the documented deliberate red |
| `bash scripts/path-b-done.sh --only 1` | **MET** — 0 OPEN defect rows, 0 stale survivor rows, 0 files drifted |
| `bash scripts/path-b-done.sh --commit f4622a9 --only 4` (ancestor receipts) | **MET** — `cells_executed=70`, `red_and_deliberate=gate_f/linux_docker_self_tests` |
| `git ls-files -ic --exclude-standard` | empty — nothing tracked is ignored |
| derived credential scan over every tracked file | no `sk-ant`, no PEM, no Bearer, no api-key assignment |

The recommended sequence is **not** "push now" and **not** "re-run at HEAD". It is one ~20-minute
fix commit, then one unattended ~2.5-hour pinned run at *that* commit, then push. The argument is in
§3.

---

## 1. Adversarial triage of the 24 findings

Confirmed: 17. Overstated: 5. Refuted: 2. (Two findings are both confirmed-in-fact and
overstated-in-severity; they are counted once, under Overstated.)

### 1.1 REFUTED AS A BLOCKER — "Owner's Claude account name, plan tier and home path ship in two test files"

The text exists. I confirmed it:

```
$ git grep -rni 'welcome back'
crates/service/tests/fixtures/claude_2_1_70_ready.txt:3:│ Welcome back <name>!      │ ...
crates/service/tests/corpus/claude-2.1.70-captures.ndjson:3:{... "visible_text": "...│ Welcome back <name>!  ...│ Sonnet 4.6 · Claude Max │...│ ~/dev/pseudomux │..."}
```

The name itself is elided in this transcript, and was not when this section was written. Quoting it
here would have republished the one thing the finding is about, in the document that reports the
finding — which is how a review outlives the fix it recommended. Both files now read `pmuxdev`.

And it is not load-bearing. I rebuilt the tree with the name replaced, in a canonical path so the
candidate guard could not confound the result:

```
$ cargo test -p pseudomux-service --test screen_corpus_replay   -> ok. 11 passed; 0 failed
$ cargo test -p pseudomux-service --test actor_model            -> ok.  9 passed; 0 failed
$ cargo test -p pseudomux-service                               -> 515 passed, 1 failed
   (the 1 is `bounded_soak::repeated_real_rmux_cycles_...`: "required candidate pmux-rmuxd is
    unavailable / No such file or directory" — a missing binary in the scratch target dir,
    unrelated to the edit)
```

**Why it does not block.** The finding bundles three facts. Two of them are already public in this
tree many times over, so they carry no incremental disclosure:

```
$ git log --format='%ad' --date=format:'%z' | sort | uniq -c
 160 -0400                       # the "America/New_York" banner adds nothing to 160 commit stamps

$ git grep -l 'subscriptionType' -- docs
docs/2.1.226-compatibility.md                # §4.1: "Claude Max" is already published there
```

The entire novel information content is **one display name**. Set against what this repo already
publishes deliberately and intends to keep — a macOS username in 2,401 places, the owner's email on
three commits, a GitHub handle on 160, uid 501, keychain item names, the subscription tier, and ten
home-directory names — a first name in a test fixture is not a different category of exposure. It is
the same category, smaller.

The internal inconsistency is the tell: the same finding calls a 2,365-occurrence map of the
owner's home directory "defensible to keep" and a first name "blocks_push: true". Both cannot be
right. **Fix it — it is cheap and there is no reason to publish it — but in the fix batch, not as a
gate.**

Also worth correcting, because it will otherwise be repeated: the timezone is *not* a leak, it is
already in every commit stamp; and I could not establish whether the display name is the repo
owner's or a second person's. UNVERIFIED either way.

### 1.2 CONFIRMED, and it correctly overturns an established fact — the home-directory count

The brief's "86 occurrences across 18 tracked files" is 86 **lines**. The occurrence count is 28×
larger:

```
$ git grep -o "$HOME" | wc -l              -> 2401
$ git grep -c "$HOME" | sort -t: -k2 -rn | head -3
evidence/model-attempt-ledger.ndjson:52     <- 52 LINES; 2,365 occurrences
evidence/turn-latency-2.1.220-macos-aarch64.json:5
evidence/turn-latency-double-macos-aarch64.json:4
$ wc -l < evidence/model-attempt-ledger.ndjson  -> 77
```

Every number in the established fact — 86, 52, 9, 4, 3 — is a `git grep -c` line count. The
reviewer was right to challenge it. See §2 for what to do.

### 1.3 REFUTED — "any privacy scrub invalidates the pinned Gate A receipts *because of the source digest*"

The conclusion is accidentally right and the mechanism is wrong, and the wrong mechanism produces
wrong advice. `scripts/path_b_done.py` has two receipt branches. The `source_digest` comparison the
finding quotes (`:1020-1027`) is in the branch for a **bare** gate receipt read against the tree in
front of it. A `pmux.pinned-worktree-run.v1` receipt takes the branch above it (`:940-1005`), which
never calls `source_digest(context.repo)` at all. It checks `describes_commit`, `tree_sha`, the
artefact digests, and that the inner receipt's workspace is the pinned worktree.

Reproduced verbatim — the refusal names the commit, not a digest:

```
$ bash scripts/path-b-done.sh --only 4 \
    --gate-a-receipt .context/gate-a/pinned-receipt-gate-a-f4622a9.json \
    --gate-a-receipt .context/gate-a/pinned-receipt-gate-b-f4622a9.json
[4/5] Gate A green except the deliberate Linux cell -- NOT MET
    because: pinned-receipt-gate-a-f4622a9.json describes commit f4622a9 and this gate is judging d851b63
    because: pinned-receipt-gate-b-f4622a9.json describes commit f4622a9 and this gate is judging d851b63
    cells_executed=0
```

I did reproduce the digest arithmetic, and extended it to show it is not about `evidence/`:

```
$ (two `git archive HEAD` trees, one with only the two fixture lines rewritten)
/tmp/dg-a  sha256 9093381dc5feb637cacc2a0e6b10b48f36d27cb03f3e5c98db5c903390aadd82  file_count 983
/tmp/dg-b  sha256 833c46a9e3f010a393112291e3c14e6eeb472c6e072758bcbc84b5e3c94e8326  file_count 983
```

**Why this matters.** The finding's advice is "sequence the merge as scrub → re-run → push, because
doing it the other way turns a 5/5 receipt into a refusal." That framing makes the scrub look
specially expensive and specially urgent. It is neither. **Any new commit** invalidates a pinned
receipt, scrub or not — which is exactly what already happened, twice, with two docs-only commits.
The scrub costs one commit. So does the README fix. So does anything else. The correct rule is not
"scrub first" but "re-pin last", and it is §3's rule.

### 1.4 CONFIRMED — `evidence/` does contain completion text, and it does not matter

The established fact "no prompt or completion text in `evidence/` JSON" is false:

```
evidence/promotion-2.1.226-macos-aarch64.json /turns[0]/text = 'E7O1-86'
evidence/promotion-2.1.226-macos-aarch64.json /turns[2]/text = '8OGA café — 日本語 ✓'
evidence/promotion-2.1.227-macos-aarch64.json /turns[0]/text = 'C1H5-78'
evidence/promotion-2.1.227-macos-aarch64.json /turns[2]/text = 'DRQS café — 日本語 ✓'
```

These are synthetic nonces from a graded prompt suite. No redaction is needed. But the claim should
not be carried into any public statement; `evidence/README.md` already makes the narrower per-file
claim that is true.

### 1.5 CONFIRMED, highest-value cheap fix — README's headline refusal example names a supported version

```
$ grep -n '2\.1\.227' README.md
26:> `2.1.227` on macOS/`aarch64`, `transparent` terminal, `sdk` input — and ships
188:pmux: pmuxd error code=UnsupportedClaudeVersion message="Claude Code 2.1.227 has no tested pmux compatibility profile for macos/aarch64, Transparent, Sdk"
385:| 2.1.220 through 2.1.227 | macos / aarch64 | transparent / sdk | 1000 |
$ claude --version
2.1.227 (Claude Code)
```

One document says 2.1.227 is what it ships against (`:26`), that 2.1.227 is admitted (`:385`), and
shows 2.1.227 being refused (`:188`) — under the sentence "It answers only if your installed Claude
Code is inside a promoted range." The most likely newcomer has exactly 2.1.227 installed and reads a
working configuration as a refusal. **This is the single best thing to fix before publication.**
Use a version genuinely outside the range (`2.1.228` or `2.1.219`), or derive the sample from
`PROMOTED_PROFILES`.

### 1.6 CONFIRMED as fact, OVERSTATED as discovery — C10 is invisible to criterion 1

The fact holds, and it is a clean instance of the house bug class at the gate that certifies the
product. Criterion 1's title is universal; its predicate is a four-row register:

```
$ bash scripts/path-b-done.sh --only 1
[1/5] No known unfixed defect in the Path B path -- MET
    defect_register_entries=4     defect_register_open=0

$ python3 -c "... json.load(open('evidence/path-b-defect-register.json')) ..."
['verdict-1a-head-proof-accepts-a-prefix', 'verdict-1b-trailing-nel-is-deleted',
 'verdict-1c-mcp-drops-the-daemon-message', 'verdict-1d-prefix-justification-was-false']
```

and `path_b_done.py:390-401` reconciles that set bidirectionally against
`lettered_defects(docs/path-b-verdict.md)` — so the register's scope is *exactly* the verdict
document's own §1 letters. C10 lives in a different document under a different letter namespace and
the gate has no path to it. C10 is genuinely OPEN, HIGH, and measured 2 in 12
(`docs/current-state.md:1574`).

**But it is not a discovery.** HEAD already states this gap, in the same words, naming the same
section:

```
$ sed -n '1032,1040p' docs/linux-handoff.md
**What "done" does not mean — and this is the house bug class aimed at the done-gate itself.**
Criterion 1 is titled *"No known unfixed defect in the Path B path"*, and its predicate is: every row
of `evidence/path-b-defect-register.json` is `CLOSED` or `ACCEPTED` ...
**It does not read `docs/current-state.md` §9.4 at all.**

$ awk 'NR<=1574 && /^#{1,4} /{h=NR": "$0} NR==1574{print h}' docs/current-state.md
1558: ### 9.4 Post-commit findings (C1-C9) and one reclassification     <- C10 is in §9.4
```

Downgraded from "standing hole the gate hides" to "disclosed gap with a written disclosure." Fix on
`main`: narrow criterion 1's published title to what it measures, or add C10 to the register as
`ACCEPTED` with its decision text. Do not add it as `OPEN` unless you intend criterion 1 to go red.

*(Nit found while checking: §9.4's heading says "(C1-C9)" and the table holds C1-C11. A hand-written
range where the table is the derivation.)*

### 1.7 CONFIRMED as fact, OVERSTATED as severity — C2/C3/C4 on the Linux arm

The code facts hold. `fine: 0` is hard-coded on the Linux arm
(`crates/rmux/src/process_boundary.rs:531-533`), and C4's comment is still false at HEAD:

```
$ sed -n '338,343p' crates/rmux/src/process_boundary.rs
            // A PID reused inside this still-live session is a real member, so
            // membership also refreshes the token retained for it.
            tracked_pids.entry(row.record.pid).or_insert(row.start_identity);
```

`or_insert` never overwrites, so it does not refresh. Textbook house bug class.

**But all of it is already recorded and dispositioned.** `docs/current-state.md:1566-1568` carries
C2, C3 and C4 with correct line citations and repair costs; `docs/linux-handoff.md` §7.1 re-reads
all of them at `0d83b7a`; and HEAD's own commit message is titled *"…and the conservative Linux
birth token that already exists in a test"* and spends two paragraphs on the `.ok()?` collapse. The
doc comment at `:479-481` is honest about the coarseness on its face (*"Fine start counter: always
`0` on Linux"*), so it is not itself an overclaim.

**One sub-claim is effectively wrong.** The report says the recycled-PID assertion "lives inside
`if observed.is_some()`", implying it may not run. On every platform pmux supports it always runs —
the assertion three lines above forces it:

```
$ sed -n '786,791p' crates/rmux/src/process_boundary.rs
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(observed.is_some(), "supported platforms must expose a birth token");
```

**One sub-claim is genuinely additive and should be kept:** C3's row argues from a 25 ms poll gap
and was measured against a microsecond macOS token; the Linux token's resolution is one jiffy with
`fine: 0`, so the coincidence window C3 calls "adversarially precise" is materially wider there.
Add that sentence to the C3 row before the Linux lane re-argues it. And no test can catch a wrong
`nth(19)`: a wrong field index returns a stable wrong value and the equality assertion still passes.

### 1.8 CONFIRMED — `docs/testing.md` §3 specifies an environment the driver does not provide

```
$ git grep -n "PMUX_GATE_A_VALIDATION_ROOT\|PMUX_GATE_A_RELEASE_DIR" -- . ':!docs'
(no output)
$ grep -n -A1 '^ENVIRONMENT_ALLOWLIST' tools/gate-a/run_gate.py
85:ENVIRONMENT_ALLOWLIST = """CARGO_HOME HOME LANG LC_ALL LOGNAME PATH RUSTUP_HOME SHELL
86-    SSL_CERT_FILE TMPDIR USER""".split()
$ python3 -c "... phase-manifest.json ..."
'PMUX_GATE_A_VALIDATION_ROOT' 0    '{validation}' 17    '{workspace}' 74    '{release}' 16
$ git grep -c 'PMUX_GATE_A_VALIDATION_ROOT' docs/testing.md   -> 19
```

The driver substitutes `{validation}` placeholders; the prose contract was never re-projected onto
that spelling. 19 lines of §3 expand the variable to the empty string for a human who runs them —
and `docs/gate-c-linux-handoff.md:663` points the Linux operator at exactly that paragraph.
**Medium, not high:** no automated gate breaks. Fix before the Linux operator reads it.

### 1.9 CONFIRMED, exactly — `gate-c-linux-handoff.md` enumerates `evidence/` as three files

```
docs/gate-c-linux-handoff.md:675: "`evidence/` holds only `README.md`,
  `model-attempt-ledger.ndjson` and `gate-b-drain-calibration.json` today"
$ git ls-files evidence | wc -l   -> 17
```

A hand-written set where `git ls-files evidence` is the derivation, in the sentence that tells the
Linux operator what committing a receipt costs. Delete the enumeration; the digest point stands
without it.

### 1.10 CONFIRMED — the citation grader's document set is hand-written, and two ungraded docs have rotted

`crates/service/tests/path_b_doc_citations.rs:354` derives `linted_documents` from the §0.0 table of
`docs/path-b.md`, which has 9 rows (2 of them `PARTIAL`, excluded). There are 22 tracked docs. Two
confirmed rots, both in ungraded files:

```
$ sed -n '387p' docs/path-b-verdict.md
... `ComposerRefusal::LineContinuation` maps to `InvalidConfig` at `driver_io.rs:552`. **Fixed**
$ grep -n 'ComposerRefusal::LineContinuation' crates/service/src/driver_io.rs
761:                | pseudomux_claude::ComposerRefusal::LineContinuation => ErrorCode::InvalidConfig,

$ sed -n '85p' docs/gate-c-linux-handoff.md
`crates/service/src/v1/backend.rs:206-210` (the expression at `:209`),
$ grep -n 'has_partial_line && self.stable_for_ms' crates/service/src/v1/backend.rs
281:        self.at_eof && !self.has_partial_line && self.stable_for_ms >= required_stable_ms
```

**The `path-b-verdict.md:387` one is overstated as "live rot."** It sits in a *"Confirmed, by
severity"* triage table (section starts `:368`) inside a document whose own header pins it to
`28bd6b2`/`48aee00` and says *"Do not read a verdict out of this document… the script never reads
them, and neither should you."* Line rot in a dated review record is the documented convention, not
a defect. The `gate-c-linux-handoff.md:85` one is the better example, because the same document
hand-maintains a 35-file "citation freshness" warning list that covers that very file — a
hand-written set warning about the right file while the citation is simply wrong.

The valid structural point stands: **`docs/linux-handoff.md` (72 citations) is the artifact the
Linux lane will be driven from and it is outside the only mechanism this repo has for keeping
citations true.** Adding it as a `CURRENT` row to §0.0 needs no code change.

### 1.11 CONFIRMED — the candidate guard names the violation, not the remedy

```
$ ls -ld /tmp
lrwxr-xr-x  /tmp -> private/tmp
$ sed -n '102,106p' tests/support/candidate_binary.rs
            return Err(format!(
                "candidate {name} must not be a path alias: {} != {}",
```

I hit this guard family incidentally while testing the scrub (`required candidate pmux-rmuxd is
unavailable`). The guard is correct and exactness is the point; the message is the problem. Nothing
in `README.md` or `docs/testing.md` states that the checkout and target dir must be canonical.

**Its Linux relevance is UNVERIFIED and probably lower than reported.** On Linux `/tmp` is not a
symlink, so the reported reproduction does not transfer; docker bind mounts and overlayfs upper dirs
are plausible but I could not exercise them without a Linux host. Add the remedy sentence to the
error anyway — it costs one line and the failure otherwise reads like binary tampering.

### 1.12 CONFIRMED — ruff lints the gate machinery at defaults, with suppressions for a rule nothing enables

```
$ git ls-files | grep -E 'ruff|pyproject'   -> clients/python/pyproject.toml      (only config)
$ python3 -c "... manifest ..."   gate_a/python_ruff argv: ruff check --no-cache {workspace}
$ ruff check --no-cache --extend-select RUF100 --output-format concise . | grep RUF100
scripts/mutation_register.py:483:31:        Unused `noqa` (non-enabled: `PLC0415`)
scripts/tests/test_register_currency.py:247:31: Unused `noqa` (non-enabled: `PLC0415`)
tools/phase0/tests/test_verify_calibration.py:2330:28: Unused `noqa` (non-enabled: `PLC0415`)
scripts/mutation_refilter.py:57:27  (+6 more)   Unused `noqa` (unused: `E402`)
```

Three suppressions for a pylint rule the gate has never enabled, plus seven for `E402`. Low, and it
is the house bug class in miniature: a comment claiming to hold back a check that does not run.

### 1.13 PARTLY WRONG — `docs/code-census.md` "publishes reproduction commands that do not reproduce"

The census's first paragraph pins its own scope, three lines above the commands:

```
$ sed -n '3,9p' docs/code-census.md
**Scope.** Every git-tracked line in the repository at `f4622a9` ...
**Denominator.**
    git ls-files | wc -l                        -> 982 files
$ git ls-tree -r --name-only f4622a9 | wc -l    -> 982      # it reproduces at its stated commit
```

"Never says it excludes itself" and "commands do not reproduce" are both answered by that sentence.
Repeating the pin inside the code block would be an improvement, not a correction.

**The valid residue is real and should be kept:** the lexer that produced the 17-category
classification of 585,839 lines lives in `/tmp` and was not committed (`docs/code-census.md:14-15`
says so). By this repo's own §0.4 rule that is a receipt a reader cannot re-derive. Commit the lexer
under `tools/`.

### 1.14 CONFIRMED, low — two tests named for the failed-start terminal assert against a test-only fork

```
$ sed -n '3648,3649p' crates/service/src/native.rs
#[cfg(test)]
async fn close_unpublished_terminal(
$ grep -n 'close_unpublished_terminal' crates/service/src/native.rs
3649: (definition)   7908, 7913, 7927, 7931: (the two `failed_start_terminal_*` tests)
```

Neither test touches production `close_terminal`. A name promising more than the predicate tests.

### 1.15 CONFIRMED, low — `§11 item 1`'s `RateLimited` claim is half wrong

`docs/current-state.md:3554` says a fix would give `ErrorCode::RateLimited` *and*
`EventPayload::RateLimit` "a real producer". `RateLimited` has had shipped producers all along
(`driver_io.rs:4032` detector → `:1119` and `v1/actor.rs:4133`); only `EventPayload::RateLimit` has
none in `src`. Drop `ErrorCode::RateLimited` from the sentence.

### 1.16 CONFIRMED, and actionable only before the push — commit authorship split

```
$ git log --format='%an <%ae>' | sort | uniq -c
   1 N0xMare <54751288+N0xMare@users.noreply.github.com>
 156 n0x <54751288+N0xMare@users.noreply.github.com>
   3 n0x <PERSONAL-ADDRESS>
```

Three commits — entries 40, 41 and 54 of `docs/defect-log.md` — publish a non-noreply address. This
is the only finding in the whole round that is **cheap now and impossible later**. If that address
is not meant to be public, rebase those three. If it is, ignore this.

Two things about that block are not what the reviewer typed, and both are here for §1.1's reason.
**The address is elided**, and was not when this section was written: a finding that spells the
identifier it exists to keep unpublished has published it, in the paragraph reporting it, and the
paragraph outlives the fix — which is the same shape as the display name in §1.1, missed in the
same batch because the fix list was read for paths and not for the findings' own text. **The three
commits are named by their ordinal in the defect log**, not by their hashes, because the squash
this finding is about destroys every hash on this branch including theirs; that is the substitution
`docs/defect-log.md` declares as its rule 2, applied here by hand and said so.

**Resolved by the squash, not by a rebase.** One commit carries one author, and the identity it is
authored with is whatever `git config user.<name|email>` holds on the host that squashes —
`54751288+N0xMare@users.noreply.github.com` here. The remedy this section recommended is subsumed;
what it was actually protecting is the property to check afterwards, and it is one command:
`git log --format='%ae' origin/main..HEAD | sort -u`.

### 1.17 Confirmed without further comment

- 101 tracked references to gitignored `.context/`, including cited receipts a public reader cannot
  open and the default `PMUX_CARGO_MUTANTS_BIN` / `PMUX_CARGO_FUZZ_BIN` paths. Reproducibility
  embarrassment, not a leak. `.gitignore` itself is correct (`git ls-files -ic` is empty).
- `docs/path-b.md` §2.2's `--safe-mode` row names "the operator's 77 `smithers-*` skills" — a
  private machine inventory and another project by name. Rephrase to "77 unrelated user-scope
  skills".
- README names no reading order, no `docs/current-state.md`, no `vendor/` (53.9% of the tree) and no
  `scripts/path-b-done.sh` (`grep -rn 'path-b-done' README.md docs/path-b.md docs/current-state.md`
  exits 1). Editorial, valid, cheap.
- 14 of 30 `ACCEPTED` mutation-survivor rows cost `cheap` to close. The register is honest and the
  gate enforces `closeable`; this is a backlog, not a defect. `admit_claude_version` is the one to
  take first.

### 1.18 Not re-verified here (taken as reported)

- `tools/crash-harness` compiles at HEAD (refuting the census's "breaks silently"). I confirmed only
  that it is tracked, is in no manifest cell, and is referenced by four docs in prose.
- Clean-clone build and full-suite reproduction. I ran the suite in the workspace, not in a clone.
- `scripts/gate-a-fuzz.sh` and `scripts/gate-a-mutants.sh` were not run.
- C10's 2-in-12 rate was not re-measured (~10 × 530 s sequences).
- Whether the fixture's display name is the owner's or a second person's.

---

## 2. The personal-path question — DECIDED

**Recommendation: LEAVE the receipts. Scrub seven non-receipt occurrences opportunistically, in the
same commit as the README fix. Do not gate the push on any of it.**

The real number is **2,401 occurrences across 18 files** (§1.2), of which **2,385 are in
`evidence/`**. What that publishes is a macOS username and ten directory names:

```
$ grep -oE "$HOME/[^\"/ ]+" evidence/model-attempt-ledger.ndjson | sort -u
<HOME>/.local  <HOME>/conductor  <HOME>/dev  <HOME>/pmux-campaign-tree
<HOME>/pmux-drain-campaigns  <HOME>/pmux-drain-low  <HOME>/pmux-drain-tree
<HOME>/pmux-phase12-campaigns  <HOME>/pmux-phase12-tree
<HOME>/pmux-validation-20260728-104907
```

That is not a secret. It is a username the commit log already carries and seven worktree names that
say nothing a reader could not guess. Against that near-zero cost stands a real one: **these paths
are provenance inside measured receipts.** Every occurrence names the binary, corpus root or
artifact directory a measurement actually used. Rewriting a receipt so it reads better is precisely
the hand-written-receipt defect this repository has caught before and built machinery against.

**Leave — do not touch:**

| where | why |
|---|---|
| `evidence/*.json`, `evidence/model-attempt-ledger.ndjson` (2,385) | provenance in measured receipts |
| `crates/claude/tests/fixtures/stop_hook_summary_turn.jsonl` (3), `crates/service/src/v1/actor.rs`'s `SCRIPTED_TRANSCRIPT` (2) | real captured Claude JSONL lines used as parser fixtures — same provenance argument |
| `docs/2.1.226-compatibility.md` §4.1 | **THE ONE ROW HERE THAT IS AN OWNER DECISION AND NOT A TECHNICAL ONE.** The reason first written here was that the arithmetic is load-bearing and unverifiable without the literal input. That reason no longer holds: §4.1 now states the input as `$HOME/.claude-1`, prints the command that takes its digest, and works the same arithmetic through a second, machine-free root, so a reader who is not the author can check the claim and the measured digest is not what carries it. What remains is that the digest **is** the home path, one truncated `sha256` away, published beside the recipe — a derived value no needle can catch, because a map asked of the environment cannot know which functions of an identifier somebody wrote down. Deleting it is a hand-edit of a `DATED RECEIPT`, which is the act this repository forbids and which no mechanical map can perform; keeping it publishes a reversible form of the login name. The literal is elided from this row, so §4.1 is the only place it now stands, and **removing it there is the owner's call.** |
| `docs/current-state.md`'s 2026-07-28 live-campaign artifact directory, `docs/linux-handoff.md` §2.2's `which claude` result | provenance |

**Scrub — free, and each is an independent improvement:**

| where | why it is free |
|---|---|
| `docs/instrument-fix-plan.md:70` | an absolute path to a file *in this repo*; it should be repo-relative regardless |
| `docs/path-b-adversarial.md` §4.1, §9, §11.7 and §12.7 | reproduction commands; `$(command -v claude)` is strictly more reproducible |
| `crates/service/src/v1/actor.rs`'s `api_error` `format!` template | an authored template, not a capture |
| `tools/phase0/tests/test_verify_calibration.py`'s `write_ordinal_70_shaped_attempt` docstring | a docstring |

Add the fixture display-name pair and the `docs/path-b.md` §2.2 smithers phrasing to that same
batch. Total: nine small edits, one commit, ~20 minutes.

**What must not happen:** a blanket `sed` over `evidence/`. It would buy nothing, cost the receipts
their provenance, and — per §1.3 — would not even be the thing that forces the re-pin.

**DISPOSITION, recorded after the fact and not folded back into the argument above.** The owner
overrode the recommendation in this section: the tree was to be redacted before publication, not
left. The objection this section raises was met rather than ignored, and the difference is what
makes it safe. A blanket `sed` is still refused; what ran instead is one committed, declared,
uniformly-applied map (`tools/evidence_common/portable_paths.py`) whose needles are asked of the
running machine, whose placeholders preserve the shape the receipt readers check, which is
idempotent, and which is re-run over `git ls-files` on every gate by
`tools/gate-a/tests/test_redaction.py`. The one file this section is most protective of —
`evidence/model-attempt-ledger.ndjson`, 2,365 of the occurrences — is still untouched, and now for
a reason the build checks rather than a reason somebody remembers: substituting into a sealed
record forges it, so a file may keep this machine's paths only if every record it holds carries a
verifying seal. §2's `docs/2.1.226-compatibility.md` §4.1 row was not deleted either; the digest is
still there, with the input restated as `$HOME/.claude-1` and the command that reproduces it, which
is a stronger claim than the literal was.

**SECOND DISPOSITION, 2026-08-14 — the ledger went too, and the reason it was held back was
measured and found wrong.** The owner extended the override to the last file. What made it safe is
not a change of mind about `sed`: `tools/phase0/reseal_ledger.py` applies the same declared map in
its root-preserving form and then recomputes every digest with `phase0_lib`'s own sealing
functions, so the file is redacted rather than forged. Two claims in the paragraph above did not
survive being run. "A file may keep this machine's paths only if every record carries a verifying
seal" was the *checker's* rule as well as the writer's, so the one file holding the largest
concentration of identifiers was the one file `tree_offences` never opened, and the tree-wide green
was measured over the complement of the worst case; the check now reads every tracked file and only
the rewriter refuses a sealed one. And the seal does not, in fact, stop a forged ledger reaching a
live campaign — `phase0` re-verifies only the records after the immutable prefix, and the driver
derives that prefix as the whole file it found, so a copy with every seal broken was accepted by
`reserve_attempt`. The property that does hold is narrower and is now the one asserted:
`portable_paths.sealed_records` re-verifies all four bindings on every gate. Details and the loss
that came with it are in `evidence/README.md`.

---

## 3. The certification question — DECIDED

**Recommendation: do not re-run at HEAD, and do not push with an ancestor certification either.
Land one fix commit, re-pin at *that* commit, then push.**

The ancestor certification is sound on the facts. I verified all of them:

```
$ git diff --name-only f4622a9..d851b63
docs/code-census.md
docs/linux-handoff.md
$ git diff --name-only f4622a9..d851b63 | grep -v '^docs/' | wc -l   -> 0

$ python3 -c "... phase-manifest.json ..."
'docs/' 0   'linux-handoff' 0   'code-census' 0   'current-state' 0   'path-b.md' 0   'README' 0
   (70 cells, none names a document)

$ git grep -rln 'code-census\|linux-handoff' -- crates bin tools scripts tests
tools/gate-a/run_gate.py             <- both hits are `docs/gate-c-linux-handoff.md`,
tools/linux-docker/tests/test_runner.py    a different file, unchanged in the delta

$ cargo test --workspace --no-fail-fast   -> exit 0, 72 lines, 1226 / 0 / 51
$ bash scripts/path-b-done.sh --commit f4622a9 --only 4 --gate-a-receipt ... --gate-a-receipt ...
[4/5] Gate A green except the deliberate Linux cell -- MET
    cells_executed=70   red_and_deliberate=gate_f/linux_docker_self_tests
```

`docs/path-b.md` §0.0 — the one table that feeds the citation grader's document set — is unchanged
in the delta, and neither changed file is a row in it. So a re-run at `d851b63` would purchase a
receipt whose only new information is that two markdown files did not break 70 cells that never
read them. **That is a 2.5-hour purchase with no informational return**, and the measured cost is
exact:

```
$ python3 -c "... pinned receipts ..."
gate-a elapsed_s 2449  (40.8 min)  exit 1   commit f4622a9
gate-b elapsed_s 6469 (107.8 min)  exit 0   commit f4622a9
                                    total ≈ 2 h 28 min, unattended
```

**But HEAD is going to move anyway.** README:188 is wrong (§1.5) and should not be published as it
stands; the nine path/name edits of §2 are worth making; and the Linux lane wants a macOS baseline
receipt at the exact commit it forks from, not at that commit's grandparent. Re-running now and then
editing README would spend the 2.5 hours twice.

So the rule is **re-pin last, not scrub first**:

1. One commit: the README refusal example's version, the display-name fixture pair, the seven free
   path scrubs, the `smithers` phrasing, the `gate-c-linux-handoff.md:675` enumeration. **~20 min.**
2. `bash scripts/gate-in-worktree.sh --commit <new> --release-build --prepare 'cd clients/typescript && npm ci' -- …`
   for Gate A and Gate B — the gate prints the exact command in its own remedy output. **~2.5 h,
   unattended.**
3. `bash scripts/path-b-done.sh` end to end. Expect **5/5**.
4. Merge and push, with the certification naming the commit that is on `main`.

**If the owner will not wait 2.5 hours:** push today with the ancestor certification stated plainly,
and do steps 1-3 on `main` before the Linux lane starts. That is defensible — the delta is
mechanically docs-only and the gate's refusal is the gate working. It is second-best only because
the Linux lane is about to fork and would fork from an uncertified commit.

**One operational caution, reproduced incidentally here:** the candidate-binary guards make
`cargo` runs fragile against concurrency and against non-canonical paths. Do not run
`path-b-done.sh` beside a build, and do not run the gates from a symlinked path.

---

## 4. Disposition

### Must fix before push — *none*

There is no blocker. Everything below is optional with respect to correctness and worth doing with
respect to publication.

### Must be true of the push *command*, which no review of the tree can establish

Everything above audits **a tree**. The squash changed what that is worth, because the redaction now
lives in a rewritten history whose predecessor this repository deliberately still holds: the tag
`pre-squash-6ab75a4` is kept on purpose, so that the squash's identity can be checked by
`git diff pre-squash-6ab75a4 <squash>` rather than believed. That preserved history is the thing a
careless push republishes, and no amount of grepping the committed tree can see it.

The rule is therefore about the refspec and not about the files: **push the branch by name.** Do not
push `--mirror`, `--all` or `--tags`.

The set this rule protects is not written down here, because a list of refs in a document is exactly
the hand-written set-of-things-to-check this repository keeps finding as a defect. Ask the
repository instead — every ref the current head does not reach is a ref that carries pre-squash
objects:

```sh
head=$(git rev-parse HEAD)
git for-each-ref --format='%(refname)' | while read -r r; do
  o=$(git rev-parse "$r^{commit}" 2>/dev/null) || continue
  git merge-base --is-ancestor "$o" "$head" || echo "$r"
done
```

MEASURED at `c221c99`, from 793 refs: **790 do not reach it** — 785 `refs/conductor-checkpoints/*`,
the three `worktree-wf_*` branches, and `pre-squash-6ab75a4` itself. What they carry that the
published tree does not: the personal address in 6 of the 334 identity fields over the replaced
167-commit range (3 commits, author and committer each), and intermediate trees holding up to
**2,399** home-directory occurrences against the 2,365 that survive in the sealed ledger — the whole
of the redaction, undone, in objects the push would carry.

Two things make the plain push safe rather than merely likely-safe, and both were checked rather
than assumed. `git push origin <branch>` sends only objects reachable from that branch. And
`--follow-tags` — which some configurations add — sends only *annotated* tags;
`git cat-file -t pre-squash-6ab75a4` answers `commit`, so the tag is lightweight and is not
followed. A future annotated tag on pre-squash history would break that second property, which is
the reason it is recorded as a property of the tag and not as a property of the flag.

### Strongly recommended in the pre-push fix commit (~20 min)

| item | where |
|---|---|
| Refusal example names a supported version | `README.md`, "Quickstart" |
| Owner's Claude display name in a fixture | `crates/service/tests/fixtures/claude_2_1_70_ready.txt:3` + the embedded `visible_text` on `crates/service/tests/corpus/claude-2.1.70-captures.ndjson:3` |
| Seven free home-directory occurrences | §2 table |
| "77 `smithers-*` skills" | `docs/path-b.md` §2.2 |
| `evidence/` enumerated as 3 files | `docs/gate-c-linux-handoff.md:675` |
| Three commits with a non-noreply email — **only possible before the push** | `aea6cf6`, `d56d83c`, `2175205` |

### Fix on `main` afterwards

- Narrow criterion 1's title, or add C10 to the register as `ACCEPTED` (§1.6).
- Add the jiffy-resolution sentence to the C3 row; fix C4's comment (~3 min) (§1.7).
- Re-project `docs/testing.md` §3 onto `--validation-root`/`--release-dir` (§1.8) — **before the
  Linux operator reads it.**
- Add `docs/linux-handoff.md` as a `CURRENT` row of §0.0 so its 72 citations come under the grader;
  fix `gate-c-linux-handoff.md:85` (§1.10).
- Add the remedy sentence to the path-alias error (§1.11).
- Add a root `[tool.ruff.lint]`; delete the three `PLC0415` and seven `E402` `noqa` (§1.12).
- Commit the census lexer under `tools/` (§1.13).
- Retarget the two `failed_start_terminal_*` tests at production `close_terminal` (§1.14).
- Drop `ErrorCode::RateLimited` from `docs/current-state.md:3554` (§1.15).
- README "Where things are" section, and name `scripts/path-b-done.sh` on the reading path (§1.17).
- Fix `docs/current-state.md` §9.4's "(C1-C9)" heading (§1.6).

### Noted, not doing

- The 2,385 `evidence/` path occurrences (§2). Provenance; leave.
- `docs/2.1.226-compatibility.md` §4.1's path (§2). Load-bearing input to a published hash.
- 64 Claude session UUIDs. Opaque, carry no content.
- 14 `cheap` ACCEPTED mutation survivors. A backlog, honestly recorded and gate-enforced.
- 101 `.context/` references. Add one paragraph to `evidence/README.md` if it bothers you.

---

## 5. What the owner is most likely to disagree with

1. **That the display name is not blocking.** The counter-argument is that a person's name is categorically
   different from a username. I do not think it survives the observation that this tree already
   publishes the owner's email, uid, keychain item names, subscription tier and timezone by
   deliberate design — but it is a values call, not a measurement, and it is the owner's to make.
   The fix is nine characters either way.
2. **That the 2,385 `evidence/` paths should stay.** Someone who weights privacy over evidential
   integrity will want them gone. The measurement that should decide it: they are the only record of
   which binary and which corpus root produced each row, and there is no way to rewrite them that
   does not make the receipt partly hand-written.
3. **That the re-pin should wait for a fix commit rather than happen at `d851b63`.** This costs a
   day of "certified" status in exchange for a receipt at the commit that actually lands. If the
   Linux lane is starting tonight, push with the ancestor certification instead — that is the
   documented second-best and it is honest.
