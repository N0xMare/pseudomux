# `pmux run` (Full cell)

One Full Claude Code turn in a directory you name. Tools, CLAUDE.md, and
subagents are Claude's defaults. The cell is not pooled: mint, one turn,
Force-close.

`pmuxd` MUST have been started with `--stateful` and `--pool-parent`.

```bash
export PMUX_SOCKET="$PWD/.context/pmux-dev/pmux.sock"
mkdir -p /tmp/task
target/release/pmux run \
  --model claude-sonnet-5 \
  --cwd /tmp/task \
  --permission-mode dangerously-skip-permissions \
  -- 'Write hello.txt containing hi'
```

`--cwd` MUST be an existing absolute path and MUST NOT lie under the pool
parent. Unattended use MUST pass `--permission-mode
dangerously-skip-permissions` or the TUI blocks on a permission prompt.

`--account NAME` is the same pin selector as `ask`.

This is the surface a coding harness SHOULD wrap (Harbor, custom eval).
Pi uses Messages, not `run`; see [pi.md](pi.md).
