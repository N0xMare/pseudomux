#!/usr/bin/env python3
"""The mutation-survivor register: read it, or hold a run against it.

WHAT THE REGISTER IS FOR
------------------------

A mutation score is one number and it hides the thing that matters: WHICH
mutants survived, and whether anybody has ever looked at them. The standing
complaint this file answers is that survivors get found faster than they get
closed, and that the same rows get rediscovered because nothing records that
they were already worked. So every survivor of the full-scope run carries
exactly one disposition -- KILLED, EQUIVALENT, ACCEPTED or REMOVED -- and this
tool refuses a run that produces a survivor the register does not hold.

WHY THE KEY IS NOT `file:line:column`
-------------------------------------

That is how `cargo mutants` names a mutant and it is the one thing about a
mutant that is guaranteed to rot: adding a test above a function moves every
line below it, and adding a test is exactly how a survivor gets closed. The key
here is (file, function, genre, replacement, occurrence), where `occurrence`
orders the mutants that agree on the first four by the position the tool
reported. Nothing in that tuple moves when a test is added, and two mutants of
the same shape in the same function are still told apart -- which is what makes
a NEW survivor inside an already-accepted function visible instead of absorbed.

The line and column ARE recorded, under `observed_at`, as a reader's aid and
never as identity. `check` reports how many have drifted so the file can be
refreshed; it never fails on drift.

WHY A KILLED ROW NAMES THE TEST THAT KILLED IT
----------------------------------------------

A KILLED row is a claim about two pieces of the tree, not one: the code the
mutant patches, and THE TEST THAT DECIDED IT. Delete or weaken that test and the
row's KILLED is simply false, with nothing in the tree saying so --
`docs/register-currency.md` section 4.1 reproduces exactly that, in a throwaway
clone, with the done-gate reporting criterion 1 MET over a row that had stopped
being true. So every KILLED row carries `caught_by`, and `validate` refuses a
register where one does not.

KILLED and not every closed row. REMOVED is the other closed disposition and it
asserts something no test decided -- that the mutant is no longer ENUMERATED --
so asking it to name a catcher would be asking it to name a test that never ran.
Its currency is the code's, not a test's.

`caught_by` is recorded by `catchers`, which distils it from the per-mutant logs
of the run that decided the mutant while those logs still exist: `cargo test`
without `--no-fail-fast` stops at the first target that fails, so each caught
mutant's log ends with the failing test names and the `-p PKG --test NAME`
argument that reruns them. A mutant decided by a TIMEOUT names neither, and its
row records `undetermined` rather than a guess -- which the currency check reads
as "stale on a change to any file of any test package", the only safe reading of
"nobody knows what caught this".

WHAT IT DELIBERATELY DOES NOT DO
--------------------------------

It does not fail on a survivor that has since been CAUGHT. Those are reported
as `retired` so the entry can be pruned, because refusing them would make
closing a survivor break the gate.
"""

import argparse
import json
import pathlib
import re
import sys
import tomllib

sys.path.insert(
    0,
    str(pathlib.Path(__file__).resolve().parents[1] / "tools" / "evidence_common"),
)

import portable_paths  # noqa: E402 -- tools/evidence_common, resolved above

KEY_FIELDS = ("file", "function", "genre", "replacement", "occurrence")
DISPOSITIONS = ("KILLED", "EQUIVALENT", "ACCEPTED", "REMOVED")
# The two dispositions that assert the mutant is NOT a survivor at the recorded
# head. A run that reports one of these missed is a regression, not a new row.
CLOSED = ("KILLED", "REMOVED")
SCHEMA = "pmux.mutation-survivor-register.v1"
ENUMERATION_SCHEMA = "pmux.mutation-enumeration.v1"

# The tail `cargo test` writes when it stops: the target that failed, and above
# it the names of the tests in that target which failed. Both come out of the
# per-mutant log, and neither is anywhere else -- `outcomes.json` records that a
# mutant was caught and never by what.
CATCHING_TARGET = re.compile(r"^error: test failed, to rerun pass `([^`]+)`", re.M)
CATCHING_TESTS = re.compile(r"^failures:\n((?:    \S.*\n)+)", re.M)


def load(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def scenarios(document):
    """(mutant, summary) for every mutant in an outcomes.json OR a --list --json.

    Both files describe a mutant with the same object; they differ in what wraps
    it and in whether an outcome exists yet. Reading both here is what lets the
    currency check enumerate the tree in front of it -- `--list` is 4.4 s --
    and compare that against a register recorded from a full campaign's outcomes,
    without a second copy of the keying rules.
    """

    if isinstance(document, list):
        for mutant in document:
            yield mutant, None
        return
    for outcome in document["outcomes"]:
        scenario = outcome.get("scenario")
        if not isinstance(scenario, dict) or "Mutant" not in scenario:
            continue
        yield scenario["Mutant"], outcome.get("summary")


def enumerated(outcomes):
    """{key -> row} for every mutant one run enumerated, keyed as above."""
    rows = []
    for mutant, summary in scenarios(outcomes):
        function = mutant.get("function") or {}
        span = function.get("span")
        rows.append(
            {
                "file": mutant["file"],
                "function": function.get("function_name") or "",
                "genre": mutant["genre"],
                "replacement": mutant["replacement"],
                "line": mutant["span"]["start"]["line"],
                "column": mutant["span"]["start"]["column"],
                # The whole item the mutant lives in, when the tool names one.
                # `None` where it does not -- the module-level `const`
                # initializers -- and the caller must state its own fallback
                # rather than read the mutant's own span as the item's.
                "item": (span["start"]["line"], span["end"]["line"]) if span else None,
                "mutant": (
                    mutant["span"]["start"]["line"],
                    mutant["span"]["end"]["line"],
                ),
                "name": mutant["name"],
                "summary": summary,
            }
        )
    rows.sort(
        key=lambda row: (
            row["file"],
            row["function"],
            row["genre"],
            row["replacement"],
            row["line"],
            row["column"],
        )
    )
    counts = {}
    keyed = {}
    for row in rows:
        group = (row["file"], row["function"], row["genre"], row["replacement"])
        counts[group] = counts.get(group, 0) + 1
        row["occurrence"] = counts[group]
        keyed[group + (row["occurrence"],)] = row
    return keyed


def key_of(entry):
    return tuple(entry[field] for field in KEY_FIELDS)


def package_of(file, packages):
    """The workspace package one mutated file belongs to, or None.

    `packages` is {repo-relative directory -> name}. Longest directory first, so
    a nested member is not swallowed by the one above it.
    """

    for directory in sorted(packages, key=len, reverse=True):
        if file == directory or file.startswith(f"{directory}/"):
            return packages[directory]
    return None


def rebuilt_in(log_text, package):
    """Whether cargo recompiled the mutated crate while grading one mutant.

    A mutant IS a source edit, so the crate holding it must be compiled before
    the tests that grade it run. A per-mutant log that says `Fresh <package>`
    and never `Compiling <package>` is a mutant tested against the PREVIOUS
    mutant's binary, and its recorded outcome is that mutant's outcome under
    another name -- a false `CaughtMutant` whenever the previous one was caught.

    MEASURED both ways. 0 of `run.bbUDg3`'s 1,653 per-mutant logs is missing its
    crate's `Compiling` line, because `scripts/gate-a-mutants.sh` lets every
    cargo-mutants worker own its `target/`. 101 of 291 were missing it in one
    filtered run that handed the tool a shared `CARGO_TARGET_DIR`, and three of
    those 101 reported `CaughtMutant` for a mutant that a hand-applied patch and
    472 passing tests say survives -- `docs/register-currency.md` section 9.
    """

    return (
        re.search(rf"^\s+Compiling {re.escape(package)} v", log_text, re.M) is not None
    )


def catcher_in(log_text):
    """What decided one mutant, distilled from that mutant's own log.

    The FIRST `error: test failed` line and the failure block above it, because
    `cargo test` without `--no-fail-fast` stops at the first target that fails:
    the log names A catcher, not every catcher. That is sound for invalidation
    and unsound for anything else -- see `docs/register-currency.md` section 4.2
    -- because invalidating on a test that no longer kills the mutant only ever
    costs a re-run, while the direction that would be a defect (a row silently
    held current) needs the recorded test to be unchanged, and an unchanged test
    still kills.

    Returns None when the log names no failing target at all, which is mostly
    what a TIMEOUT-decided mutant looks like: over `run.bbUDg3`'s 1,653 preserved
    logs, 8 of 8 timeouts named no target and every one of the 1,078 caught
    mutants named one. "Mostly" and not "only", because a later filtered run of
    274 mutants produced two CAUGHT outcomes whose logs named no target either --
    so the caller records `undetermined` with the reason rather than assuming a
    timeout.
    """

    target = CATCHING_TARGET.search(log_text)
    if target is None:
        return None
    blocks = CATCHING_TESTS.findall(log_text[: target.start()])
    tests = [name.strip() for name in blocks[-1].splitlines()] if blocks else []
    return {"test": tests[0] if tests else None, "target": target.group(1)}


def catchers(args):
    """Distil `caught_by` for every mutant a run decided, keyed as the register.

    Run BY THE GATE, while the per-mutant logs still exist. They do not survive
    the run: `scripts/gate-a-mutants.sh` copies five files out of a
    caller-supplied work directory and the 234 MB `log/` tree is not one of them,
    which is why the register's own campaign can no longer say what caught
    anything (`docs/register-currency.md` section 4.3).
    """

    run = enumerated(load(args.outcomes))
    logs = pathlib.Path(args.logs)
    document = load(args.outcomes)
    by_name = {}
    for outcome in document["outcomes"]:
        scenario = outcome.get("scenario")
        if not isinstance(scenario, dict) or "Mutant" not in scenario:
            continue
        by_name[scenario["Mutant"]["name"]] = outcome
    decided = {}
    for key, row in run.items():
        outcome = by_name.get(row["name"], {})
        if outcome.get("summary") not in ("CaughtMutant", "Timeout"):
            continue
        log = logs / pathlib.Path(outcome.get("log_path") or "").name
        found = (
            catcher_in(log.read_text(encoding="utf-8", errors="replace"))
            if log.is_file()
            else None
        )
        if found is None:
            # A mutant nobody can name a catcher for. Recorded as such rather
            # than dropped: a row with no `caught_by` and a row this could not
            # place read identically to a checker, and only one of them is a
            # gap in the evidence.
            found = {
                "test": None,
                "target": None,
                "undetermined": (
                    "timeout"
                    if outcome.get("summary") == "Timeout"
                    else "no-failing-target-in-log"
                ),
            }
        decided["|".join(str(part) for part in key)] = found
    payload = {
        "schema": "pmux.mutation-catchers.v1",
        "key_fields": list(KEY_FIELDS),
        "key_separator": "|",
        "run": args.run,
        "decided": len(decided),
        "catchers": dict(sorted(decided.items())),
    }
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(
            portable_paths.render_document(payload), handle, indent=1, sort_keys=True
        )
        handle.write("\n")
    named = sum(1 for value in decided.values() if value.get("test"))
    targeted = sum(1 for value in decided.values() if value.get("target"))
    print(f"catchers_decided={len(decided)}")
    print(f"catchers_named_a_test={named}")
    print(f"catchers_named_a_target={targeted}")
    return 0


def record_catchers(args):
    """Write `caught_by` onto every closed register row the run decided."""

    register = load(args.register)
    distilled = load(args.catchers)
    if distilled.get("key_fields") != list(KEY_FIELDS):
        print(
            f"{args.catchers} is keyed {distilled.get('key_fields')}, not "
            f"{list(KEY_FIELDS)}",
            file=sys.stderr,
        )
        return 1
    separator = distilled["key_separator"]
    written = 0
    unplaced = []
    for entry in register["entries"]:
        # KILLED and not CLOSED, for the reason `validate` gives: a REMOVED row's
        # mutant is not enumerated any more, so no run can decide it and a tool
        # that counted all thirteen of them as gaps would report a failure on
        # every invocation.
        if entry["disposition"] != "KILLED":
            continue
        key = separator.join(str(part) for part in key_of(entry))
        found = distilled["catchers"].get(key)
        if found is None:
            unplaced.append(key)
            continue
        entry["caught_by"] = dict(found, run=distilled["run"])
        written += 1
    with open(args.register, "w", encoding="utf-8") as handle:
        json.dump(
            portable_paths.render_document(register), handle, indent=1, sort_keys=True
        )
        handle.write("\n")
    print(f"caught_by_written={written}")
    print(f"killed_rows_the_run_did_not_decide={len(unplaced)}")
    for key in sorted(unplaced):
        print(f"undecided_killed_row={key}")
    return 1 if unplaced else 0


def validate(register):
    """Everything the register must be before it can judge anything."""
    problems = []
    if register.get("schema") != SCHEMA:
        problems.append(f"schema must be {SCHEMA}")
    if tuple(register.get("key_fields", ())) != KEY_FIELDS:
        problems.append(f"key_fields must be {list(KEY_FIELDS)}")
    seen = set()
    for entry in register.get("entries", []):
        try:
            key = key_of(entry)
        except KeyError as missing:
            problems.append(f"an entry is missing {missing}")
            continue
        if key in seen:
            problems.append(f"duplicate entry for {key}")
        seen.add(key)
        if entry.get("disposition") not in DISPOSITIONS:
            problems.append(f"{key} has disposition {entry.get('disposition')!r}")
        if not (entry.get("reason") or "").strip():
            problems.append(f"{key} has no reason, which is the whole point of the row")
        # A row that asserts the mutant is caught must name what catches it.
        # Without that, deleting the test leaves the row asserting something no
        # run has established since, and nothing in the tree can tell.
        #
        # KILLED and not CLOSED: a REMOVED row asserts the mutant is no longer
        # ENUMERATED, which no test decided and no test change can falsify. Its
        # currency is rule 1's -- the code moved -- and asking it to name a
        # catcher would be asking it to name a test that never ran.
        if entry.get("disposition") == "KILLED":
            caught_by = entry.get("caught_by")
            if not isinstance(caught_by, dict):
                problems.append(
                    f"{key} is {entry.get('disposition')} and carries no `caught_by`, "
                    "so nothing records which test decided it"
                )
            elif not (caught_by.get("target") or caught_by.get("undetermined")):
                problems.append(
                    f"{key} carries a `caught_by` naming neither a target nor why the "
                    "catcher is undetermined"
                )
    return problems


def check(args):
    register = load(args.register)
    problems = validate(register)
    if problems:
        for problem in problems:
            print(f"register is not well formed: {problem}", file=sys.stderr)
        return 1

    run = enumerated(load(args.outcomes))
    held = {key_of(entry): entry for entry in register["entries"]}
    missed = {key for key, row in run.items() if row["summary"] == "MissedMutant"}

    undispositioned = sorted(missed - set(held))
    regressed = sorted(
        key for key in missed & set(held) if held[key]["disposition"] in CLOSED
    )
    retired = sorted(
        key
        for key, entry in held.items()
        if entry["disposition"] not in CLOSED and key in run and key not in missed
    )
    out_of_scope = sorted(key for key in held if key not in run)
    moved = sorted(
        key
        for key, entry in held.items()
        if key in run
        and (
            entry.get("observed_at", {}).get("line") != run[key]["line"]
            or entry.get("observed_at", {}).get("column") != run[key]["column"]
        )
    )

    census = {}
    for entry in register["entries"]:
        census[entry["disposition"]] = census.get(entry["disposition"], 0) + 1
    print(f"register_entries={len(register['entries'])}")
    for disposition in DISPOSITIONS:
        print(f"register_{disposition.lower()}={census.get(disposition, 0)}")
    print(f"run_missed={len(missed)}")
    print(f"register_undispositioned={len(undispositioned)}")
    print(f"register_regressed={len(regressed)}")
    print(f"register_retired={len(retired)}")
    print(f"register_out_of_scope={len(out_of_scope)}")
    print(f"register_position_drift={len(moved)}")

    for key in retired:
        print(f"retired_survivor={run[key]['name']}")
    for key in undispositioned:
        print(
            "a mutant survived this run and the register does not hold it: "
            f"{run[key]['name']}",
            file=sys.stderr,
        )
    for key in regressed:
        print(
            f"a mutant the register calls {held[key]['disposition']} survived this run: "
            f"{run[key]['name']}",
            file=sys.stderr,
        )
    if undispositioned or regressed:
        print(
            "every survivor must carry a disposition, and a closed one must stay closed; "
            "see evidence/README.md",
            file=sys.stderr,
        )
        return 1
    return 0


def census_of(run):
    """{file: {function: {genre: {replacement: count}}}} for one enumeration.

    The register's key with `occurrence` collapsed to a count, which is exactly
    what "has this mutant ever been enumerated before" needs and is a twentieth
    of the size of the keys themselves. Written once, beside the register, so a
    later run can ask of every mutant it finds whether the register's campaign
    ever saw it -- a question the register cannot answer from 144 survivor rows,
    and the reason a brand-new function would otherwise pass in silence.
    """

    counts = {}
    for file, function, genre, replacement, _ in run:
        counts.setdefault(file, {}).setdefault(function, {}).setdefault(genre, {})
        by_replacement = counts[file][function][genre]
        by_replacement[replacement] = by_replacement.get(replacement, 0) + 1
    return counts


def covers(census, key):
    """Whether an enumeration census holds a place for one register-shaped key."""

    file, function, genre, replacement, occurrence = key
    seen = census.get(file, {}).get(function, {}).get(genre, {}).get(replacement, 0)
    return occurrence <= seen


def census(args):
    """Write the enumeration census for one `cargo mutants --list --json`.

    The frame the census records -- globs, test packages, profile, tool version,
    toolchain channel -- is READ OUT OF `scripts/gate-a-mutants.sh` and
    `rust-toolchain.toml` rather than passed in, so it is the frame that measured
    the run and not a caller's account of it. Rule 3(b) compares those five
    against the same two files later; a frame recorded from the caller's flags
    would compare a copy against itself.
    """

    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import register_currency  # noqa: PLC0415 -- circular at import time, not here

    repo = pathlib.Path(args.repo).resolve()
    script = (repo / register_currency.MUTATION_GATE).read_text(encoding="utf-8")
    toolchain = tomllib.loads(
        (repo / register_currency.TOOLCHAIN_PIN).read_text(encoding="utf-8")
    )
    run = enumerated(load(args.enumeration))
    payload = {
        "schema": ENUMERATION_SCHEMA,
        "key_fields": list(KEY_FIELDS),
        "what": (
            "Every mutant the full-scope campaign at `recorded_at.head` enumerated, "
            "keyed as the survivor register is keyed and counted rather than listed. "
            "It answers the one question 144 survivor rows cannot: whether a mutant "
            "found at some later commit is one this measurement has ever seen. A "
            "brand-new function's mutants are in no row and in no count, which is how "
            "they stop being invisible."
        ),
        "recorded_at": {
            "head": args.head,
            "scope": args.scope,
            "enumerated": len(run),
            "globs": register_currency.gate_array(script, "FULL_GLOBS"),
            "test_packages": register_currency.gate_array(script, "TEST_PACKAGES"),
            "mutation_profile": register_currency.gate_scalar(
                script, "MUTATION_PROFILE"
            ),
            "cargo_mutants_version": register_currency.gate_scalar(
                script, "REQUIRED_CARGO_MUTANTS_VERSION"
            ),
            "toolchain_channel": toolchain.get("toolchain", {}).get("channel"),
        },
        "counts": census_of(run),
    }
    if args.head_note:
        payload["recorded_at"]["head_note"] = args.head_note
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(
            portable_paths.render_document(payload), handle, indent=1, sort_keys=True
        )
        handle.write("\n")
    print(f"census_enumerated={len(run)}")
    print(f"census_files={len(payload['counts'])}")
    return 0


def report(args):
    register = load(args.register)
    problems = validate(register)
    for problem in problems:
        print(f"register is not well formed: {problem}", file=sys.stderr)
    recorded = register["recorded_at"]
    for field in sorted(recorded):
        print(f"recorded_{field}={recorded[field]}")
    census = {}
    cost = {}
    for entry in register["entries"]:
        census[entry["disposition"]] = census.get(entry["disposition"], 0) + 1
        if entry["disposition"] == "ACCEPTED":
            label = entry.get("closeable") or "unstated"
            cost[label] = cost.get(label, 0) + 1
    print(f"entries={len(register['entries'])}")
    for disposition in DISPOSITIONS:
        print(f"{disposition.lower()}={census.get(disposition, 0)}")
    for label in sorted(cost):
        print(f"accepted_closeable_{label.replace('-', '_')}={cost[label]}")
    return 1 if problems else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)

    checker = commands.add_parser(
        "check", help="hold one cargo-mutants run against the register"
    )
    checker.add_argument("--outcomes", required=True)
    checker.add_argument("--register", required=True)
    checker.set_defaults(handler=check)

    reporter = commands.add_parser("report", help="the register's own census")
    reporter.add_argument("--register", required=True)
    reporter.set_defaults(handler=report)

    distiller = commands.add_parser(
        "catchers", help="distil what caught each mutant, from the run's own logs"
    )
    distiller.add_argument("--outcomes", required=True)
    distiller.add_argument("--logs", required=True)
    distiller.add_argument("--run", required=True)
    distiller.add_argument("--out", required=True)
    distiller.set_defaults(handler=catchers)

    recorder = commands.add_parser(
        "record-catchers", help="write `caught_by` onto the register's closed rows"
    )
    recorder.add_argument("--register", required=True)
    recorder.add_argument("--catchers", required=True)
    recorder.set_defaults(handler=record_catchers)

    counter = commands.add_parser(
        "census", help="the enumeration census beside the register"
    )
    counter.add_argument("--enumeration", required=True)
    counter.add_argument("--repo", required=True)
    counter.add_argument("--head", required=True)
    counter.add_argument("--scope", required=True)
    counter.add_argument("--head-note", default="")
    counter.add_argument("--out", required=True)
    counter.set_defaults(handler=census)

    args = parser.parse_args()
    raise SystemExit(args.handler(args))


if __name__ == "__main__":
    main()
