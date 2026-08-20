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

Start a daemon with explicit owner-only paths. **Give it `--pool-parent`
and `--pool-claude`.** Without them every `pmux run` is refused with
`unsupported_feature`:

```bash
RUNTIME_DIR="$PWD/.context/pmux-dev"
mkdir -p "$RUNTIME_DIR"
chmod 700 "$RUNTIME_DIR"
SOCKET="$RUNTIME_DIR/pmux.sock"

target/release/pmuxd serve \
  --socket "$SOCKET" \
  --runtime-parent "$RUNTIME_DIR" \
  --pool-parent "$RUNTIME_DIR/pool" \
  --pool-claude "$(command -v claude)"
```

`--pool-claude` must be absolute. The binary's version must be in the
promoted table below, or you pass `--tested-claude-profile`. A newer PATH
Claude (this host's `claude` is 2.1.237) is still outside the linux ceiling:

```bash
--tested-claude-profile \
  '{"claude_version":"2.1.237","os":"linux","arch":"x86_64","terminal_profile":"transparent","input_transport":"sdk","transcript_drain_ms":250}'
```

Check the daemon (starts nothing, spends no tokens):

```bash
export PMUX_SOCKET="$PWD/.context/pmux-dev/pmux.sock"
target/release/pmux ping
target/release/pmux doctor --claude "$(command -v claude)"
```

`doctor`'s pool layer tells you whether the pool is configured. When Messages
leases are live it also reports `leased` and `conversation_leases`.

## Development

Living verification is [`tools/dev`](tools/dev/README.md): `check.sh` (fmt/clippy/tests; `--push` adds e2e and process blackbox), `operator_eval.py` (this OS, grades + Messages sticky; no pooled drain; does not edit `PROMOTED_PROFILES`), `promote.py` (drop `--tested-claude-profile` only when `evidence/pooled-transcript-drain-<os>-<arch>.json` already exists). `tools/promotion/` is the drop-flag engine. Gate A, Phase 0, linux-docker, and package-smoke have been removed.

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

Without a pin the request is refused. `--messages-allow-implicit`
is the single-session curl hatch: you did not choose the id. Release using
the `x-pmux-conversation` the response echoed, or the hash `doctor` prints
on `conversation_leases`. Two sessions that start the same way share a cell.

`GET /v1/models` lists the ids. `GET /v1/capabilities` states the closed set:
no images, reconstructed SSE after the turn commits, no `cache_control` on
tools, no temperature. Auth is presence-only (any non-empty `x-api-key` or
`Authorization`). Loopback is the trust boundary; off-box bind is refused.

Add Messages to an already-enabled pool (do not make this the first serve
example):

```bash
--messages-bind 127.0.0.1:8765 \
--pool-size 15 \
--pool-warm claude-opus-5/medium=12 \
--pool-warm claude-opus-5/xhigh=2 \
--pool-warm claude-fable-5/xhigh=1
```

Pi is the reference adapter ([examples/pi](examples/pi/README.md)). It has
been measured. TypeScript apps import `pmux-client` (`PmuxMessages` +
`runStateless`). Rust apps use `pseudomux-client`. jcode can point at the listener
([examples/jcode](examples/jcode/README.md)) but cannot pin or release per
session; read that page before using it.

```bash
# The extension imports `pmux-client` (Messages pin/release).
(cd clients/typescript && npm install && npm run build)
npm install --prefix ~/.pi/agent "$PWD/clients/typescript"
mkdir -p ~/.pi/agent/extensions
cp examples/pi/pmux.ts ~/.pi/agent/extensions/pmux.ts
# merge examples/pi/settings.json into ~/.pi/agent/settings.json
```

`settings.json` sets `defaultProvider` to `pmux`, default model
`claude-opus-5-medium`, and `packages: ["npm:pi-subagents"]`. One pool
instance per live conversation when each conversation is its own process
(the measured Pi subagent receipt used child processes). The contract itself is
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

`--pool-parent` is the enable switch. Every other pool / Messages flag is
refused without it.

| flag | default | what it bounds |
| --- | --- | --- |
| `--pool-parent DIR` | — | **Enables the pool.** Absolute parent for the per-slot trees. |
| `--pool-claude PATH` | — | Required with `--pool-parent`, and absolute. |
| `--pool-size N` | `15` | Live instances. Refused above the owner-set cap of 15, at boot. |
| `--pool-recycle-turns N` | `50` | Turns one instance serves before it is replaced at lease end (sticky resume is not refused). |
| `--pool-warm MODEL[/EFFORT]=COUNT` | none | Warm floor for one class, repeatable. |
| `--pool-system-prompt TEXT` | see below | REPLACE-mode prompt every instance launches with. 512 bytes. |
| `--pool-system-prompt-file FILE` | — | Same prompt, from a file. |
| `--pool-idle-ttl-ms MS` | `300000` | Idle hold time, down to the class warm floor. |
| `--pool-turn-timeout-ms MS` | `600000` | Default deadline for a stateless turn. |
| `--pool-retain-dir DIR` | erase | Where a quarantined tree is kept. |
| `--pool-rss-budget-mb MB` | — | Boot check against `pool_size * 1024 MB`. |
| `--messages-bind HOST:PORT` | off | Loopback Anthropic Messages facade. |
| `--messages-allow-implicit` | off | Permit headerless Messages turns. You did not choose the id; two same-start sessions share a cell. |
| `--pool-evidence-dir DIR` | beside the socket | Redacted drain-evidence corpus. |
| `--pool-no-evidence` | off | Retain no pool evidence. |

Default system prompt: `The user message is the entire instruction.`
That REPLACE text displaces Claude Code's default agent prompt. It is not
consumer policy, and it is not the entire `system` array. A minified TUI
cell still sends Claude Code's identity line and a small user reminder
(account email, date); tools / MCP / `CLAUDE.md` are already absent. A
harness such as Pi sends policy in the Messages body (`SYSTEM:` / `TOOLS:` /
`HISTORY:`). `--pool-system-prompt` overrides the REPLACE *displacer* for
every cell in that daemon; it is not where a harness puts consumer policy.

**Fifteen is an owner-set cap, not a default you may raise.** `--pool-size
16` is refused at boot. Bounds are checked before the socket is bound.

A pool cell launches with `--disallowedTools "*"` and `dont-ask`. The harness
runs tools. A sidechain row on that cell is `schema_drift`.

## Promoted compatibility cells

| Claude Code | platform | terminal / input | `transcript_drain_ms` |
| --- | --- | --- | --- |
| 2.1.220 through 2.1.227 | macos / aarch64 | transparent / sdk | 1000 |
| 2.1.227 through 2.1.236 | linux / x86_64 | transparent / sdk | 250 |

A version outside that table still needs `--tested-claude-profile` (see
quickstart). Receipts live under `evidence/`.

## The command surface

The `surface` column is the label `pmux --help` prints. `tools/dev/check.sh`
runs `tools/dev/tests/test_documented_surface.py`, which fails if this
table names a different set of subcommands or gives any one a different label.

| subcommand | surface | what it does |
| --- | --- | --- |
| `run` | API | One stateless `(model, effort, prompt)` call against the pool. Alias: `ask`. |
| `ping` | Ops | Ask the daemon for its version and protocol number. |
| `doctor` | Ops | Validate the socket, health tree, and Claude executable. |

`pmux <command> --help` is the flag reference. The published surface is
only `run`, `ping`, and `doctor`. The contract is [docs/spec.md](docs/spec.md).

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
