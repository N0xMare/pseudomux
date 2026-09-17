#!/usr/bin/env bash
# Drive the linux/aarch64 measurement lane from the Darwin host.
#
#   tools/dev/linux-arm64/run.sh build
#   tools/dev/linux-arm64/run.sh versions
#   tools/dev/linux-arm64/run.sh drain 2.1.272
#   tools/dev/linux-arm64/run.sh pool 500 2.1.258 2.1.272
#   tools/dev/linux-arm64/run.sh floor-receipt 2.1.272
#   tools/dev/linux-arm64/run.sh operator-eval 2.1.272
#   tools/dev/linux-arm64/run.sh living-run 2.1.272
#   tools/dev/linux-arm64/run.sh promote 2.1.272 --floor 2.1.272
#   tools/dev/linux-arm64/run.sh shell
#
# The container is linux/arm64 and the daemon it talks to must be too: on an
# Apple Silicon host that is native, and `preflight` refuses to run if the
# Docker daemon reports any other architecture, because a translated
# measurement is not admissible as a promotion receipt
# (docs/engineering/tart-linux-guest.md sec.2).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
IMAGE="${PMUX_LANE_IMAGE:-pmux-linux-arm64:dev}"

# The Claude Code credential store the pool authenticates from. It is a BIND
# MOUNT and nothing else: it is never copied into the image, never baked into
# a layer, never passed on argv and never written to a receipt. It is mounted
# read-write on purpose, because a refreshed OAuth token has to be written
# back or the next run starts logged out.
#
# It is mounted at $HOME/.claude, which is the UNSUFFIXED store -- the
# `--pool-securestorage-dir empty` default that every tool in tools/dev uses
# (docs/spec/02-operator.md). Only three tools expose a pin at all, so the
# unsuffixed location is the one mount that serves the whole chain.
# Derived from the checkout's own location rather than written out, so no
# host's home directory is spelled in a tracked file. Override with
# PMUX_LANE_STORE anywhere the sibling layout differs.
STORE="${PMUX_LANE_STORE:-$(dirname "$REPO")/plak/.plak/harbor/claude-linux-securestorage}"

# The transcript corpus a pooled bound is measured over. Host-local, never
# committed, and deliberately OUTSIDE the checkout: these are real prompts.
CORPUS="${PMUX_LANE_CORPUS_HOST:-$HOME/.local/state/pmux-linux-arm64-corpus}"

subcommand="${1:?usage: run.sh <build|preflight|versions|drain|pool|floor-receipt|operator-eval|living-run|promote|shell> ...}"
shift || true

preflight() {
    local arch
    arch="$(docker version --format '{{.Server.Arch}}')"
    if [ "$arch" != "arm64" ]; then
        echo "the Docker daemon reports ${arch}; this lane needs a native linux/arm64 daemon." >&2
        echo "An emulated or translated cell is a smoke test, not a promotion receipt." >&2
        exit 2
    fi
    if [ ! -r "$STORE/.credentials.json" ]; then
        echo "no credential store at ${STORE}" >&2
        echo "Set PMUX_LANE_STORE, or see this directory's README on rotating the pin." >&2
        exit 2
    fi
    mkdir -p "$CORPUS"
    chmod 0700 "$CORPUS"
}

build() {
    docker build --platform linux/arm64 \
        -f "$REPO/tools/dev/linux-arm64/Dockerfile" \
        --build-arg "HOST_UID=$(id -u)" \
        --build-arg "HOST_GID=$(id -g)" \
        -t "$IMAGE" \
        "$REPO"
}

in_container() {
    docker run --rm --platform linux/arm64 \
        --init \
        --mount "type=bind,src=$REPO,dst=/src" \
        --mount "type=bind,src=$STORE,dst=/home/pmux/.claude" \
        --mount "type=bind,src=$CORPUS,dst=/corpus" \
        "$@"
}

case "$subcommand" in
build)
    build
    ;;

preflight)
    preflight
    docker image inspect "$IMAGE" >/dev/null
    in_container "$IMAGE" versions
    ;;

# The two measuring steps that emit a receipt on stdout are redirected HERE,
# on the host, so the file is created by the host user in the host's tree.
pool)
    preflight
    bound="${1:?usage: pool <bound-ms> <version>...}"
    shift
    out="$REPO/evidence/pooled-transcript-drain-linux-aarch64.json"
    in_container "$IMAGE" pool "$bound" "$@" > "$out"
    echo "wrote $out"
    ;;

floor-receipt)
    preflight
    version="${1:?usage: floor-receipt <version>}"
    out="$REPO/evidence/promoted-profile-${version}-linux-aarch64.json"
    in_container "$IMAGE" floor-receipt "$version" > "$out"
    echo "wrote $out"
    ;;

shell)
    preflight
    in_container -it "$IMAGE" shell "$@"
    ;;

versions | drain | operator-eval | living-run | promote)
    preflight
    in_container "$IMAGE" "$subcommand" "$@"
    ;;

*)
    echo "unknown subcommand: $subcommand" >&2
    exit 2
    ;;
esac
