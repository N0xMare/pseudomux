# Cells

## Minified (`SessionCell::Minified`)

Warm pool of tool-less Claude TUI processes. Class key is
`(model, effort, account)`. The caller names no working directory, Claude
binary, or config root. Isolation root is a pool slot under
`--pool-parent`. REPLACE-mode system prompt. `--disallowedTools *`.
`dont-ask`. Recycle is `/clear` at Messages release or after a
`run_stateless` / `pmux ask` turn (then the slot returns to idle, or
remints after `--pool-recycle-turns`).

A sidechain / subagent row on this cell is `schema_drift`.

## Full (`SessionCell::Full`)

One-shot Claude Code with default tools, CLAUDE.md, and MCP as Claude
ships them. Not a pool slot.

- **cwd.** Absolute, existing directory. MUST NOT lie under `--pool-parent`
  (inode walk, including symlink). The task files live here.
- **Isolation.** `{pool-parent}/stateful/{uuid}/root` is the private config
  root. Erased after Force-close (including mint failure).
- **Owner.** `SessionOwner::Caller`. Not stealable by `pmux ask`.
- **Permissions.** Unattended use MUST pass
  `permission_mode=dangerously_skip_permissions`. Otherwise the TUI blocks
  on a prompt and the turn is not a product.
- **Admission.** `pmuxd --stateful` (requires `--pool-parent`). Without the
  flag, `run_stateful` is `stateful_not_enabled`.
- **Lifecycle.** Mint, one turn, Force-close, erase. No `/clear` recycle.
- **Account.** Same pin names as the minified pool.

A Full turn that emits an unmeasured transcript `attachment.type` MUST fail
closed (`SchemaDrift`) until that name is admitted. Measured on 2.1.272 Full
cells with a cwd that has CLAUDE.md: `instructions`.

## Drain (product)

After the final assistant JSONL row, pmux waits until the transcript is
quiet. Marked turns (`system/turn_duration` already landed) wait
`TURN_DURATION_DRAIN_FLOOR_MS` (250 ms). Unmarked turns wait the promoted
`transcript_drain_ms` (250 ms on both shipped cells). Estimator math and
campaign receipts are engineering, not this contract.

## `/clear`

Only at minified lease end (Messages release, `ask` recycle, or idle TTL).
MUST NOT run after every HTTP request. Full cells do not `/clear`; they
die.
