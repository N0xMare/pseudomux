# ping and doctor

Neither command starts a turn or spends tokens.

```bash
export PMUX_SOCKET="$PWD/.context/pmux-dev/pmux.sock"
target/release/pmux ping
target/release/pmux doctor --claude "$(command -v claude)"
```

`ping` is accept-loop liveness. `doctor` is the health tree: pool
configured, leased conversations, compatibility, per-pin `claude auth
status`. `--output json` is one object.

A green `doctor` on this OS's promoted Claude is the same comparison a
mint will make. That is what stops a green doctor from being followed by
an `ask` refused with `unsupported_claude_version`.
