# pmux product specification

This document is the product contract. The root [README](../README.md) is the
short landing page. Engineering notes (test ownership, promotion, dated
receipts) live beside this file and are not a second product.

The keywords **MUST**, **MUST NOT**, **SHOULD**, and **MAY** describe contracts
callers and operators can rely on.

## 1. What pmux is

pmux is a **local token engine**. It runs a warm pool of real foreground
Claude Code TUI processes (minified cells) and exposes them as:

1. An opt-in loopback **Anthropic Messages** listener.
2. A Unix-domain-socket method **`run_stateless`**.
3. A thin CLI: **`pmux run`**, **`pmux ping`**, **`pmux doctor`**.
4. Three clients with the same contract: **TypeScript**, **Rust**, and
   **Python**.

A harness such as Pi owns tools and context. pmux owns the cells: one
process per live conversation, `/clear` only when that conversation is
released (or idle TTL fires). The caller names no working directory, Claude
binary, config root, or session filesystem.

It MUST NOT wrap `claude --print`, scrape a terminal for semantics, or give
the caller a PTY. Claude's project JSONL is the authority for text, usage,
and stop reason.

Windows is unsupported. Linux and macOS are the intended hosts. A Claude
Code version is admitted only by a promoted cell or an operator
`--tested-claude-profile`.

## 2. What pmux is not

The following are **not** product surfaces. Current daemons refuse them on
the public wire with `unsupported_feature` / `session_surface_removed`:

- Interactive session methods (`start_session`, `run_turn`, `run_once`,
  `clear_session`, attach, agents).
- A `claude -p` compatibility facade.
- Stored launch-configuration agents (`pmux agent`, `--agent-store`).
- Off-box HTTP. Messages bind is loopback only.
- An OpenAI-compatible facade.

Protocol types for those methods MAY remain so an old client receives a
typed refusal rather than a decode failure. They MUST NOT be documented as
how to integrate.

## 3. Caller surfaces

### 3.1 Messages (harness contract)

`--messages-bind HOST:PORT` binds a loopback Anthropic Messages listener in
front of an already-enabled pool. Off unless given. Off-box addresses MUST
be refused at boot.

Three verbs:

1. **Pin.** Every `POST /v1/messages` MUST carry `x-pmux-conversation`
   (aliases: `x-session-id`, `x-session-affinity`).
2. **Release.** `POST /v1/conversations/{id}/release` on session end. That
   is when the cell `/clear`s. Idle TTL is only the backstop.
3. **Class.** Effort is in the model id (`claude-opus-5-medium`) or in
   `output_config.effort`. Compact, rewind, or a class change is a prefix
   break; the same pin reprimes.

Without a pin the request MUST be refused. `--messages-allow-implicit` is
the single-session curl hatch: the listener hashes the first turn. The
caller did not choose that id. Two sessions that start the same way share a
cell.

`GET /v1/models` lists admitted ids. `GET /v1/capabilities` states the
closed set: no images, reconstructed SSE after the turn commits, no
`cache_control` on tools, no temperature. Auth is **presence-only** (any
non-empty `x-api-key` or `Authorization`). Loopback is the trust boundary.

Successful `POST /v1/messages` echoes `x-pmux-conversation`, `x-pmux-cell`
(`s{slot}e{epoch}`), `x-pmux-lease` (`primed` / `continued` / `reprimed` /
`replayed`), `x-pmux-idle-ttl-ms`. An in-flight pin or a full lease cap is
HTTP 409 `session_busy` and MAY be retried.

Claude's tool surface stays denied (`--disallowedTools *`). The harness
runs tools and sends `tool_result`.

### 3.2 `run_stateless` / `pmux run`

`(model, effort, prompt) -> text + usage`. The caller MUST NOT name a
resource. `--model` is required. `--effort` is validated against the
resolved model. `ask` is an alias of `run`.

MCP `pmux-mcp` MUST advertise exactly `run_stateless` on `tools/list`.
Unpublished tool names on `tools/call` MUST be `unknown_tool`.

### 3.3 Ops

`ping` is liveness of the accept loop only. `doctor` is the health tree
(pool configured, leased conversations, compatibility). Neither starts a
turn.

### 3.4 Clients

| Client | Package | Product API |
| --- | --- | --- |
| TypeScript | `pmux-client` | `PmuxMessages` + `setConversationHeader` + `PmuxClient.runStateless` |
| Rust | `pseudomux-client` | `MessagesClient` + `PmuxClient::run_stateless` |
| Python | `pmux_client` | `PmuxMessages` + `PmuxClient.run_stateless` |

Each Messages helper MUST refuse a non-loopback / non-`http://` URL and an
empty conversation id. Each UDS client MUST take an explicit absolute
socket and MUST NOT discover or start a daemon.

Pi is the reference harness adapter (`examples/pi`). It uses the TypeScript
client.

## 4. Operator daemon

Public control is an explicit owner-only Unix-domain socket. There is no
path discovery and no client autostart.

`--pool-parent` enables the pool. Every other pool / Messages flag MUST be
refused without it. `--pool-claude` is required with the parent and MUST be
absolute.

| flag | default | what it bounds |
| --- | --- | --- |
| `--pool-parent DIR` | — | Enables the pool. Absolute parent for per-slot trees. |
| `--pool-claude PATH` | — | Required with `--pool-parent`, absolute. |
| `--pool-size N` | `15` | Live instances. Refused above the owner-set cap of 15. |
| `--pool-recycle-turns N` | `50` | Turns one instance serves before remint at lease end. |
| `--pool-warm MODEL[/EFFORT]=COUNT` | none | Warm floor, repeatable. |
| `--pool-system-prompt TEXT` | see README | REPLACE-mode launch prompt. 512 bytes. |
| `--pool-system-prompt-file FILE` | — | Same prompt, from a file. |
| `--pool-idle-ttl-ms MS` | `300000` | Idle hold, down to the warm floor. |
| `--pool-turn-timeout-ms MS` | `600000` | Default stateless deadline. |
| `--pool-retain-dir DIR` | erase | Quarantined tree. |
| `--pool-rss-budget-mb MB` | derived | Boot check against `pool_size * 1024 MB`. |
| `--messages-bind HOST:PORT` | off | Loopback Messages listener. |
| `--messages-allow-implicit` | off | Headerless Messages hatch. |
| `--pool-evidence-dir DIR` | `pool-evidence/` beside the socket | Redacted drain evidence. |
| `--pool-no-evidence` | off | Retain no pool evidence. |

Fifteen is an owner-set cap. `--pool-size 16` MUST be refused at boot,
before the socket is bound.

A pool cell MUST launch with `--disallowedTools "*"` and `dont-ask`. A
sidechain row on that cell is `schema_drift`.

A pool mint MUST build the child's environment from the daemon snapshot
through a closed allowlist. The order is
`allowlist(snapshot) - unset + set - policy_removals + profile_changes`.
Public `run_stateless` / Messages MUST NOT name environment names. Nested
Claude markers (`CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`,
`CLAUDE_CODE_REMOTE`, `CLAUDE_CODE_CHILD_SESSION`) MUST NOT reach the
child.

## 5. Compatibility

`require_tested` is the default for pool mint. The distribution ships one
promoted range: Claude Code 2.1.220 through 2.1.227 on macos/aarch64,
transparent/sdk. Linux is admitted with `--tested-claude-profile` until a
linux cell is promoted. Receipts live under `evidence/`.

`allow_untested` is for deliberate probes and MUST be reported as untested.
It does not skip transcript validation.

## 6. Transport

Native protocol v1 is length-prefixed JSON on an owner-only Unix socket.
The public methods are `ping`, `diagnose`, and `run_stateless`. Every other
request variant MUST be refused with `session_surface_removed`.

Pool mint uses an internal start funnel:
`start_session_owned_with_retention` (the pool also calls
`start_session_owned`). That is not a public method. Public
`start_session` is refused.

## 7. Invariants

- Transcript is authority; the screen is a veto, never a vote.
- The caller of the pool names no resource. Messages may name a
  *conversation id*, which is a harness session token, not a Claude
  `SessionId` and not a filesystem path.
- Default daemon: owner-only UDS, no INET. Messages stays opt-in loopback.
- `/clear` only at lease end (or TTL), never after every HTTP request.
- Promoted profiles require the receipt triad. Do not invent one.
- Do not delete `start_session_owned_with_retention` or
  `start_session_owned`. Public `start_session` is not the mint.

## 8. Workspace components (product)

| Path | Role |
| --- | --- |
| `bin/pmuxd` | Daemon: owner-only UDS, pool, optional Messages listener. |
| `bin/pmux` | Thin CLI: `run`, `ping`, `doctor`. |
| `bin/pmux-mcp` | stdio MCP: `run_stateless` only. |
| `bin/pmux-rmuxd` | Private rmux sidecar. |
| `bin/pmux-launcher` | One-use launch-token consumer. |
| `bin/pmux-hook` | Bounded Hybrid hook relay. |
| `crates/service` | Pool, mint, health, refuse. |
| `crates/client` | Rust Messages + `run_stateless`. |
| `clients/typescript` | TypeScript Messages + `runStateless`. |
| `clients/python` | Python Messages + `run_stateless`. |
| `examples/pi` | Reference harness adapter. |

Living verification is `tools/dev/`. `tools/promotion/` is the drop-flag engine. Gate A, Phase 0, linux-docker, and package-smoke have been removed.
