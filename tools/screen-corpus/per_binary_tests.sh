#!/usr/bin/env bash
# Runs every test binary in the workspace ISOLATED, one `cargo test` invocation
# each, and reports a line per binary.
#
# Why per binary and not one aggregate: the process-spawning blackbox binaries in
# this workspace are load-sensitive, and the same `cargo test --workspace` has
# produced 845/27, 859/13 and 872/0 on this host. An aggregate total is therefore
# not a stable number and chasing one is chasing load. A per-binary line says
# which binary was unstable, which an aggregate cannot.
#
# Targets come from `cargo metadata`, not from a list in this file. The first
# version enumerated from the source tree, which was already better than nothing
# -- but it enumerated a HAND-WRITTEN array of six packages, and this workspace
# has thirteen. Every `bin/` package was silently absent, so "every one of the N
# test targets passed" was a true sentence about a set that did not include
# `pmux`, `pmuxd`, `pmux-mcp`, `pmux-hook`, `pmux-launcher` or
# `pmux-rmuxd`. That is the same defect this script's own header warns about, one
# level up: a report whose scope is narrower than its sentence.
#
# The member cross-check below is the fix's proof. `cargo metadata` yielding a
# short list would silently narrow the report again, so the count is compared
# against the `members` array in the root manifest and a mismatch REFUSES rather
# than reports.
#
# And then it had the defect again, one level DOWN. Enumerating every target was
# only half the scope: each target ran under a plain `cargo test`, which does not
# run `#[ignore]`d tests. The last report printed "every one of the 61 test
# targets passed in isolation" while 49 test cases never executed -- among them
# all nineteen of `pseudomux-e2e --test pool_concurrency`, which reported
# `0 passed; 0 failed; 19 ignored` and is where the only real failure lived. The
# sentence was true of the targets and false of the tests. So: `--include-ignored`
# on every target, the executed and skipped CASE counts accumulated from the same
# `test result:` lines the table prints, and a coverage claim that is printed only
# when those counts prove it. A target that emits no `test result:` line at all is
# a target whose scope is unknown, and unknown scope REFUSES.
#
# And a THIRD time, latent rather than active: the kind-to-selector mapping ended
# in a bare `continue`, so a target kind nobody had classified -- an `example`, a
# `bench`, a `proc-macro` crate's library -- was dropped from the set the footer
# then called "every one of the N test targets". `cargo test --all-targets` in
# the gate would run those; this would not. The mapping below is now a table with
# a reason per entry and no default, and an unclassified kind REFUSES. It changes
# nothing today: this workspace is 47 test + 8 bin + 6 lib targets, the same 61.
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR" || exit 2

# Run one target at a time via `cargo test -p <pkg> --<target>`. Necessary rather
# than stylistic: invoking the executables directly from the workspace root gives
# spurious `NotFound` fixture failures in `pseudomux-claude`, whose tests read
# `tests/fixtures/...` relative to the manifest dir.
METADATA="$(cargo metadata --offline --no-deps --format-version 1 2>/dev/null)"
if [[ -z "$METADATA" ]]; then
  printf 'cargo metadata produced nothing; refusing to report a result\n' >&2
  exit 2
fi

DECLARED_MEMBERS="$(python3 - <<'PY'
import pathlib, re
manifest = pathlib.Path("Cargo.toml").read_text(encoding="utf-8")
block = re.search(r"^members\s*=\s*\[(.*?)\]", manifest, re.S | re.M)
print(0 if block is None else len(re.findall(r'"[^"]+"', block.group(1))))
PY
)"

mapfile -t TARGETS < <(printf '%s' "$METADATA" | python3 -c '
import json, sys

# Every target kind cargo metadata can emit, and what this report does with
# it. The third time the same defect: the first version listed six packages of
# thirteen, the second ran every target without --include-ignored, and this
# one ended if/elif with a bare continue -- so a kind nobody had thought
# about was DROPPED from a table whose footer says "every one of the N test
# targets passed". Today the workspace has 47 test, 8 bin and 6 lib targets and
# nothing else, so the drop is latent rather than active; the day someone adds
# an examples/ file with a #[test] in it, --all-targets in the gate would
# run it and this report would not, while still printing that sentence.
#
# So: a table with a reason per entry and NO default. proc-macro is here on
# purpose -- a proc-macro crate reports that kind and not lib, and the old
# "lib" in kinds test would have dropped its unit tests silently.
SELECTORS = {
    "lib": ("--lib", "(unit)", "the crate library, and its #[cfg(test)] module"),
    "rlib": ("--lib", "(unit)", "a library by another linkage spelling"),
    "dylib": ("--lib", "(unit)", "a library by another linkage spelling"),
    "cdylib": ("--lib", "(unit)", "a library by another linkage spelling"),
    "staticlib": ("--lib", "(unit)", "a library by another linkage spelling"),
    "proc-macro": ("--lib", "(unit)", "a proc-macro crate reports this, not lib"),
    "bin": ("--bin", "bin", "an executable, and its #[cfg(test)] module"),
    "test": ("--test", "", "an integration test target"),
    "bench": ("--bench", "bench", "cargo test --bench runs its #[test]s"),
    "example": ("--example", "example", "cargo test --example runs its #[test]s"),
}
# Kinds that are deliberately NOT test targets, each with the reason.
EXCLUDED = {
    "custom-build": "a build script is not a test target; it has already run",
}

metadata = json.load(sys.stdin)
packages = sorted(metadata["packages"], key=lambda package: package["name"])
print("#packages\t%d" % len(packages))
for package in packages:
    for target in package["targets"]:
        kinds = [kind for kind in target["kind"] if kind not in EXCLUDED]
        if not kinds:
            continue
        unknown = [kind for kind in kinds if kind not in SELECTORS]
        if unknown:
            sys.exit(
                "target %s/%s has kind(s) %s that this report has never "
                "classified, so its scope is unknown; add them to SELECTORS or "
                "EXCLUDED rather than letting the footer speak for them"
                % (package["name"], target["name"], unknown)
            )
        flag, prefix, _why = SELECTORS[kinds[0]]
        if flag == "--lib":
            selector, label = flag, prefix
        else:
            selector = "%s %s" % (flag, target["name"])
            label = ("%s %s" % (prefix, target["name"])).strip()
        print("%s\t%s\t%s" % (package["name"], selector, label))
')

FOUND_PACKAGES=0
FILTERED=()
for entry in "${TARGETS[@]}"; do
  if [[ "$entry" == '#packages'* ]]; then
    FOUND_PACKAGES="${entry#*$'\t'}"
    continue
  fi
  FILTERED+=("$entry")
done
TARGETS=("${FILTERED[@]}")

if [[ "$FOUND_PACKAGES" -ne "$DECLARED_MEMBERS" ]]; then
  printf 'cargo metadata reported %s workspace packages, the root manifest declares %s; refusing to report a result\n' \
    "$FOUND_PACKAGES" "$DECLARED_MEMBERS" >&2
  exit 2
fi

if [[ ${#TARGETS[@]} -eq 0 ]]; then
  printf 'enumerated no test targets; refusing to report a result\n' >&2
  exit 2
fi

# Build EVERY target once, before the loop. MEASURED on this host: without it,
# the first eight targets took fifty minutes and the whole workspace then built
# in two minutes forty. The per-target `cargo test` invocations were each
# linking and first-executing a fresh binary, and macOS stalls in dyld the first
# time it runs one. The loop below still runs one `cargo test` per target --
# that is what makes the results ISOLATED -- but each is now a freshness check
# and a test run rather than a build.
printf 'building every test target once, so the per-target timings are test time\n'
if ! cargo test --offline --workspace --all-targets --no-run >/dev/null 2>&1; then
  printf 'the workspace does not build; refusing to report a result\n' >&2
  exit 2
fi

# `pseudomux-e2e` validates SHIPPED binaries and refuses to validate whatever
# cargo happened to build: `full_stack.rs::CandidateBinaries::from_environment`
# `.expect`s PMUX_E2E_BIN_DIR to name an exact candidate directory. Without it
# SEVEN of that target's ten cases panic with `PMUX_E2E_BIN_DIR must identify the
# exact candidate directory`, one of the 61 targets is permanently red on a
# healthy tree, and this report can never print its own success sentence -- a
# report that cannot say the true thing is the same defect as one that says a
# false thing. MEASURED before this was supplied: `1 of 61 targets failed in
# isolation: pseudomux-e2e/full_stack`, `3 passed; 7 failed`.
#
# The directory is DERIVED, from cargo's own target directory and the profile
# `cargo test` builds into, and the executables that must be in it are DERIVED
# from the workspace's own `bin` targets. A literal list of eight names here is
# exactly the shape of defect this file's header records twice.
if [[ -z "${PMUX_E2E_BIN_DIR:-}" ]]; then
  CARGO_TARGET_ROOT="$(printf '%s' "$METADATA" | python3 -c '
import json, sys
print(json.load(sys.stdin)["target_directory"])
')"
  PMUX_E2E_BIN_DIR="$CARGO_TARGET_ROOT/debug"
fi
if [[ ! -d "$PMUX_E2E_BIN_DIR" ]]; then
  printf 'candidate directory %s does not exist; refusing to report a result\n' \
    "$PMUX_E2E_BIN_DIR" >&2
  exit 2
fi
mapfile -t REQUIRED_EXECUTABLES < <(printf '%s' "$METADATA" | python3 -c '
import json, sys
metadata = json.load(sys.stdin)
for package in metadata["packages"]:
    for target in package["targets"]:
        if "bin" in target["kind"]:
            print(target["name"])
' | LC_ALL=C sort)
if ((${#REQUIRED_EXECUTABLES[@]} == 0)); then
  printf 'derived no bin targets from cargo metadata; refusing to report a result\n' >&2
  exit 2
fi
MISSING=()
for name in "${REQUIRED_EXECUTABLES[@]}"; do
  [[ -x "$PMUX_E2E_BIN_DIR/$name" ]] || MISSING+=("$name")
done
if ((${#MISSING[@]} != 0)); then
  printf 'candidate directory %s is missing %d of %d workspace executables (%s); refusing to report a result\n' \
    "$PMUX_E2E_BIN_DIR" "${#MISSING[@]}" "${#REQUIRED_EXECUTABLES[@]}" "${MISSING[*]}" >&2
  exit 2
fi
export PMUX_E2E_BIN_DIR

# The OTHER precondition, named UP FRONT rather than discovered as a red target
# forty minutes in. `PMUX_E2E_TYPESCRIPT_DIST_DIR` is a staged artifact with a
# mode contract -- `dist-stage.mjs` refuses a stage whose root is not 0700 and
# `tsc` must write into it under `umask 077` -- so unlike the candidate
# directory it cannot be derived, only supplied. Gate A cell
# `release_full_stack_e2e` supplies it; this script does not stage one, and
# without it exactly one case fails:
# `full_stack::all_v1_methods_use_the_real_public_and_private_process_boundaries`
# with `PMUX_E2E_TYPESCRIPT_DIST_DIR is required for cross-client E2E`. Said
# here, by name, so the footer's refusal is legible rather than mysterious.
if [[ -z "${PMUX_E2E_TYPESCRIPT_DIST_DIR:-}" ]]; then
  printf 'PRECONDITION ABSENT: PMUX_E2E_TYPESCRIPT_DIST_DIR is unset, so\n'
  printf '  pseudomux-e2e/full_stack will fail one cross-client case and this run\n'
  printf '  will NOT claim isolation coverage. Export a staged, 0700 TypeScript\n'
  printf '  dist directory to close it (docs/testing.md, the validation root).\n\n'
fi

printf 'enumerated %d test targets across %s workspace packages\n' "${#TARGETS[@]}" "$FOUND_PACKAGES"
printf 'candidate directory %s holds all %d workspace executables\n\n' \
  "$PMUX_E2E_BIN_DIR" "${#REQUIRED_EXECUTABLES[@]}"
printf '%-22s %-42s %s\n' PACKAGE TARGET RESULT
printf '%s\n' "-------------------------------------------------------------------------------------------------"

# Sum `passed`, `failed` and `ignored` over EVERY `test result:` line a target
# emitted, and report how many lines that was. Zero lines means the target ran no
# libtest harness at all, which is the one outcome that must never be folded into
# a pass.
count_cases() {
  awk '
    /^test result:/ {
      for (field = 1; field < NF; field++) {
        if ($(field + 1) ~ /^passed;?$/) passed += $field
        else if ($(field + 1) ~ /^failed;?$/) failed += $field
        else if ($(field + 1) ~ /^ignored;?$/) ignored += $field
      }
      lines++
    }
    END { printf "%d %d %d %d", lines + 0, passed + 0, failed + 0, ignored + 0 }
  '
}

FAILED=()
UNREPORTED=()
SKIPPED=()
EXECUTED_CASES=0
SKIPPED_CASES=0
for entry in "${TARGETS[@]}"; do
  IFS=$'\t' read -r package selector label <<<"$entry"
  # `--include-ignored` is the SCOPE of this report, not a convenience: without
  # it an `#[ignore]`d test is counted as present and never run.
  # shellcheck disable=SC2086 # the selector is two words on purpose
  output="$(cargo test --offline -p "$package" $selector -- --include-ignored 2>&1)"
  status=$?
  read -r result_lines target_passed target_failed target_ignored \
    < <(printf '%s\n' "$output" | count_cases)
  EXECUTED_CASES=$((EXECUTED_CASES + target_passed + target_failed))
  SKIPPED_CASES=$((SKIPPED_CASES + target_ignored))
  summary="$(printf '%s\n' "$output" | grep -E '^test result:' | tail -1)"
  summary="${summary:-<no test result line>}"
  if [[ $result_lines -eq 0 ]]; then
    UNREPORTED+=("$package/$label")
    printf '%-22s %-42s UNREPORTED -- %s\n' "$package" "$label" "$summary"
    continue
  fi
  if [[ $target_ignored -ne 0 ]]; then
    SKIPPED+=("$package/$label ($target_ignored)")
  fi
  if [[ $status -eq 0 ]]; then
    printf '%-22s %-42s %s\n' "$package" "$label" "$summary"
  else
    printf '%-22s %-42s FAILED -- %s\n' "$package" "$label" "$summary"
    FAILED+=("$package/$label")
  fi
done

printf '\n'
if [[ ${#FAILED[@]} -ne 0 ]]; then
  printf '%d of %d targets failed in isolation:\n' "${#FAILED[@]}" "${#TARGETS[@]}"
  printf '  %s\n' "${FAILED[@]}"
fi
if [[ ${#UNREPORTED[@]} -ne 0 ]]; then
  printf '%d of %d targets emitted no test result line, so their scope is unknown:\n' \
    "${#UNREPORTED[@]}" "${#TARGETS[@]}"
  printf '  %s\n' "${UNREPORTED[@]}"
fi
if [[ ${#SKIPPED[@]} -ne 0 ]]; then
  printf '%d test cases did not run even under --include-ignored:\n' "$SKIPPED_CASES"
  printf '  %s\n' "${SKIPPED[@]}"
fi

# The claim is printed only when the counts above earn it. Anything else says
# what is NOT known, and never the sentence that would be read as full coverage.
if [[ ${#UNREPORTED[@]} -ne 0 || $SKIPPED_CASES -ne 0 ]]; then
  printf 'scope incomplete: %d test cases ran across %d of %d targets; NOT claiming isolation coverage\n' \
    "$EXECUTED_CASES" "$((${#TARGETS[@]} - ${#UNREPORTED[@]}))" "${#TARGETS[@]}"
  exit 2
fi
if [[ ${#FAILED[@]} -ne 0 ]]; then
  printf 'scope complete: %d test cases ran across all %d targets, %d target(s) failed; NOT claiming isolation coverage\n' \
    "$EXECUTED_CASES" "${#TARGETS[@]}" "${#FAILED[@]}"
  exit 1
fi
# The counts are IN the sentence, not implied by the guard above it: a claim that
# says "none ignored" because control reached it is the same shape of claim this
# script exists to stop making.
printf 'every one of the %d test targets passed in isolation: %d test cases ran, %d ignored\n' \
  "${#TARGETS[@]}" "$EXECUTED_CASES" "$SKIPPED_CASES"
