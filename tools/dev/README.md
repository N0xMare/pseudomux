# Living development workflow

`check.sh` (both invocations), `operator_eval.py` and `promote.py` gate a commit,
a push, a Linux operator pin and the drop-flag; `model_matrix.py` probes the
pool's model table and gates nothing.

| Command | When | What it proves |
| --- | --- | --- |
| `tools/dev/check.sh` | Before commit | The tree builds and living tests pass. |
| `tools/dev/check.sh --push` | Before push | Plus pool e2e and process blackbox (no real Claude). |
| `tools/dev/operator_eval.py` | Before changing the operator Claude pin | This Claude works on **this** OS with this pmux. |
| `tools/dev/model_matrix.py` | Before pinning `MODEL_TABLE` to a Claude version | Every `(model, effort)` cell the table admits answers through a real pooled cell on **this** OS. |
| `tools/dev/promote.py` | Only to drop `--tested-claude-profile` | This OS already has a pooled drain receipt; widen `PROMOTED_PROFILES` for **that os/arch only**. |

`tools/promotion/` is the drop-flag engine (`promote.py` wraps it). Gate A, Phase 0, linux-docker, and package-smoke have been removed.

## check

```bash
tools/dev/check.sh
tools/dev/check.sh --push
```

Always: `cargo fmt --check`, clippy `-D warnings`, `cargo test --workspace`, TypeScript `npm test`, Python client tests, excluded vendor lanes (including `cargo check --all-targets --no-default-features` on `vendor/rmux-server`), `tools/evidence_common/tests/test_portable_paths.py`, `tools/dev/tests` (documented surface, workflow), `tools/promotion/tests`, and `ruff check --no-cache tools/dev tools/evidence_common tools/promotion clients/python`. Tree-wide redaction lives at `tools/dev/redaction/test_redaction.py` (not in the default check: it needs a clean git index).

`--push` also runs `-p pseudomux-e2e --include-ignored`, ignored `private_runtime` sidecar tests (no real Claude), and re-runs the process-blackbox targets serially (`--test-threads=1`). Those blackbox tests already ran in the workspace invocation; the serial pass is the load-sensitive one. It unsets `PMUX_POOL_REAL_CLAUDE` so ignored real-turn lanes skip.

## operator-eval

```bash
cargo build --release -p pmux -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook
python3 tools/dev/operator_eval.py \
  --release-dir target/release \
  --claude "$HOME/.local/share/pmux/claude/2.1.257/claude" \
  --output evidence/linux-operator-eval-2.1.257-x86_64.json
```

Spends real model turns. Does **not** read or write a pooled-drain receipt. Does **not** edit `PROMOTED_PROFILES`. A green receipt is `GREEN_OPERATOR` under schema `pmux.operator-eval.v1`. Product identity is Messages same-cell + cache hit, not a `pgrep` pid-set. `python3 tools/dev/operator_eval.py --describe` prints the check list without spending a turn.

## promote

```bash
python3 tools/dev/promote.py \
  --release-dir target/release \
  --claude "$HOME/.local/share/pmux/claude/2.1.257/claude" \
  --output evidence/promotion-2.1.257-linux-x86_64.json
```

If `evidence/pooled-transcript-drain-<os>-<arch>.json` is missing, the tool exits 2 and says you cannot **drop the flag** on that OS. Use `operator_eval.py` to pin the binary instead.

macos has `evidence/pooled-transcript-drain-macos-aarch64.json`. linux/x86_64 has `evidence/pooled-transcript-drain-linux-x86_64.json` (Path B campaign versions 2.1.227/2.1.232/2.1.233, max reachable 118 ms, bound 250 ms). macos floor is 2.1.220; linux floor is 2.1.227, tested through 2.1.257. A first promotion on an OS with no shipped cell needs `--floor`.

## model-matrix

```bash
cargo build --release -p pmux -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook
python3 tools/dev/model_matrix.py \
  --release-dir target/release \
  --claude "$HOME/.local/share/pmux/claude/2.1.257/claude" \
  --output evidence/linux-model-matrix-2.1.257-x86_64.json
```

`MODEL_TABLE` (`crates/service/src/pool/class.rs`) is CHOSEN, not MEASURED, and
its own doc comment says the cells "must be probed before this table is pinned
to a Claude version -- one `--model <M> --effort <E>` probe per cell, recorded
with the version". This is that probe. One real turn per admitted cell, plus one
per model through that model's first alias, all against one pooled daemon.
The rows are derived from `class.rs` and never restated in the tool, so a model
added to the table is probed by the next run.

`python3 tools/dev/model_matrix.py --describe` prints the derived row list and
spends no turns. `--only MODEL` and `--skip-effort EFFORT` are repeatable and
narrow the matrix; a full run is one turn per printed row, so it is expensive.

`GREEN_MATRIX` under schema `pmux.model-matrix.v1` means: every cell answered
its arithmetic-nonce grade exactly, every answer's `reported_model` is the
canonical id pmux launched (or that id plus a `-YYYYMMDD` build date), and the pool neither halted nor leaked. It does
**not** mean the Claude binary is promoted: this tool reads and writes no
pooled-drain receipt, does not touch `PROMOTED_PROFILES`, and does not edit
`MODEL_TABLE`. Use `operator_eval.py` to pin an operator binary and `promote.py`
to drop `--tested-claude-profile`.
