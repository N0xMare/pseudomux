# Experimental: interactive sessions

The product is the local API: `pmux run`, MCP `run_stateless`, and the Messages
facade. This page is the older interactive-session CLI, kept for people who
want to drive a live Claude TUI by name. It is always compiled and served. It
is hidden from `pmux --help`. It is not how you integrate a harness.

Every call needs an absolute `--socket` or `PMUX_SOCKET`. Output is `text`,
`json`, or `ndjson`. Only `oneshot` and `turn` stream events ahead of the
result.

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

Native clients (Rust / TypeScript / Python) still speak protocol v1 over the
same owner-only socket. `pmux-mcp` lists only `run_stateless`;
`start_session` / `run_turn` / `run_once` remain callable and are this
experimental surface.
