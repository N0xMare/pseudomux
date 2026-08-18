# pseudomux

Pseudomux (`pmux`) is a local, Claude-aware control plane for driving the real
interactive Claude Code TUI from programs. It launches Claude in a private PTY,
injects turns through a pinned `rmux` sidecar, and reconstructs results from
Claude's own JSONL transcript.

**Two products share one binary, and every subcommand says which one it is.**

- **Path B — the stateless token engine.** One call, `pmux ask`, and it is
  `(model, effort, prompt) -> text + usage`. The caller names no resource: no
  working directory, no Claude executable, no configuration root, no system
  prompt, no session id, no generation. The daemon holds a pool of already-warm
  Claude instances, hands your call one, `/clear`s it between turns, and
  recycles it. **This is the priority product.**
- **Path A — the interactive session.** You name the working directory, the
  Claude executable and the configuration root; you own the session id and
  generation until you close it; and you get the whole tool surface, terminal
  attachment, stored agents and multi-turn state.

`ping` and `doctor` belong to neither: they start nothing and spend no tokens.

> **Pre-release status:** protocol v1, the private runtime, the Path B pool,
> native clients, and offline tests are implemented. pmux compiles in exactly
> one **promoted** compatibility cell — Claude Code `2.1.220` through
> `2.1.227` on macOS/`aarch64`, `transparent` terminal, `sdk` input — and ships
> an **empty operator registry**: `--tested-claude-profile` admits nothing until
> an operator names something. Linux (including Claude Code `2.1.233` on
> `x86_64`) is admitted only by that flag, not by `PROMOTED_PROFILES`. The
> default `require-tested` policy refuses every version outside a matching
> cell, and every other OS and architecture, *before a Claude process is
> spawned*.
> macOS and Linux are intended targets; support claims belong to reviewed
> evidence and operator configuration, not to this time-stable source document.
> Windows is unsupported.

A bounded development smoke on 2026-07-18 established transcript-authoritative
fresh, warm, replay, resume, attach, and tool behavior for Claude Code
`2.1.215` on macOS arm64 with pinned rmux `0.9.0`. A later lifecycle audit
invalidated that smoke's cleanup claim after finding a stale zombie and a
foreground-signal race. Those defects are regression gates for the current
candidate; the old smoke is not a promoted compatibility profile.

## The command surface

The `path` column is not editorial. It is the label `pmux --help` prints for
that subcommand, and `tools/gate-a/tests/test_documented_surface.py` — Gate A
cell `gate_f/gate_driver_self_tests` — reads the built binary's own help output
and fails if this table names a different set of subcommands, or gives any one
of them a different label.

| subcommand | path | what it does |
| --- | --- | --- |
| `ask` | Path B | One stateless `(model, effort, prompt)` call against the pool. The entire Path B surface. |
| `ping` | Neither path | Ask the daemon for its version and protocol number. Reaches only the accept loop. |
| `doctor` | Neither path | Validate the socket, the daemon's health tree, the working directory and the Claude executable. |
| `run` | Path A | Start, run one turn, and close one interactive session. |
| `start` | Path A | Start a persistent session, and print the `session_id` and `generation_id` every later Path A call needs. |
| `turn` | Path A | Run one turn in an existing session. `--turn-id` is the caller's idempotency key. |
| `inspect` | Path A | Print one session's snapshot as JSON: `state`, `last_turn`, and the currently bound `transcript_session_id`. |
| `cancel` | Path A | Cancel one exact in-flight turn and report recovery state. Idempotent; never resubmits prompt input. |
| `close` | Path A | Close one session and reap its Claude process tree. Exits nonzero unless the reap was positively observed. |
| `attach` | Path A | Take over a live session's terminal, or mint a short-lived one-use attach capability. |
| `probe` | Path A | Print the redacted start DTO a launch *would* send. Without `--launch` it reaches no daemon and starts nothing. |
| `agent` | Path A | Store, read and revise the launch configurations `--agent` names: `create`, `list`, `get`, `update`. |
| `clear` | Path A call on a Path B cell | Type `/clear` into a `--cell minified` session and rebind to the transcript Claude rotates to. |

`pmux <command> --help` is the normative flag reference for each one; this
document does not restate flag lists that clap already prints.

## What is different

Pseudomux does not wrap `claude --print`, scrape final text from a terminal
screen, or expose a general-purpose PTY API. Its core contracts are:

- Claude is always a real foreground, interactive process. `--print`,
  background mode, and agent-team/teammate mode are forbidden.
- The public `SessionId` is the exact, resumable Claude session UUID. Every
  launched/resumed process also receives an opaque `generation_id`; all
  generation-targeted operations require the pair so delayed requests cannot
  mutate a newer resumed process. Private rmux identifiers never cross the
  service boundary.
- Claude's project transcript is the sole authority for assistant content,
  tool records, stop reason, usage, and completion. Terminal state is not
  corroborating evidence: readiness, blocking modals, quiet, and return of the
  input prompt are independently required liveness gates, and a turn can be
  neither admitted nor committed without them. The transcript decides what is
  true; the screen can only ever say "not yet". A wrong terminal-geometry
  constant therefore causes total unavailability, never a wrong answer.
- A turn completes only after prompt acknowledgement, a terminal main-chain
  assistant message, transcript drain, the normal input prompt, and terminal
  quiet all agree. Malformed active transcript rows fail closed.
- Input submission uses two cursor-correlated barriers under one terminal lock:
  a stable empty editor plus an immediate fence, one bracketed paste, then a
  later stable editor-relative render delta plus a final fence before the sole
  Enter attempt. Banner/history/footer changes cannot substitute for editor
  evidence.
- Public **control** uses one explicit, versioned Unix-domain socket. There is
  no daemon discovery and no client-side daemon startup. An optional loopback
  Anthropic Messages facade (`--path-b-messages-bind`) may be enabled in front
  of Path B; it is off unless given and refuses any non-loopback bind.
- One actor serializes each session. One turn may be active at a time, and a
  caller-supplied `TurnId` is an idempotency key.

```text
pmux CLI | Rust/TypeScript/Python client | MCP | Smithers transport
                              |
                  protocol v1, owner-only UDS
                              |
                            pmuxd
                 Claude session/turn actors
                    |                 |
          transcript watcher    private rmux SDK
                    |                 |
          Claude project JSONL    pmux-rmuxd
                                      |
                               PTY + interactive
                                  `claude`
```

`pmuxd` owns one private `pmux-rmuxd` sidecar and a one-use launch broker. Their
endpoints and private launch files live in an ephemeral mode-`0700` directory
and are not public APIs. Process specifications are held behind short-lived
in-memory tokens and delivered to `pmux-launcher` over an owner-only socket, not
placed on its command line.

## Quickstart

Requirements are Rust 1.88 or newer, a Unix host, and an installed Claude Code
binary for live work. Build the complete workspace so all runtime companions
land beside `pmuxd`:

```bash
cargo build --workspace --release
```

Start a development daemon with explicit, owner-only paths. **Give it
`--path-b-parent` and `--path-b-claude`.** Path A is always served; the
stateless token engine is off without them, and a daemon started without them
refuses every `pmux ask` with `unsupported_feature`:

```bash
RUNTIME_DIR="$PWD/.context/pmux-dev"
mkdir -p "$RUNTIME_DIR"
chmod 700 "$RUNTIME_DIR"
SOCKET="$RUNTIME_DIR/pmux.sock"

target/release/pmuxd serve \
  --socket "$SOCKET" \
  --runtime-parent "$RUNTIME_DIR" \
  --path-b-parent "$RUNTIME_DIR/pool" \
  --path-b-claude "$(command -v claude)"
```

`pmuxd` creates the pool parent itself, `0700` and empty, and refuses at boot a
directory that already exists with group or other bits — the same bar the
socket directory is held to, because every pool instance's configuration root
and working directory live under it. `--path-b-claude` must be absolute: the
pool launches unattended, and resolving a bare name through the daemon's own
`PATH` is how a daemon launches the wrong binary.

The following checks reach the daemon, start no session, and spend no tokens:

```bash
export PMUX_SOCKET="$PWD/.context/pmux-dev/pmux.sock"
target/release/pmux ping
target/release/pmux doctor \
  --claude "$(command -v claude)" \
  --cwd "$PWD"
```

`doctor` reports one layer per subsystem, and its `pool` layer distinguishes
"no stateless pool is configured on this daemon" from a pool that is configured
and idle — so it is the fastest way to tell whether the daemon in front of you
serves Path B at all. When Messages leases are live it reports `leased` and a
`conversation_leases` map (`conversation` → `s{slot}e{epoch}`).
`configuration.evidence.path_b_enabled` is the same fact as a boolean.

Then the first Path B call, which does spend tokens:

```bash
target/release/pmux ask --model sonnet --effort low \
  "Name the three largest moons of Saturn."
```

**It answers only if your installed Claude Code is inside a promoted range.**
A stateless cell is admitted on a tested compatibility profile alone, so a
version past the tested ceiling, below the measured floor, or on another minor
line is refused — before any child is spawned, and with the version it found
named:

```text
pmux: pmuxd error code=UnsupportedClaudeVersion message="Claude Code 2.1.228 has no tested pmux compatibility profile for macos/aarch64, Transparent, Sdk" retryable=false
pmux: run and review the guarded pmux Phase 0 cell, then admit its structured compatibility profile with --tested-claude-profile
```

That refusal is a statement about the *cell*, not about your machine being
broken; see [Promoted compatibility cells](#promoted-compatibility-cells) for
what is admitted and how an operator admits their own.

## Path B — the stateless token engine

`pmux ask` is `(model, effort, prompt) -> text + usage`. It is the whole
product surface, and the flags it does *not* have are the product:

```bash
pmux ask --model claude-opus-5 --effort high "Name three prime numbers."
```

The `text` output is the answer, alone, followed by a blank line and then the
accounting — so `pmux ask ... | head -1` is the answer and nothing else. The
shape, from `bin/pmux/src/main.rs`; the values are whatever the turn reported:

```text
<the answer>

model=<canonical --model> [reported_model=<what the transcript said>]
effort=<argv tier, or - when the model takes none>
claude=<the answering instance's Claude Code version>
input_tokens=N output_tokens=N cache_creation_input_tokens=N cache_read_input_tokens=N
```

`model` is what pmux asked for and `reported_model` is what replied; they are
two fields rather than one narrowed field because conflating them is how a
probe measures the wrong thing, and the transcript row that carries the second
is not guaranteed. `cache_read_input_tokens` is on the same line as
`input_tokens` deliberately: a cached prompt reports almost all of its context
there and almost none in `input_tokens` — MEASURED at `input_tokens=2
cache_read_input_tokens=1130` for a 450-token prompt — and a reader shown only
the first number would conclude the turn carried no context at all.

`--output json` emits the same facts as one object: `model`, `reported_model`,
`effort`, `text`, `stop_reason`, `usage`, `claude_version`.

The prompt may be a positional argument, a file (`--prompt-file PATH`, where
`-` means stdin), or piped stdin. `--deadline-unix-ms` may only *shorten*
pmux's wait; nothing on this subcommand lengthens one.

### Why the caller names nothing

There is no `--cwd`, no `--claude`, no `--config-isolation-root`, no
`--system-prompt`, no session id and no generation on `ask`. The daemon mints
every one of them from its own configuration plus a slot identity. That is not
an omission: nine isolation leaks in pmux were each reachable only because a
caller could name a resource pmux also used, and a caller who cannot name a
resource cannot alias one. `docs/path-b.md` §5.6 is the list.

### Models and effort

Both halves of `(model, effort)` are the pool's class key. Instances are
fungible *within* a class and never across one, because `/clear` rotates a
transcript — it does not re-exec, and `--model`/`--effort` are launch-time
argv. Two spellings of one model resolve to one class and cannot burn two
slots.

`--model` is required: an absent model would partition the pool on whatever the
daemon's configuration happens to default to. `--effort` is validated against
the **resolved** model, never against the flag's value list alone — the tiers
are not uniform across Claude models, and a model that admits none refuses
`--effort` outright rather than silently dropping it.

| model | aliases | admitted `--effort` |
| --- | --- | --- |
| `claude-fable-5` | `fable`, `fable-5` | `low`, `medium`, `high`, `xhigh`, `max` |
| `claude-opus-5` | `opus`, `opus-5` | `low`, `medium`, `high`, `xhigh`, `max` |
| `claude-opus-4-8` | `opus-4-8`, `opus-4.8` | `low`, `medium`, `high`, `xhigh`, `max` |
| `claude-opus-4-7` | `opus-4-7`, `opus-4.7` | `low`, `medium`, `high`, `xhigh`, `max` |
| `claude-opus-4-6` | `opus-4-6`, `opus-4.6` | `low`, `medium`, `high`, `max` |
| `claude-opus-4-5` | `opus-4-5`, `opus-4.5` | `low`, `medium`, `high` |
| `claude-sonnet-5` | `sonnet`, `sonnet-5` | `low`, `medium`, `high`, `xhigh`, `max` |
| `claude-sonnet-4-6` | `sonnet-4-6`, `sonnet-4.6` | `low`, `medium`, `high`, `max` |
| `claude-sonnet-4-5` | `sonnet-4-5`, `sonnet-4.5` | none |
| `claude-haiku-4-5` | `haiku`, `haiku-4-5`, `haiku-4.5` | none |

That table is `MODEL_TABLE` in `crates/service/src/pool/class.rs`, and the same
Gate A test that checks the command surface checks these rows against it.
Aliases are matched ASCII case-insensitively. The memberships are CHOSEN from
the published model catalogue rather than probed against an installed bundle;
adding a model is an operator edit to that table, not a three-language protocol
event.

### Sizing the pool

Every knob is a `pmuxd serve` flag, and `--path-b-parent` is the enable switch:
every other `--path-b-*` flag is refused without it, because a knob that
silently does nothing is worse than an error.

| flag | default | what it bounds |
| --- | --- | --- |
| `--path-b-parent DIR` | — | **Enables Path B.** Absolute parent for the per-slot trees pmux creates and erases. No caller can name any path under it. |
| `--path-b-claude PATH` | — | Required with `--path-b-parent`, and absolute. No `PATH` lookup. |
| `--path-b-pool-size N` | `15` | Live instances the pool may hold. Refused above the owner-set cap of 15, at boot. |
| `--path-b-recycle-turns N` | `50` | Turns one instance serves before it is destroyed and replaced. |
| `--path-b-warm MODEL[/EFFORT]=COUNT` | none | A warm floor for one class, repeatable. `claude-sonnet-5/medium=2`, `haiku=1`. |
| `--path-b-system-prompt TEXT` | see below | The system prompt every instance launches with, in REPLACE mode so it survives `/clear`. Bounded at 512 bytes. |
| `--path-b-system-prompt-file FILE` | — | The same prompt, read from a file instead of argv. |
| `--path-b-instance-idle-ttl-ms MS` | `300000` | How long an idle instance is held before the sweep destroys it, down to its class's warm floor. |
| `--path-b-turn-timeout-ms MS` | `600000` | Deadline a stateless turn gets when its caller supplies none. |
| `--path-b-retain-dir DIR` | erase | Absolute directory, outside the pool parent, where a quarantined instance's tree is kept as evidence. |
| `--path-b-rss-budget-mb MB` | — | Resident-memory budget the pool is sized against, checked once at boot against `pool_size * 1024 MB`. |
| `--path-b-messages-bind HOST:PORT` | off | Opt-in loopback Anthropic Messages facade in front of the pool. One conversation pins one warm instance; only the delta is typed; `/clear` runs on release. Loopback only (`127.0.0.1` or `[::1]`). Auth is presence-only: any non-empty `x-api-key` or `Authorization` is accepted; loopback is the trust boundary. |

The default system prompt is `Answer directly and completely. If you cannot
answer, say so in one line.`

**Fifteen is an owner-set cap, not a default you may raise.** `--path-b-pool-size
16` is refused at boot, not clamped. Every one of these bounds is checked
*before the socket is bound*, so a rejected configuration leaves no socket, no
runtime directory and no rmux sidecar behind it. Verbatim, each with the rest of
a working invocation elided as `...` and long messages wrapped here:

```console
$ pmuxd serve ... --path-b-pool-size 16
Error: the stateless token engine refused to boot: --path-b-pool-size 16 is
outside 1..=15

$ pmuxd serve ... --path-b-warm 'haiku/high=1'
Error: the stateless token engine refused to boot: the warm set declares model
haiku, which the pool cannot serve: model claude-haiku-4-5 takes no effort tier;
--effort high is refused rather than silently dropped

$ pmuxd serve ... --path-b-warm 'sonnet/low=1'      # and no --path-b-parent
Error: the stateless token engine is off because --path-b-parent was not given,
but --path-b-warm was: give --path-b-parent DIR (an absolute, owner-only
directory pmuxd may create per-slot trees under) to enable it, or drop the flag
```

Each declared warm class is resolved through the **same call a live request
uses**, so a class the pool could never serve is refused at boot rather than
discovered by an operator reading a mint failure at 3 a.m. The declared warm
total may not exceed the pool size.

### What `ask` refuses, and what each refusal means

| code | when | retryable |
| --- | --- | --- |
| `unsupported_feature` | the daemon was started without `--path-b-parent` — the message names the flag and the restart that fixes it. **Also** a prompt whose first meaningful character is a composer *mode* switch, `/` or `!`. `!` puts Claude's composer into bash mode and Enter runs the rest as a shell command on the host; it was reproduced 6/6 before the guard existed. The set is measured rather than guessed (`crates/claude/src/composer.rs`), leading invisibles are read past, and the check runs client-side *and* daemon-side so a raw socket caller is refused too. | no |
| `unsupported_claude_version` | the installed Claude is not a promoted or admitted cell. Refused before a child is spawned. | no |
| `invalid_config` | the model has no table entry, or the resolved model does not admit the requested `--effort`. The refusal offers the canonical model list, or that model's admitted tiers, from the same table argv is rendered from. **Also** the prompt shapes the composer does something to other than record: one containing a character it REWRITES — U+0009 into four spaces, U+000B into `^K`, U+000C into `^L`, each measured — because the acknowledgement could then never be satisfied; and one whose last character is `\`, because the composer reads it as a line continuation and Enter DELETES it and inserts a newline instead of submitting — doubling the backslash does not escape it, and the prompt is judged on what the composer will be left holding, so a `\` with spaces after it is still refused. **Also** a prompt carrying any other control character but `\n`, wherever it stands: pmux writes a prompt into a terminal as one paste, and it will not delete such a character from the end of a prompt to avoid saying so — a trailing U+0085 was silently removed until 2026-08-11, when the composer was measured KEEPING one. Two codes rather than one because a caller retries a rewrite differently from a mode switch. | no |
| `session_busy` | every slot is live and none came back inside the bounded admission wait. The message carries the census — how many are serving a turn, clearing between turns, holding a conversation lease, idle, reserved or warming, in teardown — and how long this caller waited. A `Leased` instance is not idle and is not stolen by `pmux ask`. | yes |
| `schema_drift` | the pool halted: `/clear` selected some other command, which means pmux's model of the composer no longer matches the installed Claude. The pool stops minting and pages. | no |

There is no queue, no fairness order and no reservation table. Admission waits,
bounded, only while some slot is on its way back; the refusal says which of the
two happened (`no slot was on its way back, so none was waited for` versus `no
slot came back in the N ms this turn waited for one`).

### Messages facade, for harnesses like Pi

`--path-b-messages-bind` is **not** in the first serve example on purpose: the
default daemon stays UDS-only. Given a loopback address it serves
`POST /v1/messages` (and `POST /v1/v1/messages` if a client already put `/v1`
on the base URL) plus `POST /v1/conversations/{id}/release`.

```bash
# add to an already-enabled Path B daemon; do not make this the first serve example
--path-b-messages-bind 127.0.0.1:8765
```

A client such as Pi sets `api: "anthropic-messages"` at
`http://127.0.0.1:8765` and sends header `x-pmux-conversation: <session-id>`.
Every response repeats that id, plus `x-pmux-cell: s{slot}e{epoch}`,
`x-pmux-lease: primed|continued|reprimed|replayed`, and `x-pmux-idle-ttl-ms`.
The first turn is a primer; later turns type only the new suffix so the
Anthropic prompt cache can hit. `/clear` runs on release, not after every
HTTP request.

Auth on this listener is presence-only. Any non-empty `x-api-key` or
`Authorization` is accepted. That is not a secret check. Off-box bind is
refused at boot.

For a Pi root plus live subagents, warm the classes that will actually be
used. At the owner-set cap of 15:

```text
--path-b-pool-size 15
--path-b-warm claude-opus-5/medium=12
--path-b-warm claude-opus-5/xhigh=2
--path-b-warm claude-fable-5/xhigh=1
```

One pool instance per live Pi conversation. Agent end is release (`/clear`),
not remint. Spawn, steer and delete stay the harness's job.

### Sidechains, and the one thing a stateless cell must not do

### Retained version-drift evidence

Every Path B instance writes a `cli`-entrypoint transcript, which is the only
kind of transcript the drain measurement can read — and the pool used to erase
it at teardown. It is now mirrored first, into
`path-b-evidence/` beside the socket, **pruned to the eight fields
`tools/promotion/measure_transcript_drain.py` reads**: `entrypoint`, `isMeta`,
`isSidechain`, `promptId`, `subtype`, `timestamp`, `type`, `version`. No prompt
and no completion, and not because those look sensitive — because nothing
measures them, and the retained set is derived from what the measurement reads
rather than chosen.

On by default, bounded at 64 MiB with oldest-first pruning, and
`--path-b-no-evidence` turns it off. `--path-b-evidence-dir` moves it. Point the
tool at it to re-check the drain for a new Claude Code version at zero token
cost — which is otherwise impossible, because a new version has no `cli` turns
to re-analyse until something has run some. `docs/version-drift.md` §5 P4 is the
measurement behind that sentence.

A pool instance launches with `--disallowedTools "*"` and permission mode
`dont-ask`, so a `Task` subagent — and with it a sidechain row in the transcript
— is structurally unreachable. A sidechain row is therefore evidence that the
denial did not take effect, and pmux refuses to commit that turn (`schema_drift`)
rather than under-reporting its tokens. The host must also *count* the sidechain
rows: a host that answers "I did not count" is treated as a failed check and not
a passed one, because a sidechain row carrying no usage is invisible to the token
check beside it.

## Promoted compatibility cells

pmux ships a small set of **promoted** compatibility cells, so a supported host
needs no `--tested-claude-profile` at all — which is what makes Path B, admitted
on a tested profile alone, reachable without operator evidence. Each one names a
Claude version **range**, an OS and an architecture, and carries a
`transcript_drain_ms` that was measured rather than chosen; the measurement, its
corpus and what would invalidate it are recorded beside the constant in
`crates/service/src/compatibility.rs`, the receipts are under `evidence/`, and
`tools/promotion/measure_transcript_drain.py` reproduces them. The promoted set
today is:

| Claude Code | platform | terminal / input | `transcript_drain_ms` |
| --- | --- | --- | --- |
| 2.1.220 through 2.1.227 | macos / aarch64 | transparent / sdk | 1000 |

Linux is **not** in that table. Claude Code `2.1.233` on linux/`x86_64` is an
operator cell: restart with `--tested-claude-profile` and a measured
`transcript_drain_ms` (250 is the linux minified estimator; it is not a
promoted pooled bound). A linux `PromotedProfile` needs
`evidence/promotion-2.1.233-linux-x86_64.json`,
`evidence/promoted-profile-<floor>-linux-x86_64.json`, and
`evidence/pooled-transcript-drain-linux-x86_64.json`. Those files are not in
this tree.

The range has two closed ends and never spans a minor. Below the floor there is
no evidence at all, above the ceiling nothing has been tested, and a different
`major.minor` forces re-promotion as a policy default. The drain is a
**conservative bound pooled over every version measured**, not a fit to any one
of them, which is what makes a range defensible at all —
`docs/version-drift.md` is the measurement. Five named conditions retract a
range, each bound to code that detects it
(`compatibility::RepromotionTrigger`), and `pmuxd doctor` publishes them beside
the range.

To admit a cell pmux has not promoted — or to override a promoted one with your
own measurement, which wins — restart the daemon with a repeated strict JSON
profile. For example syntax only:

```bash
target/release/pmuxd serve \
  --socket "$SOCKET" \
  --runtime-parent "$RUNTIME_DIR" \
  --tested-claude-profile \
  '{"claude_version":"2.1.207","os":"macos","arch":"aarch64","terminal_profile":"transparent","input_transport":"sdk","transcript_drain_ms":750}'
```

`claude_version` alone means that exact version, which is what an operator who
measured one host is claiming. An operator who tested a range adds
`"claude_version_tested_through"` and pmux stops refusing everything between the
two; the field is optional and every profile written before it existed still
means exactly what it meant. Two cells whose ranges OVERLAP are refused at boot
as ambiguous, adjacent ones are not.

The profile must match the daemon's current OS and architecture plus the
resolved terminal/input cell. `auto` resolves to `sdk` before matching. The
example is not a supported-version claim.

### Deliberate development-only live turn

The command below launches Claude, consumes Claude usage, and bypasses only the
tested-profile admission gate. It does **not** weaken transcript or completion
checks. An unmatched override is reported with `compatibility.tested = false`,
uses the daemon's conservative `--untested-transcript-drain-ms` value (2,000 ms
by default), and adds an `untested_compatibility_profile` result warning. Run it
only in a trusted repository with Claude already authenticated:

```bash
target/release/pmux --output json run \
  --claude "$(command -v claude)" \
  --cwd "$PWD" \
  --compatibility allow-untested \
  "Summarize this repository in three bullets."
```

This ad hoc command is not promotion evidence. Release validation must first
pass the complete deterministic Gate A manifest and then use the guarded
immutable-ledger envelope in [`tools/phase0`](tools/phase0/README.md).

Pseudomux never auto-accepts trust, login, permission, update, or other blocking
screens. During startup or post-Enter running/completion, a recognized screen
moves the live session into typed `needs_input`,
emits a redacted event, and preserves the underlying turn phase while an
authorized operator resolves the TUI through `pmux attach`. After an
authenticated writable attach, the reservation remains unavailable to API
turns until a fresh ready+quiet terminal observation and exact-cursor
transcript drain agree; ambiguous detach state taints or leaves the session in
`needs_input`. Classification is
intentionally conservative, so an unfamiliar or localized screen can still
time out; there is no API that chooses or auto-submits a modal answer.
Inside the prompt-admission barriers, a recognized modal is instead a typed
terminal failure and the worker is force-reaped. Post-paste state is ambiguous
and is never made resumable. Phase-aware resumability for a positively
pre-write modal is a future enhancement.

## Path A — interactive sessions

Every CLI operation requires an absolute `--socket` path or `PMUX_SOCKET`.
Output is `text`, `json`, or `ndjson`; NDJSON contains sequenced events followed
by a result record. Only `run` and `turn` stream events ahead of the result;
every other subcommand emits exactly one record in either mode. For `pmux run`,
streamed events are observational: only the terminal `type: "result"` record is
the one-shot commit marker. If cleanup cannot be proved, that record is withheld
and the command exits nonzero even if a preceding `turn_completed` event was
already written. Native `run_once` returns its typed result only after the same
cleanup proof succeeds.

One-shot execution starts, runs, and closes an interactive session:

```bash
pmux --socket /absolute/path/pmux.sock --output json run \
  --claude /absolute/path/claude \
  --cwd /absolute/path/project \
  "Review the repository."
```

Persistent sessions support native multi-turn operation. `start` prints the
`session_id` and `generation_id`, and **every later call needs both**: the
generation is what stops a delayed request from mutating a newer resumed
process.

```bash
HANDLE=$(pmux --socket /absolute/path/pmux.sock --output json start \
  --claude /absolute/path/claude \
  --cwd /absolute/path/project)
SESSION_ID=$(jq -r .session_id <<<"$HANDLE")
GENERATION_ID=$(jq -r .generation_id <<<"$HANDLE")

# One turn. --turn-id is the idempotency key: resubmitting the same id replays
# the stored result instead of running a second turn. Omitted, pmux mints a
# UUID v4 and prints it on stderr when the turn is accepted.
pmux --socket /absolute/path/pmux.sock --output json turn \
  "$SESSION_ID" --generation "$GENERATION_ID" \
  --turn-id 6f1d1b2e-0f4a-4c33-9a6b-1f2f6f0a77c1 \
  --timeout-secs 300 "Find the highest-risk module."

# State, last turn, and the currently bound transcript.
pmux --socket /absolute/path/pmux.sock inspect \
  "$SESSION_ID" --generation "$GENERATION_ID"

# Stop that exact turn. Idempotent, and it never resubmits prompt input.
# A nonzero exit means the session could not be recovered and must be closed.
pmux --socket /absolute/path/pmux.sock cancel \
  "$SESSION_ID" --generation "$GENERATION_ID" \
  6f1d1b2e-0f4a-4c33-9a6b-1f2f6f0a77c1

# Close and reap. A zero exit is a released slot AND a released process.
pmux --socket /absolute/path/pmux.sock close \
  "$SESSION_ID" --generation "$GENERATION_ID"
```

`close --policy force` is carried to the daemon and recorded on the request, but
it does **not** currently change the teardown: every backend in this tree drives
the same close and reports `process_reaped` only after positively observing the
owned process boundary empty. That is stated rather than implied, because a knob
that reads as "try harder" and does nothing is worse than no knob.

`pmux turn` also accepts `--on-disconnect` and `--heartbeat-timeout-ms`, and both are
represented rather than implemented. `continue` is the only `--on-disconnect`
the daemon implements and it is the default — a turn always runs to completion
whatever happens to the connection — while `cancel-turn`, `close-session` and
*any* `--heartbeat-timeout-ms` are refused with `unsupported_feature` on the
same future leased-connection API. The way to stop a turn is `pmux cancel`;
the way to bound one is `--timeout-secs`, which the daemon does enforce.

### `probe` — read a launch before you run it

`pmux probe` is a dry run. Without `--launch` it reaches no daemon at all and starts
nothing; it prints the exact redacted start DTO a launch *would* send, including
every environment name the child will not receive. Environment values, inline
settings and MCP documents, and the system prompt are never printed.

```bash
pmux --output json probe --claude "$(command -v claude)" --cwd "$PWD"
```

With `--launch` it starts the session, inspects it and prints the snapshot, then
closes it — unless `--keep`, in which case **you** own the session and must
`pmux close` it with the id and generation the report printed.

### `attach` — take over the terminal

```bash
# Text mode: pmux takes over this terminal until you detach.
pmux --socket /absolute/path/pmux.sock attach "$SESSION_ID" --generation "$GENERATION_ID"

# JSON mode: no attach happens; the short-lived capability is printed instead.
pmux --socket /absolute/path/pmux.sock --output json attach \
  "$SESSION_ID" --generation "$GENERATION_ID"
```

In text mode `attach` mints and consumes a 30-second, one-use proxy grant and
never reveals the private rmux socket or target. In `json`/`ndjson` mode it does
not attach: it prints the capability, whose `token` is a bearer credential for
that session's terminal — treat it as a secret and do not log it. The actor
allows one writable attach only while `ready` or `needs_input`; its reservation
blocks competing turns through any required post-detach reconciliation.
`--read-only` is accepted and refused by the daemon with `unsupported_feature`
on every session, so the refusal comes from the side that owns the answer rather
than from a client guess. A `--cell minified` session cannot be attached at all.

### `clear` — a Path A call on a Path B cell

`pmux ask` never needs this: the pool clears its own instances. `pmux clear` is for a
session *you* started as a minified cell with `pmux start --cell minified
--config-isolation-root DIR`, which is the same cell the pool runs, driven by
hand.

```bash
pmux --socket /absolute/path/pmux.sock --output json clear "$SESSION_ID" \
  --generation "$GENERATION_ID" \
  --expect-transcript "$TRANSCRIPT_SESSION_ID"
```

It types `/clear` into the composer and rebinds to the transcript Claude rotates
to. The session id and generation are unchanged; what rotates is
`transcript_session_id`, which the result reports and which the next clear must
be fenced against. `--expect-transcript` is that fence: at start it is the
session id, afterwards it is the value the previous clear returned, and every
other value is refused — including one that is only a single rotation stale. To
recover after a lost response, re-read it with `pmux inspect`.

Launch options include model, effort, permission mode, tool allow/deny lists,
settings, MCP configs, plugin directories, system-prompt policy, auth policy,
terminal size, lifecycle mode, retention, cell, and compatibility policy. Run
`pmux <command> --help` for the implemented flags. `rmux-standard` terminal
identity and `attached-stream` prompt injection are represented in v1 but are
intentionally rejected in v1 and require separate future implementation and
empirical promotion; they are not hidden cells in the current campaign.
`transparent` with SDK/auto input is the implemented candidate profile.

### The launch environment is an allowlist

Claude is launched with a replacement environment, not your shell's. The
environment you snapshot is filtered through a **closed allowlist** before
anything else happens: a name pmux does not recognize is dropped, whether or not
any policy forbids it. On a typical developer machine most of your environment
does not reach Claude, and that is the intended behavior — see
`docs/spec.md` §4.5 for why (a denylist kept missing new nested-Claude markers,
and one of them silently hung every turn).

What survives is the infrastructure Claude cannot run without: `PATH`, `HOME`,
`SHELL`, `USER`, `LOGNAME`, `TMPDIR`, `PWD`, `TZ`, terminal identity and
geometry, `LANG`/`LANGUAGE`/`LC_*`, TLS-trust and proxy variables, `XDG_*`,
`NODE_OPTIONS`/`NODE_PATH`, `SSH_AUTH_SOCK` and the `GIT_*` configuration names
the Bash tool needs, `CLAUDE_CONFIG_DIR`, and the `PMUX_` namespace. With
`--auth inherit` the provider-routing families (`ANTHROPIC_*`, `AWS_*`,
`GOOGLE_*`, `GCLOUD_*`, `CLOUDSDK_*`, `AZURE_*`, `VERTEX_REGION_*`,
`CLOUD_ML_REGION`) survive too; under the default `--auth subscription` they are
denied. `INHERITED_EXACT_KEYS` and `INHERITED_PREFIXES` in
`crates/service/src/claude_launch.rs` are the exact list.

**See what was dropped** before you launch anything — `probe` is a dry run and
prints variable names only, never values:

```bash
pmux --output json probe --claude "$(command -v claude)" --cwd "$PWD" \
  | jq .environment_removed
```

**Forward what you need.** `--env-passthrough KEY` takes the *name* of a
variable and reads its value from your own environment, so the value never lands
on the pmux command line or in `ps` output:

```bash
pmux --output json run \
  --claude "$(command -v claude)" --cwd "$PWD" \
  --env-passthrough MY_MCP_SERVER_TOKEN \
  --env-passthrough CARGO_HOME \
  "Review the repository."
```

Pass-through bypasses the allowlist by design; it is the supported extension
channel. It does not bypass the `subscription` auth policy, which strips
Anthropic credential and redirect names again after the patch is applied. And it
is not a secrecy mechanism: the launched child's environment stays readable by
your own uid (macOS `ps -E`), exactly as `docs/spec.md` §10.2 states.

### Agent profiles

Spelling out a full launch takes seventeen flags, every single time (`PMUX_SOCKET`
is assumed exported, as in the startup section above):

```bash
pmux --output json run \
  --claude "$(command -v claude)" \
  --cwd "$PWD" \
  --model sonnet \
  --effort high \
  --permission-mode dangerously-skip-permissions \
  --allowed-tool Read \
  --allowed-tool Grep \
  --allowed-tool 'Bash(git:*)' \
  --allowed-tool Edit \
  --settings "$HOME/.pmux/settings.json" \
  --mcp-config "$HOME/.pmux/mcp.json" \
  --plugin-dir "$HOME/.pmux/plugins" \
  --append-system-prompt 'Prefer small diffs.' \
  --rows 48 --cols 160 \
  --compatibility allow-untested \
  --extra-arg --debug \
  "Review the repository."
```

An agent profile names that bundle once, so the same launch becomes:

```bash
pmux --output json run --agent yolo --cwd "$PWD" "Review the repository."
```

Profiles live in one JSON document that you point at explicitly. Create it
owner-only — the loader refuses any file that carries an inline settings or MCP
document unless the file itself is `0600`, and refuses any referenced
`{"source": "file"}` entry whose own mode has group or other bits:

```bash
mkdir -p "$HOME/.pmux" && chmod 700 "$HOME/.pmux"
touch "$HOME/.pmux/agents.json" && chmod 600 "$HOME/.pmux/agents.json"
```

```json
{
  "version": 1,
  "agents": {
    "base": {
      "claude": {
        "model": "sonnet",
        "effort": "high",
        "allowed_tools": ["Read", "Grep"],
        "settings": [{"source": "file", "path": "/Users/you/.pmux/settings.json"}],
        "mcp_configs": [{"source": "file", "path": "/Users/you/.pmux/mcp.json"}],
        "system_prompt": {"mode": "append", "prompt": "Prefer small diffs."}
      },
      "terminal": {"rows": 48, "cols": 160},
      "auth_policy": "subscription",
      "require_env": ["CLAUDE_CONFIG_DIR"]
    },
    "yolo": {
      "extends": "base",
      "claude": {
        "permission_mode": "dangerously_skip_permissions",
        "allowed_tools": ["Bash(git:*)", "Edit"],
        "plugin_dirs": ["/Users/you/.pmux/plugins"],
        "extra_args": ["--debug"]
      },
      "compatibility": "allow_untested"
    }
  }
}
```

Scalars replace and absent scalars inherit, so `yolo` keeps `base`'s model,
effort, geometry, and auth policy. Lists append parent-first, so `yolo` resolves
`allowed_tools` to `["Read", "Grep", "Bash(git:*)", "Edit"]` — exactly the order
argv will repeat the flag in. `extends` is a single chain, bounded at depth 4,
with cycle detection. A literal JSON `null` is a parse error rather than an
unset operator, unknown and duplicate keys are rejected, and `require_env`
checks that a variable is set without ever reading its value.

Point pmux at the file with `--profile-file PATH`, or export it once in a shell
profile:

```bash
export PMUX_PROFILE_FILE="$HOME/.pmux/profiles.json"
```

There is no discovery: no XDG search, no upward walk from the working
directory. Exporting `PMUX_PROFILE_FILE` alone selects nothing — a file without a
`--profile NAME` (or `PMUX_PROFILE`) means "profiles live here, but not this
time", so pmux never picks one for you.

**These flags were renamed.** `--agent`/`--agent-file` and
`PMUX_AGENT`/`PMUX_AGENT_FILE` used to select a client-side profile; `--agent`
now names a **stored server agent** by id (see below). Every retired spelling is
refused with the new one named in the message, never silently aliased.

`cwd` is deliberately not expressible in a profile, and neither is the session
identity, the prompt, or the turn deadline. Naming any of them in the document
is a parse error. `--cwd` stays on every command line because a config file that
silently redirects where an agent operates is the one thing this tool refuses to
do.

Expansion happens entirely in the client, before the request is framed. `pmuxd`
never learns that a profile existed and there is no `profile_name` wire field,
so the audit command is a dry-run `probe`, which prints the exact redacted start
DTO that would be sent — plus a note naming any explicit flag that overrode a
profile value:

```bash
pmux --output json probe --profile yolo --cwd "$PWD"
```

### Stored agents

A **profile** is authored and expanded on your machine. An **agent** is the same
launch policy stored in the daemon, versioned, and pinned by version on every
start:

```bash
pmux agent create --spec-file reviewer.json          # prints agent_id and version 1
pmux agent list
pmux agent get   <AGENT_ID>
pmux agent update <AGENT_ID> --expected-version 1 --spec-file reviewer.json

pmux start --agent <AGENT_ID> --agent-version 1 --cwd "$PWD"
```

`--agent-version` is required and there is deliberately no "latest": that would
make the launch depend on *when* the request arrived rather than on what it
said. A running session pins the version it started under and is unaffected by
any later `update`.

**An agent may narrow what a session names; it may never name a resource on the
session's behalf.** There is no `cwd`, no configuration root, no session
identity, no prompt and no environment snapshot in a spec — each of those is
per-session and is named on every start. What an agent *may* say about them is
`containment`: `workspace_root` bounds the cwd a session may use (it never
supplies one, and the caller still writes `--cwd`), and
`require_config_isolation` requires a session to name a pmux-owned configuration
root without naming which. Both compose with `AND` against the checks that
already run, so no value of either makes an otherwise-refused start admissible.

An agent is **not a security boundary** and is not documented as one: the daemon
and its clients run as the same uid, so anything an agent would refuse the
caller can send inline. It buys deduplication, pinning and auditability.

`--agent` and the inline launch flags are mutually exclusive, and a command that
names both is refused naming the flag that collides. `pmux agent get` returns
environment values and inline settings/MCP documents as `sha256:` digests and
never in the clear; `config_digest` still identifies the configuration exactly,
because it is computed over the unredacted spec.

Author one from a profile when you already have one:

```bash
pmux agent create --from-profile yolo --profile-file "$HOME/.pmux/profiles.json" \
  --name yolo --claude /usr/local/bin/claude
```

That refuses by name the two profile keys an agent may not carry:
`config_isolation`, which names a resource, and `require_env`, which is a check
against the calling process's environment that a daemon has no calling process
to run.

**`--dangerously-skip-permissions` disables Claude's own permission prompts for
the entire session.** It is a single flag, not a `--permission-mode` value, and
pmux emits it only for the typed `dangerously_skip_permissions` mode; it can
never be smuggled in through `--extra-arg`. Every turn result from such a
session carries the warning `dangerous_permission_bypass` — "this session
launched Claude with --dangerously-skip-permissions" — on both the completed and
the cancelled path, so a result read in isolation still says the agent was
unsupervised when it produced that result.

## Native integrations

All integrations below talk directly to protocol v1. They do not invoke the
`claude-p` facade.

### Rust

The `pseudomux-client` crate provides typed requests and a sequence-validating
event stream:

```rust,no_run
use pseudomux_client::PmuxClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = PmuxClient::new("/absolute/path/pmux.sock")?;
    let pong = client.ping().await?;
    assert_eq!(pong.protocol_version, 1);
    Ok(())
}
```

Each request uses a fresh connection. The client never discovers or starts a
daemon.

### TypeScript, Python, and Smithers

- [`clients/typescript`](clients/typescript/README.md) is the dependency-free
  Node.js 18+ `pmux-client` package.
- [`clients/python`](clients/python/README.md) is the dependency-free Python
  3.11+ `pmux-client` package.
- `PmuxClaudeAgentTransport` is the thin native building block for the intended
  Smithers agent. It maps durable attempt IDs to deterministic UUIDv5
  `TurnId`s, maps aborts to native cancellation, and surfaces replay gaps as
  explicit reconciliation boundaries. A complete published Smithers agent is
  not claimed yet.

### MCP

`pmux-mcp` is a stdio MCP server with one required explicit socket:

```json
{
  "mcpServers": {
    "pmux": {
      "command": "/absolute/path/pmux-mcp",
      "args": ["--socket", "/absolute/path/pmux.sock"]
    }
  }
}
```

It exposes exactly these tools: `start_session`, `run_turn`, `inspect_session`,
`cancel_turn`, `close_session`, `run_once`, `subscribe_events`,
`attach_session`, `run_stateless`, `create_agent`, `get_agent`, `list_agents`,
`update_agent`. `run_stateless` is Path B — the MCP surface of `pmux ask`, and
it is refused the same way on a daemon started without `--path-b-parent`. Tool
arguments mirror native v1 DTOs and every input schema is closed
(`additionalProperties: false`). `subscribe_events` is a bounded long poll (at
most 30 seconds and 512 events), not a hidden streaming transport. MCP JSON is
written to stdout and diagnostics to stderr. That list is checked against the
running server's own `tools/list` response by the same Gate A test that checks
the command surface.

### Bounded `claude-p` facade

`claude-p` exists only for narrow compatibility with callers that expect a
print-shaped executable. `-p/--print` is accepted as a marker, but the facade
always submits a native interactive `run_once`; it never passes `--print` to
Claude. It supports positional or piped prompts and `text`, `json`, or
`stream-json` output. Slash commands and unsupported flags fail closed.

```bash
PSEUDOMUX_SOCKET=/absolute/path/pmux.sock \
  claude-p -p --output-format stream-json "Review this repository."
```

The facade always uses subscription auth, transcript lifecycle, one-shot
retention, and `require-tested`. Smithers should use the native transport, not
this facade.

## Authority, security, and lifecycle

The final text contains only text blocks from the terminal main-chain assistant
message. Thinking is excluded; tool uses/results are correlated by ID; usage is
deduplicated from logical transcript messages; subscription cost remains absent
rather than fabricated. Session handles, snapshots, and results carry the exact
compatibility cell, tested status, and selected transcript drain. Every result
also carries completion provenance.

The actor never evicts accepted `TurnId`s while a session is live, so a retry
cannot become a second prompt injection. New IDs fail before terminal mutation
at the bounded count/byte ceiling. Exact results are never silently truncated;
an oversized result becomes a compact, idempotently replayable
`result_too_large` failure, and replay/event batches are bounded by both record
count and the native 8 MiB frame.

The local input-admission barrier is capped at 15 seconds and always by the
shorter immutable turn deadline; it is not a model or billing timeout. Rmux
operation success proves only that a PTY write was acknowledged, not that
Claude's Ink editor consumed it. Pmux therefore sends one bracketed paste and
makes at most one Enter attempt, never retries an ambiguous write, fails closed
and reaps on ambiguity, and treats Claude's exact main-session typed-user JSONL
record as the semantic prompt-acceptance authority. It does not claim
exactly-once acceptance.
The final snapshot fence and Enter remain separate rmux RPCs. The terminal
mutex prevents pmux-local input races, but an asynchronous pane mutation can
still occur between them; the guarantee is at-most-one Enter after the last
observed non-modal editor, not atomic compare-and-send. A future rmux
`send_key_if_revision(expected_revision, operation_id)` operation with
deduplicated status lookup is required for that stronger claim.

Under the default `subscription` auth policy, pmux removes API keys, provider
redirect variables, and nested-Claude markers from the caller's exact
environment snapshot. `inherit` is an explicit opt-in. Ordinary Claude config,
including `CLAUDE_CONFIG_DIR`, remains available. Prompt-history suppression,
agent-team variables, and teammate mode are incompatible and rejected.

The public socket and private runtime are local owner-only resources. Launch
tokens and attach tokens are short-lived and one-use. Rmux sessions use
owner-exit cleanup and short leases; daemon shutdown closes managed sessions,
reaps processes, and then stops the sidecar. Hybrid lifecycle hooks may improve
observability, but never authorize semantic output or completion.

## Verification and release gates

[`docs/testing.md`](docs/testing.md) is the normative test-ownership matrix and exact
ordered Gate A manifest. It covers locked Rust 1.88 formatting/build/Clippy/
rustdoc/tests, property/model/fuzz targets, serialized real-rmux lifecycle
faults, exact-release shipped-binary E2E, Rust/TypeScript/Python conformance,
packaging, and deterministic concurrency/resource/soak/performance evidence.
Those tests use a credential-free deterministic interactive child and never
authorize a real-Claude turn.

The release set is eight binaries. Seven are product executables; the eighth is
`pmux-test-claude`, the deterministic Claude test double, which ships in the
release set because the full-stack suite must run hermetically against exactly
the frozen binaries. The deterministic full-stack and Docker lanes require all
eight; the live campaign below exercises only the seven product binaries and
pins exactly those, because a real-Claude run never loads the test double.

Live promotion is deliberately separate from ordinary CI. The dry-run-first
[`tools/phase0`](tools/phase0/README.md) envelope freezes the canonical source,
those seven product release binaries, and the exact Claude
executable; durably reserves
every attempt in the existing immutable global ledger; enforces the approved
60-through-100 ceiling and an observed public-result token guard; invokes only
native `pmux` or the bounded `claude-p` facade; and publishes owner-only,
hash-bound evidence. It never parses transcripts or drives rmux directly.

The current promotion scope is one exact macOS `transparent`/SDK Sonnet
low/medium cell with fresh, warm, resume, replay, tool, prompt-geometry,
cancellation-next-turn, attach/detach, deadline, cleanup, and selected native/
facade/client observations. Only after that frozen macOS evidence is reviewed
does [`tools/linux-docker`](tools/linux-docker/README.md) run credential-free
`linux/arm64` and `linux/amd64` deterministic portability gates against the
identical source digest. Docker is not a native credentialed-Linux Claude
compatibility claim.

See [docs/spec.md](docs/spec.md) for the normative architecture, protocol, operator, and
client contracts.

## License

Licensed under either MIT or Apache-2.0, at your option.
