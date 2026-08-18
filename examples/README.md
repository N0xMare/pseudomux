# Harness adapters

pmux is a token engine. A harness that wants to use it as a model provider
speaks Anthropic Messages on loopback and does three things:

1. Send `x-pmux-conversation: <session-id>` on every `POST /v1/messages`.
2. `POST /v1/conversations/{id}/release` when that session ends.
3. Put effort in the model id (`claude-opus-5-medium`) or in
   `output_config.effort`.

`GET /v1/models` and `GET /v1/capabilities` are the closed catalogue. Any
non-empty `x-api-key` or `Authorization` is enough; loopback is the trust
boundary. Images, live token streaming, `cache_control` on tools, and
temperature are not offered. Compact, rewind, or a class change is a prefix
break; keep the pin and pmux reprimes.

Headerless `POST /v1/messages` needs `--path-b-allow-implicit-conversation`
and is not swarm-safe.

| Adapter | Can pin and release per session? | Measured? |
| --- | --- | --- |
| [Pi](pi/README.md) | Yes | `evidence/linux-pi-agentic-subagent-x86_64.json` |
| [jcode](jcode/README.md) | No — config only | No |

A harness that cannot set a per-request header is not swarm-safe on this
listener. Do not paper over that with a static `x-pmux-conversation`.
