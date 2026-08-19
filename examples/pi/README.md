# Pi on pmux

Pi owns tools, context, and subagents. pmux owns a pool of embedded Claude
Code processes and speaks Anthropic Messages on loopback. This is the
reference adapter for the three-verb contract in
[examples/README.md](../README.md).

## Install

```bash
mkdir -p ~/.pi/agent/extensions
cp examples/pi/pmux.ts ~/.pi/agent/extensions/pmux.ts
# merge examples/pi/settings.json into ~/.pi/agent/settings.json
```

`pmuxd` must already be serving the pool with
`--path-b-messages-bind 127.0.0.1:8765`. Override the URL with
`PMUX_MESSAGES_URL` if you bound a different loopback port.

## Models

Effort is in the model id. `/model` lists the recommended warm-set
families (`claude-opus-5-*`, `claude-sonnet-5-*`, `claude-fable-5-*`).
`GET /v1/models` is the full pool table.

Recommended warm set (at the owner-set cap of 15):

```text
--path-b-pool-size 15
--path-b-warm claude-opus-5/medium=12
--path-b-warm claude-opus-5/xhigh=2
--path-b-warm claude-fable-5/xhigh=1
```

Use medium as the workhorse, xhigh sparingly, fable for phase-gates. One
pool instance per live Pi conversation (root + each live subagent) when
each conversation is its own process. The shipped `pmux.ts` holds one
`conversationId` per process; a second in-process session reuses that pin
and `/clear`s the first. The measured parallel-subagent receipt used child
processes. Spawn, steer, and delete stay Pi's job. Session end POSTs
`/v1/conversations/{id}/release` so the cell `/clear`s.

Measured: `evidence/linux-pi-agentic-subagent-x86_64.json`.
