# Caller surfaces

## Messages (harness contract)

`--messages-bind HOST:PORT` binds a loopback Anthropic Messages listener in
front of an already-enabled pool. Off unless given. Off-box addresses MUST
be refused at boot.

Three verbs:

1. **Pin.** Every `POST /v1/messages` MUST carry `x-pmux-conversation`
   (aliases: `x-session-id`, `x-session-affinity`).
2. **Release.** `POST /v1/conversations/{id}/release` on session end. That
   is when the cell `/clear`s. Idle TTL is only the backstop.
3. **Class.** Effort is in the model id (`claude-opus-5-medium`) or in
   `output_config.effort`. Account is `x-pmux-account` (omit for `default`).
   Compact, rewind, or a class change is a prefix break; the same pin
   reprimes. Account is a class selector, not a conversation pin.

Without a pin the request MUST be refused. `--messages-allow-implicit` is
the single-session curl hatch: the listener hashes the first turn together
with model, effort, and account. The caller did not choose that id. Two
sessions that start the same way share a cell.

`GET /v1/models` lists admitted ids. `GET /v1/capabilities` states the
closed set: no images, reconstructed SSE after the turn commits, no
`cache_control` on tools, no temperature. `pin_headers` are conversation-id
aliases. `account_header` is `x-pmux-account`. Auth is **presence-only** (any
non-empty `x-api-key` or `Authorization`). Loopback is the trust boundary.

Successful `POST /v1/messages` echoes `x-pmux-conversation`, `x-pmux-cell`
(`s{slot}e{epoch}`), `x-pmux-lease` (`primed` / `continued` / `reprimed` /
`replayed`), `x-pmux-idle-ttl-ms`. An in-flight pin or a full lease cap is
HTTP 409 `session_busy` and MAY be retried.

The response body's `model` MUST be the `model` string the request carried,
byte for byte, never the pool's canonical stem.

Claude's tool surface stays denied (`--disallowedTools *`). The harness
runs tools and sends `tool_result`. The cell emits a tool call as a
`<tool_call>{...}</tool_call>` block whose payload is JSON; a payload that
is strict JSON except for raw control characters inside a string literal
MUST still be accepted as a `tool_use` block, and anything else non-JSON
stays text.

How-to: [user-guide/messages.md](../user-guide/messages.md),
[user-guide/pi.md](../user-guide/pi.md).

## `run_stateless` / `pmux ask`

`(model, effort, prompt[, account]) -> text + usage`. The caller MUST NOT
name a resource. `--model` is required. `--effort` is validated against the
resolved model. `account` is a `--pool-account` name (omit for `default`),
never a path.

MCP `pmux-mcp` MUST advertise exactly `run_stateless` on `tools/list`.
Unpublished tool names on `tools/call` MUST be `unknown_tool`. MCP MUST
NOT advertise `run_stateful`.

How-to: [user-guide/ask.md](../user-guide/ask.md).

## `run_stateful` / `pmux run`

Full Claude Code cell: tools on, caller `--cwd` required, unattended
`--permission-mode dangerously-skip-permissions` required at the daemon.
Requires `pmuxd --stateful`. One-shot: mint, one turn, Force-close, erase
isolation. Not pooled. The caller names the task directory, not the Claude
binary or config root.

How-to: [user-guide/run.md](../user-guide/run.md). Cells:
[03-cells.md](03-cells.md).

## Ops

`ping` is liveness of the accept loop only. `doctor` is the health tree
(pool configured, leased conversations, compatibility). Neither starts a
turn.

How-to: [user-guide/doctor.md](../user-guide/doctor.md).

## Clients

| Client | Package | Product API |
| --- | --- | --- |
| TypeScript | `pmux-client` | `PmuxMessages` + `setConversationHeader` + `setAccountHeader` + `PmuxClient.runStateless` + `PmuxClient.runStateful` |
| Rust | `pseudomux-client` | `MessagesClient` + `PmuxClient::run_stateless` + `PmuxClient::run_stateful` |
| Python | `pmux_client` | `PmuxMessages` + `set_conversation_header` + `set_account_header` + `PmuxClient.run_stateless` + `PmuxClient.run_stateful` |

Each Messages helper MUST refuse a non-loopback / non-`http://` URL and an
empty conversation id. Each UDS client MUST take an explicit absolute
socket and MUST NOT discover or start a daemon.

Pi is the reference harness adapter (`examples/pi`). It uses the TypeScript
client and Messages, not `pmux run`.
