#!/usr/bin/env bash

set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
readonly REPO_DIR
readonly NIGHTLY_TOOLCHAIN="nightly-2026-03-26"
readonly REQUIRED_CARGO_FUZZ_VERSION="cargo-fuzz 0.13.2"
readonly RUNS="${PMUX_FUZZ_RUNS:-50000}"
readonly EVIDENCE_ROOT="${PMUX_FUZZ_EVIDENCE_ROOT:-$REPO_DIR/.context/gate-a-fuzz-runs}"
readonly CARGO_FUZZ_BIN="${PMUX_CARGO_FUZZ_BIN:-$REPO_DIR/.context/tools/cargo-fuzz/bin/cargo-fuzz}"
readonly FUZZ_TARGET_DIR="${PMUX_FUZZ_TARGET_DIR:?PMUX_FUZZ_TARGET_DIR must name the isolated validation target}"
readonly NIGHTLY_CARGO="${PMUX_NIGHTLY_CARGO:?PMUX_NIGHTLY_CARGO must name the exact pinned nightly cargo binary}"
readonly NIGHTLY_RUSTC="${PMUX_NIGHTLY_RUSTC:?PMUX_NIGHTLY_RUSTC must name the exact pinned nightly rustc binary}"
readonly NIGHTLY_BIN_DIR="${PMUX_NIGHTLY_BIN_DIR:?PMUX_NIGHTLY_BIN_DIR must name the exact pinned nightly bin directory}"

if [[ ! "$RUNS" =~ ^[1-9][0-9]*$ ]]; then
  echo "PMUX_FUZZ_RUNS must be a positive integer (got: $RUNS)" >&2
  exit 2
fi
if [[ ! -x "$CARGO_FUZZ_BIN" ]]; then
  echo "required isolated cargo-fuzz binary is missing: $CARGO_FUZZ_BIN" >&2
  exit 2
fi
if [[ "$FUZZ_TARGET_DIR" != /* ]]; then
  echo "PMUX_FUZZ_TARGET_DIR must be absolute: $FUZZ_TARGET_DIR" >&2
  exit 2
fi
if [[ "$NIGHTLY_BIN_DIR" != /* || ! -d "$NIGHTLY_BIN_DIR" || -L "$NIGHTLY_BIN_DIR" ]]; then
  echo "pinned nightly bin directory must be one absolute real directory: $NIGHTLY_BIN_DIR" >&2
  exit 2
fi
if [[ "$NIGHTLY_CARGO" != "$NIGHTLY_BIN_DIR/cargo" || "$NIGHTLY_RUSTC" != "$NIGHTLY_BIN_DIR/rustc" ]]; then
  echo "pinned nightly cargo/rustc must be direct children of PMUX_NIGHTLY_BIN_DIR" >&2
  exit 2
fi
for tool in \
  "$NIGHTLY_CARGO" \
  "$NIGHTLY_RUSTC" \
  "$NIGHTLY_BIN_DIR/rustdoc" \
  "$NIGHTLY_BIN_DIR/rustfmt" \
  "$NIGHTLY_BIN_DIR/cargo-fmt" \
  "$NIGHTLY_BIN_DIR/cargo-clippy" \
  "$NIGHTLY_BIN_DIR/clippy-driver"; do
  if [[ "$tool" != /* || ! -x "$tool" ]]; then
    echo "pinned nightly tool must be one absolute executable: $tool" >&2
    exit 2
  fi
done
if [[ "$($CARGO_FUZZ_BIN --version)" != "$REQUIRED_CARGO_FUZZ_VERSION" ]]; then
  echo "cargo-fuzz must be exactly $REQUIRED_CARGO_FUZZ_VERSION" >&2
  "$CARGO_FUZZ_BIN" --version >&2
  exit 2
fi

mkdir -p "$EVIDENCE_ROOT"
chmod 700 "$EVIDENCE_ROOT"
RUN_DIR="$(mktemp -d "$EVIDENCE_ROOT/run.XXXXXX")"
readonly RUN_DIR
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
readonly STARTED_AT
START_EPOCH="$(date -u +%s)"
readonly START_EPOCH
FUZZ_HOST="$("$NIGHTLY_RUSTC" --version --verbose | sed -n 's/^host: //p')"
readonly FUZZ_HOST

# The target set is DERIVED from `fuzz/Cargo.toml`, not written out here.
#
# It was written out five times -- two `mkdir -p` lists, three metadata `printf`
# pairs, three `run_fuzz` calls and a `for` list -- and in sync only because
# nobody had added a fourth target. A target added to the manifest and to four
# of the five would have been built, never run, and never missed.
#
# The seeds and length bounds stay written down, because they are the evidence
# this gate produces and a generated one would make two runs incomparable. What
# is derived is WHICH targets exist; what is declared is what each is run with,
# and a target the declaration does not know ABORTS the gate rather than being
# skipped.
fuzz_target_names() {
  awk '/^\[\[bin\]\]/ { in_bin = 1; next }
       in_bin && /^name[[:space:]]*=/ {
         gsub(/^name[[:space:]]*=[[:space:]]*"|"[[:space:]]*$/, "")
         print
         in_bin = 0
       }' "$REPO_DIR/fuzz/Cargo.toml"
}

fuzz_parameters() {
  case "$1" in
    transcript_jsonl) printf '1347241301 1048576\n' ;;
    transcript_cursor) printf '1347241302 524288\n' ;;
    native_frame) printf '1347241303 8388608\n' ;;
    *)
      echo "fuzz target $1 is in fuzz/Cargo.toml with no seed or length bound" >&2
      return 1
      ;;
  esac
}

FUZZ_TARGETS="$(fuzz_target_names)"
readonly FUZZ_TARGETS
if [[ -z "$FUZZ_TARGETS" ]]; then
  echo "no [[bin]] target found in fuzz/Cargo.toml" >&2
  exit 2
fi
for fuzz_target in $FUZZ_TARGETS; do
  fuzz_parameters "$fuzz_target" >/dev/null
done

finalize() {
  local gate_exit_code=$?
  trap - EXIT
  {
    printf 'exit_status=%s\n' "$gate_exit_code"
    printf 'finished_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'elapsed_seconds=%s\n' "$(($(date -u +%s) - START_EPOCH))"
  } >"$RUN_DIR/status.txt"
  (
    cd "$RUN_DIR"
    find . -type f ! -name evidence.sha256 -print \
      | LC_ALL=C sort \
      | while IFS= read -r path; do
          shasum -a 256 "$path"
        done
  ) >"$RUN_DIR/evidence.sha256"
  echo "fuzz evidence: $RUN_DIR"
  exit "$gate_exit_code"
}
trap finalize EXIT

for fuzz_target in $FUZZ_TARGETS; do
  mkdir -p "$RUN_DIR/corpus/$fuzz_target" "$RUN_DIR/artifacts/$fuzz_target"
done

PATH="$NIGHTLY_BIN_DIR:$(dirname "$CARGO_FUZZ_BIN"):$PATH"
export PATH
export CARGO_TERM_COLOR=never
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$FUZZ_TARGET_DIR"
export RUSTC="$NIGHTLY_RUSTC"
export LC_ALL=C
unset RUSTFLAGS RUSTDOCFLAGS

{
  printf 'started_at_utc=%s\n' "$STARTED_AT"
  printf 'repo=%s\n' "$REPO_DIR"
  printf 'git_head=%s\n' "$(git -C "$REPO_DIR" rev-parse HEAD)"
  printf 'nightly_toolchain=%s\n' "$NIGHTLY_TOOLCHAIN"
  printf 'fuzz_host=%s\n' "$FUZZ_HOST"
  printf 'cargo_fuzz_version=%s\n' "$($CARGO_FUZZ_BIN --version)"
  printf 'runs_per_target=%s\n' "$RUNS"
  printf 'timeout_seconds=5\n'
  for fuzz_target in $FUZZ_TARGETS; do
    read -r seed max_len <<<"$(fuzz_parameters "$fuzz_target")"
    printf '%s_seed=%s\n' "$fuzz_target" "$seed"
    printf '%s_max_len=%s\n' "$fuzz_target" "$max_len"
  done
  uname -a
  "$NIGHTLY_CARGO" --version --verbose
  "$NIGHTLY_RUSTC" --version --verbose
} >"$RUN_DIR/metadata.txt"

git -C "$REPO_DIR" status --porcelain=v1 --untracked-files=all >"$RUN_DIR/git-status.txt"
shasum -a 256 "$CARGO_FUZZ_BIN" >"$RUN_DIR/tool.sha256"
: >"$RUN_DIR/fuzz-binaries.sha256"

(
  cd "$REPO_DIR"
  {
    printf '%s\n' \
      Cargo.toml \
      Cargo.lock \
      rust-toolchain.toml \
      scripts/gate-a-fuzz.sh \
      fuzz/Cargo.toml \
      fuzz/Cargo.lock \
      fuzz/README.md \
      bin/pmuxd/src/handler.rs
    find \
      crates/claude/src \
      crates/protocol/src \
      fuzz/fuzz_targets \
      fuzz/corpus \
      -type f -print
  } | LC_ALL=C sort -u | while IFS= read -r path; do
    shasum -a 256 "$path"
  done
) >"$RUN_DIR/inputs.sha256"
shasum -a 256 "$RUN_DIR/inputs.sha256" >"$RUN_DIR/input-set.sha256"

run_logged() {
  local name=$1
  shift
  "$@" 2>&1 | tee "$RUN_DIR/$name.log"
}

cd "$REPO_DIR"

# The fuzz package is intentionally outside the production workspace. Keep its
# formatting, compilation, lint, and test evidence explicit instead of relying
# on root-workspace commands that cannot see it.
run_logged fmt "$NIGHTLY_CARGO" fmt --manifest-path fuzz/Cargo.toml -- --check
run_logged check "$NIGHTLY_CARGO" check --locked --manifest-path fuzz/Cargo.toml --bins
run_logged clippy "$NIGHTLY_CARGO" clippy --locked --manifest-path fuzz/Cargo.toml --bins -- -D warnings
run_logged test "$NIGHTLY_CARGO" test --locked --manifest-path fuzz/Cargo.toml

run_fuzz() {
  local target=$1
  local seed=$2
  local max_len=$3
  run_logged "fuzz-$target" \
    "$NIGHTLY_CARGO" fuzz run "$target" \
      "$RUN_DIR/corpus/$target" "$REPO_DIR/fuzz/corpus/$target" -- \
      "-artifact_prefix=$RUN_DIR/artifacts/$target/" \
      "-seed=$seed" \
      "-runs=$RUNS" \
      -timeout=5 \
      "-max_len=$max_len" \
      -print_final_stats=1
  local binary="$FUZZ_TARGET_DIR/$FUZZ_HOST/release/$target"
  if [[ ! -x "$binary" ]]; then
    echo "cargo-fuzz did not leave the expected target binary: $binary" >&2
    return 1
  fi
  shasum -a 256 "$binary" >>"$RUN_DIR/fuzz-binaries.sha256"
}

for fuzz_target in $FUZZ_TARGETS; do
  read -r seed max_len <<<"$(fuzz_parameters "$fuzz_target")"
  run_fuzz "$fuzz_target" "$seed" "$max_len"
done

# cargo-fuzz unconditionally creates `<manifest_dir>/artifacts/<target>/` and a
# `fuzz/target` directory relative to its own manifest, regardless of
# CARGO_TARGET_DIR and of the explicit -artifact_prefix above. Left behind they
# are source-tree generated output and `gate-a-residue.sh` correctly fails on
# them, which made Gate A structurally unpassable whenever the fuzz phase ran.
# Remove them, but only when empty: a file under fuzz/artifacts would be a crash
# libFuzzer wrote outside the evidence root, and losing it silently is far worse
# than a residue finding.
prune_empty_source_output() {
  local dir=$1
  if [[ -d "$dir" ]]; then
    if [[ -n "$(ls -A "$dir")" ]]; then
      echo "generated output left in the source tree: $dir" >&2
      return 1
    fi
    rmdir "$dir"
  fi
  return 0
}

for fuzz_target in $FUZZ_TARGETS; do
  prune_empty_source_output "$REPO_DIR/fuzz/artifacts/$fuzz_target"
done
prune_empty_source_output "$REPO_DIR/fuzz/artifacts"
prune_empty_source_output "$REPO_DIR/fuzz/target"
