# pseudomux spec

Pseudomux is an experimental PTY multiplexer for driving terminal-based agent
interfaces from scripts and tools. It starts and manages interactive programs in
pseudo-terminals, keeps a VTE screen model of their output, and exposes the
session through a CLI, Unix domain socket protocol, optional HTTP API, MCP
server, and thin Python/TypeScript HTTP clients.

The current focus is agent TUIs such as Claude Code and OpenCode, plus shell and
custom-program sessions. Pseudomux is useful when the agent does not expose the
programmatic control surface you want, but its TUI is usable and can be driven
through a terminal.

## Status

This is a generic tool/experiment. It works against the agent UIs it has been
tested with, but it still derives semantic state from rendered terminal output.
That means state and response cleanup are best-effort and can drift when an
upstream TUI changes its layout.

The stable parts to build around today are:

- `pmux run` for one-shot prompts.
- `pmux start` + `pmux prompt` + `pmux stop` for persistent multi-turn sessions.
- `pmuxd serve --http-port <port>` with `/run` and
  `/sessions/{id}/prompt-sync`; HTTP binds to `127.0.0.1` by default.
- The shared prompt result JSON shape.
- Raw session/content access for debugging.

Known limitations:

- Tool lists can under-count parallel tool batches.
- Classifier state can be imprecise when a TUI re-renders status chrome during a
  tool run.
- The Python and TypeScript clients are thin blocking wrappers over HTTP; they
  do not wrap SSE streaming or every low-level endpoint yet.
- HTTP is a control API. It is loopback-only by default, has no permissive CORS
  layer, and should use `--http-token` or `PSEUDOMUX_HTTP_TOKEN` if bound beyond
  a trusted local boundary.

## Workspace Layout

```text
apps/
  pmux/                 CLI client
  pmuxd/                daemon, Unix socket server, optional HTTP server
  pmux-mcp/             MCP stdio server
crates/
  core/                 PTY sessions, VTE screen model, buffers, events
  protocol/             IPC request/response DTOs
  adapters/             built-in launch/input profiles for supported TUIs
  service/              facade used by daemon, CLI helpers, response cleanup
clients/
  python/               pure-stdlib Python HTTP client
  typescript/           Node 18+ TypeScript HTTP client
fixtures/               terminal output fixtures used by replay/classifier tests
scripts/                local helper scripts
```

## Architecture

```text
client process
  pmux CLI | HTTP client | pmux-mcp | Python/TypeScript client
        |
        v
pmuxd daemon
  Unix domain socket protocol, optional HTTP REST/SSE API
        |
        v
pseudomux service/core
  profile resolution, PTY lifecycle, VTE screen, content buffer,
  semantic events, input encoding, response cleanup
        |
        v
agent process running in a PTY
  claude-code | opencode | shell | custom program
```

The raw PTY stream is retained in buffers/logs for debugging. The semantic
surface is derived from the VTE screen model and content buffer. Blocking prompt
operations wait for watch events such as turn completion or a Thinking-to-Ready
state transition, then return a cleaned response.

## Build

```bash
cargo build --workspace --release
```

This builds:

- `target/release/pmux`
- `target/release/pmuxd`
- `target/release/pmux-mcp`

Development checks:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix clients/typescript run build -- --noEmit
PYTHONDONTWRITEBYTECODE=1 python3 -c 'import ast, pathlib; [ast.parse(p.read_text()) for p in pathlib.Path("clients/python/pmux_client").glob("*.py")]'
```

## Daemon

Start the daemon before using `pmux`, HTTP clients, or MCP:

```bash
pmuxd serve
```

Start with HTTP enabled. The HTTP listener binds to `127.0.0.1` unless
`--http-host` is provided:

```bash
pmuxd serve --http-port 8765
```

Use token auth when HTTP is reachable outside a trusted local boundary:

```bash
PSEUDOMUX_HTTP_TOKEN=dev-secret \
  pmuxd serve --http-host 0.0.0.0 --http-port 8765
```

Authenticated requests may use either `Authorization: Bearer <token>` or
`X-Pseudomux-Token: <token>`.

Use a specific Unix socket:

```bash
pmuxd serve --socket ~/.local/state/pseudomux/pmux.sock
```

Socket discovery for CLI/MCP clients:

1. `--socket <path>` where supported.
2. `PSEUDOMUX_SOCKET`.
3. `$PSEUDOMUX_STATE_DIR/pmux.sock`.
4. Platform default state dir:
   `~/Library/Application Support/pseudomux/pmux.sock` on macOS,
   `$XDG_STATE_HOME/pseudomux/pmux.sock` on Linux when set, otherwise
   `~/.local/state/pseudomux/pmux.sock`.
5. `./.pseudomux/pmux.sock`.
6. Legacy fallback: `/tmp/pmux.sock`.

The daemon creates state directories as owner-only and sets the Unix socket to
owner read/write where the platform supports it. It refuses to remove stale
socket paths that are not sockets or are owned by another user.

## CLI Usage

Initialize default Claude Code flags:

```bash
pmux init \
  --model opus \
  --permission-mode bypassPermissions \
  --cwd . \
  --timeout 300 \
  --effort high
```

Run one prompt in a short-lived session:

```bash
pmux run --text "Review this repo in three sentences" --json
```

Read a prompt from a file or stdin:

```bash
pmux run --file prompt.md --json
echo "what is 2+2?" | pmux run --file - --json
```

Keep the session from a one-shot run:

```bash
pmux run --text "First turn" --keep-alive --json
```

Run a persistent multi-turn session:

```bash
SID=$(pmux start --name reviewer)
pmux prompt "$SID" --text "Read main.rs and summarize" --json
pmux prompt "$SID" --text "Now suggest three improvements" --json
pmux stop "$SID"
```

Inspect a session:

```bash
pmux list
pmux agent-state "$SID"
pmux screen-text "$SID"
pmux content "$SID" --json
pmux events "$SID" --max-events 10
pmux watch "$SID" --json
```

Send input/control:

```bash
pmux send "$SID" --text "raw text"
pmux input-key "$SID" Enter
pmux input-action "$SID" submit
pmux input-prompt "$SID" --text "text plus submit"
pmux confirm "$SID" --yes
pmux interrupt "$SID"
pmux resize "$SID" --rows 50 --cols 160
pmux attach "$SID"
```

`pmux attach` is interactive. Type `/exit` to detach.

Session IDs can be addressed by UUID prefix when the prefix is unambiguous.

## Prompt Result Contract

`pmux run --json`, `pmux prompt --json`, `POST /run`,
`POST /sessions/{id}/prompt-sync`, `pseudomux_run`, and `pseudomux_prompt`
return the same success shape:

```json
{
  "session_id": "uuid-v4",
  "text": "assistant response",
  "duration_ms": 1523,
  "state": "Ready",
  "tools": [
    {"name": "Read", "duration_ms": 2441},
    {"name": "Bash", "duration_ms": 312}
  ]
}
```

Fields:

- `session_id`: the session that produced the response.
- `text`: response text after TUI chrome, prompt echo, status fragments, and
  tool progress rows have been stripped.
- `duration_ms`: wall-clock duration for the turn.
- `state`: final inferred agent state, usually `Ready`.
- `tools`: best-effort tool invocations observed during the turn.

Typed error shape:

```json
{
  "error": "timeout",
  "message": "agent did not complete within 120s",
  "session_id": "uuid-v4"
}
```

Error codes:

| Error | CLI exit | HTTP status | Meaning |
| --- | --- | --- | --- |
| `timeout` | 1 | 408 | Prompt did not complete before the timeout. |
| `agent_exited` | 2 | 502 | The child process exited mid-turn. |
| `transport` | 2 | 502 | Daemon, I/O, HTTP, or protocol failure. |
| `auth_required` | 3 | 428 | The agent is waiting for authentication. |
| `confirmation_required` | 3 | 428 | The agent is waiting for a yes/no decision. |
| `unauthorized` | 2 | 401 | HTTP token is missing or invalid. |

## Configuration

`pmux init` writes `~/.config/pseudomux/config.toml`:

```toml
[defaults]
model = "opus"
permission_mode = "bypassPermissions"
cwd = "."
agent = "claude-code"
timeout = 300
effort = "high"
```

Config file lookup:

1. `$PSEUDOMUX_CONFIG`.
2. `./.pseudomux/config.toml`.
3. `$XDG_CONFIG_HOME/pseudomux/config.toml`.
4. `~/.config/pseudomux/config.toml`.

Resolution order:

```text
CLI flag > PMUX_* environment variable > config.toml > hardcoded default
```

Supported default fields:

| Field | Meaning |
| --- | --- |
| `agent` | `claude-code`, `opencode`, `shell`, or `custom`. |
| `cwd` | Working directory for new sessions. |
| `model` | Claude Code model alias or full model ID. |
| `permission_mode` | Claude Code permission mode. |
| `timeout` | Default blocking prompt timeout in seconds. |
| `effort` | Claude Code effort level. |

## Profiles

Profiles are named per-session recipes selected with `--profile <name>`.

Profile file lookup:

1. `$PSEUDOMUX_PROFILE_FILE`.
2. `./.pseudomux/profiles.toml`.
3. `~/.config/pseudomux/profiles.toml`.

Example:

```toml
[profiles.reviewer]
agent = "claude-code"
model = "opus"
permission_mode = "default"
system_prompt = "You are a senior code reviewer."
effort = "high"
allowed_tools = "Read Bash(git:*)"
cwd = "/workspace"
rows = 50
cols = 200

[profiles.reviewer.env]
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1"
```

Use it:

```bash
pmux --profile reviewer start
pmux --profile reviewer run --text "Review this diff" --json
```

Profile fields include:

- Core: `agent`, `cwd`, `rows`, `cols`, `logging_mode`, `args`, `extra_args`.
- Environment: `[profiles.<name>.env]`.
- Claude Code shortcuts: `model`, `permission_mode`, `allowed_tools`,
  `disallowed_tools`, `system_prompt`, `append_system_prompt`, `effort`,
  `max_budget`.
- Settings pass-through: `[profiles.<name>.settings]`, converted to JSON and
  passed with `--settings`.

## HTTP API

Enable HTTP on loopback:

```bash
pmuxd serve --http-port 8765
```

Bind another interface explicitly and set a token if the endpoint is reachable
outside a trusted local boundary:

```bash
PSEUDOMUX_HTTP_TOKEN=dev-secret \
  pmuxd serve --http-host 0.0.0.0 --http-port 8765
```

When a token is configured, send `Authorization: Bearer <token>` or
`X-Pseudomux-Token: <token>`. The HTTP server does not enable permissive CORS by
default.

Health:

```bash
curl http://localhost:8765/health
```

One-shot prompt:

```bash
curl -s -X POST http://localhost:8765/run \
  -H 'Content-Type: application/json' \
  -d '{
    "text": "Summarize README.md in three bullets.",
    "session": {
      "agent": "claude-code",
      "cwd": "/workspace",
      "args": ["--model", "opus", "--permission-mode", "bypassPermissions"]
    },
    "timeout_secs": 120
  }'
```

Persistent session:

```bash
SID=$(curl -s -X POST http://localhost:8765/sessions \
  -H 'Content-Type: application/json' \
  -d '{"agent": "claude-code", "cwd": "/workspace", "name": "review"}' \
  | jq -r '.session')

curl -s -X POST "http://localhost:8765/sessions/$SID/prompt-sync" \
  -H 'Content-Type: application/json' \
  -d '{"text": "Explain this code", "timeout_secs": 120}'

curl -X DELETE "http://localhost:8765/sessions/$SID"
```

Main endpoints:

| Method/path | Purpose |
| --- | --- |
| `GET /health` | Daemon health. |
| `POST /run` | One-shot blocking prompt. |
| `POST /sessions` | Start a persistent session. |
| `GET /sessions` | List sessions. |
| `GET /sessions/{id}` | Get session details. |
| `DELETE /sessions/{id}` | Stop a session. |
| `POST /sessions/{id}/prompt-sync` | Blocking prompt on an existing session. |
| `POST /sessions/{id}/prompt` | Fire-and-forget prompt. |
| `POST /sessions/{id}/input/text` | Send text without submit. |
| `POST /sessions/{id}/input/key` | Send a key, such as `Enter` or `Ctrl-c`. |
| `POST /sessions/{id}/input/action` | Send a named profile action. |
| `POST /sessions/{id}/input/enter` | Send Enter. |
| `GET /sessions/{id}/state` | Current inferred state. |
| `GET /sessions/{id}/content` | Filtered content buffer. |
| `GET /sessions/{id}/screen` | VTE screen text. |
| `GET /sessions/{id}/terminal-state` | Terminal capability state. |
| `GET /sessions/{id}/events` | Semantic events as SSE. |
| `GET /sessions/{id}/watch` | Watch events as SSE. |
| `POST /sessions/{id}/resize` | Resize terminal. |
| `POST /sessions/{id}/interrupt` | Send SIGINT. |
| `POST /sessions/{id}/confirm` | Answer a confirmation prompt. |

`/run` and `/prompt-sync` return the prompt result contract. Most other
successful endpoints return `{ "ok": true, ... }`.

## Python Client

The Python client is a pure-stdlib wrapper around the HTTP API.

Install for local development:

```bash
cd clients/python
pip install -e .
```

Example:

```python
from pmux_client import PmuxClient, TimeoutError

c = PmuxClient("http://localhost:8765")
# If pmuxd was started with --http-token or PSEUDOMUX_HTTP_TOKEN:
# c = PmuxClient("http://localhost:8765", token="dev-secret")

try:
    result = c.run(
        text="Review this repo",
        cwd="/path/to/repo",
        args=["--model", "opus", "--permission-mode", "bypassPermissions"],
        timeout_secs=120,
    )
    print(result.text)
except TimeoutError as e:
    print(f"timed out: {e.message}")
```

Methods:

- `health()`
- `run(text, agent="claude-code", cwd=None, name=None, args=None,
  timeout_secs=120, keep_alive=False)`
- `start_session(agent="claude-code", cwd=None, name=None, args=None)`
- `wait_ready(session_id, timeout_secs=30)`
- `prompt(session_id, text, timeout_secs=120)`
- `stop_session(session_id)`
- `list_sessions()`

## TypeScript Client

The TypeScript client targets Node 18+ and uses the global `fetch`.

Install/build for local development:

```bash
cd clients/typescript
npm install
npm run build
```

Example:

```ts
import { PmuxClient, TimeoutError } from "pmux-client";

const c = new PmuxClient("http://localhost:8765");
// If pmuxd was started with --http-token or PSEUDOMUX_HTTP_TOKEN:
// const c = new PmuxClient("http://localhost:8765", "dev-secret");

try {
  const result = await c.run({
    text: "Review this repo",
    cwd: "/path/to/repo",
    args: ["--model", "opus", "--permission-mode", "bypassPermissions"],
    timeoutSecs: 120,
  });
  console.log(result.text);
} catch (e) {
  if (e instanceof TimeoutError) {
    console.log(`timed out: ${e.message}`);
  } else {
    throw e;
  }
}
```

Methods:

- `health()`
- `run({ text, agent, cwd, name, args, timeoutSecs, keepAlive })`
- `startSession({ agent, cwd, name, args })`
- `waitReady(sessionId, timeoutSecs)`
- `prompt(sessionId, text, { timeoutSecs })`
- `stopSession(sessionId)`
- `listSessions()`

## MCP Server

`pmux-mcp` exposes pseudomux sessions as MCP tools over stdio. It connects to
the running `pmuxd` daemon through the same Unix socket discovery as `pmux`.
Start the daemon before starting the MCP server.

Run:

```bash
pmuxd serve
pmux-mcp
```

Example MCP client configuration:

```json
{
  "mcpServers": {
    "pseudomux": {
      "command": "pmux-mcp",
      "args": []
    }
  }
}
```

Use an absolute `command` path if `pmux-mcp` is not on the MCP client's `PATH`.

Main tools:

| Tool | Purpose |
| --- | --- |
| `pseudomux_run` | Start a session, wait until ready, prompt, return the shared prompt result, and stop unless `keep_alive` is true. |
| `pseudomux_start_session` | Start a persistent session. Supports `agent`, `profile`, `name`, `cwd`, `rows`, `cols`, `args`, `env`, `logging_mode`, `record_path`, and Claude Code convenience fields. |
| `pseudomux_prompt` | Send a prompt to an existing session and return the shared prompt result. Supports `timeout_secs`. |
| `pseudomux_list_sessions` | List active sessions. |
| `pseudomux_stop_session` | Terminate a session. |
| `pseudomux_get_state` | Get current inferred agent state. |
| `pseudomux_get_content` | Read filtered, raw, or row-aware response content. |
| `pseudomux_screen_text` | Read current VTE screen or status text. |
| `pseudomux_terminal_state` | Read terminal keyboard/capability negotiation state. |
| `pseudomux_content_seq` | Read current content buffer sequence. |
| `pseudomux_send_text` | Send text without submitting. |
| `pseudomux_send_key` | Send a key such as `Enter`, `Tab`, `Ctrl-c`, or `F1`. |
| `pseudomux_input_action` | Send a named action such as `submit` or `hard_interrupt`. |
| `pseudomux_interrupt` | Send SIGINT/Ctrl-c. |
| `pseudomux_resize` | Resize the PTY. |
| `pseudomux_confirm` | Accept or reject a confirmation/permission prompt. |
| `pseudomux_watch_events` | Collect watch events for a bounded period and return them as an array. |
| `pseudomux_events` | Collect semantic events for a bounded period and return them as an array. |

MCP session tools accept full UUIDs or unambiguous UUID prefixes. Event tools
are bounded request/response collectors rather than true live streams because
basic MCP tools are not an SSE transport.

## Runtime Data

Defaults:

- Platforms: macOS and Linux.
- PTY backend: `portable-pty`.
- Raw scrollback: 8 MiB in memory.
- Stripped scrollback: 4 MiB in memory.
- Session data: `$PSEUDOMUX_STATE_DIR/sessions/<id>/` when set. Otherwise,
  platform default state dir: `~/Library/Application Support/pseudomux/sessions`
  on macOS, `$XDG_STATE_HOME/pseudomux/sessions` on Linux when set, or
  `~/.local/state/pseudomux/sessions`.
- Daemon log: sibling `logs/pmuxd.log` under the same state dir.

Raw PTY bytes can be recorded when starting a session:

```bash
pmux start --record ./session.pty --agent claude-code --cwd .
```

## Event Model

Agent state values:

- `Booting`
- `Ready`
- `Thinking`
- `ToolRunning`
- `AuthRequired`
- `Error`
- `Unknown`

Semantic events are emitted by the VTE classifier and include assistant deltas,
turn lifecycle, tool lifecycle, auth/confirmation signals, state changes,
screen redraws, and session exit.

Watch events are a smaller monitoring-oriented stream derived from semantic
events. They include state changes, content deltas, turn completion, input
required, tool start/finish, input sent, and session exit.

## License

Pseudomux is licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option.
