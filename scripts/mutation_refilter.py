#!/usr/bin/env python3
"""Re-decide the mutants of named functions, and nothing else.

WHY THIS IS NOT A GATE CELL
---------------------------

`scripts/gate-a-mutants.sh` enumerates every mutant in `FULL_GLOBS` and takes
three hours sixteen minutes to do it. That is the sound measurement and it is
what a score may be read out of. This is the cheap one: it grades the mutants of
the functions it is handed, writes a receipt naming exactly those, and computes
NO percentage. The sentence
`evidence/mutation-filtered-run-native-seam.json` already carries in its
`argv_note` is the standing rule -- a filtered run is a tool result, not a gate
cell -- and it stays true here. A filtered run may keep
`evidence/mutation-survivor-register.json` current; nothing may read a mutation
score out of one.

WHERE ITS FRAME COMES FROM
--------------------------

Not from this file. The test packages, the candidate binaries, the mutation
profile, the required cargo-mutants version and the full glob set are READ OUT
OF `scripts/gate-a-mutants.sh` by [`gate_array`] and [`gate_scalar`], because a
second copy of those five declarations is a second thing to keep in step and the
first one to rot. If that script stops declaring one of them this refuses rather
than guessing.

THE TIMEOUT IS MEASURED HERE AND NOT INHERITED
----------------------------------------------

`cargo mutants` sizes its per-mutant timeout from ITS OWN baseline, and its
baseline is scoped to the packages of the mutated files while each mutant is
tested against every package in `--test-package`. Narrowing the files therefore
narrows the baseline without narrowing the work: measured in
`docs/register-currency.md` section 7, a 1.5 s baseline produced the 20 s floor
and two mutants reported `Timeout` that a 300 s timeout reports caught. A
timeout is a caught mutant to the score and a false one is a row promoted on
nothing. So this runs the three test packages once, unmutated, times them, and
pins `--timeout` at five times that -- `cargo mutants`' own multiplier, applied
to a baseline that is not narrower than the work.
"""

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import pathlib
import re
import shutil
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
sys.path.insert(
    0,
    str(pathlib.Path(__file__).resolve().parents[1] / "tools" / "evidence_common"),
)

import mutation_register  # noqa: E402 -- this script's own directory, resolved above
import portable_paths  # noqa: E402 -- tools/evidence_common, resolved above
import register_currency  # noqa: E402 -- this script's own directory, resolved above

CATCHING_TARGET = mutation_register.CATCHING_TARGET
MUTATION_GATE = "scripts/gate-a-mutants.sh"
RECEIPT_SCHEMA = "pmux.mutation-filtered-run.v1"
# `cargo mutants`' own rule for a timeout it was not given, restated because it
# is applied here to a different baseline: five times the unmutated test phase,
# never below twenty seconds.
TIMEOUT_MULTIPLIER = 5
TIMEOUT_FLOOR_SECONDS = 20
BASELINE_ATTEMPTS = 2
BUILD_TIMEOUT_SECONDS = 3600.0
# Escaped for a regex the `regex` crate parses, not Python's: only the
# metacharacters both engines agree on. Function names here carry `<`, `>`, `:`
# and spaces (`<impl TerminalControl for RmuxTerminalControl>::submit_prompt`),
# and every one of those is a literal in both, so escaping them is the thing
# that would break.
REGEX_METACHARACTERS = set("\\.+*?()|[]{}^$")


class RefilterError(RuntimeError):
    """The run could not be set up. Never a result; always exit 2."""


def regex_literal(text: str) -> str:
    return "".join(
        "\\" + character if character in REGEX_METACHARACTERS else character
        for character in text
    )


def gate_array(script: str, name: str) -> list[str]:
    """One `readonly NAME=( ... )` array out of the mutation gate.

    Both spellings that script uses: one line for the short ones and one entry a
    line for `FULL_GLOBS`. Matching only the shape the array happens to have
    today is how this would come back empty the day somebody reflows it.
    """

    block = re.search(rf"^readonly {name}=\((.*?)\)$", script, re.MULTILINE | re.DOTALL)
    if block is None:
        raise RefilterError(f"{MUTATION_GATE} no longer declares {name}")
    values = []
    for line in block.group(1).splitlines():
        for word in line.split("#", 1)[0].split():
            values.append(word.strip("'"))
    if not values:
        raise RefilterError(f"{MUTATION_GATE} declares an empty {name}")
    return values


def gate_scalar(script: str, name: str) -> str:
    match = re.search(rf'^readonly {name}="([^"]*)"$', script, re.MULTILINE)
    if match is None:
        raise RefilterError(f"{MUTATION_GATE} no longer declares {name}")
    return match.group(1)


def sha256_of(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def run(argv: list[str], *, cwd: pathlib.Path, env: dict, log: pathlib.Path):
    with open(log, "w", encoding="utf-8") as handle:
        return subprocess.run(
            argv,
            cwd=str(cwd),
            env=env,
            stdout=handle,
            stderr=subprocess.STDOUT,
            timeout=BUILD_TIMEOUT_SECONDS,
            check=False,
        )


def selectors(specifications: list[str]) -> tuple[list[str], list[tuple[str, str]]]:
    """`file::function` pairs, split into the `--file` set and the pairs."""

    pairs = []
    for specification in specifications:
        file, separator, function = specification.partition("::")
        if not separator or not file or not function:
            raise RefilterError(
                f"--function takes `path/to/file.rs::function_name`, not {specification!r}"
            )
        pairs.append((file, function))
    files = sorted({file for file, _ in pairs})
    return files, sorted(set(pairs))


def filter_expression(functions: list[str]) -> str:
    """A `-F` regex that selects at least the mutants of these functions.

    AT LEAST, and the word is measured. `cargo mutants --help` says `--re` is
    "matched against the names shown by `--list`", and the mutant name spells the
    function in one of two places -- `replace FUNC ...` for a mutant that
    replaces the function itself, `... in FUNC` for one that replaces something
    inside it -- so anchoring on those two is what keeps `pool` from selecting
    every function whose name contains it. But a filter built for `active_editor`
    alone came back with 35 mutants, six of them named

        ...:2039:17: delete field lifecycle_expected from struct TerminalEvidence
        expression in <impl TerminalControl for RmuxTerminalControl>::completion_evidence

    which contains no `active_editor` anywhere. Every one of the six is genre
    `StructField`; that genre is evidently not held to `--re` by this version of
    the tool. Over-selection is the safe direction -- extra mutants cost wall
    time and grade correctly -- so this is not worked around, it is RECORDED:
    the receipt carries `functions_reached_beyond_those_named`, and [`refilter`]
    refuses outright if the filter reaches FEWER functions than it was given,
    which is the direction that would make a receipt claim something it never
    graded.
    """

    alternatives = "|".join(regex_literal(function) for function in sorted(functions))
    return f"(?:replace |in )(?:{alternatives})(?: with | -> |$)"


def measured_baseline(
    repo: pathlib.Path,
    cargo: str,
    profile: str,
    packages: list[str],
    environment: dict,
    work: pathlib.Path,
) -> tuple[float, list[dict]]:
    """Seconds for one unmutated pass of every test package, built first.

    Attempted at most BASELINE_ATTEMPTS times, and every attempt recorded. An
    unmutated pass of this suite CAN fail for its own reasons: the register's own
    `floor_derivation` names four real-resource, wall-clock targets it has
    measured drifting, and the first attempt at writing this function was refused
    by one of them -- `bounded_soak`, which lost its private `rmux.sock` at cycle
    six with no mutant anywhere near it. Refusing on the first such failure makes
    the cheap path unusable; passing over it in silence is how a red tree gets
    graded. So each attempt goes in the receipt, named by the target that failed,
    and two consecutive failures are a red tree.
    """

    build = [cargo, "test", "--locked", "--no-run", f"--profile={profile}"]
    build += [f"--package={package}" for package in packages]
    done = run(build, cwd=repo, env=environment, log=work / "baseline-build.log")
    if done.returncode != 0:
        raise RefilterError(
            f"the unmutated baseline did not BUILD (exit {done.returncode}); see "
            f"{work / 'baseline-build.log'}. No mutant would have built either."
        )
    test = [cargo, "test", "--locked", f"--profile={profile}"]
    test += [f"--package={package}" for package in packages]
    attempts = []
    for attempt in range(1, BASELINE_ATTEMPTS + 1):
        log = work / f"baseline-test-{attempt}.log"
        started = dt.datetime.now(dt.timezone.utc)
        done = run(test, cwd=repo, env=environment, log=log)
        seconds = (dt.datetime.now(dt.timezone.utc) - started).total_seconds()
        text = log.read_text(encoding="utf-8", errors="replace")
        target = CATCHING_TARGET.search(text)
        attempts.append(
            {
                "attempt": attempt,
                "seconds": round(seconds, 3),
                "exit_status": done.returncode,
                "failing_target": target.group(1) if target else None,
            }
        )
        if done.returncode == 0:
            return seconds, attempts
    raise RefilterError(
        f"the unmutated baseline failed {BASELINE_ATTEMPTS} times "
        f"({', '.join(str(one['failing_target']) for one in attempts)}); see "
        f"{work}/baseline-test-*.log. Every mutant would have been reported caught "
        "by a suite that is already red."
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", required=True, type=pathlib.Path)
    parser.add_argument("--cargo", required=True)
    parser.add_argument("--cargo-mutants", required=True)
    parser.add_argument("--work-dir", required=True, type=pathlib.Path)
    parser.add_argument("--receipt", required=True, type=pathlib.Path)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument(
        "--what",
        default="",
        help="one sentence saying what this run is the receipt for",
    )
    parser.add_argument(
        "--function",
        action="append",
        default=[],
        dest="functions",
        metavar="FILE::NAME",
        help="repeat; the functions this run grades and the only ones it says anything about",
    )
    parser.add_argument(
        "--stale",
        action="store_true",
        help="grade exactly the functions the survivor register's currency check calls stale",
    )
    # The same default `scripts/path_b_done.py` gives criterion 4, because rule
    # 3(g) is that number and a second copy of it is how the two come apart.
    parser.add_argument("--max-receipt-age-days", type=float, default=14.0)
    arguments = parser.parse_args(argv)

    repo = arguments.repo.resolve()
    work = arguments.work_dir.resolve()
    try:
        return refilter(repo, work, arguments)
    except RefilterError as error:
        print(f"mutation-refilter: {error}", file=sys.stderr)
        return 2


def stale_specifications(
    repo: pathlib.Path, cargo_mutants: str, cargo: str, max_receipt_age_days: float
) -> list[str]:
    """The functions the currency check calls stale, as `file::name`.

    Derived, not typed. The refusal criterion 1 prints names a set and a command,
    and if the command took that set as arguments a reader would be copying a
    list from a refusal into a shell -- which is one transcription away from
    grading a different set from the one that was refused, and no way to tell.

    An escalation here is a refusal and not a smaller run: if the currency check
    has said a full-scope campaign is the only thing that can refresh the
    register, a filtered run that grades the stale functions anyway would produce
    a receipt asserting exactly what the escalation says nobody knows.
    """

    currency = register_currency.assess(
        repo,
        register_currency.git(repo, "rev-parse", "HEAD").strip(),
        register=mutation_register.load(repo / register_currency.SURVIVOR_REGISTER),
        census=mutation_register.load(repo / register_currency.ENUMERATION_CENSUS),
        now=dt.datetime.now(dt.timezone.utc),
        max_receipt_age_days=max_receipt_age_days,
        cargo_mutants=pathlib.Path(cargo_mutants),
        cargo=cargo,
    )
    if currency.escalations:
        raise RefilterError(
            "the register's currency check escalates to a FULL-scope run, so a "
            "filtered one cannot refresh it: " + "; ".join(currency.escalations)
        )
    for reason in currency.reasons:
        print(f"stale: {reason}")
    return [f"{file}::{name}" for file, name in currency.stale_functions]


def refilter(repo: pathlib.Path, work: pathlib.Path, arguments) -> int:
    script = (repo / MUTATION_GATE).read_text(encoding="utf-8")
    packages = gate_array(script, "TEST_PACKAGES")
    candidates = gate_array(script, "CANDIDATE_BINARIES")
    profile = gate_scalar(script, "MUTATION_PROFILE")
    required_version = gate_scalar(script, "REQUIRED_CARGO_MUTANTS_VERSION")

    specifications = list(arguments.functions)
    if arguments.stale:
        specifications = stale_specifications(
            repo,
            arguments.cargo_mutants,
            arguments.cargo,
            arguments.max_receipt_age_days,
        )
    if not specifications:
        raise RefilterError(
            "nothing to grade: pass --function FILE::NAME, or --stale when the "
            "currency check has named a stale set"
        )
    files, pairs = selectors(specifications)
    functions = sorted({function for _, function in pairs})

    cargo = shutil.which(arguments.cargo) or arguments.cargo
    if not os.path.isabs(cargo) or not os.access(cargo, os.X_OK):
        raise RefilterError(f"--cargo must be one absolute executable: {cargo}")
    rustc = str(pathlib.Path(cargo).parent / "rustc")
    if not os.access(rustc, os.X_OK):
        raise RefilterError(
            f"the pinned rustc must sit beside the pinned cargo: {rustc}"
        )
    mutants_bin = str(pathlib.Path(arguments.cargo_mutants).resolve())
    if not os.access(mutants_bin, os.X_OK):
        raise RefilterError(f"--cargo-mutants must be executable: {mutants_bin}")
    version = subprocess.run(
        [mutants_bin, "mutants", "--version"],
        capture_output=True,
        text=True,
        timeout=60,
    ).stdout.strip()
    if version != required_version:
        raise RefilterError(
            f"cargo-mutants must be exactly {required_version} (this is {version!r}); "
            f"{MUTATION_GATE} is what pins it and this borrows that pin"
        )
    for file in files:
        if not (repo / file).is_file():
            raise RefilterError(f"{file} is not a file in {repo}")

    work.mkdir(parents=True, exist_ok=True)
    (work / "bin").mkdir(exist_ok=True)
    environment = dict(
        os.environ,
        RUSTC=rustc,
        CARGO_TERM_COLOR="never",
        CARGO_INCREMENTAL="0",
        LC_ALL="C",
        CARGO_TARGET_DIR=str(work / "target"),
        PMUX_TEST_BIN_DIR=str(work / "bin"),
        PYTHONDONTWRITEBYTECODE="1",
    )
    for name in ("RUSTFLAGS", "RUSTDOCFLAGS"):
        environment.pop(name, None)
    # THE ONE VARIABLE cargo-mutants MAY NOT INHERIT, AND THE THREE HOURS IT COST.
    # `CARGO_TARGET_DIR` above is right for the two builds this script runs in the
    # repository itself. Handed to cargo-mutants it is a defect: that tool copies
    # the tree once PER WORKER and relies on each copy owning its own `target/`,
    # so one shared directory makes four workers fingerprint the same package
    # path into the same place. Cargo then reports `Fresh pseudomux-service` for a
    # mutant whose source it has just rewritten, and the mutant is graded against
    # the PREVIOUS one's binary -- MEASURED at 101 of 291 mutants, and three of
    # them reported CaughtMutant for a mutant that a hand-applied patch and 472
    # passing tests say survives (`docs/register-currency.md` section 9).
    # `scripts/gate-a-mutants.sh` never had this: it sets `CARGO_TARGET_DIR` for
    # its probe and its candidate build and for nothing else, and 0 of
    # `run.bbUDg3`'s 1,653 logs is missing its crate's `Compiling` line.
    mutants_environment = dict(environment)
    mutants_environment.pop("CARGO_TARGET_DIR", None)

    print(f"repo={repo}")
    print(f"work_dir={work}")
    print(f"graded_functions={len(functions)} in {len(files)} file(s)")

    # The two binaries `crates/service`'s process-level tests exec, built once
    # and handed to every mutant -- the same reason `scripts/gate-a-mutants.sh`
    # builds them: without `PMUX_TEST_BIN_DIR`, `bounded_soak`,
    # `lifecycle_faults`, `private_runtime` and `performance_diagnostics` fail
    # in the unmutated baseline and nothing is graded at all.
    build = [cargo, "build", "--locked"] + [f"--package={name}" for name in candidates]
    done = run(build, cwd=repo, env=environment, log=work / "candidate-build.log")
    if done.returncode != 0:
        raise RefilterError(
            f"the candidate binaries did not build (exit {done.returncode}); see "
            f"{work / 'candidate-build.log'}"
        )
    for candidate in candidates:
        shutil.copy2(work / "target" / "debug" / candidate, work / "bin" / candidate)

    baseline_seconds, baseline_attempts = measured_baseline(
        repo, cargo, profile, packages, environment, work
    )
    timeout = max(
        TIMEOUT_FLOOR_SECONDS, math.ceil(TIMEOUT_MULTIPLIER * baseline_seconds)
    )
    print(f"baseline_test_seconds={baseline_seconds:.1f}")
    print(f"timeout_seconds={timeout}")

    output = work / "out"
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)
    expression = filter_expression(functions)
    argv = [
        mutants_bin, "mutants",
        "--no-config",
        "--gitignore=true",
        "--copy-vcs=false",
        "--profile", profile,
        "--test-workspace=false",
        "--no-times",
        "--timeout", str(timeout),
        "--output", str(output),
        "--jobs", str(arguments.jobs),
    ]  # fmt: skip
    for file in files:
        argv += ["--file", file]
    argv += ["-F", expression]
    for package in packages:
        argv += ["--test-package", package]

    listing = subprocess.run(
        argv + ["--list", "--json"],
        cwd=str(repo),
        env=mutants_environment,
        capture_output=True,
        text=True,
        timeout=BUILD_TIMEOUT_SECONDS,
        check=False,
    )
    if listing.returncode != 0:
        raise RefilterError(
            f"the filtered enumeration failed: {listing.stderr.strip()[:400]}"
        )
    enumerated = json.loads(listing.stdout)
    reached = {
        (mutant["file"], (mutant.get("function") or {}).get("function_name") or "")
        for mutant in enumerated
    }
    # A `-F` regex over-matching is safe and under-matching is not: a function
    # the filter never reached is a function this run says nothing about, and a
    # receipt that names it would be the claim this whole tree exists to refuse.
    unreached = sorted(set(pairs) - reached)
    if unreached:
        raise RefilterError(
            "the filter reached no mutant for "
            + ", ".join(f"{file}::{name}" for file, name in unreached)
            + " -- either the function is gone, or it has no mutants, and either "
            "way this run cannot re-decide its rows"
        )
    extra = sorted(reached - set(pairs))
    print(f"enumerated={len(enumerated)}")
    print(f"functions_reached_beyond_those_named={len(extra)}")

    print(f"grading {len(enumerated)} mutant(s) at {arguments.jobs} job(s)")
    graded = run(argv, cwd=repo, env=mutants_environment, log=work / "mutants.log")
    outcomes_path = output / "mutants.out" / "outcomes.json"
    if not outcomes_path.is_file():
        raise RefilterError(
            f"cargo-mutants produced no outcomes.json (exit {graded.returncode}); see "
            f"{work / 'mutants.log'}"
        )

    outcomes = mutation_register.load(outcomes_path)
    logs = output / "mutants.out" / "log"
    # EVERY GRADED MUTANT'S OWN CRATE WAS COMPILED, CHECKED AND NOT ASSUMED. The
    # package is derived from the mutated path against the workspace manifest,
    # so nothing here is a list of crate names to keep in step. A run that got
    # this wrong once produced a receipt naming a catching test for a mutant the
    # tests never saw, which is the exact claim this whole tree exists to refuse,
    # so it is a refusal and not a note -- and the outcomes are still on disk in
    # the work directory for whoever has to work out why.
    directories = {
        str(directory.relative_to(repo)): name
        for name, directory in register_currency.workspace_packages(repo).items()
    }
    unbuilt = []
    for outcome in outcomes["outcomes"]:
        scenario = outcome.get("scenario")
        if not isinstance(scenario, dict) or "Mutant" not in scenario:
            continue
        mutant = scenario["Mutant"]
        package = mutation_register.package_of(mutant["file"], directories)
        log = logs / pathlib.Path(outcome.get("log_path") or "").name
        if (
            package is None
            or not log.is_file()
            or not mutation_register.rebuilt_in(
                log.read_text(encoding="utf-8", errors="replace"), package
            )
        ):
            unbuilt.append(mutant["name"])
    if unbuilt:
        raise RefilterError(
            f"{len(unbuilt)} of {outcomes['total_mutants']} mutant(s) were graded "
            "without their own crate being compiled, so each was tested against the "
            f"previous mutant's binary -- the first is {unbuilt[0]}. See "
            f"{logs} and {work / 'mutants.log'}; no receipt was written."
        )
    distilled = {}
    for outcome in outcomes["outcomes"]:
        scenario = outcome.get("scenario")
        if not isinstance(scenario, dict) or "Mutant" not in scenario:
            continue
        if outcome.get("summary") not in ("CaughtMutant", "Timeout"):
            continue
        log = logs / pathlib.Path(outcome.get("log_path") or "").name
        found = (
            mutation_register.catcher_in(
                log.read_text(encoding="utf-8", errors="replace")
            )
            if log.is_file()
            else None
        )
        distilled[scenario["Mutant"]["name"]] = found or {
            "test": None,
            "target": None,
            "undetermined": (
                "timeout"
                if outcome.get("summary") == "Timeout"
                else "no-failing-target-in-log"
            ),
        }

    by_summary = {}
    for outcome in outcomes["outcomes"]:
        scenario = outcome.get("scenario")
        if not isinstance(scenario, dict) or "Mutant" not in scenario:
            continue
        by_summary.setdefault(outcome["summary"], []).append(scenario["Mutant"]["name"])

    commit = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    ).stdout.strip()
    dirty = subprocess.run(
        ["git", "-C", str(repo), "status", "--porcelain"],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    ).stdout.strip()

    receipt = {
        "schema": RECEIPT_SCHEMA,
        "what": arguments.what
        or (
            "A FILTERED cargo-mutants run over the functions named below, and nothing "
            "else. It is NOT a score: it enumerates only the mutants of those "
            "functions, so no percentage is computed here and none may be read out of it."
        ),
        "argv": argv,
        "argv_note": (
            f"Not `{MUTATION_GATE}`. It borrows that script's pinned toolchain, "
            "`mutants` profile, test-package set, candidate binaries and required "
            "cargo-mutants version -- all READ OUT OF it rather than copied here -- "
            "and skips its evidence directory, its floor and its register check, "
            "because the register check is a statement about a full-scope run. This "
            "is a tool result, not a gate cell."
        ),
        "commit": commit,
        "working_tree": "dirty" if dirty else "clean",
        "graded_functions": [f"{file}::{name}" for file, name in pairs],
        "functions_reached_beyond_those_named": [
            f"{file}::{name}" for file, name in extra
        ],
        "filter_expression": expression,
        "baseline": {
            "test_packages": packages,
            "unmutated_test_seconds": round(baseline_seconds, 3),
            "attempts": baseline_attempts,
            "timeout_seconds": timeout,
            "timeout_note": (
                f"{TIMEOUT_MULTIPLIER}x the unmutated pass of every test package, floor "
                f"{TIMEOUT_FLOOR_SECONDS}s. Pinned rather than left to cargo-mutants, "
                "whose own baseline is scoped to the mutated files' packages while every "
                "mutant is tested against all of them."
            ),
        },
        "counts": {
            "total_mutants": outcomes["total_mutants"],
            "caught": outcomes["caught"],
            "missed": outcomes["missed"],
            "timeout": outcomes["timeout"],
            "unviable": outcomes["unviable"],
        },
        "caught": sorted(by_summary.get("CaughtMutant", [])),
        "missed": sorted(by_summary.get("MissedMutant", [])),
        "timeout": sorted(by_summary.get("Timeout", [])),
        "unviable": sorted(by_summary.get("Unviable", [])),
        "catchers": dict(sorted(distilled.items())),
        "catchers_note": (
            "The first target `cargo test` stopped at and the first test named in it. "
            "Sound for invalidating a register row and for nothing else: the log names "
            "A catcher, not every catcher."
        ),
        "graded_sources": {file: sha256_of(repo / file) for file in sorted(files)},
        "graded_sources_note": (
            "The run copies the source tree when it starts, so what it graded is these "
            "bytes and not a commit -- the commit above is this repository's HEAD when "
            "the receipt was written. `shasum -a 256` each path at that commit to check "
            "the two are the same tree."
        ),
        "recorded_at": {
            "date": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d"),
            "start_time": outcomes["start_time"],
            "end_time": outcomes["end_time"],
            "cargo_mutants_version": outcomes["cargo_mutants_version"],
            "host": f"{os.uname().sysname} {os.uname().machine}",
            "toolchain": subprocess.run(
                [rustc, "--version"],
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            ).stdout.strip(),
        },
    }
    # `argv[0]` is the pinned cargo-mutants beside the workspace, so this
    # receipt names the checkout it ran in. Rendered whole at the point it
    # becomes bytes: see `tools/evidence_common/portable_paths.py`.
    receipt = portable_paths.render_document(receipt)
    arguments.receipt.parent.mkdir(parents=True, exist_ok=True)
    with open(arguments.receipt, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    for key, value in receipt["counts"].items():
        print(f"{key}={value}")
    print(f"receipt={arguments.receipt}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
