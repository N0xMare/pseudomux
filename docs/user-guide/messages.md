# Messages listener

Opt-in loopback Anthropic Messages in front of the minified pool.

```bash
target/release/pmuxd serve \
  --socket "$SOCKET" \
  --runtime-parent "$RUNTIME_DIR" \
  --pool-parent "$RUNTIME_DIR/pool" \
  --pool-claude "$(command -v claude)" \
  --messages-bind 127.0.0.1:8765
```

Every `POST /v1/messages` MUST carry `x-pmux-conversation: <id>`. On
session end, `POST /v1/conversations/{id}/release` so the cell `/clear`s.

Effort is in the model id (`claude-sonnet-5-low`) or `output_config.effort`.
Account is `x-pmux-account` (omit for `default`).

Clients: TypeScript `PmuxMessages`, Rust `MessagesClient`, Python
`PmuxMessages`. Each refuses a non-loopback URL.

`--messages-allow-implicit` hashes a headerless first turn. Two identical
starts share a cell. Prefer an explicit pin.
