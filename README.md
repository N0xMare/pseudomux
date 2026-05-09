# pseudomux

Pseudomux is an experimental PTY multiplexer for driving terminal-based agent
interfaces from scripts and tools. It runs an interactive TUI agent in a
pseudo-terminal, tracks the rendered terminal with a VTE screen model, and
exposes the session through a CLI, daemon protocol, optional HTTP API, MCP
server, and thin Python/TypeScript clients.

It is meant for cases where an agent is useful through its terminal UI, but you
want a programmatic control surface for one-shot prompts, persistent multi-turn
sessions, state inspection, streaming events, and cleaned response text.

Current built-in targets include Claude Code, OpenCode, shell sessions, and
custom programs.

## Quick Start

Build the workspace:

```bash
cargo build --workspace --release
```

Start the daemon:

```bash
target/release/pmuxd serve
```

In another terminal, configure default Claude Code flags:

```bash
target/release/pmux init \
  --model opus \
  --permission-mode bypassPermissions \
  --cwd . \
  --timeout 300 \
  --effort high
```

Run one prompt in a short-lived session:

```bash
target/release/pmux run --text "Summarize this repo in three bullets" --json
```

Run a persistent multi-turn session:

```bash
SID=$(target/release/pmux start --name reviewer)
target/release/pmux prompt "$SID" --text "Read the code and summarize it" --json
target/release/pmux prompt "$SID" --text "Suggest three improvements" --json
target/release/pmux stop "$SID"
```

Enable HTTP for API clients. It binds to `127.0.0.1` by default:

```bash
target/release/pmuxd serve --http-port 8765
```

Use a token if you expose HTTP beyond a trusted local boundary:

```bash
PSEUDOMUX_HTTP_TOKEN=dev-secret \
  target/release/pmuxd serve --http-host 0.0.0.0 --http-port 8765
```

Call the HTTP API:

```bash
curl -s -X POST http://localhost:8765/run \
  -H 'Content-Type: application/json' \
  -d '{
    "text": "What is 2+2?",
    "session": {"agent": "claude-code", "cwd": "."},
    "timeout_secs": 120
  }'
```

When `PSEUDOMUX_HTTP_TOKEN` or `--http-token` is set, include
`Authorization: Bearer <token>` on HTTP requests.

## What It Returns

Blocking prompt operations return a shared JSON shape:

```json
{
  "session_id": "uuid-v4",
  "text": "assistant response",
  "duration_ms": 1523,
  "state": "Ready",
  "tools": [{"name": "Read", "duration_ms": 2441}]
}
```

The `text` field is best-effort cleaned output: prompt echo, TUI chrome,
status rows, tool progress fragments, and similar terminal artifacts are
filtered out.

## More Interfaces

- `pmux` CLI for humans and shell scripts.
- `pmuxd` daemon with Unix socket protocol and optional HTTP REST/SSE API.
- `pmux-mcp` MCP stdio server for agent harnesses.
- `clients/python` pure-stdlib HTTP client.
- `clients/typescript` Node 18+ HTTP client.

Example MCP server config for clients that accept `mcpServers` JSON:

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

Start `pmuxd serve` before using the MCP server, and make sure `pmux-mcp` is on
`PATH` or replace `command` with an absolute path.

## Caveats

Pseudomux derives semantic state from rendered terminal output. It is useful,
but not magic: upstream TUI layout changes can affect state detection, tool
tracking, and response cleanup. Treat it as a generic tool/experiment with a
raw PTY escape hatch for debugging.

## Spec

See [spec.md](spec.md) for the full system description, API surface, config and
profile formats, client examples, runtime data locations, and known
limitations.

## License

Licensed under either MIT or Apache-2.0, at your option.
