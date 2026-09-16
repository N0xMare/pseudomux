# Protocol

Native protocol v1 is length-prefixed JSON on an owner-only Unix socket.

The public methods are `ping`, `diagnose`, `run_stateless`, and
`run_stateful`. `run_stateful` MUST be refused with `stateful_not_enabled`
unless `pmuxd --stateful` was given. Every other request variant MUST be
refused with `session_surface_removed`.

`run_stateless` and `run_stateful` MUST use distinct result tags
(`stateless_result` / `stateful_result`). The Full result body is the same
shape as the minified one (text, usage, stop_reason, claude_version).

Pool mint uses an internal start funnel:
`start_session_owned_with_retention` (the pool also calls
`start_session_owned`). That is not a public method. Public
`start_session` is refused.

Argv for a minified mint MUST be built by pmux, not by the caller. A caller
string in the launch is the impurity this section forbids.

Conformance goldens: `tests/conformance/v1/`.
