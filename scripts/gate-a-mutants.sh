#!/usr/bin/env bash
# Mutation coverage for the first-party crates where the leaks actually lived.
#
# The campaigns this replaces were done BY HAND: delete the guard, run the
# target, confirm red, restore, verify byte-exact. Thirty-one mutants on the
# pool, twenty-five on the adversarial fixes, twenty-two on the agent resource,
# fifteen on the health encoding, twelve on the waves -- every one of them a
# sample of a space a tool enumerates. This script is that tool, pinned.
#
# WHAT THE NUMBER MEANS, AND WHAT IT DOES NOT
#
# The score is (caught / (caught + missed)). `unviable` mutants -- ones that do
# not compile -- are excluded from BOTH sides, because a mutant the compiler
# rejects was never a test of the tests. `timeout` counts as caught: a mutant
# that makes the suite hang has been detected, just expensively.
#
# The score is a LOWER BOUND on the real one, for two reasons stated here so
# nobody reads it as tighter than it is:
#
#   * Only the test targets of the three packages in the `TEST_PACKAGES` array
#     below run -- a FIXED array, not an environment variable, printed into the
#     evidence on every run. (This line named `PMUX_MUTANTS_TEST_PACKAGES`,
#     which nothing in this tree reads or sets. "Configurable" and "three names
#     fixed in the script" are different claims, and the tell was mechanical:
#     every real `PMUX_MUTANTS_*` name occurs at least twice in this file, a
#     declaration and a use, and that one occurred once.) A mutant those three
#     miss may still be caught by `bin/pmuxd`'s or `bin/pmux`'s blackbox suites,
#     by `crates/e2e`, by the libFuzzer targets in `fuzz/` (`gate_b`), or by the
#     Python/TypeScript conformance lanes. Every one of those is a test that
#     exists and is not consulted here.
#   * `--profile mutants` is `dev` with debug info dropped, so `debug_assert!`
#     and overflow checks are ON, exactly as in `cargo test`. That direction is
#     MEASURED below by `assert_profile_properties_are_live`, which compiles a
#     probe under the profile and fires a `debug_assert!` and an overflow at it.
#     The manifest check beside it cannot substitute: `mutants` inherits `dev`,
#     `Cargo.toml` declares no `[profile.dev]`, and a `[profile.dev]` added
#     tomorrow with `debug-assertions = false` would leave every key of
#     `[profile.mutants]` exactly as this script demands and every
#     `debug_assert!` in the tree compiled out.
#
# WHY THE SCOPE IS A LIST OF FILES AND NOT `--workspace`
#
# `vendor/` is 643 of the 762 tracked `.rs` files and 311,685 of the 440,778
# tracked Rust lines -- 84.4% by file, 70.7% by line, from `git ls-files '*.rs'`
# -- and not ours. (This said "75% of the Rust", which is neither.) It is
# already outside the Cargo workspace, so cargo-mutants cannot see it -- but
# "cannot see it" is a fact about a manifest that a future `members` line can
# change silently, so this script ASSERTS the exclusion against the enumerated
# mutant list rather than inheriting it.

set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
readonly REPO_DIR
readonly REQUIRED_CARGO_MUTANTS_VERSION="cargo-mutants 27.1.0"
readonly MUTATION_PROFILE="mutants"
readonly CARGO_MUTANTS_BIN="${PMUX_CARGO_MUTANTS_BIN:-$REPO_DIR/.context/tools/cargo-mutants/bin/cargo-mutants}"
readonly EVIDENCE_ROOT="${PMUX_MUTANTS_EVIDENCE_ROOT:-$REPO_DIR/.context/gate-a-mutants-runs}"
# The committed disposition of every mutant this gate has left alive, and -- for
# the `full` scope -- the floor it ratchets against. ONE file for both, because a
# floor and the survivor list that justifies it drift apart the moment they are
# written in two places, and a floor set on a scope that excludes what fails it
# is the exact defect this tool exists to enumerate. See `evidence/README.md`.
readonly SURVIVOR_REGISTER="$REPO_DIR/evidence/mutation-survivor-register.json"
readonly REGISTER_TOOL="$REPO_DIR/scripts/mutation_register.py"
readonly JOBS="${PMUX_MUTANTS_JOBS:-4}"
readonly CARGO="${PMUX_MUTANTS_CARGO:?PMUX_MUTANTS_CARGO must name the exact pinned cargo binary}"
# DERIVED from the pinned cargo, and exported, because an unset `RUSTC` is not a
# missing option -- it is a second toolchain.
#
# MEASURED on this host: with `PMUX_MUTANTS_CARGO` bound to
# `~/.rustup/toolchains/1.88.0-*/bin/cargo` and `RUSTC` unset, cargo runs
# `rustc` from PATH. That is rustup's proxy, which resolves the toolchain from
# the CURRENT DIRECTORY -- so every workspace crate got 1.88.0 from
# `rust-toolchain.toml` and every registry crate, compiled from
# `~/.cargo/registry`, got the host default of 1.97.1. The candidate build died
# with `error[E0514]: found crate rmux_proto compiled by an incompatible version
# of rustc`, 1853 errors deep, before a single mutant ran. It fails loudly here
# and that is the only reason it is not worse.
#
# The sibling rule is `scripts/gate-a-fuzz.sh`'s, applied to the stable
# toolchain: cargo and rustc must be two files in one directory, and the version
# check below is what makes "sibling" mean "same toolchain" rather than "same
# folder".
RUSTC="$(dirname "$CARGO")/rustc"
readonly RUSTC
export RUSTC
readonly MINIMUM_SCORE="${PMUX_MUTANTS_MINIMUM_SCORE:?PMUX_MUTANTS_MINIMUM_SCORE must be the defended percentage floor}"
readonly SCOPE="${PMUX_MUTANTS_SCOPE:?PMUX_MUTANTS_SCOPE must name the file-glob scope: gate or full}"
readonly WORK_DIR="${PMUX_MUTANTS_WORK_DIR:?PMUX_MUTANTS_WORK_DIR must name the isolated directory this run may write}"

# The three packages whose test targets decide every mutant. Not `--workspace`:
# `crates/e2e` needs a real Claude and a built TypeScript client, and `bin/pmux`
# plus `bin/pmuxd` spend two and a half minutes per mutant re-testing binaries
# that the mutated crates are only a part of. Named here, printed into the
# evidence, and reported as the limit it is.
readonly TEST_PACKAGES=(pseudomux-protocol pseudomux-client pseudomux-service)

# The two candidate binaries `crates/service`'s process-level tests exec.
#
# They are built ONCE, outside the mutation loop, and handed to every mutant
# through `PMUX_TEST_BIN_DIR`. That is sound and not a shortcut: neither package
# depends on a crate this script mutates, so no mutant can change either binary,
# and `assert_candidates_carry_no_mutation` proves that from `cargo tree` rather
# than asserting it in a comment. Without them, `bounded_soak`,
# `lifecycle_faults`, `private_runtime` and `performance_diagnostics` fail in the
# unmutated baseline and no mutant is tested at all.
readonly CANDIDATE_BINARIES=(pmux-rmuxd pmux-launcher)

# Every first-party file the hand-run campaigns covered. BOTH scopes are built
# from this one list: `full` is it, and `gate` is it minus GATE_EXCLUDES. Two
# independently written lists would be free to drift, and the exclusions this
# script PRINTS are the set difference rather than a third copy -- a
# `scope_does_not_cover=` line that named `native.rs` during a `full` run would
# be the same defect the tool exists to enumerate, in the tool.
readonly FULL_GLOBS=(
  'crates/service/src/agent.rs'
  'crates/service/src/claude_launch.rs'
  'crates/service/src/pool/**'
  'crates/service/src/native.rs'
  'crates/service/src/driver_io.rs'
  'crates/protocol/src/**'
)

# The two the gate cell leaves out, and only for wall time: together they are
# 886 of the 1,588 mutants `full` enumerates.
#
# NAMING, because it is load-bearing: the scope value is `gate` and the cell is
# `mutation_score_agent_launch_pool_protocol`. Both were `admission` until the
# label was read back against the globs -- `native.rs` is excluded here, and
# `native.rs` is where `admit_bound_resources`, `admit_config_root`,
# `admit_cwd`, `claim_reaches` and `effective_config_root` are all declared. A
# number labelled "admission" that mutates no admission guard is exactly the
# defect this tool was installed to find.
readonly GATE_EXCLUDES=(
  'crates/service/src/native.rs'
  'crates/service/src/driver_io.rs'
)

# The `full` scope's floor is READ OUT OF THE REGISTER rather than written here.
# The register records the run that achieved the number, beside the disposition
# of every survivor that explains it, so this tree holds exactly one statement of
# the floor and it moves only when a new measurement is written down. It is the
# register's `floor_percent` and not its `mutation_score_percent`: this gate's
# error is one-directional -- a test that fails for its own reasons is recorded
# as the mutant being caught -- so the raw score is an over-estimate, and
# `floor_percent` is that same measurement with every mutant whose only catcher
# was a MEASURED drifter counted as missed.
register_floor_percent() {
  python3 - "$SURVIVOR_REGISTER" <<'PY'
import json
import sys

register = json.load(open(sys.argv[1], encoding="utf-8"))
recorded = register["recorded_at"]
if recorded["scope"] != "full":
    raise SystemExit(
        "the survivor register was recorded at scope %r, so it cannot supply the "
        "full scope's floor" % recorded["scope"]
    )
print(int(recorded["floor_percent"]))
PY
}

case "$SCOPE" in
  gate)
    # A defended constant, and not from the register: that file records a `full`
    # run and says nothing about what this scope has been measured at. 94 is the
    # number `docs/testing.md` defends against a measured 95.50%.
    SCOPE_FLOOR=94
    SCOPE_GLOBS=()
    for glob in "${FULL_GLOBS[@]}"; do
      excluded=0
      for skip in "${GATE_EXCLUDES[@]}"; do
        if [[ "$glob" == "$skip" ]]; then
          excluded=1
          break
        fi
      done
      ((excluded)) || SCOPE_GLOBS+=("$glob")
    done
    unset glob skip excluded
    if ((${#SCOPE_GLOBS[@]} != ${#FULL_GLOBS[@]} - ${#GATE_EXCLUDES[@]})); then
      echo "every GATE_EXCLUDES entry must name a FULL_GLOBS entry exactly; the \
gate scope came out at ${#SCOPE_GLOBS[@]} glob(s)" >&2
      exit 2
    fi
    ;;
  full)
    SCOPE_FLOOR="$(register_floor_percent)"
    SCOPE_GLOBS=("${FULL_GLOBS[@]}")
    ;;
  *)
    echo "PMUX_MUTANTS_SCOPE must be 'gate' or 'full' (got: $SCOPE)" >&2
    exit 2
    ;;
esac
readonly SCOPE_GLOBS

# What this run does NOT mutate, derived: `full` excludes nothing and prints
# nothing.
EXCLUDED_GLOBS=()
for glob in "${FULL_GLOBS[@]}"; do
  for used in "${SCOPE_GLOBS[@]}"; do
    if [[ "$glob" == "$used" ]]; then
      continue 2
    fi
  done
  EXCLUDED_GLOBS+=("$glob")
done
unset glob used
readonly EXCLUDED_GLOBS

if [[ ! "$MINIMUM_SCORE" =~ ^[0-9]+$ ]] || ((MINIMUM_SCORE > 100)); then
  echo "PMUX_MUTANTS_MINIMUM_SCORE must be an integer percentage (got: $MINIMUM_SCORE)" >&2
  exit 2
fi
if [[ ! "${SCOPE_FLOOR:-}" =~ ^[0-9]+$ ]] || ((SCOPE_FLOOR > 100)); then
  echo "the $SCOPE scope's floor did not resolve to an integer percentage (got: \
${SCOPE_FLOOR:-<unset>})" >&2
  exit 2
fi
readonly SCOPE_FLOOR
if ((MINIMUM_SCORE < SCOPE_FLOOR)); then
  echo "PMUX_MUTANTS_MINIMUM_SCORE=$MINIMUM_SCORE is below the $SCOPE scope's floor of \
$SCOPE_FLOOR%, which is what this scope has already been measured at. A caller may raise the \
floor and may not lower it: a gate re-pointed at a number the tree has already beaten reports \
green for a regression." >&2
  exit 2
fi
if [[ ! "$JOBS" =~ ^[1-9][0-9]*$ ]]; then
  echo "PMUX_MUTANTS_JOBS must be a positive integer (got: $JOBS)" >&2
  exit 2
fi
if [[ ! -x "$CARGO_MUTANTS_BIN" ]]; then
  echo "required isolated cargo-mutants binary is missing: $CARGO_MUTANTS_BIN" >&2
  exit 2
fi
if [[ "$CARGO" != /* || ! -x "$CARGO" ]]; then
  echo "pinned cargo must be one absolute executable: $CARGO" >&2
  exit 2
fi
if [[ ! -x "$RUSTC" ]]; then
  echo "pinned rustc must sit beside the pinned cargo: $RUSTC" >&2
  exit 2
fi
cargo_release="$("$CARGO" --version | awk '{print $2}')"
rustc_release="$("$RUSTC" --version | awk '{print $2}')"
if [[ "$cargo_release" != "$rustc_release" ]]; then
  echo "pinned cargo $cargo_release and pinned rustc $rustc_release are two toolchains; a mutation \
score measured across two compilers is a score for neither" >&2
  exit 2
fi
unset cargo_release rustc_release
if [[ "$WORK_DIR" != /* ]]; then
  echo "PMUX_MUTANTS_WORK_DIR must be absolute: $WORK_DIR" >&2
  exit 2
fi
if [[ "$("$CARGO_MUTANTS_BIN" mutants --version)" != "$REQUIRED_CARGO_MUTANTS_VERSION" ]]; then
  echo "cargo-mutants must be exactly $REQUIRED_CARGO_MUTANTS_VERSION" >&2
  "$CARGO_MUTANTS_BIN" mutants --version >&2
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
  echo "mutation evidence: $RUN_DIR"
  exit "$gate_exit_code"
}
trap finalize EXIT

mkdir -p "$WORK_DIR/bin" "$WORK_DIR/candidate-target" "$WORK_DIR/out"
chmod 700 "$WORK_DIR" "$WORK_DIR/bin" "$WORK_DIR/candidate-target" "$WORK_DIR/out"

export CARGO_TERM_COLOR=never
export CARGO_INCREMENTAL=0
export LC_ALL=C
unset RUSTFLAGS RUSTDOCFLAGS

cd "$REPO_DIR"

# ---------------------------------------------------------------------------
# The premises, each asserted rather than assumed
# ---------------------------------------------------------------------------

# 1. `[profile.mutants]` is `dev` with the debug info dropped and NOTHING ELSE.
#    A TABLE check, and only that: it says the profile differs from `dev` in one
#    stated way. It says nothing about what `dev` itself is -- premise 2 does.
assert_profile_is_dev_without_debuginfo() {
  python3 - "$REPO_DIR/Cargo.toml" "$MUTATION_PROFILE" <<'PY'
import sys

manifest = open(sys.argv[1], encoding="utf-8").read()
profile = sys.argv[2]
header = f"[profile.{profile}]"
if header not in manifest:
    raise SystemExit(f"Cargo.toml declares no {header}")
body = manifest.split(header, 1)[1].split("\n[", 1)[0]
settings = {}
for line in body.splitlines():
    line = line.split("#", 1)[0].strip()
    if not line:
        continue
    name, _, value = line.partition("=")
    settings[name.strip()] = value.strip()
expected = {"inherits": '"dev"', "debug": "false"}
if settings != expected:
    raise SystemExit(
        f"{header} must be exactly {expected}; it is {settings}. Any other key "
        "makes the mutation profile differ from `dev` in more than the one way "
        "this gate has defended, so a score measured under it is a score for a "
        "tree `cargo test` never runs. This check establishes NOTHING about "
        "debug-assertions or overflow-checks -- both are inherited from a "
        "`[profile.dev]` that need not exist, and "
        "assert_profile_properties_are_live is what measures them."
    )
PY
}

# 2. Under that profile a `debug_assert!` FIRES and an arithmetic overflow
#    PANICS. This is measured by compiling a probe with `--profile mutants` and
#    firing one of each at it (`crates/protocol/tests/mutation_profile.rs`), not
#    by reading `Cargo.toml`: reading the manifest is what produced the gap this
#    guard closes, because `[profile.mutants]` can be exactly right while a
#    `[profile.dev]` added beneath it turns both properties off.
#
#    PROFILE_PROPERTIES is the whole of the claim. The refusal below is BUILT
#    from it rather than written out, so this guard cannot name a property it
#    does not probe, and the probe's own report is compared against it so it
#    cannot probe fewer than it names either.
readonly PROFILE_PROPERTIES=(debug-assertions overflow-checks)
readonly PROFILE_PROBE_PACKAGE=pseudomux-protocol
readonly PROFILE_PROBE_TARGET=mutation_profile

assert_profile_properties_are_live() {
  local report="$WORK_DIR/profile-probe.txt"
  local expected observed
  rm -f "$report"
  if ! PMUX_PROFILE_PROBE_REPORT="$report" \
    CARGO_TARGET_DIR="$WORK_DIR/probe-target" \
    "$CARGO" test --locked --profile "$MUTATION_PROFILE" \
    --package "$PROFILE_PROBE_PACKAGE" --test "$PROFILE_PROBE_TARGET"; then
    echo "the --profile $MUTATION_PROFILE probe \
($PROFILE_PROBE_PACKAGE --test $PROFILE_PROBE_TARGET) did not pass; if it built, \
the assertion above names the property that is off, and if it did not, no \
mutant would have built either" >&2
    return 1
  fi
  expected="$(printf '%s\n' "${PROFILE_PROPERTIES[@]}" | LC_ALL=C sort)"
  observed="$(LC_ALL=C sort "$report" 2>/dev/null || true)"
  if [[ "$observed" != "$expected" ]]; then
    echo "the --profile $MUTATION_PROFILE probe reported [${observed//$'\n'/, }] \
live; this gate is asserted to measure with [${expected//$'\n'/, }] live. A \
score measured with any of those off counts every assertion in the tree that \
depends on it as a test that does not exist." >&2
    return 1
  fi
  printf 'profile_property_measured_live=%s\n' "${PROFILE_PROPERTIES[@]}"
}

# 3. The candidate binaries handed to every mutant cannot carry a mutation.
#    Derived from `cargo tree`, so a dependency edge added tomorrow -- which
#    would make the pinned copies stale and the score silently wrong -- fails
#    here instead of passing quietly.
assert_candidates_carry_no_mutation() {
  local mutated_packages=(pseudomux-service pseudomux-protocol)
  local candidate package tree
  for candidate in "${CANDIDATE_BINARIES[@]}"; do
    tree="$("$CARGO" tree --locked --package "$candidate" --edges normal --prefix none)"
    for package in "${mutated_packages[@]}"; do
      if printf '%s\n' "$tree" | grep -qE "^${package} "; then
        echo "candidate binary $candidate now depends on $package, so a prebuilt copy of it \
cannot be handed to a mutant; build it inside the mutation loop instead" >&2
        return 1
      fi
    done
  done
  return 0
}

# 4. Nothing under `vendor/` is mutated. Asserted against the ENUMERATED mutant
#    list, which is the only statement about vendor that a `members` line cannot
#    invalidate behind this script's back.
assert_no_vendor_mutants() {
  local listing=$1
  if grep -q '^vendor/' "$listing"; then
    echo "the mutant enumeration reached vendor/, which is 84% of this tree's Rust files and not ours" >&2
    grep '^vendor/' "$listing" | head -5 >&2
    return 1
  fi
  if ! grep -qE '^crates/(service|protocol)/src/' "$listing"; then
    echo "the mutant enumeration matched nothing under crates/{service,protocol}/src; a scope that \
silently stopped matching must refuse, not report a perfect score" >&2
    return 1
  fi
  return 0
}

run_logged() {
  local name=$1
  shift
  "$@" 2>&1 | tee "$RUN_DIR/$name.log"
}

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

run_logged profile assert_profile_is_dev_without_debuginfo
run_logged profile-properties assert_profile_properties_are_live
run_logged candidates assert_candidates_carry_no_mutation

CARGO_TARGET_DIR="$WORK_DIR/candidate-target" run_logged candidate-build \
  "$CARGO" build --locked "${CANDIDATE_BINARIES[@]/#/--package=}"
for candidate in "${CANDIDATE_BINARIES[@]}"; do
  cp "$WORK_DIR/candidate-target/debug/$candidate" "$WORK_DIR/bin/$candidate"
done
shasum -a 256 "$WORK_DIR/bin/"* >"$RUN_DIR/candidate-binaries.sha256"
export PMUX_TEST_BIN_DIR="$WORK_DIR/bin"

mutants_argv=(
  "$CARGO_MUTANTS_BIN" mutants
  --no-config
  --gitignore=true
  --copy-vcs=false
  --profile "$MUTATION_PROFILE"
  --test-workspace=false
  --no-times
  --output "$WORK_DIR/out"
  --jobs "$JOBS"
)
for glob in "${SCOPE_GLOBS[@]}"; do
  mutants_argv+=(--file "$glob")
done
for package in "${TEST_PACKAGES[@]}"; do
  mutants_argv+=(--test-package "$package")
done

{
  printf 'started_at_utc=%s\n' "$STARTED_AT"
  printf 'repo=%s\n' "$REPO_DIR"
  printf 'git_head=%s\n' "$(git -C "$REPO_DIR" rev-parse HEAD)"
  printf 'cargo_mutants_version=%s\n' "$("$CARGO_MUTANTS_BIN" mutants --version)"
  printf 'pinned_cargo=%s\n' "$CARGO"
  printf 'pinned_rustc=%s\n' "$RUSTC"
  printf 'mutation_profile=%s\n' "$MUTATION_PROFILE"
  printf 'scope=%s\n' "$SCOPE"
  printf 'minimum_score_percent=%s\n' "$MINIMUM_SCORE"
  printf 'scope_floor_percent=%s\n' "$SCOPE_FLOOR"
  printf 'survivor_register=%s\n' "$SURVIVOR_REGISTER"
  printf 'jobs=%s\n' "$JOBS"
  printf 'scope_glob=%s\n' "${SCOPE_GLOBS[@]}"
  printf 'test_package=%s\n' "${TEST_PACKAGES[@]}"
  printf 'candidate_binary=%s\n' "${CANDIDATE_BINARIES[@]}"
  uname -a
  "$CARGO" --version --verbose
  "$RUSTC" --version --verbose
} >"$RUN_DIR/metadata.txt"

git -C "$REPO_DIR" status --porcelain=v1 --untracked-files=all >"$RUN_DIR/git-status.txt"
shasum -a 256 "$CARGO_MUTANTS_BIN" >"$RUN_DIR/tool.sha256"

# ---------------------------------------------------------------------------
# Enumerate, then run
# ---------------------------------------------------------------------------

"${mutants_argv[@]}" --list >"$RUN_DIR/mutants-list.txt"
assert_no_vendor_mutants "$RUN_DIR/mutants-list.txt"
printf 'enumerated_mutants=%s\n' "$(wc -l <"$RUN_DIR/mutants-list.txt" | tr -d ' ')" \
  >>"$RUN_DIR/metadata.txt"

# The same enumeration in the machine-readable form, because the text listing
# drops the one field a currency check needs: the SPAN of the item each mutant
# lives in. Eighty-six seconds against a run measured in hours.
"${mutants_argv[@]}" --list --json >"$RUN_DIR/mutants-list.json"

set +e
"${mutants_argv[@]}" 2>&1 | tee "$RUN_DIR/mutants.log"
mutants_status=${PIPESTATUS[0]}
set -e

for artifact in outcomes.json missed.txt caught.txt timeout.txt unviable.txt; do
  if [[ -f "$WORK_DIR/out/mutants.out/$artifact" ]]; then
    cp "$WORK_DIR/out/mutants.out/$artifact" "$RUN_DIR/$artifact"
  fi
done

# `cargo mutants` exits non-zero WHENEVER any mutant survived, which is the
# thing this gate measures rather than the thing it refuses on. Only a run that
# never produced outcomes is a tool failure.
if [[ ! -f "$RUN_DIR/outcomes.json" ]]; then
  echo "cargo-mutants produced no outcomes.json (exit $mutants_status)" >&2
  exit 1
fi

# WHAT CAUGHT EACH MUTANT, distilled while the logs still exist. They do not
# survive: `log/` is 234 MB in a caller-supplied work directory that nothing here
# keeps, and the copy above takes five files that between them record THAT a
# mutant was caught and never BY WHAT. Without this, a register row saying
# KILLED cannot name the test whose deletion would make it false -- which is the
# hole `docs/register-currency.md` section 4.1 reproduces, and section 4.3 is why
# it has to be closed here rather than afterwards.
#
# Its status is CAPTURED and folded into the refusal at the end, for the reason
# the block below states: a distillation that fails must not abort the script
# before the score and the survivor list are printed. The evidence would be
# incomplete either way; only one of the two also throws away the measurement.
distillation_status=0
if [[ -d "$WORK_DIR/out/mutants.out/log" ]]; then
  python3 "$REGISTER_TOOL" catchers \
    --outcomes "$RUN_DIR/outcomes.json" \
    --logs "$WORK_DIR/out/mutants.out/log" \
    --run "$(basename "$RUN_DIR")" \
    --out "$RUN_DIR/catchers.json" >>"$RUN_DIR/metadata.txt" 2>&1 \
    || distillation_status=$?
else
  distillation_status=1
  echo "cargo-mutants kept no per-mutant logs, so nothing can say what caught \
each mutant; a register recorded from this run cannot carry \`caught_by\`" >&2
fi

# The enumeration census beside the register, for the same reason: 144 survivor
# rows cannot answer whether a mutant found later is one this campaign ever saw.
# Written for `full` only -- the gate scope is a subset and a census recorded
# from it would call every excluded mutant new.
if [[ "$SCOPE" == "full" ]]; then
  python3 "$REGISTER_TOOL" census \
    --enumeration "$RUN_DIR/mutants-list.json" \
    --repo "$REPO_DIR" \
    --head "$(git -C "$REPO_DIR" rev-parse HEAD)" \
    --scope full \
    --out "$RUN_DIR/enumeration-census.json" >>"$RUN_DIR/metadata.txt" 2>&1 \
    || distillation_status=$?
fi

# The register the run is judged against travels WITH the run, so the evidence
# says which dispositions were in force rather than which are in force today.
cp "$SURVIVOR_REGISTER" "$RUN_DIR/survivor-register.json"

# BOTH CHECKS RUN, AND THE REFUSAL IS AT THE END. A run that fails one of them
# still has to print the other: a score below the floor with no survivor list is
# a number nobody can act on, and a survivor list under a score nobody printed is
# a list nobody reads. `set -e` is why each status is captured rather than
# allowed to abort the script where it stands.
register_status=0
python3 "$REGISTER_TOOL" check \
  --outcomes "$RUN_DIR/outcomes.json" \
  --register "$SURVIVOR_REGISTER" >"$RUN_DIR/survivor-register.txt" 2>&1 \
  || register_status=$?
cat "$RUN_DIR/survivor-register.txt"

# `${a[@]+"${a[@]}"}` and not `"${a[@]}"`: under `set -u`, bash 3.2 -- which is
# what `/bin/bash` still is on macOS -- treats expanding an EMPTY array as an
# unbound variable and aborts. `EXCLUDED_GLOBS` is empty for `PMUX_MUTANTS_SCOPE=full`.
score_status=0
python3 - "$RUN_DIR/outcomes.json" "$MINIMUM_SCORE" "$SCOPE" \
  "${#SCOPE_GLOBS[@]}" "${SCOPE_GLOBS[@]}" ${EXCLUDED_GLOBS[@]+"${EXCLUDED_GLOBS[@]}"} <<'PY' \
  || score_status=$?
import json
import sys

outcomes = json.load(open(sys.argv[1], encoding="utf-8"))
minimum = int(sys.argv[2])
scope = sys.argv[3]
# The globs are printed BESIDE the number, every time, because a mutation score
# read without its scope is read as broader than it is -- which is the whole
# reason this cell is named for its files. The exclusions come from the script
# as a DIFFERENCE against the full list, so a `full` run prints none.
covered = int(sys.argv[4])
globs = sys.argv[5 : 5 + covered]
excluded = sys.argv[5 + covered :]
caught = outcomes["caught"] + outcomes["timeout"]
missed = outcomes["missed"]
decided = caught + missed
if decided == 0:
    raise SystemExit(
        "no mutant was decided; a scope that stopped matching must refuse, not "
        "report a perfect score"
    )
# Integer percent, rounded DOWN. A floor that a rounding rule can be argued
# past is not a floor.
score = (caught * 100) // decided
print(f"scope={scope}")
for glob in globs:
    print(f"scope_covers={glob}")
for glob in excluded:
    print(f"scope_does_not_cover={glob}")
print(f"enumerated={outcomes['total_mutants']} unviable={outcomes['unviable']}")
print(f"caught={caught} missed={missed} decided={decided}")
print(f"mutation_score_percent={score} minimum={minimum}")
if score < minimum:
    raise SystemExit(
        f"mutation score {score}% is below the floor {minimum}%: "
        f"{missed} of {decided} decided mutants survived every test that ran"
    )
PY

if ((register_status != 0)); then
  echo "the survivor register refused this run; the lines above name every mutant that \
survived without a disposition and every one the register calls closed. Nothing is closed by \
adding a row: write the disposition, or write the test." >&2
fi
if ((distillation_status != 0)); then
  echo "this run could not record WHAT CAUGHT each mutant, or could not census what it \
enumerated; see $RUN_DIR/metadata.txt. The score above stands -- those are two statements about \
the same run and only one of them failed -- but a survivor register recorded from this run would \
carry no \`caught_by\`, and criterion 1 refuses a register that carries none." >&2
fi
if ((register_status != 0 || score_status != 0 || distillation_status != 0)); then
  exit 1
fi
