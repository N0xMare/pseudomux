# `pmux ask` (minified)

One minified turn: `(model, effort, prompt[, account]) -> text`. The
caller names no cwd. The daemon recycles the cell with `/clear`.

```bash
export PMUX_SOCKET="$PWD/.context/pmux-dev/pmux.sock"
target/release/pmux ask --model claude-sonnet-5 --effort low -- 'Reply with exactly OK'
target/release/pmux --output json ask --model claude-sonnet-5 -- 'Reply with exactly OK'
```

`--model` is required. `--effort` is validated against that model.
`--account NAME` selects a `--pool-account` pin (omit for `default`).

MCP `run_stateless` is the same product. Messages is the sticky
conversation form; see [messages.md](messages.md).

Do not pass `--cwd`, `--claude`, or tool flags. Those are unknown here.
