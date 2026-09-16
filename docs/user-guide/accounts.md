# Accounts

`--pool-securestorage-dir empty` (the default) is the `default` account:
the unsuffixed credential store. A second Anthropic login on the **same**
`--pool-claude` is a named pin, not a second binary.

```bash
target/release/pmuxd serve \
  --socket "$SOCKET" \
  --runtime-parent "$RUNTIME_DIR" \
  --pool-parent "$RUNTIME_DIR/pool" \
  --pool-claude "$(command -v claude)" \
  --pool-securestorage-dir empty \
  --pool-account claude-1="$HOME/.claude-1" \
  --pool-warm claude-sonnet-5/low=1 \
  --pool-warm 'claude-sonnet-5/low@claude-1=1' \
  --pool-size 2
```

Callers select the name:

```bash
pmux ask --account default --model claude-sonnet-5 -- '…'
pmux ask --account claude-1 --model claude-sonnet-5 -- '…'
```

Messages: `x-pmux-account: claude-1`. Omitted is `default`.
`CLAUDE_CONFIG_DIR` is not an account picker for the caller.

`pmux run --account NAME` uses the same pins for a Full cell.
