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
`PMUX_MESSAGES_URL` if you bound a different loopback port. A named
`--pool-account` is `PMUX_ACCOUNT=<name>` (omit for `default`).
pi-subagents 0.68+ foreground children disable ambient extensions; the
adapter registers itself as a required child extension so those children
still send `x-pmux-conversation`. Without that, child POSTs 400.

## Models

Effort is in the model id. `/model` lists the recommended warm-set
families (`claude-opus-5-5-*`, `claude-sonnet-5-*`, `claude-fable-5-1-*`).
`GET /v1/models` is the full pool table.

Recommended warm set (at the owner-set cap of 15):

```text
--pool-size 15
--pool-warm claude-opus-5-5/medium=12
--pool-warm claude-opus-5-5/xhigh=2
--pool-warm claude-fable-5-1/xhigh=1
```

Use medium as the workhorse, xhigh sparingly, fable for phase-gates. One
pool instance per live Pi conversation (root + each live subagent) when
each conversation is its own process. The shipped `pmux.ts` holds one
`conversationId` per process; a second in-process session reuses that pin
and `/clear`s the first. The measured parallel-subagent receipt used child
processes. Spawn, steer, and delete stay Pi's job. Session end POSTs
`/v1/conversations/{id}/release` so the cell `/clear`s.

Measured: `evidence/macos-pi-agentic-subagent-2.1.272-aarch64.json` (Pi 0.85.1 + pi-subagents 0.68.0 on Claude Code 2.1.272, usage-bearing pin `<HOME>/.claude-1`, Messages on `127.0.0.1:8766`, 5-cell opus-5 medium/xhigh warm set: agentic `AGENTIC_OK`, sequential reviewer `MULTI_OK`, two parallel reviewers `PARALLEL_OK`; every child `pmux/claude-opus-5-xhigh` exit 0; pool back to idle 5 / leased 0). Prior receipts: `evidence/macos-pi-agentic-subagent-2.1.258-aarch64.json` (Pi 0.84.4 + pi-subagents 0.63.0, 15-cell warm set); `evidence/linux-pi-agentic-subagent-2.1.257-x86_64.json`; `evidence/linux-pi-agentic-subagent-x86_64.json` is the 2.1.233 run.
