# pseudomux

**pseudomux** (`pmux`) embeds real interactive Claude Code processes and
exposes them as a local API. A harness such as [Pi](https://github.com/badlogic/pi-mono)
owns tools and context. pmux owns the cells: a warm pool of minified Claude
instances, one process per live conversation, `/clear` only when that
conversation ends.

It does not wrap `claude --print`, scrape a terminal, or give the caller a
PTY. Each cell is a real foreground Claude TUI. Claude's project JSONL is the
authority for text, usage, and stop reason.

Windows is unsupported. Linux and macOS are the intended hosts; a version is
admitted only by a promoted cell or an operator `--tested-claude-profile`.

## Quickstart

Rust 1.88+, a Unix host, and an installed Claude Code binary:

```bash
cargo build --workspace --release
```

Start a daemon with explicit owner-only paths. **Give it `--path-b-parent`
and `--path-b-claude`.** Without them every `pmux run` is refused with
`unsupported_feature`:

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

`--path-b-claude` must be absolute. On Linux, add an operator profile until a
linux cell is promoted, for example:

```bash
--tested-claude-profile \
  '{"claude_version":"2.1.233","os":"linux","arch":"x86_64","terminal_profile":"transparent","input_transport":"sdk","transcript_drain_ms":250}'
```

Check the daemon (starts nothing, spends no tokens):

```bash
export PMUX_SOCKET="$PWD/.context/pmux-dev/pmux.sock"
target/release/pmux ping
target/release/pmux doctor --claude "$(command -v claude)" --cwd "$PWD"
```

`doctor`'s pool layer tells you whether the pool is configured. When Messages
leases are live it also reports `leased` and `conversation_leases`.

## Use it from a harness

This is the intended integration. The harness owns tools and context. pmux
owns the cells. The Messages listener is three verbs:

1. **Pin.** Every `POST /v1/messages` carries `x-pmux-conversation: <session-id>`.
   `x-session-id` and `x-session-affinity` are accepted aliases.
2. **Release.** `POST /v1/conversations/{id}/release` on session end. That is
   when the cell `/clear`s. Idle TTL is only the backstop.
3. **Name the class.** Effort is in the model id (`claude-opus-5-medium`) or
   in `output_config.effort`. Compact, rewind, or a class change is a prefix
   break; the same pin reprimes.

Without a pin the request is refused. `--path-b-allow-implicit-conversation`
is the single-session curl hatch; two sessions that start the same way then
share a cell and cannot be released on purpose.

`GET /v1/models` lists the ids. `GET /v1/capabilities` states the closed set:
no images, reconstructed SSE after the turn commits, no `cache_control` on
tools, no temperature. Auth is presence-only (any non-empty `x-api-key` or
`Authorization`). Loopback is the trust boundary; off-box bind is refused.

Add Messages to an already-enabled pool (do not make this the first serve
example):

```bash
--path-b-messages-bind 127.0.0.1:8765 \
--path-b-pool-size 15 \
--path-b-warm claude-opus-5/medium=12 \
--path-b-warm claude-opus-5/xhigh=2 \
--path-b-warm claude-fable-5/xhigh=1
```

Pi is the reference adapter ([examples/pi](examples/pi/README.md)). It has
been measured. A harness that can set a per-request header and run on
session teardown can copy that file. jcode can point at the listener
([examples/jcode](examples/jcode/README.md)) but cannot pin or release per
session; read that page before using it.

```bash
mkdir -p ~/.pi/agent/extensions
cp examples/pi/pmux.ts ~/.pi/agent/extensions/pmux.ts
# merge examples/pi/settings.json into ~/.pi/agent/settings.json
```

`settings.json` sets `defaultProvider` to `pmux`, default model
`claude-opus-5-medium`, and `packages: ["npm:pi-subagents"]`. One pool
instance per live conversation. The contract itself is
[examples/README.md](examples/README.md).

## One-shot from the CLI

`pmux run` is `(model, effort, prompt) -> text + usage`. The caller names no
working directory, Claude binary, config root, system prompt, or session.

```bash
target/release/pmux run --model sonnet --effort low \
  "Name the three largest moons of Saturn."
```

The answer is the first line(s); accounting follows a blank line, so
`pmux run ... | head -1` is the text. `--output json` emits `model`,
`reported_model`, `effort`, `text`, `stop_reason`, `usage`, `claude_version`.

It answers only if the installed Claude is inside a [promoted](#promoted-compatibility-cells)
or operator-admitted range. Otherwise it refuses *before* spawning a child.

`ask` remains an alias of `run`.

## Models and effort

Both halves of `(model, effort)` are the pool's class key. `/clear` does not
re-exec, so instances are fungible within a class and never across one.

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

`--model` is required. `--effort` is validated against the resolved model.

## Sizing the pool

`--path-b-parent` is the enable switch. Every other `--path-b-*` flag is
refused without it.

| flag | default | what it bounds |
| --- | --- | --- |
| `--path-b-parent DIR` | — | **Enables the pool.** Absolute parent for the per-slot trees. |
| `--path-b-claude PATH` | — | Required with `--path-b-parent`, and absolute. |
| `--path-b-pool-size N` | `15` | Live instances. Refused above the owner-set cap of 15, at boot. |
| `--path-b-recycle-turns N` | `50` | Turns one instance serves before it is replaced. |
| `--path-b-warm MODEL[/EFFORT]=COUNT` | none | Warm floor for one class, repeatable. |
| `--path-b-system-prompt TEXT` | see below | REPLACE-mode prompt every instance launches with. 512 bytes. |
| `--path-b-system-prompt-file FILE` | — | Same prompt, from a file. |
| `--path-b-instance-idle-ttl-ms MS` | `300000` | Idle hold time, down to the class warm floor. |
| `--path-b-turn-timeout-ms MS` | `600000` | Default deadline for a stateless turn. |
| `--path-b-retain-dir DIR` | erase | Where a quarantined tree is kept. |
| `--path-b-rss-budget-mb MB` | — | Boot check against `pool_size * 1024 MB`. |
| `--path-b-messages-bind HOST:PORT` | off | Loopback Anthropic Messages facade. |
| `--path-b-allow-implicit-conversation` | off | Permit headerless Messages turns (unsafe under concurrency). |
| `--path-b-evidence-dir DIR` | beside the socket | Redacted drain-evidence corpus. |
| `--path-b-no-evidence` | off | Retain no pool evidence. |

Default system prompt: `Answer directly and completely. If you cannot answer, say so in one line.`

**Fifteen is an owner-set cap, not a default you may raise.** `--path-b-pool-size
16` is refused at boot. Bounds are checked before the socket is bound.

A pool cell launches with `--disallowedTools "*"` and `dont-ask`. The harness
runs tools. A sidechain row on that cell is `schema_drift`.

## Promoted compatibility cells

| Claude Code | platform | terminal / input | `transcript_drain_ms` |
| --- | --- | --- | --- |
| 2.1.220 through 2.1.227 | macos / aarch64 | transparent / sdk | 1000 |

Linux is not in that table. Admit it with `--tested-claude-profile` (see
quickstart). Receipts live under `evidence/`.

## The command surface

The `surface` column is the label `pmux --help` prints. Gate A
(`tools/gate-a/tests/test_documented_surface.py`) fails if this table names a
different set of subcommands or gives any one a different label.

| subcommand | surface | what it does |
| --- | --- | --- |
| `run` | API | One stateless `(model, effort, prompt)` call against the pool. Alias: `ask`. |
| `ping` | Ops | Ask the daemon for its version and protocol number. |
| `doctor` | Ops | Validate the socket, health tree, working directory and Claude executable. |

`pmux <command> --help` is the flag reference. Session commands (`start`,
`turn`, `oneshot`, and the rest) stay compiled and invokable; they are hidden
from default `--help`. See
[docs/experimental-path-a.md](docs/experimental-path-a.md).

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

It exposes exactly these tools: `run_stateless`. That is the MCP surface of
`pmux run`.

## Further reading

[docs/README.md](docs/README.md) is the index. Protocol and test ownership
stay in `docs/spec.md` and `docs/testing.md`.

## License

Licensed under either MIT or Apache-2.0, at your option.
