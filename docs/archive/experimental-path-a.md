# Experimental: interactive sessions

The product is the local API: `pmux run`, MCP `run_stateless`, and the Messages
facade. Interactive session commands remain compiled and hidden. Current
daemons refuse every session Request on the public wire; `pmux` bails before
opening a socket. Use Messages or `pmux run`. This page is historical. It is
not how you integrate a harness.

The recipes below are historical. `pmux start` / `oneshot` / `turn` now exit
with "not part of this product" before opening a socket.

Every call needs an absolute `--socket` or `PMUX_SOCKET`. Output is `text`,
`json`, or `ndjson`. Only `oneshot` and `turn` used to stream events ahead of
the result.

## One-shot

`oneshot` starts a session, runs one turn, and closes it:

```bash
pmux --socket /absolute/path/pmux.sock --output json oneshot \
  --claude /absolute/path/claude \
  --cwd /absolute/path/project \
  "Review the repository."
```

## Persistent session

`start` prints `session_id` and `generation_id`. Every later call needs both.

```bash
HANDLE=$(pmux --socket /absolute/path/pmux.sock --output json start \
  --claude /absolute/path/claude \
  --cwd /absolute/path/project)
SESSION_ID=$(jq -r .session_id <<<"$HANDLE")
GENERATION_ID=$(jq -r .generation_id <<<"$HANDLE")

pmux --socket /absolute/path/pmux.sock --output json turn \
  "$SESSION_ID" --generation "$GENERATION_ID" \
  --timeout-secs 300 "Find the highest-risk module."

pmux --socket /absolute/path/pmux.sock inspect \
  "$SESSION_ID" --generation "$GENERATION_ID"

pmux --socket /absolute/path/pmux.sock close \
  "$SESSION_ID" --generation "$GENERATION_ID"
```

`pmux <command> --help` is the flag reference. `probe` is a dry-run of a
launch. `attach` takes over the TUI. `clear` types `/clear` into a session
*you* started as `--cell minified`; the pool already does that for `pmux run`.

Native TypeScript and Rust clients advertise Messages + `run_stateless`.
`pmux-mcp` lists only `run_stateless`. Session tools remain in the
`tools/call` dispatch table so a stale caller gets a mapped request, and the
daemon refuses that request with `session_surface_removed`. Python and
`claude-p` are not product surfaces.
