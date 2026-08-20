# jcode on pmux

jcode can speak Anthropic Messages at a `base_url`. It cannot set
`x-pmux-conversation` per session and it cannot call release on teardown.
There is no Pi-style extension hook.

This directory is a named-profile fragment, not a swarm-safe adapter.

## What this delivers

- `jcode --provider-profile pmux` (or `default_provider = "pmux"`) talks to
  a running Messages listener.
- `/model` lists the warm-set ids in `config.toml` (`claude-opus-5-medium`
  is the default, matching the recommended warm floor).
- Effort in the model id is the pool class. `/effort` alone does not change
  a suffixed id.

## What this does not deliver

- One cell per jcode session or swarm worker. Two `jcode run 'hello'` that
  start the same way share one implicit cell.
- Eager `POST /v1/conversations/{id}/release` on `/quit`, detach, or worker
  exit. Idle TTL is the only recycle unless you release using the
  `x-pmux-conversation` the response echoed (or the hash `doctor` prints).
  jcode will not do that for you.
- A stable pin across memory/skill injects. Those change system/tools; an
  implicit id includes them, so a new lease can open every turn.

Do not put a literal `x-pmux-conversation` in `[providers.pmux.headers]`.
That pins every jcode session and swarm worker to one cell.

## Install

`pmuxd` must already be serving the pool with
`--messages-bind 127.0.0.1:8765`. Because jcode cannot send a pin,
the listener also needs `--messages-allow-implicit`. That flag
is the sequential-experiment escape hatch, not a multi-session contract.

Merge `config.toml` into `~/.jcode/config.toml`. Then:

```bash
jcode --provider-profile pmux run 'hello'
```

Override the URL by editing `base_url`. Auth is presence-only; the dummy
`api_key` is enough.

## If you want Pi-grade later

The gap is in jcode: send `x-pmux-conversation: <session-id>` on every
Messages request and POST release on session end. Until that exists, use
Pi (`examples/pi`) or a sequential single session.
