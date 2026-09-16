# Pi on pmux

Pi owns tools, context, and subagents. pmux owns minified cells and speaks
Messages on loopback. This is not `pmux run`.

Install and warm-set: [examples/pi/README.md](../../examples/pi/README.md).

`pmuxd` must serve `--messages-bind 127.0.0.1:8765` (or set
`PMUX_MESSAGES_URL`). A named pin is `PMUX_ACCOUNT=<name>`.

Copy `examples/pi/pmux.ts` into `~/.pi/agent/extensions/`. pi-subagents
0.68+ foreground children disable ambient extensions; the adapter
registers itself as a required child extension so children still send
`x-pmux-conversation`.

Measured: `evidence/macos-pi-agentic-subagent-2.1.272-aarch64.json`.
