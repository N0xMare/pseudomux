# Living development workflow

Three commands. Nothing else can stop a Linux operator pin or a commit.

| Command | When | What it proves |
| --- | --- | --- |
| `tools/dev/check.sh` | Before commit | The tree builds and living tests pass. |
| `tools/dev/check.sh --push` | Before push | Plus pool e2e and process blackbox (no real Claude). |
| `tools/dev/operator_eval.py` | Before changing the operator Claude pin | This Claude works on **this** OS with this pmux. |
| `tools/dev/promote.py` | Only to drop `--tested-claude-profile` | This OS already has a pooled drain receipt; widen `PROMOTED_PROFILES` for **that os/arch only**. |

`tools/promotion/` is the drop-flag engine (`promote.py` wraps it). Gate A, Phase 0, linux-docker, and package-smoke have been removed.

## check

```bash
tools/dev/check.sh
tools/dev/check.sh --push
```

Always: `cargo fmt --check`, clippy `-D warnings`, `cargo test --workspace`, TypeScript `npm test`, Python client tests, excluded vendor lanes (including `cargo check --all-targets --no-default-features` on `vendor/rmux-server`), `tools/evidence_common/tests/test_portable_paths.py`, `tools/dev/tests` (documented surface, workflow), and `ruff check --no-cache tools/dev tools/evidence_common clients/python`. Tree-wide redaction lives at `tools/dev/redaction/test_redaction.py` (not in the default check: it needs a clean git index).

`--push` also runs `-p pseudomux-e2e --include-ignored`, ignored `private_runtime` sidecar tests (no real Claude), and re-runs the process-blackbox targets serially (`--test-threads=1`). Those blackbox tests already ran in the workspace invocation; the serial pass is the load-sensitive one. It unsets `PMUX_POOL_REAL_CLAUDE` so ignored real-turn lanes skip.

## operator-eval

```bash
cargo build --release -p pmux -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook
python3 tools/dev/operator_eval.py \
  --release-dir target/release \
  --claude "$HOME/.local/share/pmux/claude/2.1.236/claude" \
  --output evidence/linux-operator-eval-2.1.236-x86_64.json
```

Spends real model turns. Does **not** read or write a pooled-drain receipt. Does **not** edit `PROMOTED_PROFILES`. A green receipt is `GREEN_OPERATOR` under schema `pmux.operator-eval.v1`. Product identity is Messages same-cell + cache hit, not a `pgrep` pid-set. `python3 tools/dev/operator_eval.py --describe` prints the check list without spending a turn.

## promote

```bash
python3 tools/dev/promote.py \
  --release-dir target/release \
  --claude /path/to/claude \
  --output evidence/promotion-2.1.227-macos-aarch64.json
```

If `evidence/pooled-transcript-drain-<os>-<arch>.json` is missing, the tool exits 2 and says you cannot **drop the flag** on that OS. Use `operator_eval.py` to pin the binary instead.

Linux currently has no pooled-drain receipt. macos does (`evidence/pooled-transcript-drain-macos-aarch64.json`).
