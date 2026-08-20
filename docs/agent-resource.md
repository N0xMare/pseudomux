# Stored agents (removed)

The product is [spec.md](spec.md): Messages + `run_stateless` + the thin
`pmux` CLI (`run` / `ping` / `doctor`) and the TypeScript, Rust, and Python
clients.

Stored launch-configuration agents are not part of this product. Current
daemons refuse every agent Request on the public wire. The protocol
Request/Response variants remain so old clients get a typed refusal.

The historical design page is [archive/agent-resource.md](archive/agent-resource.md).
Do not copy those commands.
