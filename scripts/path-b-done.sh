#!/usr/bin/env bash
# Historical Path B certification. Not living verification.
# Living: tools/dev/check.sh, operator_eval.py, promote.py.
# Gate A has been removed. Criterion 4 is MET when run_gate.py is gone.
#
# The Path B done-gate: the owner's five criteria, run rather than read.
#
# WHAT THIS IS
#
# The five criteria live in `docs/path-b-verdict.md` section 1. Until now they
# were checked by an agent reading the tree and writing a paragraph, which is
# why they have drifted and been re-verified three times with three different
# answers. `scripts/path_b_done.py` binds each published criterion to a function
# that READS EVIDENCE -- a Gate A receipt, two registers, the installed
# `claude --version`, a test run -- and refuses, before measuring anything, if
# the set of criteria it implements is not the set the document publishes.
#
# This script is the environment half of that: it resolves the tools each
# criterion needs and hands them over explicitly. It resolves them WITHOUT
# failing: a missing `claude` is criterion 3 NOT MET with a reason, not an
# aborted run, because "the tool was not here" is exactly the state a done-gate
# must report rather than skip. The only faults that abort are the ones that
# stop the gate deciding at all, and those exit 2.
#
#   exit 0  every criterion MET
#   exit 1  at least one NOT MET, each named with why
#   exit 2  the gate could not decide
#   exit 3  a partial run under `--only`, which is never a verdict
#
# USAGE
#
# Living tree check is tools/dev/check.sh. This script is leftover Path B
# certification. Criterion 4 is MET iff run_gate.py is gone and check.sh exists.
#
#   bash scripts/path-b-done.sh \
#     [--commit <rev>] [--only N] [--max-receipt-age-days N]
#

set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_DIR
readonly CHECKER="$SCRIPT_DIR/path_b_done.py"

# Every cell of the gate that invokes python carries this, and the residue audit
# fails on a source-tree `__pycache__`. A done-gate that leaves residue behind
# has failed one of the things it is checking.
export PYTHONDONTWRITEBYTECODE=1

if [[ ! -f "$CHECKER" ]]; then
  echo "the criteria checker is missing: $CHECKER" >&2
  exit 2
fi

PYTHON="${PMUX_DONE_PYTHON:-}"
if [[ -z "$PYTHON" ]]; then
  PYTHON="$(command -v python3 || true)"
fi
if [[ -z "$PYTHON" || ! -x "$PYTHON" ]]; then
  echo "no python3 to run the criteria with (set PMUX_DONE_PYTHON)" >&2
  exit 2
fi

# Resolved and passed through rather than left to PATH inside the checker, so
# the run records which binary answered. Neither is fatal here: a criterion that
# cannot find its tool reports itself NOT MET.
CARGO="${PMUX_DONE_CARGO:-$(command -v cargo || true)}"
CLAUDE="${PMUX_DONE_CLAUDE:-$(command -v claude || true)}"
[[ -x "$CARGO" ]] || CARGO=""
[[ -x "$CLAUDE" ]] || CLAUDE=""

echo "repo=$REPO_DIR"
echo "python=$PYTHON"
echo "cargo=${CARGO:-<none>}"
echo "claude=${CLAUDE:-<none>}"

exec "$PYTHON" "$CHECKER" \
  --repo "$REPO_DIR" \
  --cargo "$CARGO" \
  --claude "$CLAUDE" \
  "$@"
