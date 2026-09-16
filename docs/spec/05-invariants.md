# Invariants

- Transcript is authority; the screen is a veto, never a vote.
- The caller of the minified pool names no resource. Messages may name a
  *conversation id*, which is a harness session token, not a Claude
  `SessionId` and not a filesystem path.
- The Full-cell caller names only the task cwd (and optional account).
  Isolation root and Claude binary stay daemon configuration.
- Default daemon: owner-only UDS, no INET. Messages stays opt-in loopback.
- `/clear` only at minified lease end (or TTL), never after every HTTP
  request.
- Promoted profiles require the receipt triad. Do not invent one.
- Do not delete `start_session_owned_with_retention` or
  `start_session_owned`. Public `start_session` is not the mint.
- MCP MUST NOT advertise `run_stateful`.
- Unmeasured JSONL attachment types fail closed.

## Workspace (product)

| Path | Role |
| --- | --- |
| `bin/pmuxd` | Daemon: owner-only UDS, pool, optional Messages, opt-in Full cells. |
| `bin/pmux` | Thin CLI: `run`, `ask`, `ping`, `doctor`. |
| `bin/pmux-mcp` | stdio MCP: `run_stateless` only. |
| `bin/pmux-rmuxd` | Private rmux sidecar. |
| `bin/pmux-launcher` | One-use launch-token consumer. |
| `bin/pmux-hook` | Bounded Hybrid hook relay. |
| `crates/service` | Pool, mint, Full one-shot, health, refuse. |
| `crates/client` | Rust Messages + `run_stateless` + `run_stateful`. |
| `clients/typescript` | TypeScript Messages + `runStateless` + `runStateful`. |
| `clients/python` | Python Messages + `run_stateless` + `run_stateful`. |
| `examples/pi` | Reference harness adapter (Messages). |

Living verification is `tools/dev/`. `tools/promotion/` is the drop-flag
engine. How-to: [user-guide](../user-guide/README.md).
