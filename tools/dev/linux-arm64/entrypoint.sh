#!/bin/bash
# One subcommand per tool in the linux/aarch64 lane. Runs as the unprivileged
# `pmux` user with /src bind-mounted from the host checkout, so every receipt
# lands in the working tree and `promote.py` reads the real
# crates/service/src/compatibility.rs.
#
# Credentials are never an argument here. The pool reads them from the
# unsuffixed store at $HOME/.claude, which `run.sh` bind-mounts; nothing in
# this file prints, copies or logs them.
set -euo pipefail

RELEASE_DIR="${PMUX_LANE_RELEASE_DIR:-/opt/pmux/release}"
CORPUS="${PMUX_LANE_CORPUS:-/corpus}"
ARCH_TAG=aarch64

claude_for() {
    local version="$1"
    local path="$HOME/.local/share/pmux/claude/$version/claude"
    if [ ! -x "$path" ]; then
        echo "no Claude Code $version in this image; the Dockerfile pins the versions it installs" >&2
        exit 2
    fi
    printf '%s' "$path"
}

# Nothing hands ownership back. The container runs as the host uid and gid
# (run.sh passes both at build and at run), and virtiofs passes bind-mount
# ownership through numerically, so every receipt under /src/evidence and
# every transcript under /corpus is already the host user's. No step here
# runs as root, so nothing root-owned can appear.

subcommand="${1:-shell}"
shift || true

case "$subcommand" in
versions)
    cat /home/pmux/.local/share/pmux/claude/SHA256SUMS 2>/dev/null || true
    for path in /home/pmux/.local/share/pmux/claude/*/claude; do
        printf '%s -> ' "$path"
        "$path" --version
    done
    python3 -c 'import platform; print("host_identity:", platform.system().lower(), platform.machine().lower())'
    ;;

drain)
    version="${1:?usage: drain <version> [extra args]}"
    shift
    install -d -m 0700 "$CORPUS/$version"
    exec python3 /src/tools/dev/drain_n50.py \
        --release-dir "$RELEASE_DIR" \
        --claude "$(claude_for "$version")" \
        --corpus-out "$CORPUS/$version" \
        --output "/src/evidence/linux-drain-n50-${version}-${ARCH_TAG}.json" \
        "$@"
    ;;

pool)
    # The pooled bound, over every version with a kept corpus. This is the
    # only step that writes evidence/pooled-transcript-drain-linux-aarch64.json,
    # and it writes it whole: no key of it is composed by hand.
    bound="${1:?usage: pool <bound-ms> <version>...}"
    shift
    args=()
    for version in "$@"; do
        args+=(--corpus "$CORPUS/$version" --version "$version")
    done
    exec python3 /src/tools/promotion/measure_transcript_drain.py \
        "${args[@]}" \
        --bound-ms "$bound" \
        --os linux --arch "$ARCH_TAG" \
        --json
    ;;

floor-receipt)
    # The floor's own single-version receipt, read by
    # compatibility.rs::every_promoted_drain_is_the_one_its_receipt_recommends.
    version="${1:?usage: floor-receipt <version>}"
    exec python3 /src/tools/promotion/measure_transcript_drain.py \
        --corpus "$CORPUS/$version" --version "$version" \
        --os linux --arch "$ARCH_TAG" \
        --json
    ;;

operator-eval)
    version="${1:?usage: operator-eval <version> [extra args]}"
    shift
    exec python3 /src/tools/dev/operator_eval.py \
        --release-dir "$RELEASE_DIR" \
        --claude "$(claude_for "$version")" \
        --output "/src/evidence/linux-operator-eval-${version}-${ARCH_TAG}.json" \
        "$@"
    ;;

living-run)
    version="${1:?usage: living-run <version> [extra args]}"
    shift
    exec python3 /src/tools/dev/living_pmux_run.py \
        --release-dir "$RELEASE_DIR" \
        --claude "$(claude_for "$version")" \
        --output "/src/evidence/linux-living-pmux-run-${version}-${ARCH_TAG}.json" \
        "$@"
    ;;

promote)
    version="${1:?usage: promote <version> [extra args]}"
    shift
    exec python3 /src/tools/dev/promote.py \
        --release-dir "$RELEASE_DIR" \
        --claude "$(claude_for "$version")" \
        --output "/src/evidence/promotion-${version}-linux-${ARCH_TAG}.json" \
        "$@"
    ;;

shell)
    exec /bin/bash "$@"
    ;;

*)
    echo "unknown subcommand: $subcommand" >&2
    echo "known: versions drain pool floor-receipt operator-eval living-run promote shell" >&2
    exit 2
    ;;
esac
