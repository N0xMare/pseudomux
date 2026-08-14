#!/usr/bin/env python3
"""Measure the `transcript_drain_ms` a promoted compatibility profile must carry.

The drain answers exactly one question:

    how long after pmux has a terminal candidate can the transcript still move?

pmux's terminal candidate is the turn's final `assistant` row. So the quantity
this tool measures is the arrival gap between every pair of consecutive rows AT
OR AFTER that row, per turn, over a corpus of Claude Code transcripts written by
one or more exact versions on one platform.

THE RECOMMENDATION IS A POOLED BOUND, NOT A PER-VERSION FIT
-----------------------------------------------------------

`--version` is REPEATABLE, and the recommendation is taken over the pooled
maximum of every version named. That is not a convenience: it is the whole
policy, and `docs/version-drift.md` sec.3.3 and sec.3.5 are why.

* The per-version pin measures noise. Between 2.1.215 and 2.1.220 the maxima
  differ by 100 ms while the within-version p95 of the same statistic is
  176-216 ms; a permutation test on the difference in maxima gives p = 0.730.
* A small sample under-estimates a tail maximum, so a per-version fit is wrong
  in exactly the direction that TRUNCATES an answer. Fitting 2.1.223 from its
  own single arrival recommends 250 ms, which is below the 438 ms already
  observed one version earlier and below
  `POST_MARKER_CATCH_WINDOW_FLOOR_MS`.

So this tool publishes `per_version_recommendations` beside the pooled one --
labelled as what NOT to ship -- because the difference between them is the
entire argument, and a receipt that hid it would be asking to be re-fitted.

WHAT IT DELIBERATELY DOES NOT DO
--------------------------------

It does not drop outliers. A corpus of ordinary interactive agent sessions
contains rows that arrive hours after the answer -- a queued task, an away
summary, a `<task-notification>` -- and a drain sized to cover those would be an
hour long and would still be a race. Instead every arrival is bucketed by its
`(type, subtype)` and EVERY bucket is published with its own maximum, so nothing
is hidden; the recommendation is then taken over the buckets a minified (Path B)
cell can structurally produce.

That classification is a table, `ROW_KINDS`, with a reason per entry, and the
tool FAILS on a kind it has never seen rather than guessing. A silent default is
how an arrival that should have widened the drain comes to be ignored.

EXIT CODES
----------

Each non-zero exit is a named re-promotion trigger from `docs/version-drift.md`
sec.5 P2, and the trigger ids below are the same strings
`crate::compatibility::RepromotionTrigger` spells -- `compatibility.rs`'s
`every_repromotion_trigger_names_a_detector_that_exists` reads THIS FILE and
fails if one of them stops appearing in it.

    0  nothing fired
    2  an unclassified post-answer row kind        -> TRIGGER_UNCLASSIFIED_ROW_KIND
    3  a `retrospective` premise no longer holds   -> TRIGGER_UNCLASSIFIED_ROW_KIND
    4  a reachable arrival above `--bound-ms`      -> TRIGGER_ARRIVAL_ABOVE_THE_BOUND
    5  `--bound-ms` was given and there was NOTHING TO CHECK

Exit 5 exists because exit 0 on an empty corpus is the failure mode this tool
is most likely to have: at a brand-new Claude Code version there are no `cli`
turns to read yet (sec.2.1), so "the tool passed" and "the tool found nothing"
are the same exit code unless one of them is given its own.

Usage:

    python3 tools/promotion/measure_transcript_drain.py \\
        --corpus ~/.claude/projects --version 2.1.220 --json

    python3 tools/promotion/measure_transcript_drain.py \\
        --corpus ~/.claude/projects --corpus ~/.claude-1/projects \\
        --version 2.1.207 --version 2.1.215 --version 2.1.220 --version 2.1.223 \\
        --bound-ms 1000 --json

The corpus is host-local and is NOT part of this repository: it is the
operator's own transcripts, it contains their prompts, and it does not travel
with a clone. What travels is this tool and the receipt it emits.
"""

from __future__ import annotations

import argparse
import collections
import json
import math
import pathlib
import platform
import sys
from datetime import datetime
from typing import Any, Iterable

# Every argument a Path B mint's child receives, and the whole of it.
#
# NOT this file's own claim about pmux. The Rust test
# `stateless::tests::the_documented_minified_launch_bundle_is_the_argv_a_mint_emits`
# reads this tuple and compares it, spelling for spelling, against the argv
# `stateless::launch_request_for` actually produces, in the same idiom
# `pool::evidence` uses to bind `RETAINED_ROW_FIELDS` to `FIELDS_READ` below.
#
# It is a tuple rather than a sentence because the sentence was wrong. It read
# "a minified cell is launched with `--disallowedTools "*"`, `--strict-mcp-config`
# and `--safe-mode`" and pmux emitted exactly one of the three. No `reachable`
# value below ever rested on the other two, so no number this tool has ever
# published is wrong because of it -- the exposure was the next reader, who
# would have marked an MCP-derived row kind unreachable on a flag nothing
# passed.
MINIFIED_LAUNCH_FLAGS: tuple[str, ...] = (
    "--session-id",
    "--model",
    "--effort",
    "--permission-mode",
    "--disallowedTools",
    "--strict-mcp-config",
    "--system-prompt-file",
)

# Every `(type, subtype)` that can follow a turn's final assistant row, and
# whether a MINIFIED cell can produce it.
#
# `reachable` is the load-bearing column. A minified cell is launched with
# exactly `MINIFIED_LAUNCH_FLAGS` above: no tools, no MCP, no skills, no
# CLAUDE.md, no task queue, no away mode. A row kind that needs one of those
# cannot appear on that cell, and sizing the drain to cover it would be sizing
# it against a harness that is not there.
#
# `stop_hook_summary` is reachable on purpose: pmux's own hybrid lifecycle hook
# is a Stop hook, so this cell CAN emit one, and a classification that fails
# closed is the one to be wrong in.
#
# The optional `retrospective` column names a kind whose `timestamp` records
# WHEN AN EVENT HAPPENED rather than when the row arrived, so its gaps are not
# arrival gaps and must never size the drain. That exclusion is only sound while
# the event really does predate the turn's terminal candidate, so it is not
# taken on trust: `main` CHECKS it on every such row and fails the run when one
# is stamped after the candidate, exactly as an unclassified kind does.
ROW_KINDS: dict[tuple[str, str | None], dict[str, Any]] = {
    ("system", "turn_duration"): {
        "reachable": True,
        "why": "the end-of-turn marker Claude Code writes for every turn",
    },
    ("system", "stop_hook_summary"): {
        "reachable": True,
        "why": "a Stop hook ran; pmux's own hybrid lifecycle hook is one",
    },
    ("system", "away_summary"): {
        "reachable": False,
        "why": "away mode is an interactive-session feature; a minified cell has none",
    },
    ("system", "api_error"): {
        "reachable": False,
        "retrospective": True,
        "why": (
            "NOT A POST-ANSWER ARRIVAL. An api_error records an HTTP failure "
            "that happened INSIDE the turn -- Connection error., a 529 "
            "overload, a timeout -- and is stamped at the moment of that "
            "failure, not at the moment the row is appended. Every one seen "
            "post-answer is stamped BEFORE the turn's final assistant row, "
            "which is the row the successful retry produced; the `retrospective` "
            "column above makes that a checked premise rather than a claim. It "
            "lands after the answer in FILE order only where a queue reorders "
            "the append stream, and a minified cell has no queue. A retry that "
            "does produce a further answer arrives as an ('assistant', None) "
            "row, which is classified reachable below and is the entry that "
            "guards that case. Measured in docs/version-drift.md"
        ),
    },
    ("queue-operation", None): {
        "reachable": False,
        "why": "the task queue is a harness feature; a minified cell has no queue",
    },
    ("user", None): {
        "reachable": False,
        "why": (
            "a post-answer user row is a harness injection, e.g. a "
            "<task-notification>; nothing can inject one into a minified cell"
        ),
    },
    ("assistant", None): {
        "reachable": True,
        "why": (
            "A SEMANTIC ROW AFTER THE ANSWER. This is the arrival the drain "
            "exists for, and observing one retracts any measured value taken "
            "without it"
        ),
    },
}

# The margin the recommendation applies to the largest reachable arrival. Not a
# percentile: a percentile over 189 samples throws away the tail, and the tail
# is the entire subject.
RECOMMENDATION_MARGIN = 2.0

# Recommendations are rounded UP to this granularity, so a corpus that grows by
# one sample does not produce a new three-digit constant every time.
RECOMMENDATION_STEP_MS = 250

# The row that says a turn's duration was already accounted for, and therefore
# the row whose ABSENCE makes a turn owe the full drain. `v1::backend`'s
# graduated drain charges `TURN_DURATION_DRAIN_FLOOR_MS` to a turn that has one
# and `transcript_drain_ms` to a turn that does not, so the share of turns
# without it is the price of every millisecond of the bound.
TURN_DURATION_KIND = ("system", "turn_duration")

# The entrypoint a Path B cell writes. sec.2.1: `turn_duration` is a `cli`
# feature and ZERO of the corpus's 169,237 versioned SDK rows carry one, so a
# price quoted over every turn in the corpus would be quoting the SDK's.
CLI_ENTRYPOINT = "cli"

# The re-promotion triggers this tool can detect, spelled exactly as
# `crate::compatibility::RepromotionTrigger` spells them. Two exits map to the
# first: an unclassified kind and a broken `retrospective` premise are the same
# claim -- "a row kind in this corpus is not the row kind the classification
# describes" -- reported separately only so the operator knows which table
# column to re-read.
TRIGGER_UNCLASSIFIED_ROW_KIND = "unclassified_transcript_row_kind"
TRIGGER_ARRIVAL_ABOVE_THE_BOUND = "reachable_arrival_above_the_bound"

EXIT_OK = 0
EXIT_UNCLASSIFIED_ROW_KIND = 2
EXIT_RETROSPECTIVE_PREMISE_BROKEN = 3
EXIT_ARRIVAL_ABOVE_THE_BOUND = 4
EXIT_NOTHING_TO_CHECK = 5

# EVERY row field this tool reads, and the whole of it.
#
# It is not documentation: `read_rows` PRUNES each row to these keys, so a
# reader added below that needs a ninth field gets `None` and the measurement
# visibly changes instead of this list quietly becoming a lie.
#
# The list exists because pmux RETAINS its own Path B transcripts as the corpus
# for the next Claude Code version (`docs/version-drift.md` sec.5 P4), and what
# it retains is a mirror carrying exactly these keys and nothing else --
# `crate::pool::evidence::RETAINED_ROW_FIELDS`, which
# `evidence::tests::the_retained_fields_are_the_ones_the_measurement_tool_reads`
# checks against THIS constant by reading this file. A prompt or a completion is
# not in it, and cannot be, because nothing here would read one.
FIELDS_READ = (
    "entrypoint",
    "isMeta",
    "isSidechain",
    "promptId",
    "subtype",
    "timestamp",
    "type",
    "version",
)


def version_key(value: str) -> tuple[int, ...]:
    """Numeric ordering, because `2.1.99` sorts above `2.1.207` as text.

    A non-numeric component sorts as -1 rather than raising: this orders a
    receipt's keys and must never be the thing that fails a measurement.
    """

    return tuple(int(part) if part.isdigit() else -1 for part in value.split("."))


def parse_timestamp(row: dict[str, Any]) -> float | None:
    value = row.get("timestamp")
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000
    except ValueError:
        return None


def read_rows(path: pathlib.Path) -> tuple[list[dict[str, Any]], set[str]]:
    """Every timestamped row of `path` in file order, and every version it names.

    Split out of [`read_transcript`] so a pooled run reads each file ONCE and
    then decides admission per version, rather than re-reading the whole corpus
    per `--version`. The admission rule itself is unchanged and still lives in
    `read_transcript`.
    """

    rows: list[dict[str, Any]] = []
    versions: set[str] = set()
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return [], versions
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(row, dict):
            continue
        if isinstance(row.get("version"), str):
            versions.add(row["version"])
        # PRUNED to `FIELDS_READ` here, at the one place rows enter the tool.
        # Two things follow, and both are the point: nothing downstream can read
        # a prompt or a completion even by accident, and the retained mirror
        # pmux writes (which carries exactly these keys) is provably as good an
        # input as the operator's own transcripts.
        row = {key: value for key, value in row.items() if key in FIELDS_READ}
        if parse_timestamp(row) is not None:
            rows.append(row)
    return rows, versions


def read_transcript(path: pathlib.Path, version: str) -> list[dict[str, Any]] | None:
    """Rows of `path` in file order, or `None` when it is not this version.

    A transcript is admitted when ANY row names the version under measurement.
    Claude Code stamps every row it writes, and a file whose rows disagree is a
    session that spanned an upgrade -- reported by the caller, not silently
    mixed in.
    """

    rows, versions = read_rows(path)
    if version not in versions:
        return None
    return rows


def turns(rows: list[dict[str, Any]]) -> Iterable[list[dict[str, Any]]]:
    """Split a transcript into turns at each caller prompt.

    A turn starts at a non-meta, non-sidechain `user` row carrying a `promptId`
    -- which is the row `TranscriptEngine` acknowledges -- and runs to the row
    before the next one.
    """

    starts = [
        index
        for index, row in enumerate(rows)
        if row.get("type") == "user"
        and not row.get("isMeta")
        and not row.get("isSidechain")
        and row.get("promptId")
    ]
    for position, start in enumerate(starts):
        end = starts[position + 1] if position + 1 < len(starts) else len(rows)
        yield [row for row in rows[start:end] if not row.get("isSidechain")]


def post_answer_arrivals(
    turn: list[dict[str, Any]],
) -> list[tuple[float, str, str | None, float]]:
    """Gaps between consecutive rows at or after this turn's final assistant row.

    The fourth element is the same row's offset from the TERMINAL CANDIDATE
    itself -- the final assistant row -- rather than from its file-order
    predecessor. The drain is sized on the consecutive gap, because the drain is
    a quiet window and not a deadline; the offset exists so a `retrospective`
    classification can be checked against the candidate it claims to predate.
    """

    last_assistant = None
    for index, row in enumerate(turn):
        if row.get("type") == "assistant":
            last_assistant = index
    if last_assistant is None:
        return []
    tail = turn[last_assistant:]
    candidate = parse_timestamp(tail[0])
    arrivals = []
    for earlier, later in zip(tail, tail[1:]):
        gap = parse_timestamp(later) - parse_timestamp(earlier)
        arrivals.append(
            (
                gap,
                later.get("type"),
                later.get("subtype"),
                parse_timestamp(later) - candidate,
            )
        )
    return arrivals


def drain_price(turn: list[dict[str, Any]]) -> tuple[bool, bool] | None:
    """`(is a cli turn, owes the FULL drain)`, or `None` for a turn that owes none.

    A turn that never produced an `assistant` row never reached a terminal
    candidate, so no drain is ever charged to it and counting it would deflate
    the price. Among the turns that did, the graduated drain
    (`crates/service/src/v1/backend.rs`) charges the floor to a turn whose own
    `system/turn_duration` marker has landed and the full
    `transcript_drain_ms` to a turn whose has not -- so the share WITHOUT a
    marker is exactly the share on which a wider bound would cost latency.
    """

    if not any(row.get("type") == "assistant" for row in turn):
        return None
    cli = any(row.get("entrypoint") == CLI_ENTRYPOINT for row in turn)
    marked = any(
        (row.get("type"), row.get("subtype")) == TURN_DURATION_KIND for row in turn
    )
    return cli, not marked


def quantile(sorted_values: list[float], percentile: int) -> float:
    index = min(
        len(sorted_values) - 1,
        max(0, math.ceil(percentile / 100 * len(sorted_values)) - 1),
    )
    return sorted_values[index]


def distribution(values: list[float]) -> dict[str, Any] | None:
    if not values:
        return None
    ordered = sorted(values)
    return {
        "count": len(ordered),
        "min_ms": round(ordered[0]),
        "median_ms": round(quantile(ordered, 50)),
        "p90_ms": round(quantile(ordered, 90)),
        "p95_ms": round(quantile(ordered, 95)),
        "p99_ms": round(quantile(ordered, 99)),
        "max_ms": round(ordered[-1]),
    }


def new_version_state() -> dict[str, Any]:
    return {
        "files": 0,
        "turns": 0,
        "cli_turns": 0,
        "cli_turns_without_a_turn_duration_marker": 0,
        "by_kind": collections.defaultdict(list),
        "unknown_kinds": collections.Counter(),
        "not_retrospective": collections.Counter(),
    }


def accumulate(state: dict[str, Any], rows: list[dict[str, Any]]) -> None:
    """Fold one admitted transcript into one version's state."""

    state["files"] += 1
    for turn in turns(rows):
        state["turns"] += 1
        price = drain_price(turn)
        if price is not None:
            cli, owes_the_full_drain = price
            if cli:
                state["cli_turns"] += 1
                if owes_the_full_drain:
                    state["cli_turns_without_a_turn_duration_marker"] += 1
        for gap, kind, subtype, since_candidate in post_answer_arrivals(turn):
            key = (kind, subtype)
            entry = ROW_KINDS.get(key)
            if entry is None:
                state["unknown_kinds"][f"{kind}/{subtype}"] += 1
            elif entry.get("retrospective") and since_candidate > 0:
                state["not_retrospective"][f"{kind}/{subtype}"] += 1
            state["by_kind"][key].append(gap)


def recommend(reachable: list[float]) -> int | None:
    """The conservative estimator, in one place both the pooled and the
    per-version numbers go through.

    Written once because the WHOLE POINT of the pooled bound is that it is the
    same estimator applied to a wider sample: two spellings of it would let the
    "do not fit this per version" argument be made against a statistic nobody
    actually computed per version.
    """

    if not reachable:
        return None
    raw = max(reachable) * RECOMMENDATION_MARGIN
    return int(math.ceil(raw / RECOMMENDATION_STEP_MS) * RECOMMENDATION_STEP_MS)


def report(state: dict[str, Any]) -> dict[str, Any]:
    """One version's -- or the pool's -- measured body."""

    reachable: list[float] = []
    unreachable: list[float] = []
    kinds_report = {}
    for key, gaps in sorted(state["by_kind"].items(), key=lambda item: str(item[0])):
        entry = ROW_KINDS[key]
        (reachable if entry["reachable"] else unreachable).extend(gaps)
        kinds_report[f"{key[0]}/{key[1]}"] = {
            "reachable_on_a_minified_cell": entry["reachable"],
            # Published because it is the only thing that explains a NEGATIVE
            # gap in a table of arrivals: this kind's timestamp is an event
            # time, so its distribution below describes when the event
            # happened, not when the row landed.
            "timestamp_is_retrospective": bool(entry.get("retrospective")),
            "why": entry["why"],
            **(distribution(gaps) or {}),
        }

    everything = reachable + unreachable
    cli_turns = state["cli_turns"]
    unmarked = state["cli_turns_without_a_turn_duration_marker"]
    return {
        "files_at_this_version": state["files"],
        "turns": state["turns"],
        # The price of the bound, MEASURED rather than asserted. A turn whose
        # own `turn_duration` marker has landed already owes only the graduated
        # floor, so widening `transcript_drain_ms` costs latency on exactly
        # this share of turns and on no others.
        "full_drain_binds_on": {
            "cli_turns_with_a_terminal_candidate": cli_turns,
            "without_a_turn_duration_marker": unmarked,
            "fraction": round(unmarked / cli_turns, 3) if cli_turns else None,
        },
        "post_answer_arrivals": {
            "all": distribution(everything),
            "reachable_on_a_minified_cell": distribution(reachable),
            "unreachable_on_a_minified_cell": distribution(unreachable),
            "by_kind": kinds_report,
        },
        "partition_balances": len(everything)
        == sum(len(gaps) for gaps in state["by_kind"].values()),
        "recommended_transcript_drain_ms": recommend(reachable),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--corpus",
        type=pathlib.Path,
        action="append",
        help="root of a Claude Code transcript tree (host-local, never committed); "
        "repeatable",
    )
    parser.add_argument(
        "--version",
        required=True,
        action="append",
        dest="versions",
        help="exact Claude Code version to measure; REPEATABLE, and the "
        "recommendation is pooled over every one named",
    )
    parser.add_argument(
        "--bound-ms",
        type=int,
        default=None,
        help="the drain already shipped. Given, the run becomes a CHECK: it "
        "fails when a reachable arrival exceeds it, and fails differently when "
        "there was nothing to check at all",
    )
    parser.add_argument(
        "--os", default=None, help="platform token to record (default: this host)"
    )
    parser.add_argument(
        "--arch", default=None, help="arch token to record (default: this host)"
    )
    parser.add_argument("--json", action="store_true", help="emit the receipt as JSON")
    arguments = parser.parse_args()

    corpora = arguments.corpus or [pathlib.Path.home() / ".claude" / "projects"]
    versions = sorted(set(arguments.versions), key=version_key)

    files = sorted({path for root in corpora for path in root.rglob("*.jsonl")})
    states = {version: new_version_state() for version in versions}
    pooled = new_version_state()
    # ONE pass over the corpus. Each file is read once and offered to every
    # version that names it, which is the same admission rule `read_transcript`
    # applies -- and a file that spans an upgrade is still admitted to each,
    # exactly as the single-version run admits it.
    for path in files:
        rows, present = read_rows(path)
        pooled_this_file = False
        for version in versions:
            if version not in present:
                continue
            accumulate(states[version], rows)
            if not pooled_this_file:
                accumulate(pooled, rows)
                pooled_this_file = True

    unknown_kinds: collections.Counter[str] = collections.Counter()
    not_retrospective: collections.Counter[str] = collections.Counter()
    for state in states.values():
        unknown_kinds.update(state["unknown_kinds"])
        not_retrospective.update(state["not_retrospective"])

    if unknown_kinds:
        # FAIL, do not default. A row kind nobody has classified is either an
        # arrival the drain must cover or one it must not, and guessing is how a
        # measurement quietly stops measuring.
        print(
            f"[{TRIGGER_UNCLASSIFIED_ROW_KIND}] unclassified post-answer row "
            "kind(s); add them to ROW_KINDS with a reason: "
            + json.dumps(dict(unknown_kinds), sort_keys=True),
            file=sys.stderr,
        )
        return EXIT_UNCLASSIFIED_ROW_KIND

    if not_retrospective:
        # FAIL for the same reason. A kind marked `retrospective` is excluded
        # from the drain because its timestamp is an event time that precedes
        # the answer -- so a row of that kind stamped AFTER the terminal
        # candidate is not the thing the classification described, and the
        # exclusion it justifies has stopped being justified.
        print(
            f"[{TRIGGER_UNCLASSIFIED_ROW_KIND}] row kind(s) classified "
            "`retrospective` were stamped AFTER the turn's terminal candidate; "
            "the exclusion no longer holds -- re-read them and re-classify: "
            + json.dumps(dict(not_retrospective), sort_keys=True),
            file=sys.stderr,
        )
        return EXIT_RETROSPECTIVE_PREMISE_BROKEN

    by_version = {version: report(state) for version, state in states.items()}
    pooled_report = report(pooled)
    recommended = pooled_report["recommended_transcript_drain_ms"]
    observed_max = (
        pooled_report["post_answer_arrivals"]["reachable_on_a_minified_cell"] or {}
    ).get("max_ms")

    receipt = {
        # The lowest version measured, and the identity a single-version
        # receipt is filed under. `claude_versions` is the honest key.
        "claude_version": versions[0],
        "claude_versions": versions,
        "os": arguments.os
        or ("macos" if platform.system() == "Darwin" else platform.system().lower()),
        "arch": arguments.arch or platform.machine().replace("arm64", "aarch64"),
        "corpus": {
            "roots": [str(root) for root in corpora],
            "tracked": False,
            "note": (
                "Host-local operator transcripts. Not committed: they contain prompts. "
                "The receipt is the durable artifact; re-run the tool to regenerate it."
            ),
            "files_scanned": len(files),
            "files_at_these_versions": pooled["files"],
            "turns": pooled["turns"],
        },
        "full_drain_binds_on": pooled_report["full_drain_binds_on"],
        "post_answer_arrivals": pooled_report["post_answer_arrivals"],
        "partition_balances": pooled_report["partition_balances"],
        "recommended_transcript_drain_ms": recommended,
        "recommendation_basis": {
            "statistic": (
                "max of every post-answer arrival a minified cell can produce, "
                "POOLED over every version measured"
            ),
            "margin": RECOMMENDATION_MARGIN,
            "rounded_up_to_ms": RECOMMENDATION_STEP_MS,
            "pooled_over_versions": versions,
        },
        "by_version": by_version,
        # Published so the pooled bound can be COMPARED with the thing it
        # replaces, in the artifact itself. A per-version fit on a thin corpus
        # is 87% low at n=1 (docs/version-drift.md sec.3.5) and would ship a
        # drain that truncates answers; shipping any of these is the defect.
        "per_version_recommendations_not_to_be_shipped": {
            version: body["recommended_transcript_drain_ms"]
            for version, body in by_version.items()
        },
        "repromotion_triggers_this_tool_detects": {
            TRIGGER_UNCLASSIFIED_ROW_KIND: (
                f"exit {EXIT_UNCLASSIFIED_ROW_KIND} on a post-answer row kind "
                f"absent from ROW_KINDS, exit {EXIT_RETROSPECTIVE_PREMISE_BROKEN} "
                "on a `retrospective` kind stamped after the terminal candidate"
            ),
            TRIGGER_ARRIVAL_ABOVE_THE_BOUND: (
                f"exit {EXIT_ARRIVAL_ABOVE_THE_BOUND} when --bound-ms is given "
                "and a reachable arrival exceeds it; exit "
                f"{EXIT_NOTHING_TO_CHECK} when there was nothing to check"
            ),
        },
        "what_would_invalidate_it": [
            "an assistant row arriving after a turn's final assistant row -- see ROW_KINDS",
            "any reachable arrival above the recommended value",
            "a different os or arch, or a claude_version outside the promoted range",
            "a minified cell acquiring a harness able to inject rows (a hook, a queue, an MCP server)",
        ],
    }

    # Imported HERE and not at the top of the file, which is where it belongs
    # and where it cannot go. Three documents linted by
    # `crates/service/tests/path_b_doc_citations.rs` cite this file by line --
    # the highest is line 608 -- and an import above them would move every one
    # of those lines while the sentences beside them stayed put. That is the
    # rot the citation guard exists to catch, and rotting nine citations to
    # save three lines of indentation is not a trade. Nothing below 608 is
    # cited, so the bootstrap sits below 608.
    sys.path.insert(
        0, str(pathlib.Path(__file__).resolve().parents[1] / "evidence_common")
    )
    import portable_paths  # noqa: E402 -- tools/evidence_common, resolved above

    # The corpus roots are under the operator's home directory, and this
    # receipt is committed. Rendered whole rather than at `corpus.roots`, for
    # the reason `measure_turn_latency.main` gives.
    receipt = portable_paths.render_document(receipt)

    if arguments.json:
        json.dump(receipt, sys.stdout, indent=1, sort_keys=True)
        sys.stdout.write("\n")
    else:
        print(json.dumps(receipt, indent=2, sort_keys=True))

    if arguments.bound_ms is None:
        return EXIT_OK

    if observed_max is None:
        # NOT a pass. At a version pmux has never served a `cli` turn at, the
        # corpus is empty and every check above is vacuous; reporting that as
        # exit 0 is how "we checked" and "there was nothing to check" become
        # the same sentence.
        print(
            "no reachable post-answer arrival was found for "
            + ", ".join(versions)
            + ": there was NOTHING TO CHECK against the "
            f"{arguments.bound_ms} ms bound, which is not the same as passing",
            file=sys.stderr,
        )
        return EXIT_NOTHING_TO_CHECK

    if observed_max > arguments.bound_ms:
        print(
            f"[{TRIGGER_ARRIVAL_ABOVE_THE_BOUND}] a reachable post-answer "
            f"arrival of {observed_max} ms exceeds the {arguments.bound_ms} ms "
            "bound pmux ships; the promoted range is retracted until it is "
            "re-measured",
            file=sys.stderr,
        )
        return EXIT_ARRIVAL_ABOVE_THE_BOUND

    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main())
