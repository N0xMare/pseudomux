# What pmux is

pmux is a **local token engine**. It runs real foreground Claude Code TUI
processes and exposes them as a local API. A harness such as Pi owns tools
and context. pmux owns the cells.

It MUST NOT wrap `claude --print`, scrape a terminal for semantics, or give
the caller a PTY. Claude's project JSONL is the authority for text, usage,
and stop reason.

Windows is unsupported. Linux and macOS are the intended hosts. A Claude
Code version is admitted only by a promoted cell or an operator
`--tested-claude-profile`.

The keywords **MUST**, **MUST NOT**, **SHOULD**, and **MAY** in this book
are contracts callers and operators can rely on. How-to text lives in
[the user guide](../user-guide/README.md). Pool internals, drain
estimator math, and dated receipts live in
[engineering](../engineering/README.md) and are not this contract.

## Two cells

| Cell | Product names | Tools | Cwd | Recycle |
| --- | --- | --- | --- | --- |
| **Minified** | Messages, `pmux ask`, MCP `run_stateless` | denied (`--disallowedTools *`) | daemon-owned | `/clear` at lease end |
| **Full** | `pmux run`, UDS `run_stateful` | Claude defaults (Write, Bash, CLAUDE.md, …) | caller `--cwd` | none; one-shot Force-close |

Minified cells are pooled. Full cells are not.

## What pmux is not

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

## Position

Shipped: minified pool + Messages + `run_stateless` / `pmux ask`; opt-in
Full `run_stateful` / `pmux run` behind `pmuxd --stateful`; promoted Claude
Code 2.1.258 through 2.1.272 on macos/aarch64 and 2.1.227 through 2.1.272
on linux/x86_64, both drain 250 ms.

Not in this repository: a Harbor/Docker adapter. `pmux run` is the Full-cell
product a harness MAY wrap.
