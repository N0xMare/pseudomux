# Pi on pmux

Pi owns tools, context, and subagents. pmux owns a pool of embedded Claude
Code processes and speaks Anthropic Messages on loopback. This is the
reference adapter for the three-verb contract in
[examples/README.md](../README.md).

## Install

```bash
# The extension imports `pmux-client` (Messages pin/release).
(cd clients/typescript && npm install && npm run build)
npm install --prefix ~/.pi/agent "$PWD/clients/typescript"
mkdir -p ~/.pi/agent/extensions
cp examples/pi/pmux.ts ~/.pi/agent/extensions/pmux.ts
# merge examples/pi/settings.json into ~/.pi/agent/settings.json
```

`pmuxd` must already be serving the pool with
`--messages-bind 127.0.0.1:8765`. Override the URL with
`PMUX_MESSAGES_URL` if you bound a different loopback port.

## Models

Effort is in the model id. `/model` lists the recommended warm-set
families (`claude-opus-5-*`, `claude-sonnet-5-*`, `claude-fable-5-1-*`).
`GET /v1/models` is the full pool table.

Recommended warm set (at the owner-set cap of 15):

```text
--pool-size 15
--pool-warm claude-opus-5/medium=12
--pool-warm claude-opus-5/xhigh=2
--pool-warm claude-fable-5-1/xhigh=1
```

Use medium as the workhorse, xhigh sparingly, fable for phase-gates. One
pool instance per live Pi conversation (root + each live subagent) when
each conversation is its own process. The shipped `pmux.ts` holds one
`conversationId` per process; a second in-process session reuses that pin
and `/clear`s the first. The measured parallel-subagent receipt used child
processes. Spawn, steer, and delete stay Pi's job. Session end POSTs
`/v1/conversations/{id}/release` so the cell `/clear`s.

Measured: `evidence/linux-pi-agentic-subagent-2.1.257-x86_64.json` (Pi 0.84.2 + pi-subagents 0.50.0 on the promoted 2.1.257 cell, no operator flag; agentic, sequential and parallel subagents, release returns the same cell to idle); `evidence/linux-pi-agentic-subagent-x86_64.json` is the 2.1.233 run.
