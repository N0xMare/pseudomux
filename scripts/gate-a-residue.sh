#!/usr/bin/env bash
# Read-only, defense-in-depth residue audit for the exact Gate A candidate.
# Individual Rust process/lifecycle tests remain responsible for their exact
# PID/session/socket identities; this final command catches surviving candidate
# processes and well-known workspace/test artifacts after the ordered gate.

set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
source_root=$(cd "$script_dir/.." && pwd -P)

# The /tmp prefixes this audit scans for are DERIVED from the test sources, not
# listed here. The list that used to live here named ten prefixes; the tree uses
# well over three times that, and the exact figure is deliberately not written
# down here -- it moved the day this comment was written (a new
# `CancellationFixture::start` prefix landed with transport layer (b)) and a
# number nothing derives is the same defect one paragraph away from the fix for
# it. Run `derive_temp_prefixes` to count them. Every prefix belonging to a
# `bin/` blackbox test (`ph-`, `pl-`, `prd-`, `pmd-`, `clp-`, `pmcp-`,
# `pmux-cli-`) was absent from the old list, as was every prefix added after it
# was written -- `pmux-pool-wave-`, `pmux-containment-`, `pmux-spellings-`. So
# "Gate A residue audit passed." was a sentence about ten names while claiming
# to be a sentence about leaked test runtime, and a whole wave of leaked pool
# roots sat under /tmp inside the set it did not look at.
#
# `FLOOR` is those original ten, kept only as a LOWER bound on the derivation: a
# regex that silently stops matching narrows this scan back to nothing while
# still printing "passed", so a derivation that loses a known-good prefix
# REFUSES rather than reports. It can only make this stricter, never looser.
FLOOR=(
  pmux-e2e-
  pmux-private-smoke-
  pmux-private-timeout-
  pmux-kill-
  pmux-pane-kill-
  pmux-soak-
  pmux-sidecar-loss-
  pmux-observed-escape-
  pmux-l5-daemon-
  pmux-performance-
)

derive_temp_prefixes() {
  python3 - "$source_root" <<'PY'
import pathlib
import re
import sys

# A prefix reaches /tmp one of three ways. Over-collection is deliberate and
# safe: it can only widen the scan. Under-collection is the failure this
# derivation exists to prevent, so anything ambiguous is kept.
DIRECT = re.compile(r'\.prefix\(\s*&?(?:format!\(\s*)?"([^"{]+)')
JOINED = re.compile(
    r'PathBuf::from\("/tmp"\)\s*\.\s*join\(\s*format!\(\s*"([^"{]+)', re.S
)
# `.prefix(some_variable)` hides the literal behind a helper, so take the
# literals its callers pass in. Every `CancellationFixture::start("pmux-...", ..)`
# in `private_runtime.rs` reaches /tmp this way and every one was invisible to a
# source scan that only read `.prefix("...")`. That set grows whenever a
# transport regression is added -- layer (b) added one -- which is exactly why
# it is derived and why no count of it is written here.
INDIRECT = re.compile(r"\.prefix\(\s*[a-z_][a-z0-9_]*\s*\)")
FORWARDED = re.compile(r'::(?:start|new|with_prefix)\(\s*"([^"{]+)"')
# mktemp-shaped: a hyphenated lowercase stem. Rejects `ps` and friends that the
# forwarded-argument rule sweeps up from unrelated calls.
SHAPE = re.compile(r"^[a-z][a-z0-9]*(-[a-z0-9]+)*-?$")

root = pathlib.Path(sys.argv[1])
prefixes = set()
for area in ("crates", "bin"):
    for path in sorted((root / area).rglob("*.rs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        found = set(JOINED.findall(text))
        if 'tempdir_in("/tmp")' in text:
            found |= set(DIRECT.findall(text))
            if INDIRECT.search(text):
                found |= set(FORWARDED.findall(text))
        prefixes |= {item for item in found if "-" in item and SHAPE.match(item)}
for item in sorted(prefixes):
    print(item)
PY
}

mapfile -t derived_prefixes < <(derive_temp_prefixes)
if ((${#derived_prefixes[@]} == 0)); then
  printf 'derived no /tmp test-root prefixes from %s; refusing to report a result\n' \
    "$source_root" >&2
  exit 2
fi
for floor_prefix in "${FLOOR[@]}"; do
  found=0
  for derived in "${derived_prefixes[@]}"; do
    if [[ $derived == "$floor_prefix" ]]; then
      found=1
      break
    fi
  done
  if ((found == 0)); then
    printf 'prefix derivation lost the known /tmp test root %s (derived %d); refusing to report a result\n' \
      "$floor_prefix" "${#derived_prefixes[@]}" >&2
    exit 2
  fi
done
temp_patterns=()
for derived in "${derived_prefixes[@]}"; do
  temp_patterns+=("$derived"'*')
done

scan_find() {
  local label=$1
  shift
  local output
  if ! output=$(find "$@" 2>&1); then
    printf 'unable to complete %s scan: %s\n' "$label" "$output" >&2
    return 2
  fi
  printf '%s' "$output"
}

scan_known_temp_root() {
  local temp_root=$1
  local -a expression
  local first pattern output
  # `-H` follows a symlink named on the command line and nothing below it. On
  # macOS -- the only platform Gate A runs on -- `/tmp` IS a symlink to
  # `private/tmp`, and BSD `find` without `-H` does not descend through it: the
  # scan below matched the symlink itself, failed `-type d`, and returned empty
  # for every pattern on every run. This arm has therefore never observed a
  # leaked test root on the platform it was written for, while the comment at
  # its call site said "including macOS where /tmp resolves to /private/tmp".
  # A vanished root still errors out of `find`, which is what the
  # `--self-test-disappearing-temp-root` mode requires.
  expression=(-H "$temp_root" -maxdepth 1 -type d \()
  first=1
  for pattern in "${temp_patterns[@]}"; do
    if ((first == 0)); then
      expression+=(-o)
    fi
    expression+=(-name "$pattern")
    first=0
  done
  expression+=(\) -print)
  scan_find "known temp-root $temp_root" "${expression[@]}"
}

if [[ ${1:-} == --self-test-disappearing-temp-root ]]; then
  if (($# != 1)); then
    printf 'residue self-test accepts no additional arguments\n' >&2
    exit 2
  fi
  self_test_root=$(mktemp -d /tmp/pmux-residue-self-test.XXXXXXXX)
  rmdir "$self_test_root"
  if scan_known_temp_root "$self_test_root" >/dev/null 2>&1; then
    printf 'disappearing temp-root self-test unexpectedly passed\n' >&2
    exit 1
  fi
  printf 'Gate A residue disappearing-root self-test passed.\n'
  exit 0
fi
if (($# != 0)); then
  printf 'unknown residue-audit argument: %s\n' "$1" >&2
  exit 2
fi

candidate_input=${PMUX_E2E_BIN_DIR:?PMUX_E2E_BIN_DIR must name the exact candidate directory}

if [[ $candidate_input != /* || ! -d $candidate_input ]]; then
  printf 'invalid candidate directory: %s\n' "$candidate_input" >&2
  exit 2
fi
candidate_dir=$(cd "$candidate_input" && pwd -P)

# The executables scanned for process residue are DERIVED from the candidate
# directory, exactly as `tools/gate-a/run_gate.py:require_release_depinfo` derives
# the set it checks depinfo for, from the same directory by the same predicate.
# This list used to be the eight names below and `candidate_executables=%d` at the
# foot of this script printed `${#required_binaries[@]}` -- so the receipt's last
# line read as "we found eight executables" when it only ever meant "our literal
# has eight entries", and it would have printed 8 against a directory of twenty
# while scanning none of the other twelve for a surviving process.
#
# `FLOOR_BINARIES` is those eight, kept only as a LOWER bound, the same shape as
# the `FLOOR` of /tmp prefixes above: a directory that no longer holds a known
# candidate binary REFUSES rather than reporting a pass over a directory this
# audit no longer understands. It can only make the scan stricter, never looser.
FLOOR_BINARIES=(
  pmux
  pmuxd
  pmux-mcp
  claude-p
  pmux-rmuxd
  pmux-launcher
  pmux-hook
  pmux-test-claude
)

mapfile -t candidate_executables < <(
  find "$candidate_dir" -maxdepth 1 -type f -perm -u+x -print \
    | LC_ALL=C sort
)
if ((${#candidate_executables[@]} == 0)); then
  printf 'derived no executables from candidate directory %s; refusing to report a result\n' \
    "$candidate_dir" >&2
  exit 2
fi
for floor_name in "${FLOOR_BINARIES[@]}"; do
  found=0
  for executable in "${candidate_executables[@]}"; do
    if [[ ${executable##*/} == "$floor_name" ]]; then
      found=1
      break
    fi
  done
  if ((found == 0)); then
    printf 'candidate directory %s has no %s (derived %d executables); refusing to report a result\n' \
      "$candidate_dir" "$floor_name" "${#candidate_executables[@]}" >&2
    exit 2
  fi
done

failures=0
for executable in "${candidate_executables[@]}"; do
  matches=$(ps -axo pid=,command= | awk -v exact="$executable" '$2 == exact { print }')
  if [[ -n $matches ]]; then
    printf 'candidate process residue for %s:\n%s\n' "$executable" "$matches" >&2
    failures=$((failures + 1))
  fi
done

socket_roots=("$candidate_dir")
if [[ -d $source_root/.context ]]; then
  socket_roots=("$source_root/.context" "${socket_roots[@]}")
fi
if socket_paths=$(scan_find "workspace/candidate socket" "${socket_roots[@]}" -type s -print); then
  while IFS= read -r socket_path; do
    [[ -z $socket_path ]] && continue
    printf 'workspace/candidate socket residue: %s\n' "$socket_path" >&2
    failures=$((failures + 1))
  done <<<"$socket_paths"
else
  failures=$((failures + 1))
fi

if cache_paths=$(scan_find "generated Python/Ruff cache" "$source_root" \
    \( -path "$source_root/target" -o -path "$source_root/.git" \
       -o -path "$source_root/clients/typescript/node_modules" \) -prune -o \
    \( -type d \( -name .ruff_cache -o -name __pycache__ \) \
       -o -type f \( -name '*.pyc' -o -name '*.pyo' \) \) -print); then
  while IFS= read -r cache_path; do
    [[ -z $cache_path ]] && continue
    printf 'generated Python/Ruff cache residue: %s\n' "$cache_path" >&2
    failures=$((failures + 1))
  done <<<"$cache_paths"
else
  failures=$((failures + 1))
fi

if typescript_packages=$(scan_find "TypeScript package artifact" \
  "$source_root/clients/typescript" -maxdepth 1 -type f -name 'pmux-client-*.tgz' -print); then
  while IFS= read -r package_path; do
    [[ -z $package_path ]] && continue
    printf 'package-smoke artifact residue: %s\n' "$package_path" >&2
    failures=$((failures + 1))
  done <<<"$typescript_packages"
else
  failures=$((failures + 1))
fi

# Gate A compiles TypeScript and fuzz targets only into the separately fenced
# external validation root. These legacy source-tree output paths must be
# absent before candidate capture and remain absent throughout the gate; an
# ignored stale tree is not acceptable residue merely because source hashing
# excludes its contents.
for generated_path in \
  "$source_root/clients/typescript/dist" \
  "$source_root/fuzz/target" \
  "$source_root/fuzz/artifacts"; do
  if [[ -e $generated_path || -L $generated_path ]]; then
    printf 'source-tree generated-output residue: %s\n' "$generated_path" >&2
    failures=$((failures + 1))
  fi
done

if [[ -d $source_root/clients/python/dist ]]; then
  if python_packages=$(scan_find "Python package artifact" \
    "$source_root/clients/python/dist" -mindepth 1 -print); then
    while IFS= read -r package_path; do
      [[ -z $package_path ]] && continue
      printf 'package-smoke artifact residue: %s\n' "$package_path" >&2
      failures=$((failures + 1))
    done <<<"$python_packages"
  else
    failures=$((failures + 1))
  fi
fi

# Every named Gate A process/lifecycle fixture places its identifiable runtime
# directly below stable /tmp, including macOS where /tmp resolves to
# /private/tmp. Per-test Rust assertions own anonymous tempfile roots. Avoid an
# ambient per-process TMPDIR here: Conductor/CI may remove it asynchronously.
if known_temp_paths=$(scan_known_temp_root /tmp); then
  while IFS= read -r temp_path; do
    [[ -z $temp_path ]] && continue
    printf 'known pmux test-runtime residue: %s\n' "$temp_path" >&2
    failures=$((failures + 1))
  done <<<"$known_temp_paths"
else
  failures=$((failures + 1))
fi

if ((failures != 0)); then
  printf 'Gate A residue audit failed with %d finding(s).\n' "$failures" >&2
  exit 1
fi

printf 'Gate A residue audit passed.\n'
printf 'source_root=%s\n' "$source_root"
printf 'candidate_dir=%s\n' "$candidate_dir"
printf 'candidate_executables=%d\n' "${#candidate_executables[@]}"
