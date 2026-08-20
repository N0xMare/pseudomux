#!/usr/bin/env bash
# Living tree check. This is the commit/push gate, not Gate A.
set -euo pipefail

usage() {
  cat <<'EOF'
tools/dev/check.sh [--push]

  (default)  fmt, clippy -D warnings, cargo test --workspace,
             TypeScript tests, Python client tests, vendor lanes,
             portable_paths tests, tools/dev tests,
             ruff check --no-cache tools/dev tools/evidence_common clients/python
  --push     also pool e2e, ignored sidecar private_runtime, process blackbox

--push unsets PMUX_POOL_REAL_CLAUDE so ignored real-turn lanes skip.
EOF
}

push=0
for arg in "$@"; do
  case "$arg" in
    --push) push=1 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "unknown argument: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

echo "== cargo fmt --check"
cargo fmt --all -- --check

echo "== cargo clippy"
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

echo "== cargo test --workspace"
cargo test --locked --workspace --all-targets

echo "== typescript tests"
if [[ ! -d clients/typescript/node_modules ]]; then
  (cd clients/typescript && npm ci)
fi
(cd clients/typescript && npm test)

echo "== python client tests"
(cd clients/python && PYTHONPATH=. python3 -m unittest discover -s tests -q)

echo "== vendor (excluded from workspace)"
cargo fmt --manifest-path vendor/rmux-client/Cargo.toml --all -- --check
cargo clippy --locked --offline --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --locked --offline --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features -- --test-threads=1
rustfmt --edition 2021 --check vendor/rmux-server/src/lib.rs vendor/rmux-server/build.rs
cargo check --locked --offline --manifest-path vendor/rmux-server/Cargo.toml --all-targets --no-default-features
cargo clippy --locked --offline --manifest-path vendor/rmux-server/Cargo.toml --all-targets --all-features -- -D warnings -A clippy::collapsible-else-if -A clippy::uninlined-format-args
cargo test --locked --offline --manifest-path vendor/rmux-server/Cargo.toml --lib --no-default-features pane_io::tests:: -- --test-threads=1

echo "== evidence_common portable_paths"
python3 -m unittest discover -s tools/evidence_common/tests -p 'test_portable_paths.py' -q

echo "== tools/dev tests"
# Prefer the debug tree cargo test just built. A stale target/release must
# not own documented-surface.
PMUX_DOCUMENTED_SURFACE_BIN_DIR="$root/target/debug" \
  python3 -m unittest discover -s tools/dev/tests -q

echo "== ruff"
if command -v ruff >/dev/null; then
  ruff check --no-cache tools/dev tools/evidence_common clients/python
else
  python3 -m ruff check --no-cache tools/dev tools/evidence_common clients/python
fi

if [[ "$push" -eq 1 ]]; then
  unset PMUX_POOL_REAL_CLAUDE || true

  echo "== e2e living product"
  cargo test --locked -p pseudomux-e2e --all-targets -- --include-ignored --test-threads=1

  echo "== ignored sidecar (no real Claude)"
  cargo test --locked -p pseudomux-service --test private_runtime -- --ignored --test-threads=1

  echo "== process blackbox"
  cargo test --locked -p pmux --test native_cli --test process_boundary -- --test-threads=1
  cargo test --locked -p pmuxd --test process_blackbox -- --test-threads=1
  cargo test --locked -p pmux-mcp --test stdio_blackbox -- --test-threads=1
  cargo test --locked -p pmux-rmuxd --test process_blackbox -- --test-threads=1
  cargo test --locked -p pmux-launcher --test process_blackbox -- --test-threads=1
  cargo test --locked -p pmux-hook --test process_blackbox -- --test-threads=1
fi

echo "OK"
