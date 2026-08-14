# Pseudomux protocol-v1 architecture and contracts

This document specifies the implemented pseudomux (`pmux`) architecture: a
local, Claude-aware service that programmatically drives a real interactive
Claude Code process through a private rmux PTY and derives semantic results from
Claude's project transcript.

The keywords **MUST**, **MUST NOT**, **SHOULD**, and **MAY** describe contracts
that callers and operators can rely on. A feature described as *reserved* is in
the v1 type system but is not an implemented behavior and MUST NOT be assumed by
an integration.

## 1. Status and support boundary

The protocol-v1 DTOs and Unix-domain-socket transport, private runtime, session
actors, strict transcript engine, CLI, MCP adapter, bounded compatibility
facade, and Rust/TypeScript/Python clients are implemented and covered by
offline tests.

This source distribution intentionally contains no built-in tested
compatibility profile and makes no claim about the current contents of external
promotion reports. Operators explicitly admit reviewed evidence at runtime.
Consequently:

- `require_tested` is the default compatibility policy.
- A daemon starts with an empty tested compatibility-profile registry.
- A default session start fails with `unsupported_claude_version` until its
  exact normalized Claude version, current OS/architecture, terminal profile,
  and resolved input transport are admitted together.
- `allow_untested` is for deliberate development probes. It bypasses only the
  profile registry, not transcript validation or completion gates, and is
  reported as untested.
- macOS and Linux are intended release targets and have Unix implementation
  paths; any production support claim belongs to a reviewed external matrix and
  the operator's explicit profile configuration.
- Windows is unsupported.

The implemented candidate is the `transparent` terminal profile with `sdk` or
`auto` input. It is never admitted by source defaults.
`rmux_standard` identity and `attached_stream` prompt injection are reserved and
rejected by the service pending Phase 0 evidence.

## 2. Scope and non-negotiable invariants

Pseudomux is a high-level Claude session and turn service. It is not a public
terminal multiplexer, a general agent adapter, or an emulation of Claude's
official non-interactive streaming API.

The implementation MUST preserve these invariants:

1. **Interactive process only.** The child is the normal foreground `claude`
   TUI in a PTY. Pseudomux MUST NOT invoke Claude with `-p`, `--print`,
   `--background`, `--bg`, input/output-format flags, or teammate mode.
2. **Forced identity.** A new session is assigned a UUID before launch and uses
   `claude --session-id <UUID>`. A resume uses
   `claude --resume <UUID>`. The public `SessionId` is this exact Claude UUID.
   Each process incarnation also has a fresh opaque `SessionGenerationId`.
   Every operation that targets a live process carries both values; the daemon
   never forwards a stale operation to a newer resume of the same Claude UUID.
3. **Transcript authority.** The validated main Claude JSONL transcript is the
   sole semantic authority for prompt acknowledgement, assistant output, tools,
   stop reason, usage, and terminal-message status.
4. **Terminal liveness gate.** Rendered terminal state establishes readiness,
   classifies a recognized blocking screen, confirms the normal `❯` input
   prompt, and establishes quiet. It MUST NOT manufacture assistant output or
   complete a turn by itself. It is **not** corroborating evidence: it is an
   independently required liveness gate, and a turn can be neither admitted nor
   committed without it. The separation is safety from liveness, not a hedge
   between two authorities — the transcript decides what is true (the completion
   authority enum has the single value `transcript`), while the screen can only
   ever say "not yet". A wrong terminal-geometry constant therefore causes total
   unavailability, never a wrong answer; deleting the gate as redundant would
   convert that loud failure mode into a silent one.
5. **Single high-level path.** CLI, MCP, language clients, Smithers, and the
   compatibility facade all map to the same v1 service operations. They MUST
   NOT implement independent completion loops.
6. **Private terminal plane.** The rmux socket, rmux session names, pane IDs,
   launch broker, and process specifications are private daemon details.
7. **Fail closed.** Identity ambiguity, transcript replacement/truncation,
   relevant schema drift, unsafe flags, and unadmitted compatibility fail rather
   than silently falling back to terminal-derived text.
8. **Local explicit transport.** Public integrations use an exact owner-only
   Unix socket. Native clients never discover or start `pmuxd`.

## 3. System topology

```text
                                      Claude JSONL transcript
                                               ^
                                               |
pmux / Rust / TS / Python / MCP / Smithers     | strict locate + tail
                    |                          |
                    v                          |
          owner-only public protocol-v1 UDS    |
                    |                          |
                  pmuxd -----------------------+
                    |
          NativeService + one actor/session
                    |
         explicit rmux-sdk 0.9.0 connection
                    |
       private UDS   v
              pmux-rmuxd
                    |
             leased pane / PTY
                    |
      pmux-launcher -> execve(interactive claude)
```

### 3.1 Workspace components

| Component | Contract |
| --- | --- |
| `crates/protocol` | Versioned public DTOs and error/event types. It does not expose terminal backend identifiers. |
| `crates/claude` | Transcript location, complete-line framing, strict parsing, parent-graph correlation, logical-message grouping, tools, usage, and terminal-candidate rules. |
| `crates/rmux` | Narrow explicit-socket terminal backend over exactly pinned rmux `0.9.0`; launch and attach capability helpers. |
| `crates/service` | Canonical Claude-aware service, private runtime, secure launch preparation, transcript/terminal driver, per-session actors, hooks, and attach grants. |
| `crates/client` | Typed Rust v1 client and reconnecting, sequence-validating event stream. |
| `bin/pmuxd` | Owner-only UDS server and process-wide runtime owner. |
| `bin/pmux` | Native operator and scripting CLI. |
| `bin/pmux-mcp` | Stateless stdio MCP-to-v1 mapping. |
| `bin/claude-p` | Optional bounded print-shaped compatibility facade over `run_once`. |
| `bin/pmux-rmuxd` | Private rmux server sidecar with an explicit socket and readiness record. |
| `bin/pmux-launcher` | One-use launch-token consumer that replaces itself with Claude. |
| `bin/pmux-hook` | Bounded stdin-to-UDS relay for opt-in Hybrid lifecycle events. |
| `clients/typescript` | Node.js 18+ native client and Smithers transport helper. |
| `clients/python` | Python 3.11+ synchronous native client and durable-ID helper. |
| `tools/phase0` | Guarded empirical compatibility harness; not a runtime dependency. |

### 3.2 Daemon bootstrap

The operator MUST pass `pmuxd serve --socket <ABSOLUTE_PATH>` explicitly (or set
the equivalent `PMUX_SOCKET` process environment for the daemon). Pmuxd has no
state-path search, socket discovery, or implicit daemon-start contract.

Startup performs the following operations in order:

1. Validate or create the public socket parent as a directory owned by the
   effective user with no group/other permission bits.
2. Refuse a live listener, a non-socket endpoint, or a stale socket owned by
   another user. An owned stale socket may be replaced after identity checks.
3. Bind the public UDS under a private umask and set its mode to `0600`.
4. Create an owner-only `logs/` directory beside the public socket and append
   structured daemon metadata to `logs/pmuxd.log`. The active file has a hard
   16 MiB ceiling: the writer reserves and emits one
   `log_capacity_reached` record, discards later events for that process, and
   rotates exactly one bounded `pmuxd.log.previous` file on the next start.
   Non-file replacements fail closed. Daemon logging call sites do not log
   request payloads, prompts, environment values, or capabilities.
5. Locate `pmux-rmuxd` and `pmux-launcher` beside `pmuxd`, unless absolute
   overrides were provided. Hybrid mode expects the packaged `pmux-hook`
   companion as well.
6. Create a random ephemeral mode-`0700` runtime directory, optionally below
   `--runtime-parent`.
7. Bind the private launch broker, spawn `pmux-rmuxd` with an explicit private
   socket, and require a readiness record that reports exact rmux version
   `0.9.0` and the expected endpoint.
8. Connect the SDK only to that explicit socket. There is no rmux discovery or
   auto-start path in the runtime crate.
9. Begin serving v1 requests.

By default at most 64 public connections are serviced concurrently. Each
inbound and outbound frame has a ten-second I/O deadline, so a client cannot
hold a slot indefinitely with a partial request or an unread response. SIGINT
or SIGTERM stops acceptance, gives in-flight requests five seconds to drain,
closes all registered panes/process trees, then terminates the sidecar and
removes the public socket only if its filesystem identity is unchanged.

**Both handlers are installed before step 6 above, not at step 9.** They were
installed by the future `serve_until` polls, which is after the Path B warm set
is minted, so a daemon stopped while it was still starting took its default
disposition: exit 143, one epoch tree per instance the mint had reached, a
stale socket, and a log holding only the raw startup record because the
appender's `WorkerGuard` never dropped. What is NOT promised is that the signal
shortens startup: a daemon signalled mid-mint finishes minting its declared
warm set and then shuts down gracefully, which was MEASURED at 4.56 s for
`--path-b-warm claude-sonnet-5/low=3` signalled 2.6 s in. Racing the mint is
not available — `bin/pmuxd` holds no `NativeService` until startup returns, so
abandoning it orphans exactly the trees and children
`native::start_pool` exists to keep accountable.

**A startup that fails after minting part of a warm set drains what it minted
before it returns.** It did not, and the cost was not linear: a failed start
erases the one epoch tree it collided with and abandoned every tree it had
already minted, so the leftover set moved `L → (L \ {min L}) ∪ {0..min L - 1}`
— every transition of which was observed — and three abandoned trees took
**seven** consecutive refusing restarts to clear, `2^w - 1` for a warm set of
`w`. Draining makes it `L → L \ {min L}`: three planted trees, three refusing
restarts, MEASURED.

Runtime companions MUST be deployed in the same directory unless the daemon is
given explicit absolute companion paths. `cargo build --workspace --release`
satisfies this layout. The development supervisor in
[`scripts/pmuxd-run.sh`](scripts/pmuxd-run.sh) builds the four daemon companions
before starting the debug `pmuxd` binary.

### 3.3 Private rmux ownership

One `pmuxd` owns one private `pmux-rmuxd`. Each Claude process generation uses
an rmux owned-session name containing the public Claude UUID plus a fresh random
generation UUID, a single pane, cleanup policy `KillOnOwnerExit`, and a
five-second lease TTL. The generation component prevents a stale backend object
from aliasing a later process that reuses the same public Claude session UUID. A
lost lease is surfaced as `daemon_lost`; it is never interpreted as successful
completion.

The sidecar socket and launcher broker socket remain inside the random private
runtime. No public response contains either socket, an rmux session name, or a
pane ID.

Embedding the rmux server in `pmuxd` as a library was considered and rejected.
Its bind path unconditionally installs process-wide `sigaction` handlers for
seven signals, including `SIGCHLD` and `SIGHUP`, with no uninstall path; that
would silently replace the daemon's own signal dispositions and put a foreign
`SIGCHLD` handler under the async runtime's process driver. Independently, the
sidecar process is the only cleanup authority that survives a `pmuxd` SIGKILL,
because its owner-pipe write end closes even then.

## 4. Session start and Claude launch

### 4.1 Start request

`StartSessionRequest` contains:

- `identity`: `new` with an optional caller UUID, or `resume` with a required
  UUID;
- absolute `cwd`;
- `claude`: absolute executable and validated launch policy;
- `environment`: a complete snapshot plus deterministic `set` and `unset`
  changes;
- `auth_policy`: `subscription` or `inherit`;
- `config_isolation`: optional, a pmux-owned Claude configuration root for this
  session. Absent means "inherit the caller's root", which is what every release
  before the field did and what every caller that omits it still gets;
- terminal rows, columns, profile, and input transport;
- lifecycle, retention, and compatibility policies;
- `cell`: `full` (the default, and what every caller that omits the field gets)
  or `minified`. A `full` cell is **omitted from the wire**: request DTOs reject
  unknown fields, so a `"cell"` key is a hard rejection on a daemon built before
  the field existed, and the default asks such a daemon for exactly what it
  already does. A non-default cell is always serialized — refusing it loudly on
  an old daemon is correct, and silently downgrading it would run a caller's
  turns under a proof it did not ask for.

The cell is a property of a session, not an operation on one, so it is chosen
once at start and there is deliberately no request that changes it mid-session:
a cell change mid-flight would mean a turn could finish on a proof it did not
start under. `minified` narrows what a session may do — it is the only cell
whose turns are eligible for the calibrated fast-path drain and the only cell
`clear_session` will type into — and it changes nothing about how a turn is
proven finished. The transcript remains the sole completion authority for both
cells.

`minified` is admitted only on a **tested** compatibility profile, under the same
evidence rule as every other compatibility decision, and the refusal happens
before a Claude process is spawned. Because a tested profile is exactly what
`--compatibility allow_untested` cannot produce, the two are not alternatives:
an operator who wants the minified cell must admit a profile with
`--tested-claude-profile`, which is an operator assertion that reviewed evidence
exists (Section 4.3).

`config_isolation` answers *whose configuration*, which is a different question
from `auth_policy`'s *whose credentials*, and is deliberately not a third
`auth_policy` variant: every consumer of `auth_policy` decides which credential
names survive to the child, and the honest answer for config isolation is
"exactly the same ones as before". An isolated session shares the caller's
credential store **by construction** — see Section 4.5 — so no value of this
field changes which account the session authenticates as. The root MUST already
exist, be a directory owned by the daemon's effective uid with mode `0700`, and
be neither `cwd` nor an ancestor or descendant of it, and it MUST NOT be the
config root the same request would have resolved without isolation. pmux
verifies and refuses; it never creates the root and never relaxes its
permissions. Before launch pmux seeds `<root>/.claude.json` and
`<root>/settings.json` atomically at mode `0600` — onboarding completed, the
canonical cwd trusted, auto-updates off — and refuses the start rather than
writing while another live session is bound to the same root.

Native callers SHOULD provide a complete environment snapshot. At minimum the
effective post-policy environment needs absolute `CLAUDE_CONFIG_DIR` or `HOME`
so pmux can locate Claude history. The CLI and compatibility facade snapshot
their complete current environments. A complete snapshot is not a complete
inheritance: Section 4.5 filters the snapshot term through a closed allowlist
before any other step, and `set` is the channel for anything that must survive
regardless.

A `config_isolation` root and the cwd may not contain one another. Containment
is decided on the directory — `(device, inode)` ancestry — rather than on a path
prefix, because two canonical spellings of one directory need share no prefix.

Paths for the executable, cwd, file-backed settings/MCP configs, and plugin
directories are canonicalized and MUST exist. The executable MUST be an
executable regular file, cwd and plugin paths MUST be directories, and
file-backed settings/MCP paths MUST be regular files. Terminal dimensions MUST
be non-zero. Public JSON uses `snake_case` enum and field names.

### 4.2 Identity and transcript preconditions

The **effective configuration root** is `CLAUDE_CONFIG_DIR` from the
post-policy environment of Section 4.5, or `<HOME>/.claude` when that name is
absent. Under `config_isolation` step 6 has already replaced it with the private
root, so every consumer below — the collision scan, the transcript locator, the
transcript source, and the clear-and-rebind directory watch — follows the
private root with no separate computation and no code of their own.

The effective configuration root MUST be absolute and MUST carry no `..`
component, whichever of those three shapes produced it. `..` is the one path
construct whose meaning depends on what exists and on whether the component
before it is a symlink: the kernel resolves left to right, so a path through a
missing directory reports "no such file" even when it lexically names a live
directory, and a recursive create — which is what Claude performs on its own
`CLAUDE_CONFIG_DIR` — creates the missing component and then lands the path on
that live directory. Pmux refuses the spelling rather than collapsing it, since
collapsing `..` lexically is not equivalent to kernel resolution. For the same
reason, an absent directory only counts as evidence that no live session holds
it when the path carries no `..`.

A `cell: minified` start MUST NOT set `CLAUDE_CONFIG_DIR` or
`CLAUDE_SECURESTORAGE_CONFIG_DIR` in `environment.set`; `config_isolation` is
the supported way to give such a cell its own configuration root, and it is the
only one pmux canonicalizes, owner-checks and pristine-checks. An ordinary cell
keeps the environment channel.

For `new`, pmux selects the UUID before any settings or launch files are
prepared. If any `projects/*/<UUID>.jsonl` file already exists beneath the
effective configuration root, start fails with `id_collision`, even when its
contents identify another cwd. Pmux never appends a new session onto old
history or reuses a Claude UUID across projects.

For `resume`, exactly one transcript must validate both the requested session
UUID and canonical/Unicode-normalized cwd. Missing history produces
`transcript_unavailable`; multiple matches or a bounded-scan ambiguity produces
`schema_drift`.

The locator first checks deterministic
`<effective-config-root>/projects/<project>/<session>.jsonl` candidates, then
performs a bounded project-directory scan. A matching filename alone is not
sufficient: early transcript records must corroborate session ID and cwd.

The effective config root is `CLAUDE_CONFIG_DIR` when set, otherwise
`$HOME/.claude`, evaluated after environment policy is applied.

### 4.3 Compatibility profile admission

Before the interactive process starts, pmux runs the resolved Claude executable
with `--version` in the exact cwd and effective environment. The first strict
`major.minor.patch` token is normalized as the Claude version.

- `require_tested` requires one admitted profile whose Claude version RANGE
  contains the normalized Claude version, and whose `std::env::consts::OS`,
  `std::env::consts::ARCH`, terminal profile and resolved input transport match
  exactly. Request-level `auto` resolves to the actual `sdk` transport before
  lookup and reporting.
- A profile's range is a measured floor and a tested-through ceiling, inclusive
  at both ends. It may never span a major or minor version, so a `2.2.x` is
  refused by a `2.1.x` cell by construction rather than by a second clause. An
  operator profile that states only `claude_version` is an exact match, which is
  what it meant before ranges existed. Two profiles whose ranges OVERLAP on one
  platform are refused at boot as ambiguous; adjacent ones are admitted.
- Each admitted profile carries its empirically calibrated
  `transcript_drain_ms`, bounded to 1 through 60,000 ms.
- The admitted set is the operator's profiles followed by pmux's own
  **promoted** ones (`compatibility::PROMOTED_PROFILES`). Operator profiles are
  searched first, so an operator who measured their own host overrides a
  promoted cell whose range contains the same version rather than colliding with
  it. A promoted cell's drain is measured, and it is a bound POOLED over every
  version measured rather than a fit to any one of them — the measurement,
  corpus, margin, price and invalidating observations are recorded with the
  constant, the receipts are under `evidence/`, and
  `tools/promotion/measure_transcript_drain.py` regenerates them.
- Every destroyed Path B instance's transcripts are mirrored into a retained
  evidence corpus before its tree is erased, pruned to the exact row fields the
  drain measurement reads and therefore carrying no prompt or completion text.
  On by default, bounded, and disabled with `--path-b-no-evidence`.
- Five named conditions retract a promoted range
  (`compatibility::RepromotionTrigger`), each bound to the code that detects it:
  an unclassified transcript row kind, a reachable arrival above the bound, a
  rejected launch bundle, a `/clear` screen or preamble that does not match, and
  a major or minor version change. The daemon publishes them in the
  configuration layer of the health tree.
- `allow_untested` permits an unmatched attempt after version detection, uses
  the daemon's explicit conservative `--untested-transcript-drain-ms` fallback,
  and reports `tested: false` plus an `untested_compatibility_profile` warning.
  A matching tested profile still wins and reports `tested: true`.
- Both policies retain all identity, launch, transcript, and completion checks.

The daemon accepts repeated `--tested-claude-profile <JSON>` values with exactly
these fields:

```json
{
  "claude_version": "2.1.207",
  "os": "macos",
  "arch": "aarch64",
  "terminal_profile": "transparent",
  "input_transport": "sdk",
  "transcript_drain_ms": 750
}
```

Unknown fields, malformed/non-normalized versions or platform tokens, zero or
oversized drains, and duplicate keys fail daemon startup. Operators MUST add a
profile only after reviewing and promoting guarded evidence for that exact
cell. The registry is empty by default.

### 4.4 Exact interactive argv

**Child argv is a pure function of the request the daemon received and of the
immutable stored version that request NAMES.** The second clause was added when
the agent resource landed, and it is a weaker claim than the one that stood
before it, so it is written here rather than glossed. It costs nothing only
because of four properties Section 4.8 states and enforces: the reference is
pinned by an explicit `AgentRef::version`, a stored version is immutable, the
resolution `(AgentSpec, per-session fields) -> StartSessionRequest` is a pure
function run once at admission, and the resolved configuration's digest is
echoed on the response. Drop any one of them and this section is false. In
particular there is no "latest at start time": that would make argv a function
of *when* the request arrived, which this section has always forbidden.

Nothing downstream of resolution knows an agent exists. `resolve_claude_launch`
and `admit_bound_resources` receive a DTO indistinguishable from one a caller
typed inline, and
`crates/service/tests/agent_resource.rs::agent_resolution_is_a_pure_function_of_the_spec_and_the_session_fields`
pins that as a wire-form equality rather than as a description.

The driver owns session and execution-mode flags. The launch begins with exactly
one of:

```text
claude --session-id <UUID> [validated options]
claude --resume <UUID> [validated options]
```

Validated structured options may add model, effort, permission mode, allowed
tools, denied tools, settings files, MCP config files, plugin directories, and a
system-prompt file. The only raw `extra_args` admitted in v1 are `--debug` and
`--verbose`.

**A third source of argv exists and this paragraph used to omit it: the CELL.**
A `cell: minified` launch appends `claude_launch::MINIFIED_CELL_FLAGS` — today
`--strict-mcp-config`, and nothing else — after the structured options. It is
driver-owned in the strongest sense the daemon has: no caller supplies it, no
caller can suppress it, it carries no value, and `extra_args`' two-spelling
allowlist cannot express it. It is not an option a request names; it is a
consequence of the cell the request asked for, which is why it belongs in this
section and not in §4.6. Added to the launch by `20bf20f` after a minified cell
was MEASURED reaching the caller's account MCP connector list over HTTP without
it; this section was not updated at the time, so for one commit the normative
argv description was missing a flag every minified launch emitted. The binding
check is `stateless.rs::the_documented_minified_launch_bundle_is_the_argv_a_mint_emits`,
which drives a real mint and compares it against the two sites that publish the
list.

**The effort admission is pmux's own policy, and this must not be justified as
CLI enforcement.** `EffortLevel`'s admitted spellings and any model/effort
pairing pmux applies are CHOSEN product rules. Two MEASURED facts bound what the
CLI itself does, and both point the same way:

- **An unknown effort spelling is NOT rejected.** `--effort ultracode` was
  accepted by `2.1.220`. An unrecognized spelling **warns on stderr and silently
  uses the default** — and pmux never reads the child's stderr, so pmux cannot
  observe that warning at all. A guard that relies on the CLI refusing is a guard
  with no predicate.
- **The CLI does not enforce model/effort pairs.** `haiku-4-5/xhigh` and
  `sonnet-4-6/max` both ran successfully with the requested model. The
  API-level pairing table is a property of `output_config.effort`, not of the
  interactive CLI path pmux drives.

Consequently the guard must enumerate what pmux admits rather than assume a
downstream refusal, and the enumeration is derived from the enum through a
wildcard-free match (`every_variant!`) so a new variant cannot be invisible to
it: a hand-written array of five spellings was MEASURED to pass unchanged with a
sixth variant live and emitting `--effort ultracode`. Any refusal message must
also print the spelling that the flag actually takes — one refusal previously
read *"does not admit --effort XHigh; it admits [\"low\", …]"*, two spellings in
one sentence, and the one after the literal `--effort` is rejected by clap and by
`EffortLevel`'s own `Deserialize`. `EffortLevel::as_str` is the single spelling,
pinned against `Serialize`, and a test parses the token each message prints back
through the same parser the CLI uses.

#### Permission mode, and the one variant that is a single flag

`PermissionMode` has seven wire values: `default`, `accept_edits`, `plan`,
`auto`, `bypass_permissions`, `dont_ask`, and `dangerously_skip_permissions`
(`crates/protocol/src/v1.rs::PermissionMode`). Six emit the two-argument pair
`--permission-mode <claude-value>`. The seventh emits the single argument
`--dangerously-skip-permissions` and nothing else, because Claude exposes no
`--permission-mode` value for it. The mapping is a total, wildcard-free
`PermissionModeArgv::{Pair, Single}` match
(`crates/service/src/claude_launch.rs::{PermissionModeArgv,permission_mode_argv}`,
applied in `::build_args`), so a
future variant added without choosing its argv shape is a compile error rather
than a silently dropped flag.

`dangerously_skip_permissions` disables Claude's own permission prompts for the
whole session. It is reachable only through this typed variant: the closed raw
allowlist remains exactly `--debug` and `--verbose`
(`claude_launch.rs::SAFE_EXTRA_FLAGS`), so
the flag cannot be smuggled through `extra_args`
(`claude_launch.rs::dangerously_skip_permissions_is_one_flag_and_no_other_mode_emits_it`).
Before this variant existed the flag was not forbidden, merely unreachable.

A launch resolved with it sets `ResolvedClaudeLaunch.dangerous_permission_bypass`
(`claude_launch.rs::ResolvedClaudeLaunch`), and every `TurnResult` the session
produces MUST carry the warning code `dangerous_permission_bypass` with the
message `this session launched Claude with --dangerously-skip-permissions`
(`crates/service/src/v1/actor.rs::permission_bypass_warnings`). The warning is
republished on every turn — on the completed path
(`actor.rs::build_turn_result`) and on the cancelled-turn path
(`actor.rs::finish_cancel`) — so a result read in isolation still states that
the agent was unsupervised when it produced that result.

The warning's scope is exactly `TurnResult.warnings`, and therefore the
`turn_completed` event and CLI `--output ndjson`. It is deliberately not a
standalone `EventPayload::Warning`: that stream carries engine warnings about
transcript content, and `untested_compatibility_profile` behaves identically
(`actor.rs::compatibility_warnings`). `ProtocolWarning.code` is an open string
domain, so this code is not a protocol change and does not alter the closed v1
method, result, event, or error-code surface.

The service rejects driver-owned or non-interactive arguments, including:

```text
-p --print --bg --background --session-id --resume --continue
--output-format --input-format --teammate-mode
```

Inline settings, inline MCP documents, and replacement/appended system prompts
are materialized as random session-scoped mode-`0600` files below the private
runtime before argv construction. Only their private paths appear in argv.
Files are retained until the Claude process is reaped, then removed. Each file
is bounded to 8 MiB.

The final executable, argv, cwd, and full environment are registered with the
launch broker. A random launch token expires after 30 seconds and can retrieve
the process specification exactly once over a mode-`0600` UDS. Rmux sees only
the `pmux-launcher` path, broker socket, and token. `pmux-launcher` fetches the
specification and replaces itself with Claude. Secret values and prompt bodies
MUST NOT be logged.

The broker and launcher are required, not stylistic. Rmux's pane-spawn API is
additive only: it can set a variable on a pane but cannot remove one the shared
sidecar already inherited, while Section 4.5 defines an exact replacement
environment that MUST be able to delete inherited credentials such as
`ANTHROPIC_API_KEY`. A per-process `execve` wrapper is the only way to apply
per-session unset patches through one long-lived sidecar. The broker exists so
that the credential-bearing environment is transferred over an owner-only socket
and never written to a filesystem. Neither component claims that the launch is
invisible to process inspection: argv and environment remain readable by the
owning user (for example, macOS `ps -E`), and the trust boundary is the owning
UID as stated in Section 10.2. Replacing the broker with a mode-`0600`
specification file was considered and rejected: it is smaller, but it puts
secrets at rest on disk.

### 4.5 Environment and authentication

Environment application is deterministic:

```text
effective = allowlist(snapshot) - unset + set - policy_removals + profile_changes
            + config_isolation + minified_cell_environment
```

The order is fixed, applied exactly once, and pinned by
`crates/service/src/claude_launch.rs::documented_environment_order_is_allowlist_then_unset_then_set_then_removals_then_isolation`
(producer at `claude_launch.rs::build_environment`):

1. **`allowlist(snapshot)`** — the inherited caller snapshot, and only that
   term, is filtered by a closed allowlist. A name that is neither an admitted
   exact name (`crates/protocol/src/v1/launch_environment.rs::INHERITED_EXACT_KEYS`)
   nor covered by an admitted prefix (`::INHERITED_PREFIXES`) is dropped before
   anything else runs, and its **name** is recorded as a removal. Matching is
   case-sensitive in both the exact and the prefix form, so `path` is not `PATH`.
2. **`- unset`**, then 3. **`+ set`** — the caller's explicit patch, unchanged.
   **`set` bypasses the allowlist entirely.** It is the deliberate extension
   channel: any name pmux does not know about, including one the allowlist
   denies, reaches the child if and only if the caller states it.
4. **`- policy_removals`** — under `subscription` the Anthropic credential and
   redirect names are removed a second time here, so a name reintroduced
   through `set` is still stripped.
5. **`+ profile_changes`** — the terminal profile's own removals, the tmux-shim
   `PATH` prune, and `TERM=xterm-256color`.
6. **`+ config_isolation`** — when the request names a private root,
   `CLAUDE_CONFIG_DIR` is replaced by that root in canonical form and
   `CLAUDE_SECURESTORAGE_CONFIG_DIR` is set to the value `CLAUDE_CONFIG_DIR`
   would have carried *without* isolation, read from the pre-allowlist view
   exactly as step 5 reads `TMUX_PROGRAM`. That second variable is what keeps
   the isolated child on the caller's own credential store: Claude names its
   keychain item `Claude Code<suffix>` where the suffix is
   `sha256(securestorage_dir or config_dir)[0..8]`, so a private config root
   alone would look up an empty item and report "Not logged in". An absent
   pre-isolation root pins the **empty string**, which selects the default,
   unsuffixed store — the value is deliberately preserved rather than dropped.
   This step runs last so that a future `CLAUDE_`-prefixed denylist entry cannot
   strip the pin; the root is canonicalized because it must name the directory
   pmux seeds and the transcript locator walks, and the pin is passed
   byte-for-byte because Claude hashes it.
7. **`+ minified_cell_environment`** — applied only when
   `StartSessionRequest.cell` is `minified`, after step 6 and for the same
   reason. It is a closed table of exactly one entry today,
   `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1`
   (`claude_launch.rs::MINIFIED_CELL_ENVIRONMENT`), and it is MEASURED rather
   than reasoned: every private configuration root pmux seeds otherwise
   downloads the official plugin marketplace from GCS on first launch — 428
   files, 6.2 MB, 39 plugin directories, 31 `SKILL.md` files and 8+ third-party
   `.mcp.json` — starting 11 s after launch, inside the readiness window, into a
   root Section 4.2's pristine check requires to be empty. A cell whose claim is
   that it carries nothing from the caller before it cannot also carry a
   third-party plugin tree it did not ask for. It is a **table and not a
   prefix** because four adjacent names are deliberately absent:
   `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, `DISABLE_TELEMETRY`,
   `DO_NOT_TRACK` and `CLAUDE_CODE_SAFE_MODE` were each MEASURED to break the
   cell 5/5 by rendering a persistent notice that changes the screen shape and
   fails startup. Suppressing traffic is not the goal; delivering an instance
   nothing distinguishes from any other is, and a notice is a distinguishing
   mark. An ordinary cell's plugins are the caller's business and this step does
   not run for one.

`subscription` is the default. It removes Anthropic API keys/tokens, Anthropic
base/provider redirect variables, Bedrock/Vertex/Foundry selection variables,
and nested/remote Claude markers before launch. This forces use of the
interactive subscription authentication already associated with the effective
Claude config root.

`inherit` retains those values and is an explicit caller decision. It does not
relax any other validation.

**The allowlist is auth-policy aware.** Under `inherit` the provider-routing
families — `ANTHROPIC_*`, `AWS_*`, `GOOGLE_*`, `GCLOUD_*`, `CLOUDSDK_*`,
`AZURE_*`, `VERTEX_REGION_*`, plus `CLOUD_ML_REGION` — survive step 1
(`launch_environment.rs::{PROVIDER_ROUTING_PREFIXES,PROVIDER_ROUTING_EXACT_KEYS}`,
branch in `claude_launch.rs::inherited_from_snapshot`). Provider routing is not
one variable: Bedrock resolves credentials through the AWS SDK's own environment,
Vertex through Google ADC, Foundry through Azure, so admitting a selector while
denying the credential it selects would leave `inherit` broken in a way that
looks like an auth outage. Under `subscription` those same names are denied at
step 1 **and** removed at step 4; both mechanisms are kept deliberately.

Both modes reject invalid names/values, `CLAUDE_CODE_SKIP_PROMPT_HISTORY`, and
environment names containing agent-team or teammate markers. Prompt history is
mandatory because exact typed-prompt acknowledgement is a completion
precondition.

The `transparent` profile removes rmux/tmux/terminal-program identity variables,
removes `RMUX_*`, removes agent-team identity, and sets `TERM=xterm-256color`.

Caller configuration does **not** survive as a class. The snapshot filter is
`unknown means denied`, so an inherited name reaches the child only because it
is named in the allowlist or is restored by `set`. The allowlist deliberately
admits the infrastructure Claude cannot run without — `PATH`, `HOME`, `SHELL`,
`USER`, `LOGNAME`, `TMPDIR`, `PWD`, `TZ`; terminal identity and geometry;
`LANG`/`LANGUAGE`/`LC_*`; TLS-trust and proxy names; `XDG_*`; `NODE_OPTIONS`,
`NODE_PATH`; `SSH_AUTH_SOCK` and the Git configuration names its Bash tool
needs; and the `PMUX_` namespace — and **`CLAUDE_CONFIG_DIR` is among them**, so
the effective config root of Section 4.2 is inherited and remains intact unless explicitly
unset or replaced by `config_isolation` (step 6). Anything outside that list is dropped even though no policy forbids it.

#### Why an allowlist rather than a denylist

A denylist cannot be completed, and this one demonstrably was not.
`CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_REMOTE` and
`CLAUDE_CODE_CHILD_SESSION` were each added to the transparent profile's removal
list only *after* a live failure, because a Claude launched from inside another
Claude reads them and changes behavior. The fourth one cost a whole gate
attempt: with `CLAUDE_CODE_CHILD_SESSION` inherited, the child Claude never
wrote its own project transcript, so the transcript authority of Section 6 had
nothing to read and every turn hung at `awaiting_prompt_ack` until the deadline.
Every one of the four was invisible to the entire deterministic suite, because
the deterministic fake Claude does not read them.

There is no reason to believe the fifth such marker will not exist. Under a
denylist it arrives silently and is diagnosed by another live failure; under
`unknown means denied` it is dead on arrival and nobody has to know it was
invented. The cost of that choice is real and is paid in the other direction:
an inherited name that some caller genuinely needs is dropped until it is either
added to the allowlist or passed through `set`. That is a loud, locally
diagnosable failure, and it is the trade this specification chooses.

**The removal set is the audit surface.** `ResolvedClaudeLaunch` reports every
name the caller offered that the child does not receive, whichever step dropped
it; the reasons are deliberately not distinguished, because completeness rather
than attribution is what makes it honest. `pmux probe` renders that set by name
only. Environment **values** are never serialized into a probe report, a log,
or argv (Sections 4.4 and 10.2).

**The escape hatch is `set`, and the CLI reaches it by name.**
`--env-passthrough KEY`, accepted by every command that builds a launch
(`run`, `start`, `probe`), reads `KEY` from the CLI's own process environment and
places the pair in `EnvironmentSpec.set`, so a variable the allowlist denies can
be forwarded without its value ever appearing on a pmux command line.
Section 4.4's non-claim still applies: the launched child's environment remains
readable by the owning uid, and the trust boundary is that uid.

### 4.6 Lifecycle modes

`transcript` is the default and requires no injected hooks.

`hybrid` composes caller settings into one private generated settings document,
preserves caller hooks, and appends pmux `SessionStart`, `Stop`, and
`StopFailure` commands that invoke `pmux-hook`. Each hook forwards bounded JSON
stdin to a session-specific mode-`0600` relay using an explicit session UUID.
Hybrid preparation fails closed if settings cannot be composed, the hook binary
is unavailable, or the relay cannot be secured.

Hook observations contain only lifecycle corroboration. Hook-provided output,
usage, or other semantic content is ignored. A Stop/StopFailure observation may
set `completion.lifecycle_hook_observed`; a missing per-turn observation adds a
`lifecycle_hook_missing` warning but cannot block transcript-based success or
cause an authority fallback.

The composed Hybrid settings document is bounded to 1 MiB, each hook frame and
stdin payload to 64 KiB, and the relay to 16 concurrent connections. The
configured hook I/O timeout defaults to five seconds and MUST be non-zero and
no greater than ten minutes.

### 4.7 Retention

`run_once` forces `one_shot` retention internally and closes the session after
a result or failure path. It forces it **after** any `agent` reference is
resolved, and that order is the whole rule: resolution replaces the entire
launch policy with the stored version's, so a `one_shot` written into the
request before the start ran was replaced by the agent's `persistent` and the
session was registered with the agent's idle TTL. This is the one field a
method may decide over an agent's stored value, and only because the method
closes the session itself. Public `start_session` accepts only `persistent`;
passing `one_shot` is rejected with `unsupported_feature` so a caller cannot
create a session with no durable cleanup policy. A persistent session remains
available for later turns and is closed explicitly by `close_session` or daemon
shutdown.

The v1 persistent policy carries `idle_ttl_ms` (30 minutes by default), and the
current snapshot reports the computed `idle_deadline_ms`. Registration and any
later timestamp mutation fail before publication if adding the idle TTL would
leave the protocol-v1 safe-integer domain; an idle-eligible snapshot never
silently omits an unrepresentable computed deadline. The native service
periodically asks each actor to expire itself. Expiration is atomic inside the
actor: it closes only a non-running, unattached `ready` or startup
`needs_input` session whose deadline is past, then unregisters the actor and
drops its launch artifacts after process reaping. An active turn or attached
needs-input interaction therefore cannot be closed by a stale reaper
observation. Explicit close remains the deterministic cleanup API.

### 4.8 Agents, profiles, and what each may say

**This section was amended.** It used to read "pmux has no server-side agent
registry and MUST NOT grow one." pmux now has one, and the sentence it replaces
is recorded here rather than deleted, because the two arguments that clause
rested on did not fare equally and the difference is where the design lives.

**The uid argument survives, and is CONCEDED IN FULL.** The daemon and its
clients run as the same uid (Section 10.2), so a server-side registry adds zero
enforcement: anything it would refuse, the caller can send directly as an
ordinary DTO. **An agent is therefore not a security boundary and MUST NOT be
documented as one.** Every containment rule in this section is a *narrowing* of
what one request may say, composed with `AND` against the checks that already
run, and never a capability. The value of the resource is deduplication,
pinning, and auditability. Section 4.8.3 refuses several otherwise-attractive
features precisely because they would only make sense if this argument were
false.

**The argv-purity argument is answered, and only by a specific shape.** Section
4.4 now reads "a pure function of the request and of the immutable version the
request names", and the four properties that make that true — mandatory version
pinning, immutable stored versions, pure resolution at admission, and an echoed
digest — are what this section requires. Without all four the registry would be
the impurity the old clause refused, and it would not be worth building.

**One thing the old section got exactly right and this one keeps:** *evidence
admission belongs to the operator; preferences belong to the caller.* An agent
carries preferences. It carries no evidence. It cannot claim its cell is tested,
cannot widen the launch-environment allowlist, cannot admit an untested
compatibility profile the registry of Section 4.3 would refuse, and cannot reach
anything in the Path B pool's settings.

#### 4.8.1 The one invariant

> **An agent may narrow what a session may name. It may never name a resource on
> the session's behalf.**

Every field of `AgentSpec` classifies mechanically under it:

| Class | Rule | Fields |
|---|---|---|
| Launch policy | No filesystem or process identity. Moves to the agent. | `claude`, `environment.set`/`unset`, `auth_policy`, `terminal`, `lifecycle`, `retention`, `compatibility`, `cell` |
| Bound resource | Names a directory or identity pmux *claims*. Stays per-session; the agent may bound it. | `cwd`, `config_isolation`, `identity` |
| Caller process snapshot | Is a fact about the calling process at call time. Stays per-session, structurally. | `environment.snapshot` |

`cwd` is never expressible in an agent, for the reason it is never expressible
in a profile: it is the most consequential launch parameter, and a stored value
that silently redirects where an agent operates is exactly the ambient
resolution this product refuses everywhere else. An agent may carry
`containment.workspace_root`, which BOUNDS a cwd and never supplies one — the
caller still writes `--cwd` on every call — and which is decided on the
*resource* rather than on a path prefix, in the direction its own name states.

`environment.snapshot` is not merely discouraged: `AgentEnvironmentSpec` deletes
the field, so the sentence "an agent stores a caller snapshot" is unsayable.
`environment.set` may not carry any name that would move the child's Claude
configuration root; that table is `claude_launch::CONFIG_ROOT_ENV_DOORS`, and an
agent is refused for every cell rather than only the minified one, because an
agent id is a name every session started from it shares.

Exactly one of `agent` and the inline launch fields may be present on a start.
Merging is refused rather than resolved: a merge surface needs one documented
rule per field and one test per rule, and nothing derives that list. The
conflicting set IS derived — from the serialized leaf paths of the two types,
intersected — by
`crates/protocol/tests/v1_wire.rs::the_agent_supplied_start_paths_are_exactly_the_serialized_leaf_collision`.

#### 4.8.2 Versioning, and the store

`AgentVersion` is a monotonic `u64` counter starting at 1, deliberately not a
timestamp: two updates inside one clock tick would share a timestamp, and a
clock that steps backwards would order them wrongly. The counter ORDERS;
`config_digest`, a SHA-256 over the canonical serialization of the unredacted
spec, is IDENTITY.

`UpdateAgentRequest::expected_version` is REQUIRED and is a fence rather than a
routing key, the same idiom and the same argument as
`ClearSessionRequest::expected_transcript_session_id`: any value that is not the
current head is `id_conflict`, including one stale by exactly one revision, and
nothing is ever answered as "your update already landed". Recovering a lost
response costs one `get_agent` and never a wrong answer — and the head that
fence is compared against is the RESOLVED one below, not the raw pointer, which
is what makes consecutive attempts on one fence answerable identically instead
of stale in opposite directions.

A running session is unaffected by any update. It resolves and COPIES its spec
at start and pins `(agent_id, version, config_digest)` for life, which is the
same rule, for the same reason, as `SessionCell`.

The store is held to **the same bar as the socket directory and the Path B pool
parent**: every level pmux creates is `0700` from birth and every file `0600`
from birth, passed to `mkdir(2)`/`open(2)` rather than chmod'd afterwards; a
tree the operator already made and did not make owner-only is REFUSED at boot,
naming what is wrong and what would be right; and pmux never re-permissions a
tree it did not create. The mode is re-checked on every READ, because a version
file is read at `start_session` time and a file an operator widened between
boots is a file whose contents pmux should not trust.

That per-read check is on **the bytes, not the name in front of them**: the file
is opened with `O_NOFOLLOW`, the mode and owner come from `fstat` on the open
handle, and the handle is what is read, so nothing can be substituted between
the check and the read and a symlink is refused outright. A symlink's own mode is
`umask`-dependent -- under `umask 077` it is born `0700` -- so a guard that
`stat`ed the link and then read through it was checking a mode that had nothing
to do with the file it returned. The per-agent DIRECTORY is re-checked on every
read too, for the same reason its files are.

**A version is published atomically and exclusively.** The bytes are written and
`fsync`ed under a temporary name no other writer shares, and `link(2)` gives the
finished inode its real name: naming the file and refusing to overwrite one are
the same syscall, so two concurrent `update_agent` calls holding one
`expected_version` publish one version or neither. The head comparison is a
courtesy that produces the good message for an ordinary stale caller; it cannot
be the fence, because the daemon serves many connections at once and there is no
lock between reading `head` and writing the next version. `create_agent` is
assembled under a staging name that is not a UUID and becomes visible at its real
name in one `rename(2)`, so a listing can never see a half-made agent — MEASURED
under SIGKILL at a jittered offset, 40 of 40 trials listed only complete records,
0 unreadable, every listed record readable through `get_agent`.

**`head` is a durable LOWER BOUND on the newest published version, not the
newest published version.** Publishing a version and moving the pointer are two
operations on two files and cannot be one syscall, so a crash between them is
not a window that can be narrowed away — it is recovered by the next reader.
Every read resolves the head by walking forward from the pointer over every
version NAME that exists, and `update_agent` mints the next number after that,
so the number it publishes at is one no name is taken for. The walk is a loop
rather than a one-step lookahead because `head` may transiently REGRESS: it is
written as an absolute value with no lock behind it, so a writer descheduled
between its `link(2)` and its pointer write can land that write after two later
writers moved the pointer past it.

**A version published before the pointer reached it is ADOPTED, not discarded,
and the caller-visible consequence is that an update interrupted by a crash MAY
have landed.** That is not a weakening of the fence and no ordering avoids it:
the same crash one line later would have moved the pointer, and either way the
response was never delivered. It is the case this section already prescribes a
recovery for — read `get_agent`, compare `config_digest` — and adoption is what
makes that recovery truthful. Discarding instead would require unlinking a
published file, which would make "a version is never removed" false for a
version a session may have pinned, and no reader can tell a crashed writer's
orphan from a live writer's version published microseconds ago, so a recovering
reader would delete versions live writers were about to point at.

The sentence that stood here until this commit said such a crash "reads as 'it
did not land', which is the safe direction". **MEASURED FALSE**: with a SIGKILL
harness killing a looping updater at a jittered offset, 15 of 40 trials left an
agent that could never be updated again — `update_agent` recomputed the same
next number every time and `link(2)` refused it every time, so consecutive
attempts were told the fence was stale in OPPOSITE directions, and `list_agents`
reported the record healthy at the older version with nothing unreadable:

```text
trial 20: head=2  published_max=3
  retry@2 -> id_conflict: agent ef7f31ff-… is at version 3, not the expected version 2
  retry@3 -> id_conflict: agent ef7f31ff-… is at version 2, not the expected version 3
  list: agents=[2] unreadable=0
```

After the change, 50 trials, 0 wedged, 19 of them landing in exactly that window.

A version NAME that is taken by something that is not a readable version — a
torn file, a symlink, a directory — is **reported, not stepped over**: the walk's
step predicate is `link(2)`'s and no narrower, so `update_agent` never targets it,
`get_agent` refuses naming the file, and the record appears in
`AgentList::unreadable` rather than being summarized at the older version behind
it. Any narrower predicate reaches the same wedge by a second road.

**Scope of "durable", stated rather than assumed.** `File::sync_all` is a real
media barrier on the platform this ships on — rustc 1.88.0's
`library/std/src/sys/fs/unix.rs:1212` issues `fcntl(F_FULLFSYNC)` on Apple
targets, not `fsync(2)` — and MEASURED, it also succeeds on the read-only
directory handle `sync_parent_directory` opens. But that directory flush is
best-effort by construction: its failure is discarded, because the publication is
already atomic against a concurrent reader and a platform that refuses to sync a
directory is not a reason to fail a write that landed. So the measured claim is
against PROCESS DEATH, which is what the harness kills with; against power loss
the version bytes carry a barrier and the directory entry naming them does not
necessarily.

**`list_agents` reports the records it could not read and never loses the ones it
could.** `AgentList::unreadable` names each such record by id with the refusal
`get_agent` would have given for it, and is omitted from the wire when empty. A
listing that answered with one bad record's refusal would make the missing-agent
recommendation -- "list the stored agents with `pmux agent list`" -- unreachable
in precisely the state it is offered; a listing that dropped the record instead
would be a stored agent silently ceasing to exist.

A stored agent is named by a daemon-minted UUID used verbatim as the directory
name, and never by its human `name`. That makes directory traversal
unconstructible rather than filtered, which is the same move
`CONFIG_ROOT_ENV_DOORS` makes for the config-root environment names.
`agent_profile::validate_agent_name` is not a path-component validator and must
never be used as one: MEASURED, it admits `..`, `.`, `...`, `a..b` and `-`.

`get_agent` and `list_agents` never emit an environment value or an inline
settings/MCP document body; each is replaced by `sha256:<hex>`. The system
prompt is deliberately NOT redacted: `pmux probe` redacts it because `probe`
prints to a terminal, and an agent's system prompt is the single most important
thing about it. `config_digest` is computed over the *unredacted* spec, so it
still identifies the configuration exactly while the frame discloses nothing.

`AgentDescriptor::spec` is carried as an opaque document on the response and
decoded with the strict `AgentSpec` type by whoever wants it typed. That
asymmetry is the two halves of the wire contract, both kept: a request DTO is
`deny_unknown_fields`, so `{"auth_polcy": "inherit"}` can never be stored as a
silent default; a response DTO must accept unknown fields, so a newer daemon can
add one without breaking an older client. A single strict type on a response
would force every client in all three languages to keep two decoders for one
type, and none of them does.

#### 4.8.3 What deliberately does not exist

- **Path B never gains an agent reference.** `RunStatelessRequest` names no
  resource, and an `agent_id` is a name a caller can write, which means two
  callers can write the same one. It also breaks three specific things: the
  pool's class key is `(model, effort)` and an agent reference would make it
  `(model, effort, agent_version)`, so `--path-b-warm MODEL[/EFFORT]=COUNT`
  could no longer name a class; an agent carries a `system_prompt`, which is the
  field `RunStatelessRequest` refuses *by name*; and the minified cell's whole
  claim is that the daemon minted both its config root and its cwd.
- **No per-session overrides.** See the merge argument in Section 4.8.1. To vary
  one field, `update_agent` mints a version or `create_agent` mints an agent.
- **No `delete` or `archive` on the wire.** The uid argument applies to this
  design's own surface: a delete method adds zero enforcement over `rm -rf
  <store>/<agent-id>`, and a running session is unaffected either way because it
  pinned by value.
- **No server-side `extends`.** Inheritance would make a stored agent's
  effective configuration depend on another stored object's *current* state,
  which is the exact impurity versioning was introduced to remove. Composition
  stays a client-side authoring concern; the stored version is flattened at
  create.
- **No discovery, no vault, no `messages[]`.** pmux never *selects* an agent for
  the caller: `--agent` names one exact id and there is no search path.

#### 4.8.4 Profiles remain client-side, and are now an AUTHORING tool

An *agent profile* is a named bundle of the repetitive parts of a
`StartSessionRequest` — model, effort, permission mode, tool lists, settings and
MCP sources, plugin directories, terminal geometry, and auth/lifecycle/
retention/compatibility policy. Profiles are expanded entirely in the client,
before the request is framed (`crates/client/src/agent_profile.rs`). The daemon
never learns that a profile existed.

There is consequently **no `profile_name` wire field**, in any request or
result, ever. The expanded DTO is the complete truth about a launch. `extends`
chains, composition operators and `require_env` are how a human WRITES a
configuration; `pmux agent create --from-profile` flattens one into a stored
agent, refusing BY NAME the two keys an agent may not carry (`config_isolation`,
which names a resource, and `require_env`, which is a check against the calling
process's environment that a daemon has no calling process to run).

**The CLI flags were renamed, and the old spellings are refused rather than
aliased.** `--agent`/`--agent-file`/`PMUX_AGENT`/`PMUX_AGENT_FILE` selected a
client-side profile; they are now `--profile`/`--profile-file`/`PMUX_PROFILE`/
`PMUX_PROFILE_FILE`, and `--agent`/`PMUX_AGENT_ID` names a stored server agent.
Each retired spelling is refused with the new one named in the message. A silent
alias is exactly how a caller reaches for one feature and gets the other, and
the two disagree about the single most consequential thing a launch
configuration can say.

Contrast the tested-profile registry of Section 4.3, which is correctly
server-side. It does not carry a caller's preferences; it carries the operator's
assertion that reviewed evidence exists for one exact compatibility cell, and a
caller MUST NOT be able to claim that its own cell is tested.

Profile rules that are part of the product contract:

- `cwd` is never expressible in a profile, and neither are session identity, the
  prompt, or the turn deadline. Naming any of them is a parse error, not a
  silent no-op (`agent_profile.rs::PER_INVOCATION_KEYS`, refused by
  `::reject_unknown_keys`). `cwd` is the most consequential launch parameter; a
  config file that silently redirects where an agent operates is exactly the
  ambient resolution this product refuses everywhere else.
- There is no discovery: no XDG search and no upward walk from the working
  directory. A document is named by an explicit absolute path, or by the single
  `PMUX_PROFILE_FILE` environment fallback (`bin/pmux/src/cli.rs::LaunchArgs`,
  held to an absolute path by `agent_profile.rs::load_agent_profile`).
- Supplying a profile file without naming a profile is not an error. It means
  "profiles live here, but not this time" (`cli.rs::resolve_agent_profile`);
  `PMUX_PROFILE_FILE` is meant to be exported once in a shell profile, so
  refusing it would break every invocation that does not want one. The invariant
  preserved is that pmux never *selects* a profile for the caller: no name, no
  profile.
- Composition: scalars replace and absent inherits; lists append parent-first,
  because argv repeats one flag per element; `extends` is a single chain bounded
  at depth 4 with cycle detection (`agent_profile.rs::MAX_AGENT_CHAIN_DEPTH`,
  `::resolve_chain`, `::merge_into`). A literal JSON `null` is a parse error,
  because v1 has no unset operator (`::reject_nulls`). Unknown keys, duplicate
  keys, and values that are reserved but unimplemented in v1 (`rmux_standard`,
  `attached_stream`, `retention: one_shot`) are rejected at expansion time,
  naming the profile and the key, rather than surfacing as an opaque daemon error
  one launch later (`::reject_unknown_keys`, `::reject_duplicate_keys`, `::reject_reserved_values`).
- An explicit CLI flag overrides the profile's scalar and says so: the note is
  built in `cli.rs::override_scalar` and printed to stderr as `pmux: <note>` in
  `cli.rs::build_start_request`, keeping stdout exactly one machine-readable
  record. Silence would make the effective launch depend on a file the caller
  cannot see in the command they typed.
- The loader performs safety checks the service does not. Any inline settings or MCP
  document anywhere in the file forces the file itself to be owner-only (the inline
  check in `agent_profile.rs::expand`); every `{"source":"file"}` entry MUST be absolute
  and satisfy `mode & 0o077 == 0` (`::validate_resolved_paths`); and `require_env` asserts
  a name's presence without ever reading, copying, or printing its value, warning when
  the resolved policies — `auth_policy: subscription` or a `transparent` terminal
  profile — would strip that name from the child (`::verify_required_environment`).

## 5. Turn execution

### 5.1 Actor and idempotency contract

Every session has one actor mailbox. It serializes state mutation, event
sequencing, submission, cancellation, inspection, and close. Only one turn can
be active.

`TurnRequest` contains a caller UUID `turn_id`, prompt, optional absolute Unix
deadline in milliseconds, and a lease policy. On submission:

- An unseen `turn_id` in a ready session is accepted and starts one worker.
- The same `turn_id` with the same line-ending-normalized prompt returns
  `replayed: true`; it does not inject again.
- Reusing a `turn_id` with a different normalized prompt returns `id_conflict`.
- A distinct turn while one is active returns retryable `session_busy`.
- A turn submitted to a tainted, failed, closing, or closed session fails with a
  state-appropriate error.

An actor remembers at most 128 distinct `TurnId` records and 64 MiB of logical
prompt/terminal-result bytes by default. Records are never evicted while the
actor is live: forgetting an accepted ID could make a retry inject the prompt a
second time. Once either ceiling is reached, a new ID fails as
`turn_history_capacity_exceeded` before any PTY mutation; an existing ID keeps
its exact conflict/idempotent-replay behavior. Closing and explicitly resuming
creates a new actor and is therefore an orchestration recovery boundary, not a
transparent retry.

The default service deadline is ten minutes when the request omits one. The CLI
normally supplies a 120-second deadline; `claude-p` normally supplies 300
seconds. The worker computes one immutable wall-clock/monotonic boundary before
arming the transcript. That boundary covers transcript arm and polling,
terminal observation and completion evidence, prompt submission, drain
confirmation, and polling sleeps. The terminal adapter receives the same
absolute deadline and rechecks it immediately before Enter, so lock contention
or a slow input-admission gate cannot submit a prompt after expiry. Input
admission has its own 15-second infrastructure cap, always shortened by the
immutable turn deadline; this is neither a model-execution nor billing timeout.
The worker rechecks before
publishing success and the actor rechecks once more at the serialized terminal
commit. Expiry is stored and emitted exactly once by the actor as
`turn_timeout`; a duplicate `TurnId` replays that same failure and never races
into success or reinjection. `run_once` waits for this stored actor outcome and
does not manufacture a second local timeout (a later bounded infrastructure
guard can report `daemon_lost`, but never a competing turn result).

`TurnLeasePolicy.on_disconnect` and `heartbeat_timeout_ms` are reserved DTO
fields in the current daemon: request connections are short-lived, so only the
default `continue` action with no heartbeat is accepted. Any non-default lease
fails closed as `unsupported_feature`; it is never silently ignored. Durable
callers MUST reconnect using the same `TurnId` and MUST explicitly call
`cancel_turn` or `close_session` when policy requires it.

### 5.2 Prompt safety and injection

Before terminal mutation, pmux normalizes CRLF/CR to LF and rejects an empty
prompt, a leading slash command, NUL, ESC, or unsafe control characters. Space
and tab content is otherwise significant. The public 8 MiB frame limit is an
upper transport bound; the service and CLI independently cap prompt content at
1 MiB.

The actor arms transcript correlation at the current EOF and reads no history
(Section 6.1). Under the terminal mutex, the input adapter then requires a
nonzero-revision, cursor-correlated empty Claude editor to remain unchanged for
250 ms and pass an immediate full-snapshot fence. Cursor-less prompt-text
matching exists only for test doubles and cannot authorize production input.

After that first gate, the adapter issues exactly one bracketed paste. It waits
for a later nonzero revision whose active editor changed relative to the
pre-paste prompt anchor (rendered input rows or relative cursor position),
requires that editor signature to remain stable for 250 ms, and applies a
second immediate full-snapshot fence. Wrapped, multiline, whitespace, and
Claude-collapsed paste displays are accepted without requiring the literal full
prompt to be visible. Unrelated banner, history, or footer revisions are not
paste-render proof. A fence mutation restarts only the relevant observation;
the paste is never repeated.

Only after both gates, a final lease/deadline check, and no recognized blocking
screen does the adapter make one Enter attempt. Rmux success acknowledges a PTY
write, not Claude/Ink consumption. An ambiguous paste sends no Enter; an
ambiguous Enter is reported as a non-retryable recovery failure. Both paths fail
closed and reap the terminal, and Enter is never retried. A write the *turn
deadline* interrupts is reported as `turn_timeout` rather than as an ambiguity,
because the 15-second admission cap and the turn deadline are two clocks and
only one of them ending is the turn ending; the Enter case still records that
Enter was attempted, since which clock ran out says nothing about the byte that
already left. The later exact
main-session typed-user JSONL record remains the semantic prompt-acceptance
authority; pmux claims at-most-one Enter attempt, not exactly-once acceptance.
The final snapshot fence and Enter are separate rmux RPCs: the pmux terminal
mutex excludes local competing writes, but it cannot make asynchronous pane
changes atomically impossible between those operations. The present contract is
therefore at-most-one Enter after the last observed non-modal editor, not an
atomic compare-and-send. A future rmux primitive should provide
`send_key_if_revision(expected_revision, operation_id)` plus deduplicated status
lookup before pmux claims a stronger boundary.

Modal handling is deliberately stricter inside these admission gates. Startup
and post-Enter running/completion observations can publish resumable
`NeedsInput`. A modal discovered while `submit_prompt` is proving the pre-paste
or post-paste editor instead returns a typed `Needs*` terminal failure and the
worker force-reaps. This is mandatory after paste because editor consumption is
already ambiguous. A future adapter may make a positively pre-write modal
resumable, but must not weaken the post-paste fail-closed rule.

### 5.3 State model

The v1 state enum is:

```text
creating, booting, ready, submitting, awaiting_prompt_ack, running,
needs_input, terminal_candidate, draining, cancelling, tainted,
closing, closed, failed
```

The current native path registers a session after interactive readiness, then
normally follows:

```text
ready -> submitting -> awaiting_prompt_ack -> running
      -> terminal_candidate -> draining -> ready
```

Interactive readiness and turn admission deliberately use different stability
keys and MUST NOT be consolidated into one poll loop. Startup readiness requires
the whole terminal snapshot — every visible cell, the cursor, and the pane
revision — to be unchanged across its stability window, while the admission
gates in Section 5.2 compare only the editor region from the prompt anchor
through the cursor and deliberately ignore history, banners, and footer
animation. Startup is therefore the stricter of the two, and a future Claude
that animates anything while idle would surface as a start-session readiness
timeout rather than as a turn failure. The loops also differ in budget source
and in failure semantics.

Poisoning after an unpublishable terminal event is the one state change that is
not observable on the event stream. When the actor cannot publish a turn's
terminal event — the event-sequence domain is exhausted, or the commit-time
transition is refused — it moves directly to `tainted` and deliberately emits no
state-change or synthetic terminal event; under exhaustion no representable
sequence remains to carry one. It does so from whichever active turn phase it
was in, so the reachable state graph is a superset of the normal path's edges.
Callers observe the outcome through the stored terminal failure and
`inspect_session`, and close the exact generation.

When a startup or post-Enter running/completion snapshot contains a recognized trust, login,
permission, update, quota, or conservatively recognized unknown modal, the
actor enters `needs_input`, stores a redacted typed snapshot, and emits one
deduplicated `needs_input` event. The daemon never answers the screen. Startup
returns a live attachable handle in that state; during a turn the actor retains
the underlying phase and resumes it only after observing the unambiguous ready
or running condition. Turn deadlines, cancellation, and close remain effective
while blocked, and transcript completion cannot win while a modal is present.
Modal observations inside prompt-admission gates follow the terminal
failure/reap exception in Section 5.2 rather than this resumable path.
Unrecognized or localized screens can still time out and require Phase 0
signatures; rate-limit/authentication/billing taxonomy is not yet complete.

### 5.4 Cancellation and close

`cancel_turn(session_id, generation_id, turn_id)` targets exactly one active turn. The actor
sends one Ctrl-C, then waits up to five seconds for the normal ready prompt and
terminal quiet.

- Recovery returns `cancelled`, stores a cancelled result, and returns the
  session to `ready`.
- Failure returns `recovery_failed` and taints the session. A tainted session
  cannot accept another turn and SHOULD be closed.
- Cancelling an already terminal turn returns `already_terminal`.

`close_session` aborts any worker, closes the owned rmux session, and reports
whether the process was reaped. On Unix, confirmation requires observing the
dedicated POSIX session empty after cleanup and observing no descendant that
escaped that session; an rmux kill acknowledgement by itself is insufficient.
The observer retains every PID it sees in or below the boundary across teardown
polls. Exact members still in the isolated session can be SIGKILLed and
re-verified when asynchronous rmux cleanup or its transport is inconclusive;
an observed escape is never signalled by PID alone and permanently invalidates
positive proof. Before owner-pipe shutdown, `pmux-rmuxd` independently captures
all live pane boundaries, shuts down rmux, and performs the same bounded reap
pass, including when pmuxd was SIGKILLed. This is a conservative
process-boundary proof, not a claim that process-table sampling can rule out a
previously unobserved double-fork escape. Simultaneously SIGKILLing both pmuxd
and its sidecar prevents any userspace cleanup code from running and remains an
OS-level fault boundary rather than a claimed guarantee. An
unconfirmed reap leaves the actor in `closing` and registered so close can be
retried; only a confirmed reap moves the actor to `closed`, terminates it, and
releases its launch artifacts. The
operation is idempotent for an exact recently closed `(SessionId,
SessionGenerationId)` pair through a bounded tombstone cache. A delayed close
for generation A can replay A's closed result but cannot close active resume B;
all other A operations fail with non-retryable `stale_session_generation` and
the error does not disclose B's fence. Graceful and force policies are
represented in v1; the current rmux close path performs
owned-session cleanup for both. `run_once` and the manual `pmux run` composition
never report a successful turn if their final cleanup does not confirm reaping.

## 6. Transcript authority and completion

### 6.1 Location and incremental framing

The transcript source validates session ID and cwd, records stable file
identity, and reads only exact appended ranges. JSONL is framed on complete
newline-terminated records; an unterminated final line remains buffered and can
never complete a turn.

During an active turn:

- A file-generation change fails with `transcript_unavailable`.
- A backwards cursor, conflicting duplicate UUID, malformed complete JSON/UTF-8
  record, active-chain unknown row, or semantic identity mismatch fails with
  `schema_drift`.
- The source validates one atomic session-ID/cwd identity pair, verifies a
  complete-line boundary, and arms at the exact current EOF without reading
  unbounded history. Only post-arm rows enter correlation, so historical
  messages cannot acknowledge or finish the new turn.
- Unknown data outside correlation-critical paths may become a structured
  warning rather than a success signal.

There is no terminal-text fallback.

### 6.2 Exact turn correlation

The first new main-scope typed-user row after the arm point MUST contain the
exact normalized prompt and a UUID. This row becomes the prompt
acknowledgement. A different typed prompt or multiple acknowledgements fails
with `prompt_not_acknowledged`.

The engine follows the main parent-UUID graph from that acknowledgement. It
excludes historical branches and non-main scopes from completion. Assistant
fragments are grouped into logical messages by message ID, then request ID, then
row UUID. Exact duplicate rows are ignored; conflicting duplicates fail.

Known Claude `attachment` rows are typed structural nodes in that parent graph.
They are UUID/session/cwd validated and their subtype is allowlisted, but their
payload is never interpreted as a prompt, assistant result, tool record, or
usage source. An unknown or malformed attachment subtype fails closed; treating
all unknown rows as ignorable metadata would allow a correlation bypass.

Claude `system` rows on the active chain are held to the same standard: a
subtype is allowlisted only when the row's own payload proves it carries no
semantics and that no further model output can follow it, and every subtype
outside that allowlist remains active-chain unknown data and fails closed.

Tool uses and tool results are correlated and de-duplicated by `tool_use_id`.
Usage snapshots are counted once per logical assistant message; conflicting
usage for one logical message fails in strict mode. Arithmetic overflow fails.

### 6.3 Terminal message

The latest eligible main logical assistant message is terminal only when:

- it is marked as an API error; or
- its stop reason is `end_turn`, `max_tokens`, `refusal`, or `stop_sequence`.

`tool_use`, `pause_turn`, and an absent stop reason are non-terminal. Completed
and max-token outcomes, including `stop_sequence`, require at least one text
block in strict mode. An unknown stop reason on the active path is schema drift
in strict mode. A trailing user tool-result or attachment row prevents an
earlier assistant fragment from being treated as the current leaf.

### 6.4 Completion gate

A normal turn result is emitted only when all of the following hold at the same
poll boundary:

1. The exact typed prompt was acknowledged after the arm offset.
2. The active main chain has a terminal assistant logical message.
3. The transcript cursor generation is unchanged and its offset never moved
   backward.
4. The source is at EOF with no partial line.
5. The transcript has remained unchanged for at least the selected
   compatibility profile's calibrated drain. An explicit unmatched
   `allow_untested` session uses the daemon's conservative fallback instead.
6. A structured rmux snapshot has a nonzero revision and its visible cursor is
   correlated to the normal empty `❯` editor. Placeholder text is allowed;
   historical prompt glyphs without the active cursor are not readiness
   evidence.
7. Manual snapshot polling observes the same rendered terminal state stable for
   250 ms within the bounded evidence check. Raw rmux wait-timeout screens never
   cross the adapter.
8. No recognized blocking screen or private rmux lease loss overrides the
   evidence.

Every one of these conditions is independently necessary: none of them
corroborates another, and removing any single factor admits a completion the
others do not justify. In particular the terminal factors 6 through 8 are
liveness preconditions in their own right, not confirmation of the transcript
evidence in factors 1 through 5.

Terminal quiet without a transcript candidate cannot succeed. A transcript
candidate without ready/quiet/drain evidence remains in `draining` and is
polled. Hybrid Stop evidence is recorded but is never a required authority.

The Hybrid `Stop` hook is nevertheless retained deliberately as the designated
fallback for factor 6. It is the only completion signal in the product that is
independent of both the transcript and the rendered screen, fired by Claude
itself at the moment factor 6 infers from geometry. If a future Claude release
changes the composer geometry such that ready-prompt observation can no longer
be calibrated (Section 13), a promoted Hybrid `Stop` observation is the planned
replacement for that liveness factor — it never becomes a semantic authority.
The hook machinery MUST NOT be removed on the grounds that it currently
contributes only `completion.lifecycle_hook_observed`.

### 6.5 Result normalization

`TurnResult` contains:

- session and turn UUIDs and outcome;
- text formed only by concatenating text blocks from the terminal main logical
  assistant message;
- those final text blocks, ordered tool records, model, and stop reason;
- token usage, timings, warnings, detected Claude version, the exact
  `CompatibilityReport`, completion provenance, and final event sequence.

Thinking blocks are excluded. Tool progress cannot become final prose. Cost is
`null`/absent unless a trustworthy source exists; the current subscription path
never fabricates it.

The protocol has `main`, `sidechain`, and `combined` usage fields. The native
service groups and deduplicates usage independently for the selected main
chain and sidechain logical messages, excludes team/meta rows, and reports the
sum separately as `combined`. Tool timestamps are currently absent and MUST
NOT be inferred from transcript row order.

Every successful result reports `completion.authority = transcript` plus booleans
for prompt acknowledgement, terminal message, ready prompt, quiet, drain, and
optional lifecycle-hook observation.

`SessionHandle`, `SessionSnapshot`, and `TurnResult` each report the selected
compatibility cell: Claude version, OS, architecture, terminal profile, resolved
input transport, tested status, and transcript drain. This makes an untested
override visible before the first turn as well as in the final result.

`SessionSnapshot` additionally reports `transcript_session_id` — the transcript
this session's turns are currently proven from, equal to the session ID until a
`clear_session` rotates it — and `cell`. Both are published so a caller reads the
`clear_session` fence and the session's cell rather than reconstructing them from
its own bookkeeping; a reconstructed fence is a guess, and a wrong fence is
either refused or, worse, answered as an already-completed clear.

Before storage or emission, the actor sizes the exact result in both direct
`turn_result` and single-event `turn_completed` wire envelopes. A value that
cannot fit the 8 MiB native frame is not silently truncated: the accepted
`TurnId` stores and replays one compact `result_too_large` terminal failure, and
no `turn_completed` event is emitted. The daemon retains a final defensive
oversized-response replacement as a transport backstop.

## 7. Native protocol v1

### 7.1 Transport

The public transport is a Unix-domain byte stream. Each frame is:

```text
4-byte unsigned big-endian JSON byte length
UTF-8 JSON payload of exactly that length
```

The maximum request or response frame is 8 MiB. An oversized request receives a
bounded error and the connection closes because the unread body cannot be
resynchronized. If a dispatcher unexpectedly produces an oversized response,
the daemon replaces it with a bounded typed `result_too_large` error instead of
writing an invalid frame. A connection may carry multiple request/response
pairs sequentially. Official clients use a fresh connection per request so
retry and long-poll behavior has no hidden shared state.

Clients MUST supply an exact absolute socket path and MUST validate response
version and `request_id`. There is no public TCP listener or daemon autostart.

### 7.2 Envelopes

A request is:

```json
{
  "version": 1,
  "request_id": "d10cb900-5d9b-4ad9-9ac5-73bb40e31b69",
  "method": "ping"
}
```

A successful response contains exactly one typed `result`:

```json
{
  "version": 1,
  "request_id": "d10cb900-5d9b-4ad9-9ac5-73bb40e31b69",
  "result": {
    "type": "pong",
    "data": { "server_version": "0.1.0", "protocol_version": 1 }
  }
}
```

Execution-affecting request envelopes and nested request DTOs reject unknown or
missing fields so a misspelled launch/security option cannot be ignored.
Response and event object fields are additive within v1: older decoders ignore
fields they do not understand while still requiring all known mandatory fields,
exactly one result/error, recognized result/event/enum discriminants, matching
request/session identity, monotonic event sequences, and the supported major
version. A failure contains exactly one `error`:

```json
{
  "version": 1,
  "request_id": "d10cb900-5d9b-4ad9-9ac5-73bb40e31b69",
  "error": {
    "code": "session_not_found",
    "message": "session is unavailable",
    "retryable": false
  }
}
```

Version mismatch is explicit and fails closed. A malformed request whose UUID
cannot be recovered is correlated with the nil request UUID.

All protocol-owned JSON integers are exact non-negative values in
`0..=9_007_199_254_740_991` (the largest integer represented exactly by every
supported JavaScript client). This applies to timestamps, deadlines,
sequences/cursors, token counters, offsets, durations, capacities, dimensions,
and version/count fields even when a Rust storage type could represent more.
Numbers recursively contained in otherwise opaque JSON values are limited to
the signed exact range
`-9_007_199_254_740_991..=9_007_199_254_740_991`. Non-finite, fractional where
an integer is required, rounded, or coerced values are rejected. A producer
MUST fail before emitting an out-of-domain value; saturating arithmetic MUST
NOT reuse a cursor or sequence. A future protocol that needs full-width
integers can add an explicit decimal-string representation rather than
silently weakening v1 interoperability.

### 7.3 Methods

| Method | Parameters | Result | Semantics |
| --- | --- | --- | --- |
| `ping` | none | `pong` | Server and protocol versions; no Claude turn. |
| `start_session` | `StartSessionRequest` | `session_started` | Validate and start one persistent interactive session; `one_shot` is rejected. |
| `run_turn` | session ID + generation ID + `TurnRequest` | `turn_accepted` | Idempotently submit; completion arrives through events. |
| `cancel_turn` | session ID + generation ID + turn ID | `turn_cancelled` | Interrupt exact turn and report recovery. |
| `inspect_session` | session ID + generation ID | `session_snapshot` | Current state/cursor/last-turn snapshot. |
| `attach_session` | session ID + generation ID, optional size, read-only flag | `attach_capability` | Mint one short-lived proxy capability; read-only currently rejected. |
| `close_session` | session ID + generation ID + policy | `session_closed` | Abort work and reap owned process tree. |
| `subscribe_events` | session ID + generation ID, `after_sequence`, `wait_ms`, `max_events` | `events` | Bounded replay/long-poll batch. |
| `run_once` | start request + turn request | `turn_result` | Canonical start/turn/wait/close operation. |
| `clear_session` | session ID + generation ID + expected transcript ID, optional deadline | `session_cleared` | Clear one minified-cell session's context between turns. |
| `diagnose` | none | `diagnosis` | Complete one real operation against the private runtime and report per session; no Claude turn. |
| `run_stateless` | model + effort + prompt, and nothing else | `stateless_result` | Serve one self-contained turn from the Path B pool. pmux mints every resource; the caller names none. |

`run_stateless` is the whole Path B caller surface, and its shape is the guarantee. The request
`deny_unknown_fields`, so the sixteen resource names a caller might reach for — session id, cwd,
configuration root, tools, environment, terminal geometry — are refused **by name** rather than
ignored. `StatelessResult` publishes exactly `model`, `reported_model`, `effort`, `text`,
`stop_reason`, `usage` and `claude_version`: **no session id and no generation id**. That absence is
load-bearing rather than tidy — it makes `attach_session`, `inspect_session`, `subscribe_events`,
`cancel_turn` and `close_session` **unconstructible** against a pool instance rather than merely
refused, and `SessionOwner` refuses a pool instance to every session-addressed method with the same
byte a caller gets for a session that never existed. `docs/path-b.md` §12 describes the pool behind
it.

`diagnose` exists because `ping` structurally cannot answer for anything behind
the accept loop: it is served without dereferencing the service, so the private
runtime, the session registry, the launch broker and the rmux sidecar are all
untouched by it. `diagnose` completes one `list-sessions` request against the
private sidecar — the cheapest operation that takes the sidecar's dispatch state
lock — reconciles the returned terminals against the registry, and reports a
`pass`/`unproven`/`fail` outcome for the runtime and for each session. It costs
one rmux round trip for the whole daemon whatever the pool size, and it never
starts a turn.

Three outcomes, not two, because two cannot distinguish "checked and fine" from
"not checked". A session pmux has already declared unusable is `unproven` rather
than either: its terminal's absence proves nothing about the sidecar, and it is
not health either. A daemon holding no sessions is `pass`; a cold class is a
capacity fact, not a fault. A terminal the sidecar reports that no session claims
is published as a count and folded into nothing, because pmux publishes a session
only after its terminal exists, so that is the normal shape of an in-flight start.

`StartSessionRequest.cell = minified` additionally REQUIRES `config_isolation`,
and requires that private root to be unshared and unused: a root already bound to
a live session is refused, and so is one containing anything but the two files
pmux seeds. `history.jsonl`, `paste-cache/`, `projects/` and `backups/` are all
per-ROOT rather than per-session, so a shared root makes "after `/clear` nothing
distinguishes this instance from any other" false at the storage layer regardless
of how clean the transcript is.

**"Unshared" means CONTAINMENT, not equality.** No directory a live minified
cell binds may be reachable by any other session, in any role, at any depth: not
as a configuration root, not as a working directory, not as a config-isolation
root, and not as an ancestor or a descendant of any of those. A start is refused
when any directory it would bind — the configuration root
`CLAUDE_CONFIG_DIR`/`HOME` actually resolves to, and the canonical cwd —
contains, is contained by, or is a live minified cell's own configuration root
or working directory. Both containment directions are refused and both sides of
the cell are: an ordinary start reaching into a live cell, and a minified start
whose private root would land inside a live ordinary session's workspace. The
comparison is on `(st_dev, st_ino)` and on ancestry, never on path text, so no
alias, trailing slash, `.`, or firmlink spelling changes the answer. Two
ORDINARY sessions are unaffected: they may share one configuration root, and
they may nest freely.

`clear_session` types `/clear` into a session whose `StartSessionRequest.cell`
was `minified`, and re-arms the transcript tail on the session id Claude rotates
to. Nothing the caller holds changes: the session ID and generation ID name the
same pmux session and the same process incarnation before and after. What
rotates is Claude's own transcript id, which the result returns because the
caller needs it to fence the next clear.

`expected_transcript_session_id` is a compare-and-swap fence in exactly the sense
`generation_id` already is one level up. At start it equals the session ID; every
later value is what the previous `session_cleared` returned, and
`session_snapshot` publishes the current one as `transcript_session_id` so a
caller that lost it reads it back rather than re-deriving it.

**Every stale fence is refused. There is no already-cleared answer.** Any value
other than the currently bound transcript is `id_conflict` with
`violation: "stale_transcript_fence"`, including a value stale by exactly one
rotation, and the refusal types nothing. `rotated` is therefore always `true` on
success; the field is retained because it is pinned by the shared conformance
golden and read by both shipped clients.

The one-behind case is the one that looks answerable and is not. A retry of a
lost response is one rotation behind — but so is the fence a session STARTS with,
because at start `expected_transcript_session_id` equals the session ID. The two
are identical on the wire, so any rule that answers a retry also answers a second
caller, or a restarted one, that never saw the first clear: it is told "already
cleared, nothing to do" about a transcript that has since served another caller's
turn, and drops its own turn into it. Two attempts to bound the answer by session
state both leaked — first on the abandoned id, which nothing invalidates, then on
the event sequence, which the writable-attach path mutates the session without
touching — and the mutation channel a writable attachment opens does not pass
through the daemon's actor at all, so no state the actor holds can bound it.

Recovery after a lost response is a read, not an inference: `session_snapshot`
reports `transcript_session_id`; if it moved, a clear landed. A caller that needs
certainty about the cell's contents clears again on the current fence, which is
semantically idempotent — an empty transcript is abandoned for another empty one
and the emptiness proof runs again. If exactly-once clear ever becomes a stated
requirement it gets a caller-supplied idempotency token and a stored result, the
way `run_turn` already does, never an inference from session state.

**A minified cell refuses `attach_session` with `read_only: false`**, with
`unsupported_feature` and
`violation: "writable_attach_forbidden_on_minified_cell"`, before any rmux grant
is minted. A writable attachment is an authenticated byte channel from a second
party straight into the TUI, and none of those bytes reach the daemon: composer
text left behind PREFIXES the next caller's prompt, up-arrow recall reads the
instance's own per-root `history.jsonl` — which `/clear` appends to rather than
truncating, and which recall scopes to the cwd rather than the session id — and a
hand-typed `/clear` or `/model` rotates Claude's session id underneath the one
pmux has bound. Read-only attachment is unaffected.

A clear that lands but whose result is not provably empty, and a clear whose
rebind cannot be resolved, both taint the session: every later turn is refused
with `recovery_failed` rather than timing out against a transcript nothing is
writing to. A clear refused **before the command was submitted** — a deadline
that has already passed OR that expires while the command is still being pasted,
or a clear issued before the session's first turn, when no transcript exists to
observe a rotation against — does not taint: nothing was typed, nothing was
abandoned, and the session remains provable from the transcript it is still
bound to. The paste case is not an edge: the default clear deadline and the
admission cap are both 15 seconds and the deadline is computed first, so the
deadline is what bounds every clear's gate.

`run_turn` does not block for completion. A durable integration submits once,
then calls `subscribe_events` beginning at
`turn_accepted.next_sequence - 1`. `run_once` is appropriate for a one-shot
caller that wants one blocking operation.

Every session-scoped method requires both identifiers, and v1 has no method that
recovers a `SessionGenerationId` from a `SessionId` and no method that enumerates
live sessions. The generation ID is therefore unrecoverable once lost: a caller
that has the session UUID alone can no longer inspect, attach to, cancel, or
close that session, and the daemon holds it until its idle TTL expires or the
daemon shuts down. Consumers MUST persist the generation ID in the same durable
write as the session ID.

### 7.4 Events and replay

Every event contains:

```text
schema_version = 1
session_id
generation_id
optional turn_id
monotonically increasing per-process-generation sequence
timestamp_ms
typed event { type, data }
```

The v1 event union includes session-state changes, prompt acknowledgement,
logical messages, tool start/completion, rate limit, needs input, terminal
candidate, completion, cancellation, failure, warning, replay gap, and
heartbeat. The current native service emits state, acknowledgement, main
logical-message, tool, needs-input, terminal-candidate,
completion/cancellation/failure, and warning events. Rate-limit and heartbeat
events remain reserved for truthful future observations; replay loss is
reported structurally in an event batch rather than synthesized as progress.

Actors retain at most 256 events and 16 MiB of serialized event records by
default. `after_sequence` is the last event the caller has durably processed.
`max_events = 0` selects the server default of 128; `wait_ms = 0` requests
immediately available data. Every public transport caps `wait_ms` at 30 seconds
and `max_events` at 512, including direct native callers and strict wire
decoding. The sole valid no-data/long-poll cursor is the last published event
(`after_sequence = next_sequence - 1`). A cursor at or beyond `next_sequence`
claims an unpublished event and fails as `invalid_config` before waiting or
mutating actor history. Batches page at the 8 MiB frame limit even when a caller
requests more: within the requested count bound, they include the contiguous
retained suffix until adding the next event to the exact response payload
(including the `events` member, array delimiters, commas, and resulting cursor)
would exceed the frame. A batch never advances `next_sequence` past a retained
event it did not return. An individual oversized nonterminal payload is
replaced by a typed warning carrying only its type and sizes; terminal results
follow the exact-failure rule above.

The final event sequence is at most `9_007_199_254_740_990`, leaving the next
cursor representable at the v1 safe-integer maximum. The actor uses checked
advancement and reserves terminal plus close lifecycle slots before accepting
a turn; near exhaustion it rejects before recording the `TurnId` or mutating
the PTY. It never saturates/reuses a sequence. An exhausted persistent
generation is closed and explicitly resumed with a fresh generation/cursor.

If the requested cursor predates retained history, the response contains no
ordinary events and includes `replay_gap` with the requested cursor, oldest
available sequence, next sequence, and a current `SessionSnapshot`. Clients
MUST treat this as a reconciliation boundary, not silently skip it. A gap is
valid only when `requested_after + 1 < oldest_available <= next_sequence`, and
`next_sequence` must equal both the snapshot's checked next cursor and the
batch cursor. This proves that at least one requested event was actually lost
and prevents a malformed peer from manufacturing a reconciliation boundary.
Official event streams validate session identity, event continuity, these gap
range/cursor relationships, and batch cursors, and reconnect only transport
failures from the last validated cursor.

### 7.5 Errors

Every error has a stable `code`, human-readable `message`, `retryable` boolean,
and optional structured `details`. Callers SHOULD branch on code and retryable,
not message text.

Important classes are:

| Class | Representative codes | Caller action |
| --- | --- | --- |
| Invalid request/feature | `invalid_config`, `unsupported_feature`, `protocol_version_mismatch` | Correct the request; do not retry unchanged. |
| Compatibility/launch | `unsupported_claude_version`, `claude_not_found`, `rmux_unavailable`, `rmux_incompatible` | Fix deployment or promote evidence. |
| Transcript correctness | `transcript_unavailable`, `schema_drift`, `prompt_not_acknowledged` | Preserve evidence and fail closed. |
| Concurrency/identity | `session_busy`, `id_conflict`, `id_collision`, `session_not_found`, `stale_session_generation` | Reconcile and persist a newly returned session handle; never retarget a delayed operation. |
| Operator input | `needs_trust`, `needs_login`, `needs_permission`, `needs_update`, `needs_input` | Obtain explicit authorized human action outside the turn. |
| Execution | `rate_limited`, `authentication_failed`, `billing_failed`, `permission_denied`, `turn_timeout`, `cancelled`, `claude_exited` | Apply caller policy; never fabricate a success. |
| Recovery/control plane | `recovery_failed`, `daemon_lost`, `replay_gap`, `internal` | Inspect/reconcile; close tainted sessions where possible. |

Six codes are published but reserved, in the same sense as the reserved events
in Section 7.4: the current daemon never emits `rmux_incompatible`,
`authentication_failed`, `billing_failed`, `permission_denied`, `claude_exited`,
or `persistence_disabled` (the last defined by the wire enum but not classified
in the table above). `replay_gap` is likewise reserved as an error code, because
replay loss is reported structurally in an event batch. Reserved codes MUST NOT
be removed from the union: the error code is a closed union on the deserialize
path in Rust, TypeScript, and Python, and all three shipped clients already
decode every one of them, so deleting a code breaks conformant peers exactly as
adding one does.

## 8. Attach capability contract

`attach_session` never exposes the private rmux socket or target. For a live
session it creates a random endpoint inside the private runtime and returns:

```text
session_id, generation_id, endpoint, token, expires_at_ms, read_only=false
```

The endpoint is mode `0600`, expires after 30 seconds, and accepts one
connection. The consumer authenticates by sending a four-byte unsigned
big-endian token length followed by the token bytes. On success, the service
opens the private rmux attach stream and proxies bytes bidirectionally. The
token is one-use even if a later step fails.

The text-mode `pmux attach` command mints and immediately consumes this grant in
the caller terminal. JSON output exposes capability metadata for a trusted
native consumer. MCP returns the metadata but intentionally does not consume
the raw stream. Capability responses are sensitive and MUST NOT be logged or
persisted beyond their short use.

Writable attach is an actor-owned reservation, not an inspect-then-open check.
Exactly one reservation is allowed only from `ready` or `needs_input`. While it
is pending, connected, or reconciling, distinct terminal-mutating turns, a
second attach, and idle expiry are rejected; inspect and explicit close remain
available. An unused/expired/unauthenticated grant releases immediately. Once
an authenticated rmux stream was established (including ambiguous proxy
errors), detach starts a nonblocking actor-owned reconciliation task. The
reservation is released only after terminal ready+quiet evidence and a fresh
exact-cursor transcript stabilization/drain; recognized modals remain
`needs_input`, and ambiguity taints the session. Completion callbacks carry the
original generation and attach ID, so neither a stale detach nor a late worker
can mutate a replacement actor. This prevents raw attach input
from bypassing turn serialization during submission or execution.

An optional resize is applied before the grant. `read_only=true` is currently
rejected because the pinned rmux attach stream does not enforce read-only I/O.

## 9. Integration contracts

### 9.1 CLI

The native CLI requires `--socket <ABSOLUTE_PATH>` or `PMUX_SOCKET`. The global
output modes are:

- `text`: final human-facing text or a concise operation result;
- `json`: one typed result object;
- `ndjson`: sequenced turn event records followed by a result record.

For the manual `pmux run` composition, NDJSON events are pre-commit
observations. Only the terminal `type: "result"` record commits one-shot
success. If session cleanup is unconfirmed, pmux withholds that record and exits
nonzero even when a `turn_completed` event was already streamed. Native
`run_once` is atomic and returns its typed result only after cleanup proof.

Commands are:

```text
ping run start turn inspect cancel close attach doctor probe
```

`doctor` validates the socket/protocol, cwd, and executable, AND completes one
`diagnose` against the daemon, without starting a Claude turn. It reports
`status` as one of `healthy`, `unproven`, or `unhealthy` — never a boolean. The
boolean it replaced was `errors.is_empty()` over four checks, three of which
never left the client process and one of which reached only the daemon's accept
loop, so every check it could not run arrived as `"healthy": true`. `unproven` is
what makes "I could not prove it" expressible: a daemon that does not answer
`diagnose` — an older one, or an unreachable one — leaves every claim about the
private runtime and every session unmade, and that must not read as health.
`errors` and `unproven` are separate lists because the two demand different
operator responses.

"I could not prove it" is deliberately NOT the same answer as "there was nothing
to prove". A health layer whose subject is an empty set **that nothing declared
should be occupied** — a registry holding no sessions, a pool with no declared
warm floor holding no instances, a pool on a daemon that was never given
`--path-b-parent`, or the compatibility profile on a daemon with no pool, since
the pool is what makes a promoted cell mandatory — reports `nothing_to_exercise`,
which folds to `pass`. That is the same rule as folding an empty set of outcomes
to `pass`: absence of a session is a capacity fact, not a fault. Encoding it as
`not_established` made `doctor` exit `1` permanently on every correct daemon
serving only stateless turns, because such a daemon reports `sessions: []` on
every probe it will ever answer — pool instances are never registered as caller
sessions, since an instance's session id is the one name no client may learn. A
surface that cries wolf on every healthy daemon makes a genuine `unproven`
unreadable, which is the same failure as the boolean it replaced. A layer that
HAS a subject and could not reach it still reports `not_established`, and still
never rolls up as healthy.

The qualifier is not decoration. The question a layer must ask is **not** "is the
set empty?" but "is the set empty when something declared it should not be?", and
a layer that asks the first has this defect whichever answer it gives. `pmux
doctor` shipped both wrong answers in succession: first `not_established` for
every empty set, which made a correct Path B daemon permanently unprovable; then
`nothing_to_exercise` for every empty set, which made a daemon holding none of an
operator-declared `--path-b-warm` floor — measured refusing six consecutive
`pmux ask` calls — report `healthy` and exit `0`. A pool holding no instances is
a capacity fact when no floor was declared and a `faulted` layer when one was,
and the emptiness alone cannot tell those apart. `spawn_rewarm` records a failed
re-mint nowhere, so this tree is the only surface that can say it. A layer's
`detail` may state only what its own predicate tested: the `nothing_to_exercise`
pool detail used to close with "and the next call of any class mints one", a
promise nothing tested and which was false in the state that produced it.

`probe` builds a redacted exact start DTO and does not
launch unless `--launch` is supplied; `--keep` requires launch. Use command help
as the authoritative flag list:

```bash
pmux --help
pmux run --help
pmux start --help
pmux turn --help
pmuxd serve --help
```

Example persistent flow:

```bash
export PMUX_SOCKET=/absolute/private/pmux.sock
HANDLE=$(pmux --output json start \
  --claude /absolute/path/claude \
  --cwd /absolute/path/project)
SESSION_ID=$(jq -r .session_id <<<"$HANDLE")
GENERATION_ID=$(jq -r .generation_id <<<"$HANDLE")
pmux --output ndjson turn "$SESSION_ID" --generation "$GENERATION_ID" \
  "Inspect the failing tests."
pmux inspect "$SESSION_ID" --generation "$GENERATION_ID"
pmux close "$SESSION_ID" --generation "$GENERATION_ID"
```

Ctrl-C while `run` or `turn` is waiting issues native cancellation. A client-side
deadline also attempts cancellation. Diagnostics go to stderr so structured
stdout remains machine-readable.

Exit status `2` is reserved for command-line parser failures such as missing or
invalid arguments and mutually exclusive sources. After parsing succeeds,
local semantic validation, daemon/transport/protocol failures, and unsuccessful
terminal outcomes exit `1`. A failed operation does not emit a terminal success
or result record; NDJSON turn observations already emitted before the failure
remain observations. `doctor` is the deliberate exception: its typed health
report is emitted in every output mode before a non-healthy report exits `1`.
Both `unhealthy` and `unproven` exit `1` — there is no third status code to
spend, and the distinction an operator acts on is `status`, which is always in
the emitted report. What no longer happens is an unprovable answer exiting `0`.
Dry-run `probe` never connects to the daemon, so daemon-unavailable and
malformed-peer behavior applies to `probe --launch` rather than the dry-run
path.

### 9.2 Rust

`pseudomux_client::PmuxClient` is constructed with exactly one socket and
provides typed methods for every v1 operation:

```rust,no_run
use pseudomux_client::PmuxClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = PmuxClient::new("/absolute/private/pmux.sock")?;
    let pong = client.ping().await?;
    assert_eq!(pong.protocol_version, 1);
    Ok(())
}
```

Use `start_session`, `run_turn`, and `event_stream` for durable multi-turn work;
use `run_once` for bounded one-shot work. The client defaults to an 8 MiB frame,
a five-second connect timeout, and a 45-second request timeout, extended for an
event long poll.

### 9.3 TypeScript

The `clients/typescript` package is ESM `pmux-client` for Node.js 18+ and uses
only Node built-ins at runtime. A valid native start supplies an exact
environment snapshot:

```ts
import { PmuxClient } from "pmux-client";

const snapshot = Object.fromEntries(
  Object.entries(process.env).filter((entry): entry is [string, string] =>
    entry[1] !== undefined
  ),
);
const client = new PmuxClient("/absolute/private/pmux.sock");
const session = await client.startSession({
  identity: { mode: "new" },
  cwd: "/absolute/path/project",
  claude: { executable: "/absolute/path/claude" },
  environment: { snapshot },
  auth_policy: "subscription",
  terminal: {
    rows: 24,
    cols: 120,
    profile: "transparent",
    input_transport: "sdk",
  },
  lifecycle: { mode: "transcript" },
  retention: { mode: "persistent", idle_ttl_ms: 1_800_000 },
  compatibility: "require_tested",
});
```

Every request accepts an `AbortSignal`. Its event iterator retries transport
failures from the last validated cursor and exposes protocol/sequence/replay
failures.

### 9.4 Python

The `clients/python` package is dependency-free, synchronous, and requires
Python 3.11+:

```python
import os
from pmux_client import PmuxClient

client = PmuxClient("/absolute/private/pmux.sock")
session = client.start_session(
    {
        "identity": {"mode": "new"},
        "cwd": "/absolute/path/project",
        "claude": {"executable": "/absolute/path/claude"},
        "environment": {"snapshot": dict(os.environ)},
        "auth_policy": "subscription",
        "terminal": {
            "rows": 24,
            "cols": 120,
            "profile": "transparent",
            "input_transport": "sdk",
        },
        "lifecycle": {"mode": "transcript"},
        "retention": {"mode": "persistent", "idle_ttl_ms": 1_800_000},
        "compatibility": "require_tested",
    }
)
```

Each request uses a fresh UDS connection. The event iterator validates sequence
continuity, reconnects transport failures, and returns replay loss as an
explicit `ReplayGapItem`.

### 9.5 Smithers

Smithers is intended to use a native pmux agent, not the compatibility facade.
The implemented TypeScript `PmuxClaudeAgentTransport` is the transport building
block, not a claim that a complete Smithers package has been published.

A durable Smithers adapter SHOULD:

1. Persist the complete returned handle—Claude `SessionId`, opaque
   `SessionGenerationId`, mode (`new` or `resume`)—and the last validated event
   sequence. A new resume replaces the stored generation fence; delayed work
   from the prior generation must fail rather than auto-retarget.
2. Map one durable task-attempt ID to the shared deterministic UUIDv5 namespace
   with `turnIdForAttempt`; retries of the same attempt then reuse pmux
   idempotency instead of launching duplicate work.
3. Submit native `run_turn`, persist acceptance/cursor, and consume events until
   the matching terminal event.
4. Forward truthful logical-message/tool/warning events rather than claiming
   official Claude token deltas.
5. Map task abort to best-effort `cancel_turn`, then reconcile the session.
6. Treat `PmuxReplayGapError` and its snapshot as an explicit recovery boundary.
7. On worker or transport loss, reconnect to the same session and resubmit the
   same `TurnId`; do not start a replacement Claude process without durable
   orchestration policy.

The current request/response transport has no leased connection lifetime.
Consequently the daemon accepts only the default `continue`/no-heartbeat lease;
`cancel_turn`, `close_session`, or heartbeat-on-disconnect policies fail closed
as `unsupported_feature`. Smithers remains responsible for explicit abort and
cancellation until a connection-scoped leased API is implemented.

### 9.6 MCP

`pmux-mcp` is configured with one absolute socket or `PMUX_SOCKET`:

```json
{
  "mcpServers": {
    "pmux": {
      "command": "/absolute/path/pmux-mcp",
      "args": ["--socket", "/absolute/private/pmux.sock"]
    }
  }
}
```

It supports newline-delimited stdio JSON-RPC frames up to 8 MiB and negotiates
supported MCP protocol revisions. Protocol output is stdout-only; diagnostics
are stderr-only. The eight tools map directly to same-named v1 operations:

```text
start_session  run_turn       inspect_session  cancel_turn
close_session  run_once       subscribe_events attach_session
```

Tool inputs mirror strict v1 DTOs. `run_turn` returns acceptance, so an MCP
caller uses `subscribe_events` for progress and completion. MCP bounds each
subscription to 30 seconds and 512 events. Successful calls use
`structuredContent` as the single canonical result representation rather than
duplicating the same JSON as text; every outbound JSON-RPC frame is preflighted
against the 8 MiB limit. It has no daemon discovery, prompt loop, terminal
interpretation, or capability-stream consumer.

### 9.7 `claude-p`

`claude-p` is a deliberately bounded adapter for software that requires a
print-shaped command:

```bash
PSEUDOMUX_SOCKET=/absolute/private/pmux.sock \
  claude-p -p --output-format stream-json "Review this project."
```

Its `-p/--print` option is only a compatibility marker. The adapter sends native
`run_once` and the service launches interactive Claude without print mode. It
accepts one positional or piped UTF-8 prompt, forced session/resume identity,
and a bounded subset of launch fields. Unknown flags, slash commands, unsafe
controls, and unsupported behavior fail at the facade boundary.

Output modes are `text`, `json`, and `stream-json`. The last is reconstructed
from `TurnResult`, explicitly labels its provenance
`pmux_interactive_transcript_reconstruction`, and does not claim token deltas or
wire parity with Claude's official stream.

The facade always uses subscription auth, transcript lifecycle, one-shot
retention, transparent/default terminal behavior, and `require_tested`. Its
socket environment variable is currently `PSEUDOMUX_SOCKET`; native clients and
MCP use `PMUX_SOCKET`.

## 10. Operations and security

### 10.1 Recommended startup

Build and place all companions together:

```bash
cargo build --workspace --release
```

Create an owner-only location and start the daemon in the foreground or under a
supervisor that preserves signals:

```bash
RUNTIME_DIR="$PWD/.context/pmux-runtime"
mkdir -p "$RUNTIME_DIR"
chmod 700 "$RUNTIME_DIR"

target/release/pmuxd serve \
  --socket "$RUNTIME_DIR/pmux.sock" \
  --runtime-parent "$RUNTIME_DIR"
```

For an admitted release, add only evidence-promoted full profiles:

```bash
target/release/pmuxd serve \
  --socket /absolute/private/pmux.sock \
  --tested-claude-profile \
  '{"claude_version":"2.1.207","os":"macos","arch":"aarch64","terminal_profile":"transparent","input_transport":"sdk","transcript_drain_ms":750}'
```

The profile above is an example of syntax, not a claimed supported cell.

### 10.2 Trust boundary

The public UDS authenticates through filesystem ownership and mode, so the
socket directory MUST remain owner-only. It is not designed for cross-user or
network exposure. Anyone able to connect can request Claude work under the
daemon user's effective credentials and read semantic results.

Prompts, environment values, inline config, MCP credentials, results, attach
tokens, launch tokens, and terminal output are sensitive. Operators MUST keep
public/private runtime and evidence directories private and SHOULD avoid
ambient process supervisors that capture command output indiscriminately.

Subscription sanitization reduces accidental provider-key routing but is not a
general secret scrubber. Caller settings, plugins, MCP servers, permission
modes, and project content execute with the authority the caller requested.

### 10.3 Failure and restart behavior

- A sidecar startup/readiness/version mismatch prevents daemon service startup.
- A lost rmux lease fails active work; it does not trigger prompt reinjection.
- Daemon shutdown closes registered sessions before stopping the sidecar.
- Rmux `KillOnOwnerExit` and five-second leases provide a second process cleanup
  boundary if pmuxd dies.
- Resume after a daemon/process restart requires the caller's known Claude UUID
  and a uniquely validated transcript. Pmux does not scan history to guess a
  session.
- Event replay is memory-bounded and does not survive daemon restart. Durable
  callers reconcile through session/transcript identity and explicit policy.

## 11. Phase 0 and release promotion

Release promotion is not specified here. The normative freeze, live-attempt,
and portability process is [`testing.md`](testing.md) Section 7, and the
evidence-harness contract is
[`tools/phase0/README.md`](tools/phase0/README.md). Live real-Claude mode is
never authorized by a Gate A command and MUST NOT run in ordinary CI.

## 12. Build and verification

The workspace requires Rust 1.88 or newer. The exact ordered deterministic
release manifest is normative in [`testing.md`](testing.md), including locked
all-target/all-feature Rust checks, strict Clippy and rustdoc, ordinary and
serialized real-rmux tests, bounded property/model/fuzz runs, exact-release
binary E2E, TypeScript/Python/package conformance, lifecycle/concurrency/
resource/soak/performance gates, and Phase-0/Linux evidence-tool self-tests.

No Gate A command authorizes real-Claude usage. Ignored private-sidecar tests
use only the deterministic fake interactive child. A Phase 0 live command is a
separate Gate B action and requires the frozen candidate, immutable-ledger
inputs, and every explicit consent and usage guard defined by
[`tools/phase0/README.md`](tools/phase0/README.md).

## 13. Known gaps and explicit non-goals

The following boundaries are intentional or remain before release:

- The source distribution has no built-in Claude version/platform admissions;
  reviewed external evidence must be configured explicitly.
- `rmux_standard` terminal identity and `attached_stream` prompt injection are
  rejected.
- Read-only attach is rejected.
- Non-default turn disconnect/heartbeat leases are rejected until a
  connection-scoped leased API exists.
- Tool timestamps are not yet derived.
- Needs-input classification is conservative and is not guaranteed for every
  Claude version, locale, and blocking-screen variant; unfamiliar screens can
  still time out.
- The composer-geometry constants that make ready-prompt observation possible
  are empirical and calibrated against exactly two reviewed Claude Code
  versions: five recorded terminal captures at `2.1.70`, and 24 of 24 live macOS
  `aarch64` `transparent`/`sdk` turns at `2.1.215`, every one of which recorded
  `terminal_prompt_observed: true`. A third and deliberately weaker leg exists
  and is **not** a calibration: the 2026-07-28 macOS `aarch64`
  `transparent`/`sdk` campaign ran against Claude Code `2.1.220`. That version
  binding lives only in `evidence/model-attempt-ledger.ndjson`, ordinals 30-43
  (`normalized_version` on each of those 14 reservations); the Gate B receipt
  `evidence/gate-b-drain-calibration.json` records no Claude version at all. Ten
  of that campaign's attempts committed a turn, which Section 6.4 does not
  permit without a ready-prompt observation, but the campaign stored no terminal
  snapshots (`raw_terminal_snapshots_stored: false`), so no `2.1.220` screen was
  ever reviewed. Read `2.1.220` as observed-working, never as calibrated. The
  constants are not a stable Claude interface. A
  future composer re-theme is expected to surface as start-session or completion
  unavailability rather than as wrong output; the response is to recalibrate the
  constants, or to promote the fallback in Section 6.4, never to remove the
  gate.
- That prediction has now been collected once, and the two composer-growth laws
  it left behind are calibrated on different versions. The bound on how far the
  cursor may sit from the end of the composer's frame is measured as two
  rendered rows on both reviewed versions (all five `2.1.70` captures; 85 of 85
  live empty-composer screens at `2.1.220`) and enforced at four. It is measured
  from the LAST RENDERED ROW, not from the bottom of the grid: Ink repaints from
  where the previous frame ended and leaves the remainder of the grid blank, so
  the frame a `/clear` leaves behind on `2.1.220` is four rows tall at the TOP of
  a 24-row screen — composer at row 5, rows 8-23 of length zero, byte-identical
  for 285 s. Measuring to the grid bottom made that provably empty composer
  unfindable and refused the first turn after every successful clear with
  `PromptNotAcknowledged`: unavailability, never wrong output, exactly as the
  bullet above predicts. Separately, `rendered_prompt_is_proven` admits two
  growth directions — UPWARD off a moving `❯` anchor with the cursor row
  invariant (`2.1.70` captures and mid-session `2.1.220`), and DOWNWARD off a
  pinned anchor (`2.1.220` ONLY, measured by bracketed-paste probes into a
  post-clear composer that never pressed Enter). The `2.1.70` captures are
  cursor-less single-row composers and cannot corroborate the downward law;
  read it as one-version evidence.
- That function's geometry is now a NECESSARY condition and not the whole test.
  It compared cursor geometry alone, so a composer holding text the caller never
  sent satisfied every clause of it; it then required the first rendered ROW to
  be the prompt's own head, which had no lower bound and admitted a composer
  showing one character of a seventeen-character prompt. It now requires every
  rendered row to spell the prompt, or the single placeholder row a collapsed
  paste of this prompt's line count renders. That is the whole buffer and not
  the whole prompt: `MAX_PROMPT_BYTES` is 1 MiB and a pane is 24 rows, and
  MEASURED at 2.1.226 a single line of 1000 characters or more collapses to a
  placeholder that carries no prompt text at all. Full equality over the text
  remains where it always was and where it can be complete: post-Enter, in
  `TranscriptEngine::ingest`'s `UnexpectedTypedPrompt`.
- Actor/session metadata, idempotency records, results, and replay events are
  memory-only. Automatic interrupted-session recovery after daemon restart is
  not implemented; explicit resume requires a known UUID and validated
  transcript.
- Active-turn cancellation requires a fresh ready/quiet terminal plus a
  post-interrupt transcript EOF drain before the session can return to ready;
  any support claim requires repeated cancel-then-next-turn evidence.
- Transcript replacement, truncation, or relocation during an active turn
  fails closed instead of reconciling a bounded replacement graph.
- Rate-limit, authentication, billing, permission, and unexpected child-exit
  classification coverage remains incomplete without reviewed external
  evidence for the claimed cell.
- The optional TypeScript Smithers transport is only an example external
  consumer; a published Smithers agent is outside the pmux v1 release scope.
- Hybrid hooks are implemented, require separate external evidence for any
  support claim, and never become semantic authority.
- There is no network control API, daemon discovery contract, generic program
  adapter, raw public PTY/session API, automatic trust/permission acceptance,
  arbitrary Claude flag passthrough, streaming prompt input, or fabricated
  token-delta parity.

These gaps MUST be resolved by implementation plus empirical evidence, or kept
explicitly unsupported. They MUST NOT be papered over with terminal scraping,
automatic prompt retries, synthetic transcripts, or non-interactive Claude
execution.

## License

Pseudomux is licensed under either the Apache License, Version 2.0, or the MIT
license, at your option.
