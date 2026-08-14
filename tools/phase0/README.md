# pmux Phase 0 evidence envelope

`tools/phase0` is a dry-run-first acquisition and accounting envelope for the
bounded macOS real-Claude promotion campaign. It is not a Claude driver and it
is never a product correctness oracle.

The envelope freezes one exact caller-supplied release directory containing
`pmux`, `pmuxd`, `pmux-mcp`, `claude-p`, `pmux-rmuxd`, `pmux-launcher`, and
`pmux-hook`. Native scenarios talk through `pmux`; the optional bounded facade
scenario talks through `claude-p`. Both use the same pmuxd owner-only Unix
socket and native product implementation. Product success, failure, completion,
transcript interpretation, input admission, cleanup proof, and result
construction remain pmux responsibilities. `pmux-mcp` is frozen with the
shipped candidate but exercised by deterministic Gate A, not this credentialed
Phase 0 envelope.

The Python code does not:

- open or parse Claude JSONL;
- inspect or classify a terminal screen;
- connect to rmux, attach to a PTY, paste text, or press Enter;
- locate Claude history or reconstruct usage/completion;
- answer trust, login, update, permission, quota, or other prompts;
- retry an ambiguous prompt write;
- launch `claude --print` or any other non-interactive Claude mode.

The removed Phase 0 launcher, transcript fixture/parser, terminal classifier,
direct rmux controller, and standalone SDK input helper are intentionally not
part of this architecture.

## Commands

```text
phase0.py source-digest   compute the shared canonical source identity
phase0.py matrix          describe supported native envelope scenarios
phase0.py probe           print a plan, or execute with all live guards
phase0.py audit           verify ledger chains and published artifact hashes
phase0.py budget          recount the global attempt budget from the ledger
```

`tools/phase0/tests/test_phase0.py::test_every_subcommand_the_parser_offers_is_listed_in_the_readme`
derives that table from the parser, because a command list kept by hand is a
command list that loses its newest entry.

`probe` is a no-write dry run unless `--live` is present. Dry run does not
stat binaries, read prompts, inspect the ledger, create directories, or execute
commands. It validates the declared shape and prints a sanitized plan.

## Candidate freeze and identities

Phase 0 and Docker use exactly one source/mode-digest authority:
`tools/linux-docker/source_digest.py::workspace_source_manifest`. Obtain the
value after Gate A and independent review:

```sh
python3 tools/phase0/phase0.py source-digest \
  --source-root "$PWD"
```

Pass the returned `digest` back as `--expected-source-digest`. The envelope
recomputes it before the campaign, before every reservation, and after each
public pmux result. A source change stops the run. This includes tracked and
untracked source admitted by the shared algorithm; its exclusions are also the
Docker runner's exclusions.

Build the release candidate before live use:

```sh
cargo +1.88 build --locked --workspace --release --bins
```

The supplied `--release-bin-dir` must contain all seven exact executable names
at its top level. Supply one independently recorded digest for each:

```text
--binary-sha256 pmux=<sha256>
--binary-sha256 pmuxd=<sha256>
--binary-sha256 pmux-mcp=<sha256>
--binary-sha256 claude-p=<sha256>
--binary-sha256 pmux-rmuxd=<sha256>
--binary-sha256 pmux-launcher=<sha256>
--binary-sha256 pmux-hook=<sha256>
```

Also supply an absolute Claude path and `--claude-sha256`. The envelope runs
only `<claude> --version` before usage, records the exact bounded output/hash,
and pmuxd performs its normal version/profile admission before interactive
launch. Every attempt reservation binds:

- the canonical source digest, Git HEAD, digest-program hash, and file count;
- every release binary's path, content hash, size, mode, device, inode, and
  modification identity;
- the Claude executable identity and exact version-output identity;
- pinned rmux SDK `0.9.0` and the exact `pmux-rmuxd` digest;
- actual OS, architecture, kernel, and Python identities;
- terminal rows/columns, transparent profile, resolved candidate input mode,
  lifecycle, compatibility policy, model, effort, and subscription auth;
- raw prompt-file hash/size/filesystem identity without its content or path;
- a value-binding digest of the exact inherited environment without recording
  environment values.

Release and Claude file identities and the optional tested-profile file are
rechecked around attempts. Any mismatch fails closed.

Every reservation lists both the complete seven-binary frozen manifest and the
smaller exact set expected to be exercised by that scenario. This prevents
"candidate frozen" from being confused with "binary executed in this attempt."

## Immutable ledger and global budget

The campaign uses the existing NDJSON ledger as an immutable prefix. Live use
must explicitly provide:

```text
--ledger /absolute/private/model-attempt-ledger.ndjson
--ledger-prefix-records <immutable-record-count>
--ledger-prefix-sha256 <sha256-of-those-exact-lines>
--prefix-last-global-attempt <last-global-ordinal>
--global-attempt-ceiling <60-through-100>
--campaign-id <canonical-uuid>
```

`--global-attempt-ceiling` is the total approved ceiling, not a request to use
that many attempts. Values below 60 or above 100 are rejected. The explicit
last ordinal is checked against recognized legacy prefix fields when present.

The envelope opens the ledger without following symlinks, requires one
owner-only regular file with no hard links, takes an exclusive advisory lock,
revalidates the exact immutable prefix and all later hash-chained reservations,
selects the next global ordinal, appends one canonical reservation line, and
`fsync`s both the file and parent directory. Before returning launch authority,
it also proves that the ledger pathname still names the opened device/inode
with the exact final size, owner, mode, and link count. Only after that durable append can
`pmux start`, `pmux turn`, or `pmux run` possibly launch or submit Claude work.

Reservations are never edited, removed, or reused. A failed start, nonzero
pmux result, timeout, signal, source invalidation, or harness crash still
consumes its ordinal. A later run may use the same overall campaign ID or a new
one; global ordinals and the append hash chain continue across both. A crash
cannot reset the prefix, source candidate, or global budget.

The ledger line is the reservation. Outcome and cleanup state are published in
separate, hash-manifested artifacts, so no in-place ledger update is required.

## Usage guard

Live mode requires all of:

- `--live`;
- `--acknowledge-claude-usage`;
- `--max-attempts-this-run`, exactly equal to the prompt-file count;
- `--max-observed-tokens`, a positive bound;
- the immutable-prefix and 60–100 global-ceiling inputs above;
- all frozen source/binary/Claude identities;
- one requested `--model`, one required `--effort low|medium`, and at least
  one exact repeatable `--allowed-model-id` expected from the public
  `TurnResult`;
- one or more bounded UTF-8 prompt files.

The live envelope accepts exact integer timeouts only. Turn timeouts are capped
at 600 seconds, while daemon readiness and shutdown are each capped at 120
seconds. These are campaign authorization bounds, not suggestions that a short
turn should consume the full window; one-past values fail before path access or
process launch.

For an initial unpromoted compatibility cell, `--compatibility
allow-untested` additionally requires
`--acknowledge-untested-compatibility`. A `require-tested` run requires a
strict caller-supplied `--tested-profile-file`, which is passed to pmuxd and
content-bound in evidence.

The token guard consumes only `usage.combined` from pmux's authoritative public
`TurnResult`. It does not parse Claude history. Once the reported cumulative
total reaches the configured value, no later attempt is reserved. A failed or
malformed public result stops the campaign immediately because later usage
cannot be bounded safely. Like any observed guard, it cannot cap the provider's
current turn and is not a billing quota.

The requested model name is launch policy, while each `--allowed-model-id` is
an exact result identity authorized for the campaign. They are intentionally
separate: an alias such as `sonnet` must not cause an arbitrary returned model
identifier to be accepted. The complete campaign cell—including this allowlist
and effort—is content-addressed once and must match every later reservation,
outcome, attempt artifact, and campaign summary. A resumed harness process
reconciles all prior reservations and their durable usage before it may reserve
another attempt. It must also receive one externally retained
`--prior-campaign-anchor RUN_ID=SHA256` for every earlier run of the same
campaign. The digest printed by the earlier live probe is deliberately carried
outside the evidence tree, so replacing a complete internally self-consistent
tree cannot authorize a later reservation.

## Native scenarios

The envelope has four narrow orchestration shapes:

| Scenario | Public pmux operations | Reservation rule |
| --- | --- | --- |
| `one-shot` | one `pmux run` per prompt | before every fresh run |
| `persistent` | `start`, first `turn`, warm `turn` calls, `close` | first reservation precedes `start`; one before every later turn |
| `resume` | explicit UUID `start --resume`, first `turn`, warm turns, `close` | first reservation precedes resume launch; one before every later turn |
| `claude-p-one-shot` | one native `claude-p` `run_once` call per prompt | before every facade call; fixed 24x120 transparent/auto/transcript tested cell |

The envelope extracts only the public session/generation handle needed to call
the next public operation. It does not implement an actor, event state machine,
completion loop, transcript graph, or terminal fallback. `pmux` exit status and
its JSON/NDJSON result are authoritative. In NDJSON mode, the envelope uses the
single product-emitted `type: "result"` record only to acquire public usage and
does not reinterpret progress events. Acquisition rejects duplicate JSON keys,
non-finite numbers, and NDJSON in which the single result commit is not the
final nonblank record.

Prompt and tested-profile bytes are read and identified once. The retained
prompt bytes—not a reopened pathname—are passed on stdin through `--prompt-file
-` or the facade stdin contract. The source, binaries, Claude, profile, and
prompt pathname identities are revalidated before reservation. The public
result must return the expected canonical session/turn IDs and an exact
compatibility report; evidence records its Claude version, OS/architecture,
resolved `sdk` transport, tested flag, and drain value.

The supported promotion cell is deliberately bounded to `transparent` with
`sdk` or `auto` input, low/medium effort, and transcript or Hybrid lifecycle.
The native product's stable rejections—not this harness—own unsupported
`rmux_standard`, `attached_stream`, read-only attach, and other reserved modes.

## Forwarded launch options

The envelope forwards six launch options to the `pmux` CLI it already invokes.
Each one changes how Claude is launched, so each one is bound into the immutable
campaign contract at `cell.launch_options`; a launch option that changed
behaviour but was invisible in the receipt would make the receipt unreproducible.

| Option | Forwarded as | Recorded in the contract |
| --- | --- | --- |
| `--permission-mode MODE` | `pmux --permission-mode` | the exact mode string |
| `--env KEY=VALUE` (repeatable) | `pmux --env-passthrough KEY`, VALUE placed in the pmux child's environment | **`KEY` only** |
| `--env-passthrough KEY` (repeatable) | `pmux --env-passthrough` | the name |
| `--agent NAME` with `--agent-file PATH` | `pmux --profile` / `--profile-file` | the name plus the file's `sha256`/`size`/`path_sha256` |
| `--denied-tool PATTERN` (repeatable) | `pmux --denied-tool`, or `claude-p --disallowedTools` | every pattern, in order |
| `--system-prompt-file PATH` | `pmux --system-prompt <text of PATH>` | `system_prompt_policy: "replace"` plus the file's `sha256`/`size`/`path_sha256` — **never the text** |

`MODE` is one of the seven `PermissionArg` values of `bin/pmux/src/cli.rs`:
`default`, `accept-edits`, `plan`, `auto`, `bypass-permissions`, `dont-ask`,
`dangerously-skip-permissions`. The last one launches Claude with
`--dangerously-skip-permissions` and makes every turn of the session republish
the `dangerous_permission_bypass` warning; a campaign that uses it is a campaign
about that mode. `tools/phase0/tests/test_phase0.py::
test_permission_modes_are_exactly_the_pmux_cli_value_enum` reads the Rust enum
and fails if this list ever drifts from it.

**`--env` values are secrets by assumption, and the delivery reflects that.**
`pmux --env` puts the value in argv, where `ps` can read it
(`bin/pmux/src/cli.rs:216-219`), and this envelope binds the launched argv
verbatim into an evidence-grade process receipt — so an argv-borne value would
be written to disk under the evidence root. phase0 therefore puts the value in
the pmux child's environment and forwards pmux's own name-only channel,
`--env-passthrough KEY`, which lands in the same launch `set` term. The contract
records `environment_set_delivery: "env_passthrough_name_only"` so the receipt
states the mechanism rather than leaving it to be inferred. Only the name is
ever written to an artifact — exactly as the inherited-environment binding
records `values_recorded: false` — and every supplied value also joins the
redaction set, so it is stripped from captured stderr.

**`--denied-tool` is a policy, not a payload, so it is bound in full and in
order.** It is one element of `ClaudeLaunchConfig::denied_tools`
(`crates/protocol/src/v1.rs:743`), which the daemon emits as one
`--disallowedTools` pair per element. The single pattern `*` empties builtins
**and** MCP, which `--tools ""` does not. A comma in a pattern is rejected:
`pmux` declares `--denied-tool` with `value_delimiter = ','`
(`bin/pmux/src/cli.rs:192`) and the facade declares `--disallowedTools` without
one, so one comma would mean two denied tools through one entrypoint and one
through the other, and the reservation would bind a launch nobody asked for. A
leading `-` is rejected too: neither spelling sets `allow_hyphen_values`, so such
a value is a parse failure *after* the ordinal is already spent.

**`--system-prompt-file` replaces Claude's whole system prompt, and it is the one
launch value with no name-only channel.** `pmux --system-prompt`
(`bin/pmux/src/cli.rs:210-211`) takes the *text*; the 0600 `--system-prompt-file`
Claude finally reads is materialized daemon-side
(`crates/service/src/sensitive_launch.rs`), far past argv. The text therefore
does reach the launched argv and, through it, the process receipt — whose `argv`
is covered by `receipt_sha256`, so redacting it afterwards would make a faithful
receipt look forged. Rather than hide that, phase0:

* admits only an **absolute, owner-only (0600), singly-linked** UTF-8 file of at
  most 64 KiB, with no control characters beyond tab and newline;
* **refuses a document that reads as carrying a credential** (an `api key`,
  `token`, `password`, `bearer`, `secret` or `authorization` assignment) — the
  one guard that earns the argv route;
* binds only the document's identity, never its text, and adds the text to the
  redaction set so it cannot survive in captured output;
* records `system_prompt_delivery: "pmux_argv_replace"` and
  `system_prompt_text_recorded: false` in the contract, so the route is stated
  rather than inferred;
* re-runs the whole admission path between attempts: a replacement that lost
  mode 0600 mid-campaign stops the campaign exactly as changed bytes do.

Only `SystemPromptPolicy::Replace` is expressible. `Append` exists in the
protocol and is deliberately absent here: every value this tool can name is a
value it must also bind.

The `claude-p-one-shot` scenario forwards `--permission-mode`, `--env`,
`--env-passthrough`, `--denied-tool` and `--system-prompt-file`. The facade has
six permission modes (`bin/claude-p/src/main.rs:133-140`, no
`dangerously-skip-permissions`) and no `--agent`; asking for either is rejected
before anything is launched. It also spells the denied-tool field
`--disallowedTools`, and phase0 chooses the spelling from the entrypoint —
guessing it costs an already-reserved ordinal.

Two launch-option shapes are accepted when a contract is *read back*: the
original seven names, and the twelve written today. `inspect_ledger` re-validates
every post-prefix reservation on every append, so requiring the newer names would
fail an audit on evidence that is merely older. Both sets are closed; an unknown
name is refused either way.

## Drain calibration is this campaign's real product

`transcript_drain_ms` is the one tunable in pmux's per-turn cost, and the only
defensible way to choose it is to measure **how much later than the terminal
candidate the last transcript row actually arrived**. That is not the same
number as the drain pmux waited: an observed `drain_ms` of 2,400 against a
configured 2,000 measures the configured wait plus poll slack, and says nothing
about what Claude needed.

Every attempt therefore captures the whole published `timings` object verbatim
into `public_result_binding.timings`, and derives
`public_result_binding.drain_calibration` from it:

```text
terminal_candidate_at_ms   completed_at_ms   drain_ms
late_arrival_field         late_arrival_basis    late_arrival_gap_ms
```

The late-arrival field is **discovered, not named**: any key in `timings`
outside the five this tool already knows (`submitted_at_ms`,
`prompt_acknowledged_at_ms`, `terminal_candidate_at_ms`, `completed_at_ms`,
`drain_ms`) is taken as the product's late-row observation. A `*_at_ms` key is
differenced against the terminal candidate; any other `*_ms` key is already a
gap. Two unrecognized keys is ambiguous and fails loudly rather than guessing
which one to calibrate against.

The difference is kept **signed and unclamped**. The candidate stamp and the
last-activity stamp are taken from the same transcript read, so when no row
arrives late the difference straddles zero by a few milliseconds; clamping at
zero would erase exactly the boundary between "the candidate row was the last
row" and "one more row landed a millisecond later". Read any gap within one
actor poll interval of zero as no late rows.

The campaign summary and the audit both carry a `drain_calibration` block:
count, min, median, p95 (exact integer nearest-rank, no interpolation), max,
`no_late_row_attempts`, `late_row_attempts`, the configured drain, and the
headroom between the configured drain and the measured worst case.

**`no_late_row_attempts` is the number to read first.** A drain justified only
by turns where nothing ever arrived after the terminal candidate is calibrated
against *absence of evidence*, which is far weaker than a measured worst case.
The summary's `interpretation` string says so in words, so a run of zeros cannot
be quoted as permission to cut the drain.

### Prompt suite guidance: shape decides whether the number means anything

A trivial prompt is the wrong instrument. `"Reply with exactly: ok"` produces a
single-block answer that Claude flushes in one write: the terminal candidate and
the last transcript row are the same row, the gap is 0, and the campaign has
measured nothing about drain safety.

Late rows appear where the response has **structure**:

- a tool call following text — the text block looks terminal, then a `tool_use`
  row and later a `tool_result` row arrive;
- multiple content blocks in one assistant message, written incrementally;
- a long answer chunked across many writes;
- a usage row written after the last content row.

A calibration campaign must therefore use structured prompts — ask for a tool to
be used, ask for a long multi-section answer, ask for something that provokes
several content blocks — or its distribution is optimistically low and the drain
it recommends is not trustworthy. State the prompt suite's shape alongside any
proposed `transcript_drain_ms`; the prompt digests are already in the contract's
`prompt_suite`, so the claim is checkable.

### Gate B drain-calibration prompt suite and verifier

`tools/phase0/prompts/` and `tools/phase0/verify_calibration.py` are the
concrete instrument for the guidance above: a graded prompt suite that
produces the structured, multi-row transcripts a drain calibration needs, and
a standalone verifier that checks the resulting evidence rather than trusting
it.

**Why "write a poem, then hash it, then report the hash."** A poem is cheap,
original text with no external dependency; hashing it forces a genuine tool
round trip. The turn's transcript then has the shape that actually stresses
the drain — an assistant text block (the poem) that looks terminal, followed
by a `tool_use` row and a `tool_result` row arriving afterward, followed by
one more assistant text block (the hash) — instead of the single flushed
write a trivial prompt produces. The hash is also a **verifiable proof of
work**: given the poem text pmux's own result captured, anyone can
independently recompute the SHA-256 and confirm the tool actually ran rather
than the model fabricating a plausible-looking hex string. That is exactly
what `verify_calibration.py` does.

**The nine prompts, least to most transcript structure:**

`verify_calibration.py` grades each attempt by its prompt file's own name (its
`prompt_suite_index`, mapped back to a sorted `tools/phase0/prompts/*.txt`
listing), so every file below is always its own distinct row in that tool's
report. The "Category" column here just groups files that stress the same
kind of structure for this table's sake.

| File | Category | Structure it adds |
| --- | --- | --- |
| `01-baseline-trivial.txt` | trivial | none — reproduces the existing 24-turn corpus's control shape (single-block, no tools) |
| `02-poem-only-no-tool.txt` | poem only | a multi-line text block, still one flushed write, no tool |
| `03-poem-hash-single-tool.txt` | poem + hash | text → `tool_use` → `tool_result` → text (sample A) |
| `04-poem-hash-single-tool-variant.txt` | poem + hash | the same shape, different theme/wording (sample B, so the campaign's central case has more than one data point) |
| `05-poem-hash-reverse-transform.txt` | poem + hash, transform chain | two tool round trips (hash the poem, then hash a full-string reversal of it) |
| `06-poem-hash-triple-transform.txt` | poem + hash, transform chain | three tool round trips (poem, reversed, uppercased), to see whether the gap keeps growing with more structure |
| `07-long-poem-hash.txt` | long poem | a 40+ line poem, testing chunked writing of one large block |
| `08-unicode-poem-hash.txt` | non-ASCII poem | a CJK/emoji poem, exercising the non-ASCII terminal-geometry path the review flagged as never tested |
| `09-long-unicode-poem-hash.txt` | long + non-ASCII poem | both of the above at once — the apex structural case in this batch |

Nine files: one control, one weak-structure case, two samples of the central
poem+hash case, two transform-chain prompts, and three large/non-ASCII cases.
That is inside the "around 8–10" a first calibration batch should use — enough
to see whether the gap grows with structure without spending the campaign's
bounded, credentialed attempt budget on redundant samples.

**Running the suite.** `phase0.py`'s `--max-attempts-this-run` must equal
`len(prompt_paths)`, and a `prompt_suite_index` is assigned by CLI argument
order (`--prompt-file` is `action="append"` in `phase0.py`, order preserved).
Pass the nine files in exactly their numeric order so index *N* means grade
*N*:

```sh
python3 tools/phase0/phase0.py probe \
  ... \
  --prompt-file "$PWD/tools/phase0/prompts/01-baseline-trivial.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/02-poem-only-no-tool.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/03-poem-hash-single-tool.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/04-poem-hash-single-tool-variant.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/05-poem-hash-reverse-transform.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/06-poem-hash-triple-transform.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/07-long-poem-hash.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/08-unicode-poem-hash.txt" \
  --prompt-file "$PWD/tools/phase0/prompts/09-long-unicode-poem-hash.txt" \
  --max-attempts-this-run 9 \
  --effort low \
  ...
```

**`effort` is a campaign-wide flag, not a per-prompt one.**
`APPROVED_EFFORTS = ("low", "medium")` (`tools/phase0/phase0_lib.py:97`) is
the complete set `phase0.py` will accept for `--effort`; it caps this suite at
`medium`. `"high"` is outside the current authorization and would need
explicit user approval plus a new authorized-attempt budget before any run
uses it — this suite does not attempt to work around that. Because effort
applies to the whole run rather than to individual prompts, calibrating both
approved efforts means running this same nine-prompt suite twice (once with
`--effort low`, once with `--effort medium`), each as its own campaign against
its own attempt budget — not encoding effort into the prompt text.

**The hash-extraction contract.** Every prompt that asks for a hash requires
the final reply to be exactly: the poem's lines unchanged, one blank line,
then one hash line per computed hash, the last line of the reply always being
a hash line. A hash line is `SHA256: <hex>` (the poem itself) or
`SHA256(<label>): <hex>` (`reversed`, `upper`), where `<hex>` is the
64-character lowercase digest `shasum -a 256` reported. `shasum -a 256` is
used throughout rather than `sha256sum` because the campaign is macOS-only and
macOS ships `shasum` (Perl) but not GNU coreutils' `sha256sum` by default.

**Running the verifier** against a published evidence root:

```sh
python3 tools/phase0/verify_calibration.py \
  --evidence-root /absolute/private/evidence
```

It is a stdlib-only, read-only report, deliberately **not** a caller of
`phase0_lib.py`: it re-derives both numbers this campaign cares about from
scratch, independently of the code that produced them, so a bug shared
between "the tool that measured it" and "the tool that checks the
measurement" is far less likely.

- For each attempt, it reads the raw `pmux-{run,turn,claude-p}.stdout.{json,ndjson}`
  artifact (the one place the actual poem/hash text survives —
  `public_result_binding` deliberately excludes assistant content, per this
  envelope's own "does not ... reconstruct usage/completion" boundary above),
  extracts the poem and reported hash per the contract above, and
  **independently recomputes the SHA-256** over a small, explicit set of byte
  variants (the extracted text as-is, with one trailing newline, and their
  NFC-normalized forms) before calling a hash unreproducible. A mismatch is
  printed loudly and makes the process exit non-zero.
- For each attempt, it independently recomputes
  `last_transcript_activity_at_ms - terminal_candidate_at_ms` from the
  attempt's own captured `timings`, signed and unclamped, the same way this
  section defines the gap above — but by name
  (`crates/protocol/src/v1.rs:1265-1330`), not by phase0_lib.py's
  discover-the-extra-field trick, so a rename upstream turns into a loud
  "uncomputable" reason instead of a silently wrong number.
- It prints the count/min/median/p95/max distribution **and the
  `no_late_row_attempts` count broken out separately, both overall and per
  prompt grade**, so it is visible whether the gap grows with response shape.
  Wherever every attempt in a group only produced a zero-or-negative gap, the
  report prints an explicit "ABSENCE OF EVIDENCE" block in words — a drain
  calibrated only against turns where nothing ever arrived late is calibrated
  against absence of evidence, not a measured worst case, and this tool will
  not let that distinction be silently lost in a table of numbers.
- It reports the **effective (required) drain** separately from the
  **configured** one, and headroom is taken against the first. On a graduated
  build the commit gate calls `graduated_drain_ms(configured,
  turn_duration_seen)` (`crates/service/src/v1/backend.rs:244`), so a turn that
  saw the in-band `turn_duration` end-of-turn marker owed 250 ms, not 2,000 —
  and headroom published against 2,000 credits the run with margin from a
  mechanism that was not running. The verifier derives the required value from
  each attempt's own published timings: `turn_duration_observed_at_ms` present
  means the marker was seen, and `drain_ms < compatibility.transcript_drain_ms`
  *proves* the gate required less than the configured value, because the gate
  is `stable_for_ms >= required` and a commit at lower stability is otherwise
  impossible. The converse proves nothing (a graduated turn whose transcript
  simply stayed quiet reports a high `drain_ms` too), so that case is reported
  as `graduation_indeterminate` with a lower bound rather than resolved.
  `run_is_graduated` is the one field a regression check can watch: before
  this existed, a change that silently disabled graduation left every field in
  this report identical. The whole drain block — configured, required, the
  lower bound, the paid `drain_ms`, the graduation states, the headroom basis
  and the headroom itself — prints in the **default** text output in *every*
  branch, including the branch where no gap was computable and the branch
  where a value could not be derived at all. A quantity that reaches only
  `--json` is this project's signature defect, and the block used to sit
  entirely inside `if overall_gap_distribution is not None`.
- It states, in **both** `--json` and the default text, how many **answers**
  had **no truncation oracle** over them — how many replies had no hash
  independently recomputed, so a truncated or entirely empty reply would have
  graded exactly as a complete one. `{'match': 7, 'not_applicable': 2}` reads
  as nine attempts cleared and is seven checked plus two never examined; the
  tally line now carries its own denominator and the unchecked answers are
  named. This is an observation, not a failing condition: grades
  `01-baseline-trivial` and `02-poem-only-no-tool` are un-oracled *by design*,
  and a gate must gate exactly the claim it protects.

  The denominator is **answers, not discovered attempts**, and the two are
  reported on separate lines. An attempt that produced no public result at all
  — an incomplete run, a crash, a non-zero `pmux` exit — is *nothing to check*,
  not an *unchecked answer*; it is already counted in the bucket header at the
  top of the report. Folding the two together headlined `10 of 17 discovered
  attempt(s) had NO TRUNCATION ORACLE` on
  `pmux-validation-20260728-104907/gate-b-evidence`, whose real figure is 3
  unchecked answers out of 10, plus 7 attempts that answered nothing. The
  by-grade rows follow the same rule: `oracle=0/2 answers … no-answer=6`,
  never `oracle=0/8`.
- Constancy claims name what they excluded. `configured_transcript_drain_ms`
  is derived only from attempts that published an integer value, so the note
  beside it says how many of the successful attempts that was — and, when some
  published none, says explicitly that they were excluded and that the claim
  says nothing about them.

`verify_calibration.py` does not check manifest hashes, ownership, or
tamper-evidence; that remains `phase0.py audit`'s job. Run `audit` first to
establish the evidence is trustworthy, then run `verify_calibration.py` to ask
it the two questions this campaign exists to answer.

Its own offline tests live in
`tools/phase0/tests/test_verify_calibration.py`, built entirely from
synthetic evidence directories; like the rest of this envelope's test suite,
none of them drive `phase0.py`, pmux, rmux, or Claude.

**Every product line number `verify_calibration.py` prints is resolved, not
written down.** `cite(path, anchor, after=…, through=…)` searches the cited file
at import for the text the sentence is about, and returns a citation with **no
line number and the reason in it** when the file is absent, the anchor is
missing, or the anchor matches more than once — it never guesses.

The rule exists because this file held **22 product citations and 16 of them
were wrong**. `gate_f/phase0_self_tests` was refusing on 4, all in printed
banners. Of the other 12, one more was also being printed — the actor poll
interval, cited two lines short of where it lives, under a self-test named for
exactly that citation which was grading a copy in a comment — and 11 sat in
comments and docstrings that nothing looked at.
`BannerCitationTests` now refuses any hand-written product line number anywhere
in this file. So when you cite product source from here, name the anchor and let
`cite` find it; when there is no anchor worth naming, cite the SYMBOL and leave
the number out.

## Dry-run example

The following shape does not execute or write anything because it omits
`--live`:

```sh
python3 tools/phase0/phase0.py probe \
  --source-root "$PWD" \
  --expected-source-digest <source-sha256> \
  --release-bin-dir "$PWD/target/release" \
  --binary-sha256 pmux=<sha256> \
  --binary-sha256 pmuxd=<sha256> \
  --binary-sha256 pmux-mcp=<sha256> \
  --binary-sha256 claude-p=<sha256> \
  --binary-sha256 pmux-rmuxd=<sha256> \
  --binary-sha256 pmux-launcher=<sha256> \
  --binary-sha256 pmux-hook=<sha256> \
  --claude-bin /absolute/path/to/claude \
  --claude-sha256 <sha256> \
  --cwd /absolute/trusted/workspace \
  --prompt-file /absolute/private/prompt.txt \
  --evidence-root /absolute/private/evidence \
  --ledger /absolute/private/model-attempt-ledger.ndjson \
  --ledger-prefix-records <count> \
  --ledger-prefix-sha256 <sha256> \
  --prefix-last-global-attempt <ordinal> \
  --campaign-id <uuid> \
  --global-attempt-ceiling 100 \
  --max-attempts-this-run 1 \
  --max-observed-tokens 50000 \
  --scenario one-shot \
  --model sonnet \
  --allowed-model-id <exact-public-model-id> \
  --effort low \
  --compatibility allow-untested
```

### phase0 cannot run a minified cell, and this section used to say it could

**This block previously read "a minified (Path B) cell adds three flags to that
shape and nothing else". That was false in the way that matters most: the flag
it omitted is `--cell`, and `SessionCell::Minified` is the ONLY thing
`require_tested_for_minified_cell` gates
(`crates/service/src/v1/registry.rs`).** `_forwarded_launch_args` forwards six
options and `--cell` has never been among them, so **no phase0 campaign has ever
exercised a minified cell — including the one that promoted 2.1.220.** The three
flags below narrow a **full** cell's tool surface; they do not make it Path B.

```sh
  --permission-mode dont-ask \
  --denied-tool '*' \
  --system-prompt-file /absolute/private/system-prompt.txt
```

`'*'` is quoted only to keep a shell from expanding it; phase0 hands argv to the
launcher as a list, never as a shell word.

**Forwarding `--cell` would be worse than not forwarding it**, which is why the
omission is now a written decision
(`phase0_lib.LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD["--cell"]`) rather than an
oversight: seven of the nine graded prompts in `prompts/` instruct the model to
run `shasum -a 256`, and a cell launched with `denied_tools: ["*"]` has no way
to run it. Those grades would spend an ordinal each to produce a guaranteed
failure — `verify_calibration.py` recomputes each reported digest from the poem
text pmux captured, so a fabricated one is reported as `mismatch` and a refusal
to hash at all as `missing`, and both are `failing_conditions`.

**Minified-cell evidence comes from `tools/promotion/promote_claude_version.py`
instead.** It drives `pmux ask`, which is Path B and therefore always
`SessionCell::Minified`, against a grade suite whose every prompt is answerable
by reasoning alone — the contract Path B actually has. See
`docs/version-drift.md` §P3.

`LaunchSurfaceTests` (`tests/test_phase0.py`) now reads the clap declarations in
`bin/pmux/src/cli.rs` and `bin/claude-p/src/main.rs` and holds the argv builder
to them in both directions, so an option `pmux start` gains is either forwarded
or given a written reason, and a spelling the product retires cannot go on being
emitted. It found two: `--cell` above, and `--agent-file`, renamed to
`--profile-file` and kept only as a hidden spelling that refuses by name — a
campaign configured with a profile could not launch at all, through either
entrypoint, and would have discovered it one ordinal after reserving one.

**The flags a campaign passes are not the whole of what the child receives.**
The daemon appends its own flags for `cell: minified`
(`crates/service/src/claude_launch.rs::MINIFIED_CELL_FLAGS`), and the complete
argv is published as one list in `crates/service/src/v1/minified.rs`, checked
against the launch by a test rather than written down twice.

What a campaign still cannot reach is anything outside pmux's `--extra-arg`
allowlist, which is exactly `--debug`/`--verbose`
(`crates/service/src/claude_launch.rs::SAFE_EXTRA_FLAGS`): `--safe-mode` and
`--setting-sources` have no pmux protocol field and no daemon-side emission, so
a campaign that needs one needs a product change first.

After reviewing the dry-run output and only after Gate A freeze/review, the
coordinator may add:

```text
--live
--acknowledge-claude-usage
--acknowledge-untested-compatibility
```

Never place those flags in ordinary CI or a reusable shell alias.

## Private artifacts and publication

The evidence root and ledger parent must be real, current-user-owned mode-0700
directories. Ledger and artifact files are mode 0600; published artifact
directories are mode 0700.

Each reserved attempt is built in a unique hidden staging directory and
published by one same-filesystem rename only after its files and directory are
`fsync`ed. It contains:

- the exact immutable reservation copy;
- raw private pmux JSON/NDJSON stdout;
- prompt/secret-redacted pmux stderr;
- command shape, return status, duration, bounded byte counts, and hashes;
- public-result token accounting without assistant/transcript reinterpretation;
- an artifact manifest with every file's path, mode, size, and SHA-256.

Raw pmux result output can contain assistant/project information. It is kept
only in the owner-only artifact and is never printed by the envelope. Prompts
and environment values are not written to manifests or the ledger. Command
descriptions use placeholders rather than prompt content/paths. Daemon stdout,
stderr, and structured logs are redacted before publication.

Each process invocation has a separate `run_id` and atomically published
`campaign-run-<run_id>` directory containing candidate identity, attempt
references, daemon evidence, cleanup, and limitations. Interrupted hidden
staging directories are retained for diagnosis; the harness never silently
deletes or promotes them.

After publication, live `probe` prints both `run_id` and
`campaign_manifest_sha256`. Retain that pair outside the evidence root. Supply
all earlier pairs as `--prior-campaign-anchor RUN_ID=SHA256` on a later live
run, and every final pair as `--campaign-anchor RUN_ID=SHA256` to `audit`.
Within one process, each attempt manifest digest is retained in memory and must
match before the next reservation.

Command stdout and stderr are drained through nonblocking pipes with a hard
16-MiB per-stream retained-data bound. Timeout, output overflow, interruption,
or an internal observer exception always performs bounded exact-group
termination and waits to reap the direct child before control leaves the
command runner.

## Process and residue boundary

Pmuxd runs in a harness-created dedicated POSIX process group/session with a
short private socket path. The socket's device/inode/owner/mode identity is
captured across the successful readiness ping and revalidated before every
reserved public command, close, and daemon shutdown. Linux `/proc` start ticks or macOS
`proc_pidinfo(PROC_PIDTBSDINFO)` provide PID-reuse-resistant process identities.
The observer records every descendant it sees under harness-owned pmuxd/pmux
roots. Shutdown first uses SIGTERM on the exact pmuxd process group. A required
targeted rescue of an exact observed PID, forced daemon shutdown, scan error,
surviving process, socket, or private-runtime child makes the campaign fail even
if cleanup eventually leaves the host clean.

The short runtime root is created under `/tmp` only to stay within macOS Unix
socket path limits. It is random, owner-only, recorded, scoped to one run,
copied/redacted into evidence, and exactly removed after shutdown. The harness
never scans by command substring, kills an unrelated process, removes a shared
socket, or performs broad cleanup.

The proof covers exact descendants observed while rooted under owned
processes. It explicitly cannot prove absence of a never-observed double-fork
escape; pmux's own process-boundary cleanup remains the product authority.

## Crash/restart audit

Audit never launches pmux or Claude and never repairs evidence:

```sh
python3 tools/phase0/phase0.py audit \
  --ledger /absolute/private/model-attempt-ledger.ndjson \
  --ledger-prefix-records <count> \
  --ledger-prefix-sha256 <sha256> \
  --prefix-last-global-attempt <ordinal> \
  --campaign-id <uuid> \
  --evidence-root /absolute/private/evidence \
  --campaign-anchor <run-uuid>=<artifact-manifest-sha256>
```

It revalidates the immutable prefix, every later reservation/hash-chain link,
global ordinal continuity, private non-symlink artifact trees, cross-bindings
among ledger/reservation/outcome/campaign manifests, file hashes, source/run
identity, and hidden staging residue. A durable reservation with no final
attempt artifact is reported as an incomplete crashed attempt and remains
consumed. The audit also rejects missing, orphaned, unknown, failed, or mixed-
campaign-contract artifacts, unreconciled usage, missing cleanup proof, and any
hidden staging residue. The supplied anchor set must also match every reserved
run exactly; a missing, extra, or mismatched externally retained campaign
manifest blocks promotion. `accounting_verdict` says whether every consumed
attempt is explained; `promotion_eligible` is separately true only when all
outcomes, campaign acquisition, cleanup, source binding, durable usage, and
accounting gates succeeded. The audit CLI exits successfully only for
promotion-eligible evidence.

## Offline self-tests

These tests use temporary fake public pmux binaries and a Claude-version executable;
they never call real Claude, rmux, Docker, a provider, or the existing ledger:

```sh
PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/phase0/tests -v
python3 -m ruff check tools/phase0
```

The suite covers dry-run non-mutation, explicit consent, 60–100 ceiling
validation, canonical source-digest equivalence, exact file/prompt/environment
binding, legacy `global_attempt` records 5–29, torn/tampered ledger rejection,
concurrent atomic reservations, ledger path-replacement fencing, budget
exhaustion, crash accounting, artifact publication/tamper/symlink detection,
redaction, strict public JSON/NDJSON acquisition, exact result compatibility/ID
binding, usage guarding, OS process-start identities, timeout/interruption/
exception/high-output cleanup, forwarded launch options (permission mode,
`--env`/`--env-passthrough` names without values, agent profiles, denied tools,
system-prompt replacement) with value-enum and flag-name drift fences against
`bin/pmux/src/cli.rs` and `bin/claude-p/src/main.rs`, drain calibration and its
absence-of-evidence labelling, and complete fake-native one-shot/persistent/
facade campaigns.

The system-prompt tests pin the exposure as well as the guard: one test asserts
that the replacement text appears in **exactly one** place under the evidence
root — the `process_receipt.argv` of the launched command — and nowhere else. If
that assertion ever fails, either a new leak appeared or somebody rewrote a
receipt instead of closing one.

`pmux-test-claude` is a deterministic credential-free Gate A fixture only. It
does not count as a Gate B real-Claude attempt and cannot substitute for the
bounded macOS credentialed campaign. Docker later provides same-source
linux/arm64 and linux/amd64 portability evidence; it is not native credentialed
Linux Claude promotion.

These checks establish evidence acquisition/accounting/publication behavior
only. They cannot close transcript, actor, protocol, PTY, CLI, client, or other
product correctness rows in `docs/testing.md`.
