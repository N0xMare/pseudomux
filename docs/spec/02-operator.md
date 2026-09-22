# Operator daemon

Public control is an explicit owner-only Unix-domain socket. There is no
path discovery and no client autostart.

`--pool-parent` enables the pool. Every other pool / Messages / Full-cell
flag MUST be refused without it. `--pool-claude` is required with the parent
and MUST be absolute.

| flag | default | what it bounds |
| --- | --- | --- |
| `--pool-parent DIR` | — | Enables the pool. Absolute parent for per-slot trees. |
| `--pool-claude PATH` | — | Required with `--pool-parent`, absolute. |
| `--pool-size N` | `15` | Live minified instances. Refused above the owner-set cap of 15. |
| `--pool-recycle-turns N` | `50` | Turns one minified instance serves before remint at lease end. |
| `--pool-securestorage-dir empty\|DIR` | `empty` | Default account pin. `empty` is the unsuffixed store. |
| `--pool-account NAME=empty\|DIR` | none | Additional named pin, repeatable. Caller selects `NAME`. |
| `--pool-warm MODEL[/EFFORT][@ACCOUNT]=COUNT` | none | Warm floor, repeatable. Omitted `@ACCOUNT` is `default`. |
| `--pool-system-prompt TEXT` | `The user message is the entire instruction.` | REPLACE-mode displacer, 512 bytes. Empty MUST be refused. |
| `--pool-system-prompt-file FILE` | — | Same prompt, from a file. |
| `--pool-idle-ttl-ms MS` | `300000` | Idle hold, down to the warm floor. |
| `--pool-turn-timeout-ms MS` | `600000` | Default stateless deadline. |
| `--pool-retain-dir DIR` | erase | Quarantined tree. |
| `--pool-rss-budget-mb MB` | derived | Boot check against `pool_size * 1024 MB`. |
| `--stateful` | off | Admit Full-cell `pmux run` / `run_stateful`. Requires `--pool-parent`. |
| `--messages-bind HOST:PORT` | off | Loopback Messages listener. |
| `--messages-allow-implicit` | off | Headerless Messages hatch. |
| `--pool-evidence-dir DIR` | `pool-evidence/` beside the socket | Redacted drain evidence. |
| `--pool-no-evidence` | off | Retain no pool evidence. |

With `--stateful`, every Full turn copies its transcripts to
`{pool-parent}/transcripts/` before the isolation tree is erased, so a wrap
can collect them. Those files are FULL transcripts and hold prompts and
completions, unlike the redacted `--pool-evidence-dir` mirror. There is no
flag: the directory is bounded at 256 MiB and pruned oldest-first after each
turn. An operator who does not want conversation content on that disk should
not enable `--stateful`.

Fifteen is an owner-set cap. `--pool-size 16` MUST be refused at boot,
before the socket is bound.

A minified pool cell MUST launch with `--disallowedTools "*"` and
`dont-ask`. A sidechain row on that cell is `schema_drift`.

REPLACE MUST displace Claude Code's default agent prompt. It MUST NOT be
documented as the entire `system` array the model is sent. Consumer policy
MUST live in the typed prompt / Messages flatten, not in
`--pool-system-prompt`. `--bare` MUST NOT be a pool mint flag.

A pool mint MUST build the child's environment from the daemon snapshot
through a closed allowlist. The order is
`allowlist(snapshot) - unset + set - policy_removals + profile_changes`.
Public `run_stateless` / Messages MUST NOT name environment names. Nested
Claude markers (`CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`,
`CLAUDE_CODE_REMOTE`, `CLAUDE_CODE_CHILD_SESSION`) MUST NOT reach the
child.

## Compatibility

`require_tested` is the default for pool mint and Full mint. The
distribution ships one promoted range per os/arch: Claude Code 2.1.258
through 2.1.280 on macos/aarch64, 2.1.227 through 2.1.280 on
linux/x86_64, both transparent/sdk at `transcript_drain_ms` 250, and
2.1.272 through 2.1.280 on linux/aarch64, transparent/sdk at `transcript_drain_ms` 500.
A version outside those ranges needs `--tested-claude-profile`. Receipts
live under `evidence/`. The linux/aarch64 cell is one version wide and was
measured in a native linux/arm64 container (`tools/dev/linux-arm64/`); its
wider drain is the post-answer `attachment` row every minified turn at
2.1.272 emits on that cell, max 165 ms.

macos 2.1.220 through 2.1.257 are **out of the flagless window on purpose**:
the 250 ms drain cannot cover the 2.1.220 campaign's 438 ms max reachable
post-answer arrival. Those binaries still run with an explicit operator
profile.

`allow_untested` is for deliberate probes and MUST be reported as untested.
It does not skip transcript validation.

### `--allow-unpromoted-claude`

CHOSEN policy, not a measurement, and EXPLORATORY ONLY.

| flag | default | what it does |
| --- | --- | --- |
| `--allow-unpromoted-claude` | off | Admit a (version, os, arch, terminal profile, transport) tuple no cell covers, labelled. |

Off MUST be today's behaviour exactly: an uncovered tuple is refused with
`unsupported_claude_version`.

On, the observed tuple is admitted for this daemon on the same runtime path
`--tested-claude-profile` takes, and:

- the drain is `--untested-transcript-drain-ms` (default 2000 ms), the
  conservative shipped floor an `allow_untested` probe already falls back to.
  A promoted cell's 250 ms MUST NOT be borrowed: that number is what a
  promotion exists to produce.
- `CompatibilityReport.tested` stays `false` and `CompatibilityReport.unpromoted`
  is `true`. Every artifact carrying identity MUST state `unpromoted`: the
  compatibility report on each session, the per-turn
  `unpromoted_compatibility_profile` warning, the retained evidence directory's
  `pmux-unpromoted.json` marker, and `pmux doctor`
  (`configuration.unpromoted_claude_opt_in`,
  `compatibility_profile.pool_claude_unpromoted`).
- pmuxd MUST warn once at startup and once per cell launch, naming the tuple
  and that no measurement backs it.
- it MUST NOT widen `PROMOTED_PROFILES` and MUST NOT write a receipt under
  `evidence/`.
- it MUST be refused together with `--tested-claude-profile`: one admits a cell
  the operator measured, the other admits one nobody did.

`tools/promotion` MUST refuse anything produced under it: a corpus carrying
`pmux-unpromoted.json`, and a `pmux doctor` report whose configuration layer
says `unpromoted_claude_opt_in`.

The supported route is measurement: `tools/dev/drain_n50.py`,
`tools/dev/operator_eval.py`, `tools/dev/living_pmux_run.py`, then
`tools/dev/promote.py` ([tools/dev/README.md](../../tools/dev/README.md)).

## Accounts

`--pool-securestorage-dir empty` (default) pins the unsuffixed credential
store for the `default` account. `--pool-account NAME=empty|ABS_PATH` adds
another pin in the same process. The class key is
`(model, effort, account)`; a request names the account, never the path
(`run_stateless.account`, Messages `x-pmux-account`, `pmux ask --account`,
`pmux run --account`). `pmux doctor` exercises isolated-shape `claude auth
status` for each configured pin.

How-to: [user-guide/accounts.md](../user-guide/accounts.md).
