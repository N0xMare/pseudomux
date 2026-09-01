#!/usr/bin/env python3
"""Run the ordered check list one Claude Code version must pass to be promoted.

WHY THIS EXISTS
---------------

2.1.226 was promoted by argument and then, later, by an agent improvising a
measurement session: `docs/2.1.226-compatibility.md` and
`docs/2.1.226-acceptance.md` are excellent evidence and are not a procedure.
Nothing in them is runnable, so 2.1.227 costs another improvisation, and an
improvisation is exactly where a promotion stops being repeatable.

Living entrypoint: `tools/dev/promote.py`. That wrapper is what refuses a
missing per-OS drain as “cannot drop the operator flag”, and points pin
confirmation at `tools/dev/operator_eval.py`.

This file is that session's checks, in order, with their pass criteria, as a
program. It is deliberately NOT a new promotion policy: every number it
asserts against is derived from something already committed, and the one number
it must never invent is the drain. It is not how you confirm an operator pin.

WHAT IT EXERCISES, AND WHY A MINIFIED CELL IS THE WHOLE POINT
-------------------------------------------------------------

`require_tested_for_minified_cell` (`crates/service/src/v1/actor.rs`) gates
exactly one thing: `SessionCell::Minified`. Living probe is `pmux run`
(always the minified pool). Session launch flags are refused; this tool is
the graded no-tools oracle.

Every turn here goes through `pmux run`, which is Path B and therefore always
`SessionCell::Minified`, and the caller can name no resource on it. The oracle
is a nonce plus a result the prompt makes computable, so it needs no tool: a
cell launched with `--disallowedTools "*"` can satisfy it. Historical Phase 0
prompts that instructed the model to run `shasum` cannot be satisfied by one
at all (those prompts were deleted with the freeze envelope).

THE DRAIN IS READ, NEVER FITTED
-------------------------------

`docs/version-drift.md` P1: the drain pmux ships is a POOLED conservative bound
over every observed version, and a per-version fit from a thin corpus lands
below `POST_MARKER_CATCH_WINDOW_FLOOR_MS`. This tool therefore

* takes the bound from `evidence/pooled-transcript-drain-<os>-<arch>.json`'s
  own `recommended_transcript_drain_ms`, which
  `compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit`
  already pins to the shipped `PromotedProfile::transcript_drain_ms`; and
* asserts only that no reachable post-answer arrival AT THE TARGET VERSION
  exceeds it, publishing the per-version fit under the same
  `per_version_recommendations_not_to_be_shipped` key
  `measure_transcript_drain.py` uses.

It never proposes a drain of its own, and `--bound-ms` is not an option here on
purpose.

EXIT CODES
----------

    0  every check passed; the receipt carries a promotable profile
    1  a check failed; the receipt names it and no profile is proposed
    2  this tool refused to run (a precondition it will not work around)

Usage:

    python3 tools/promotion/promote_claude_version.py --describe

    python3 tools/promotion/promote_claude_version.py \\
        --release-dir target/release \\
        --claude "$HOME/.local/share/claude/versions/2.1.227" \\
        --output evidence/promotion-2.1.227-macos-aarch64.json

    python3 tools/dev/promote.py \\
        --release-dir target/release \\
        --claude "$HOME/.local/share/pmux/claude/2.1.236/claude" \\
        --floor 2.1.227 \\
        --output evidence/promotion-2.1.236-linux-x86_64.json

`--driver-environment double` runs the same ordered path against
`pmux-test-claude`. It is a REHEARSAL and says so in its own verdict: the double
answers the graded prompts by echo rather than by reasoning, and it writes no
`"version"` row, so the version-keyed checks report `not_applicable` rather than
passing. A rehearsal never proposes a profile.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import pathlib
import platform
import random
import re
import string
import subprocess
import sys
import time
import unicodedata
from typing import Any, Callable

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "evidence_common"))

import portable_paths  # noqa: E402 -- tools/evidence_common, resolved above
from measure_transcript_drain import MINIFIED_LAUNCH_FLAGS  # noqa: E402
from measure_turn_latency import (  # noqa: E402
    Daemon,
    MeasurementError,
    Sandbox,
    claude_version,
    host_identity,
    resolve_binaries,
    run_client,
)

SCHEMA_VERSION = 1
PROMOTION_ALGORITHM = "pmux-claude-version-promotion-v1"

REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]
COMPATIBILITY_RS = REPOSITORY_ROOT / "crates/service/src/compatibility.rs"
CLAUDE_LAUNCH_RS = REPOSITORY_ROOT / "crates/service/src/claude_launch.rs"
DRAIN_TOOL = pathlib.Path(__file__).resolve().parent / "measure_transcript_drain.py"

# One probe value per flag in the minified launch bundle, for the non-executing
# parse probe of `docs/2.1.226-compatibility.md` sec.1.1. The SET is
# `MINIFIED_LAUNCH_FLAGS`, imported rather than retyped, so a flag pmux starts
# emitting is probed here without anyone remembering to add it; these are only
# the values a probe needs.
#
# A real value is required, not a placeholder: `--effort` and `--permission-mode`
# validate their choices and commander exits on a bad one BEFORE it reaches the
# sentinel, so a generic value turns "accepted" into "unreadable" for exactly the
# two flags whose vocabularies are most likely to move. Measured, at 2.1.226.
#
# The valueless flags are not listed here. They are the ones the daemon appends
# for a minified cell, read from `claude_launch.rs::MINIFIED_CELL_FLAGS` by
# `valueless_bundle_flags()`, because `stateless.rs`'s workspace scan gives
# exactly four files the right to spell them and this is not one. A flag in
# NEITHER place refuses the run rather than being probed valueless and reported
# as accepted.
PROBE_VALUES: dict[str, str] = {
    "--session-id": "00000000-0000-4000-8000-000000000000",
    "--model": "sonnet",
    "--effort": "low",
    "--permission-mode": "dontAsk",
    "--disallowedTools": "*",
    "--system-prompt-file": "/dev/null",
}
# Appended after the flag under test. Commander reports the FIRST unknown
# option and exits without running anything, so the reply names this sentinel
# when the flag was accepted and names the flag when it was not. `doctor` is a
# real subcommand and is never reached.
PROBE_SENTINEL = "--pmux-probe-sentinel"

# The grade suite. Every prompt is answerable by a cell with NO tools, which is
# what makes it a Path B suite: `all it can do is think/reason then respond with
# text output`. Each carries a fresh nonce so the answer cannot come from a
# cache, a template, a fixture or a neighbouring transcript, and each states an
# exact reply so the oracle is equality rather than a judgement.
#
# `expected` is a function of the nonce so the prompt and the oracle are built
# from one value; a suite whose expected answers were written down separately is
# a suite that can drift from its own prompts.


@dataclasses.dataclass(frozen=True)
class Grade:
    id: str
    render: Callable[[str], str]
    expected: Callable[[str], str]
    proves: str


def _arithmetic(nonce: str) -> str:
    left, right = 17 + (ord(nonce[0]) % 40), 25 + (ord(nonce[1]) % 40)
    return (
        f"Nonce: {nonce}. Add {left} and {right}. Reply with exactly the nonce, "
        "a hyphen, and the sum, and nothing else."
    )


def _arithmetic_expected(nonce: str) -> str:
    left, right = 17 + (ord(nonce[0]) % 40), 25 + (ord(nonce[1]) % 40)
    return f"{nonce}-{left + right}"


# NFC by construction: `normalize_prompt` (crates/claude/src/composer.rs) applies
# canonical composition to the prompt, and the composer records a decomposed
# `e` + U+0301 as U+00E9, so a decomposed literal here would be a prompt pmux
# admits and cannot acknowledge. Written composed, and asserted composed below.
UNICODE_LINE = "café — 日本語 ✓"

GRADES: tuple[Grade, ...] = (
    Grade(
        id="01-arithmetic-nonce",
        render=_arithmetic,
        expected=_arithmetic_expected,
        proves="a model produced these bytes in this turn: the nonce is unique "
        "to the turn and the sum is not in the prompt",
    ),
    # The one grade whose prompt carries NO nonce, and deliberately: its own
    # nonce would be a nonce it was given, which is the exact thing it asks
    # about. What makes it turn-unique is the turn before it -- grade 01's
    # nonce, on the same process, one `/clear` earlier.
    Grade(
        id="02-context-is-empty-after-clear",
        render=lambda nonce: (
            "Earlier in this conversation you may have been given a "
            "four-character nonce. If you were, reply with exactly that nonce. "
            "If you were given none, reply with exactly the word NONE. Reply "
            "with nothing else."
        ),
        expected=lambda nonce: "NONE",
        proves="`/clear` really cleared: the instance that answers this is the "
        "same operating-system process that answered grade 01, so a context "
        "that survived recycling WOULD have shown a presence here",
    ),
    Grade(
        id="03-unicode-echo",
        render=lambda nonce: (
            f"Reply with exactly this line and nothing else: {nonce} {UNICODE_LINE}"
        ),
        expected=lambda nonce: f"{nonce} {UNICODE_LINE}",
        proves="non-ASCII text survives the composer, the bracketed paste, the "
        "acknowledgement comparison and the transcript, byte for byte",
    ),
    Grade(
        id="04-ordered-long-reply",
        render=lambda nonce: (
            f"Nonce: {nonce}. Reply with exactly the integers 1 through 20 "
            "separated by single spaces, then a single space, then the nonce, "
            "and nothing else."
        ),
        expected=lambda nonce: " ".join(str(n) for n in range(1, 21)) + f" {nonce}",
        proves="a multi-line-sized reply arrives complete: a truncated drain "
        "loses the tail, which is the failure the transcript drain exists for",
    ),
)


@dataclasses.dataclass(frozen=True)
class Check:
    id: str
    # Which `compatibility::RepromotionTrigger` ids this check is the
    # promotion-time exercise of. The UNION over this table must be every
    # trigger; `_require_every_repromotion_trigger_is_exercised` reads the ids
    # out of `crates/service/src/compatibility.rs` and refuses to run if it is
    # not, so a sixth trigger stops this tool rather than being quietly
    # unpromoted-against.
    exercises: tuple[str, ...]
    costs_real_turns: bool
    criterion: str
    run: Callable[["Run"], dict[str, Any]]


class PromotionRefused(RuntimeError):
    """A precondition this tool will not work around. Exit 2, never exit 1."""


class CheckFailed(RuntimeError):
    """A check's pass criterion was not met. Exit 1."""


def _nonce(rng: random.Random) -> str:
    alphabet = string.ascii_uppercase + string.digits
    return "".join(rng.choice(alphabet) for _ in range(4))


def repromotion_trigger_ids() -> list[str]:
    """Every `RepromotionTrigger` id, read out of the Rust that defines them.

    Derived rather than restated: `TriggerDetector::id` is the one spelling
    every language reports, `compatibility.rs` already has a test that each
    trigger names a detector file containing its symbol, and a hand-written
    copy here is how a promotion path comes to be silent about a trigger that
    exists.
    """

    source = COMPATIBILITY_RS.read_text(encoding="utf-8")
    match = re.search(
        r"pub const fn detector\(self\) -> TriggerDetector \{(.*?)\n    \}",
        source,
        re.S,
    )
    if match is None:
        raise PromotionRefused(
            f"{COMPATIBILITY_RS} no longer defines `RepromotionTrigger::detector`, "
            "so this tool cannot derive which triggers a promotion must exercise"
        )
    ids = re.findall(r'\bid:\s*"([a-z_]+)"', match.group(1))
    if not ids:
        raise PromotionRefused(
            "`RepromotionTrigger::detector` named no trigger ids; refusing to "
            "promote against an empty trigger set"
        )
    return ids


def valueless_bundle_flags() -> tuple[str, ...]:
    """The bundle flags that take no value, read from the launcher's own list.

    `claude_launch.rs` appends `MINIFIED_CELL_FLAGS` with
    `args.extend(MINIFIED_CELL_FLAGS.iter()...)` and pushes no value beside
    them, so "daemon-appended" and "valueless" are the same set BY THAT CODE,
    not by an assumption made here. Reading it also keeps this file out of the
    four `stateless.rs::BUNDLE_SPELLING_HOMES` are allowed to be -- a fifth site
    spelling the bundle is the defect that scan exists for.
    """

    source = CLAUDE_LAUNCH_RS.read_text(encoding="utf-8")
    match = re.search(
        r"MINIFIED_CELL_FLAGS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", source, re.S
    )
    if match is None:
        raise PromotionRefused(
            f"{CLAUDE_LAUNCH_RS} no longer publishes MINIFIED_CELL_FLAGS, so this "
            "probe cannot tell which bundle flags take a value"
        )
    return tuple(re.findall(r'"(--[A-Za-z0-9-]+)"', match.group(1)))


def promoted_profiles() -> list[dict[str, str]]:
    """Every shipped `PROMOTED_PROFILES` entry, read from the Rust.

    A promotion is per os/arch. Parsing the whole table — rather than the first
    `claude_version_floor` in the file — is what lets a linux cell exist next
    to the macos one without this tool widening the wrong range.
    """

    source = COMPATIBILITY_RS.read_text(encoding="utf-8")
    marker = "pub const PROMOTED_PROFILES: &[PromotedProfile] = &["
    start = source.find(marker)
    if start < 0:
        raise PromotionRefused(
            f"{COMPATIBILITY_RS} no longer declares PROMOTED_PROFILES"
        )
    body = source[start + len(marker) :]
    end = body.find("\n];")
    if end < 0:
        raise PromotionRefused(
            f"{COMPATIBILITY_RS} PROMOTED_PROFILES array is not closed with ];"
        )
    profiles = []
    for entry in re.split(r"PromotedProfile \{", body[:end])[1:]:
        fields = dict(re.findall(r'(\w+): "([^"]*)"', entry))
        wanted = ("claude_version_floor", "claude_version_tested_through", "os", "arch")
        if all(name in fields for name in wanted):
            profiles.append({name: fields[name] for name in wanted})
    if not profiles:
        raise PromotionRefused(
            f"{COMPATIBILITY_RS} declares PROMOTED_PROFILES and this read none of them"
        )
    return profiles


def promoted_version_floor(
    host_os: str, host_arch: str, explicit_floor: str | None = None
) -> str:
    """The floor this os/arch already ships, or `--floor` on a first promotion.

    The floor is the version with a receipt; a promotion widens the range
    forward from it and never invents a new one. Read rather than guessed so
    the proposed profile cannot disagree with the shipped cell about where the
    range starts. A host with no cell yet cannot infer a floor from another
    OS's range — that is how linux would have shipped `2.1.220..=2.1.236`
    with macos Gate B prose attached.
    """

    matches = [
        profile
        for profile in promoted_profiles()
        if profile["os"] == host_os and profile["arch"] == host_arch
    ]
    if len(matches) > 1:
        raise PromotionRefused(
            f"{COMPATIBILITY_RS} ships {len(matches)} cells for {host_os}/{host_arch}; "
            "overlapping ranges are ambiguous and this tool will not pick a floor"
        )
    if len(matches) == 1:
        shipped = matches[0]["claude_version_floor"]
        if explicit_floor is not None and explicit_floor != shipped:
            raise PromotionRefused(
                f"--floor {explicit_floor} disagrees with the shipped "
                f"{host_os}/{host_arch} floor {shipped}"
            )
        return shipped
    if explicit_floor is None:
        raise PromotionRefused(
            f"no promoted cell for {host_os}/{host_arch} yet; pass --floor "
            "(the version with a drain receipt on this OS) rather than inheriting "
            "another OS's floor"
        )
    version_parts(explicit_floor)
    return explicit_floor


def version_parts(version: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"([0-9]+)\.([0-9]+)\.([0-9]+)", version)
    if match is None:
        raise PromotionRefused(f"{version!r} is not an exact x.y.z Claude version")
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def pooled_bound(
    evidence_dir: pathlib.Path, host_os: str, host_arch: str
) -> tuple[int, pathlib.Path]:
    path = evidence_dir / f"pooled-transcript-drain-{host_os}-{host_arch}.json"
    if not path.is_file():
        raise PromotionRefused(
            f"{path} does not exist, so this OS cannot drop "
            "`--tested-claude-profile`. Confirm the binary with "
            "tools/dev/operator_eval.py instead. Do not invent this file and "
            "do not use another OS's pooled-drain receipt."
        )
    try:
        receipt = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PromotionRefused(f"{path} is unreadable as a drain receipt: {error}") from error
    rec_os = receipt.get("os")
    rec_arch = receipt.get("arch")
    if rec_os != host_os or rec_arch != host_arch:
        raise PromotionRefused(
            f"{path} is a {rec_os}/{rec_arch} drain receipt, not "
            f"{host_os}/{host_arch}. Do not copy another OS's pooled-drain "
            "receipt."
        )
    bound = receipt.get("recommended_transcript_drain_ms")
    if not isinstance(bound, int) or bound <= 0:
        raise PromotionRefused(
            f"{path} states no usable recommended_transcript_drain_ms"
        )
    return bound, path


# ---- The run ----------------------------------------------------------------


class Run:
    """Everything the checks share, and the only thing that spends a turn."""

    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.binaries = resolve_binaries(args.release_dir)
        self.claude = args.claude.resolve(strict=True)
        self.version = claude_version(self.claude)
        self.os, self.arch = host_identity()
        self.floor = promoted_version_floor(self.os, self.arch, args.floor)
        self.bound_ms, self.bound_receipt = pooled_bound(
            args.evidence_dir, self.os, self.arch
        )
        self.rng = random.Random(args.seed)
        self.real = args.driver_environment == "operator"
        self.sandbox: Sandbox | None = None
        self.daemon: Daemon | None = None
        self.turns: list[dict[str, Any]] = []
        self.evidence_dir: pathlib.Path | None = None
        self.baseline_pids: set[int] = set()

    # -- lifecycle ------------------------------------------------------------

    def start(self) -> None:
        # Sampled before anything of this run's exists, so every later reading
        # is this run's instances and not the operator's own session.
        self.baseline_pids = self._pids_of_the_executable()
        self.sandbox = Sandbox(self.args.driver_environment)
        profile = {
            "claude_version": self.version,
            "os": self.os,
            "arch": self.arch,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            # The PROBE cell the daemon boots on, not a promotion: a version
            # with no promoted profile cannot mint a minified cell at all, so
            # there is no way to measure one without giving the daemon a cell to
            # admit it under. It carries the pooled bound, so nothing here runs
            # at a drain the product does not ship.
            "transcript_drain_ms": self.bound_ms,
        }
        self.profile_probe = profile
        self.daemon = Daemon(
            self.binaries,
            self.sandbox,
            profile,
            self.claude,
            f"{self.args.model}/{self.args.efforts[0]}=1",
        )

    def stop(self) -> None:
        if self.daemon is not None:
            self.daemon.stop()
            self.daemon = None

    def cleanup(self) -> None:
        if self.sandbox is not None and not self.args.keep_sandbox:
            self.sandbox.remove()

    # -- instruments ----------------------------------------------------------

    def doctor(self) -> dict[str, Any]:
        """`pmux doctor`, read for its layers rather than for its exit status.

        `doctor` exits 1 on any finding, including ones this tool is not asking
        about -- a hermetic double sandbox has no `claude` on `PATH`, which is
        an honest complaint about the operator's shell and says nothing about
        the pool. The layers are the answer; the exit status is a different
        question and conflating them would make this tool refuse a healthy pool.
        """

        assert self.sandbox is not None
        argv = [
            str(self.binaries["pmux"]),
            "--socket",
            str(self.sandbox.socket),
            "--output",
            "json",
            "doctor",
        ]
        done = subprocess.run(
            argv,
            env=self.sandbox.environment(),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=180,
        )
        payloads = [line for line in done.stdout.splitlines() if line.startswith("{")]
        if not payloads:
            raise CheckFailed(
                f"`pmux doctor` printed no JSON:\n{done.stdout}\n{done.stderr}"
            )
        return json.loads(payloads[-1])

    def layer(self, name: str) -> dict[str, Any]:
        report = self.doctor()
        for layer in report.get("diagnosis", {}).get("layers", []):
            if layer.get("layer") == name:
                return layer
        raise CheckFailed(f"`pmux doctor` reported no `{name}` layer")

    def pool_evidence(self) -> dict[str, Any]:
        return self.layer("pool")

    def daemon_evidence_dir(self) -> pathlib.Path:
        """Where the daemon says it mirrors Path B transcripts.

        Asked of the running daemon rather than reconstructed from the socket
        path: `--pool-evidence-dir` moves it and `--pool-no-evidence` turns
        it off, so a path this tool derived itself could name a directory the
        daemon was never going to write.
        """

        evidence = self.layer("configuration").get("evidence", {})
        directory = (evidence.get("path_b") or {}).get("evidence_dir")
        if not directory:
            raise CheckFailed(
                "the daemon publishes no `path_b.evidence_dir`, so there is no "
                "corpus at this version for the drain check to read"
            )
        return pathlib.Path(directory)

    def _pids_of_the_executable(self) -> set[int]:
        """`pgrep -f '^<absolute path>'` rather than `ps | grep`: an unanchored
        pattern matches this tool's own command line, which is how a prior
        session measured a false PRESENCE of orphans
        (`docs/2.1.226-acceptance.md` sec.9)."""

        done = subprocess.run(
            ["pgrep", "-f", f"^{self.claude}"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        return {int(line) for line in done.stdout.split() if line.strip()}

    def claude_pids(self) -> list[int]:
        """This run's instances, and only this run's.

        The operator's own Claude Code execs the same versioned binary, so the
        raw pgrep set is not this pool's. `baseline_pids` is sampled before the
        daemon starts and subtracted from every later reading: without it, an
        operator with Claude open would see `nothing_survived` and the pid
        identity of `grades_answer` fail -- a refusal rather than a false
        promotion, but a promotion path nobody can run while working is a
        promotion path nobody runs.

        BOUNDED CLAIM: the baseline excludes what already existed, and cannot
        exclude a Claude the operator STARTS mid-run. One that appears between
        two grades is indistinguishable here from a leaked instance, and is
        refused as one. That is the safe direction and it is measured -- two
        concurrent runs of this tool were used as the positive control, and the
        later one's baseline held the earlier one's pid while the earlier one's
        pid set held the later one's.
        """

        return sorted(self._pids_of_the_executable() - self.baseline_pids)

    def ask(self, grade: Grade, effort: str, nonce: str) -> dict[str, Any]:
        assert self.sandbox is not None
        prompt = grade.render(nonce)
        if unicodedata.normalize("NFC", prompt) != prompt:
            raise PromotionRefused(
                f"grade {grade.id} renders a prompt that is not NFC; pmux "
                "normalizes the prompt it types and would answer a different one"
            )
        if not self.real:
            # The double answers by echo. Same client, same daemon, same pool,
            # same acknowledgement, same oracle -- only the reasoning is absent,
            # and the verdict says so rather than the oracle being weakened.
            prompt = f"PMUX_TEST_ECHO:{grade.expected(nonce)}"
            expected = f"pmux-test-echo:{grade.expected(nonce)}"
        else:
            expected = grade.expected(nonce)
        deadline = int(time.time() * 1000) + self.args.turn_deadline_ms
        started = time.monotonic()
        result, wall_ms = run_client(
            self.binaries,
            self.sandbox,
            [
                "run",
                "--model",
                self.args.model,
                "--effort",
                effort,
                "--deadline-unix-ms",
                str(deadline),
                prompt,
            ],
            timeout=self.args.turn_deadline_ms / 1000.0 + 60.0,
        )
        del started
        text = result.get("text", "")
        usage = result.get("usage") or {}
        sample = {
            "grade": grade.id,
            "effort": effort,
            "nonce": nonce,
            "client_wall_ms": round(wall_ms, 1),
            "expected": expected,
            "answered": text.strip() == expected,
            # Recorded on every turn, not only on a failure: the emptiness
            # probe reads it, and a check that fell back to `expected` when the
            # text was absent would have been asserting against its own
            # expectation. The prompts carry nothing but a nonce and an
            # arithmetic instruction, so the reply is safe to publish.
            "text": text,
            "claude_version": result.get("claude_version"),
            "model": result.get("model"),
            "reported_model": result.get("reported_model"),
            "stop_reason": result.get("stop_reason"),
            "usage": usage,
            "pids_after": self.claude_pids(),
        }
        self.turns.append(sample)
        return sample


# ---- The checks, in the order they run --------------------------------------


def check_version_identity(run: Run) -> dict[str, Any]:
    """Free. A patch inside the promoted minor line widens a range; a new minor
    line needs its own floor, its own receipt and its own campaign."""

    if not run.real:
        return {
            "not_applicable": "the test double reports the 9.9.9 zero-latency "
            "sentinel, which is deliberately not a Claude Code version line",
            "claude_version": run.version,
            "promoted_floor": run.floor,
        }
    target = version_parts(run.version)
    floor = version_parts(run.floor)
    same_line = target[:2] == floor[:2]
    if not same_line:
        raise CheckFailed(
            f"{run.version} is not on the same major.minor line as the promoted "
            f"floor {run.floor}. `RepromotionTrigger::MajorOrMinorVersionChange` "
            "is a policy default, not a measurement: promote a new line against "
            "its own floor rather than widening this range across it"
        )
    if target < floor:
        raise CheckFailed(
            f"{run.version} is below the promoted floor {run.floor}; the range "
            "widens forward from a measured floor and never backward"
        )
    return {
        "claude_version": run.version,
        "promoted_floor": run.floor,
        "same_major_minor_line": same_line,
    }


def check_launch_bundle_parses(run: Run) -> dict[str, Any]:
    """Free, and executes nothing. `claude <FLAG> [value] --pmux-probe-sentinel
    doctor`: commander reports the FIRST unknown option and exits 1 before
    running anything, so an accepted flag names the sentinel and a rejected one
    names itself."""

    if not run.real:
        return {
            "not_applicable": "the test double is not a commander CLI, so a "
            "parse probe against it would measure this tool rather than Claude",
            "flags": list(MINIFIED_LAUNCH_FLAGS),
        }
    valueless = valueless_bundle_flags()
    unknown = [
        flag
        for flag in MINIFIED_LAUNCH_FLAGS
        if flag not in PROBE_VALUES and flag not in valueless
    ]
    if unknown:
        raise PromotionRefused(
            f"the minified launch bundle gained {unknown}, and this tool has "
            "neither a probe value for it nor a statement that it takes none. "
            "Give it one rather than letting an unprobed flag be reported as "
            "accepted"
        )
    probed: dict[str, str] = {}
    for flag in MINIFIED_LAUNCH_FLAGS:
        argv = [str(run.claude), flag]
        if flag in PROBE_VALUES:
            argv.append(PROBE_VALUES[flag])
        argv += [PROBE_SENTINEL, "doctor"]
        done = subprocess.run(argv, capture_output=True, text=True, timeout=120)
        reply = (done.stdout + done.stderr).strip()
        if PROBE_SENTINEL in reply and flag not in reply:
            probed[flag] = "accepted"
        elif flag in reply:
            probed[flag] = "REJECTED"
        else:
            probed[flag] = f"unreadable: {reply[:200]!r}"
    rejected = {flag: state for flag, state in probed.items() if state != "accepted"}
    if rejected:
        raise CheckFailed(
            f"the minified launch bundle is not parsed at {run.version}: {rejected}"
        )
    control = subprocess.run(
        [str(run.claude), "--definitely-not-a-flag", PROBE_SENTINEL, "doctor"],
        capture_output=True,
        text=True,
        timeout=120,
    )
    control_reply = (control.stdout + control.stderr).strip()
    if "--definitely-not-a-flag" not in control_reply:
        raise CheckFailed(
            "the negative control was not rejected, so this probe cannot "
            f"distinguish an accepted flag from an ignored one: {control_reply[:200]!r}"
        )
    return {"probed": probed, "negative_control_rejected": True}


def check_minified_cell_is_admitted(run: Run) -> dict[str, Any]:
    """Free. The declared warm instance is a `SessionCell::Minified`
    registration: it passes `require_tested_for_minified_cell`, proves its
    launch transcript empty, and reaches `idle` only after pmux's own emulator
    found a ready composer. One census read establishes all three."""

    deadline = time.monotonic() + run.args.warm_timeout_ms / 1000.0
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        layer = run.pool_evidence()
        last = layer.get("evidence", {})
        if last.get("halted"):
            raise CheckFailed(
                f"the pool HALTED before serving a turn: {last['halted']}"
            )
        if last.get("idle", 0) >= 1:
            return {
                "finding": layer.get("finding"),
                "census": last,
                "claude_pids": run.claude_pids(),
            }
        time.sleep(0.1)
    raise CheckFailed(
        "the declared warm minified instance never reached `idle` within "
        f"{run.args.warm_timeout_ms} ms; census was {last}"
    )


def check_grades_answer(run: Run) -> dict[str, Any]:
    """PAID. The grade suite, then one arithmetic grade per additional effort.

    Recycle is grade 02 (emptiness after `/clear`), not pgrep. linux 2.1.233
    and 2.1.236 replace the Claude OS pid across `pmux run` turns
    (`evidence/linux-operator-eval-2.1.236-x86_64.json` `process_tree_note`;
    promotion-2.1.236-linux first pass: one new pid per grade). Product
    identity is the pool census (`leaked=0`, `halted` null) plus the emptiness
    probe. Pids are recorded, not asserted. (2026-09-01: that per-turn pid
    change was the post-turn `/clear` refusing `menu_not_rendered` and the pool
    reminting; fixed for the menu-above-composer layout, and the 2.1.257
    promotion shows one pid across four turns.)
    """

    effort = run.args.efforts[0]
    before = run.claude_pids()
    samples = [run.ask(grade, effort, _nonce(run.rng)) for grade in GRADES]
    wrong = [s["grade"] for s in samples if not s["answered"]]
    if wrong:
        raise CheckFailed(
            f"{len(wrong)} grade(s) did not return the exact reply their prompt "
            f"specified: {wrong}"
        )
    extra = []
    for other in run.args.efforts[1:]:
        extra.append(run.ask(GRADES[0], other, _nonce(run.rng)))
    wrong_extra = [s["effort"] for s in extra if not s["answered"]]
    if wrong_extra:
        raise CheckFailed(f"effort(s) {wrong_extra} did not answer their grade")
    census = run.pool_evidence().get("evidence", {})
    if census.get("halted"):
        raise CheckFailed(f"the pool HALTED during the suite: {census['halted']}")
    if census.get("leaked"):
        raise CheckFailed(f"the pool leaked {census['leaked']} slot(s) during the suite")
    return {
        "suite_effort": effort,
        "pids_before": before,
        "pids_after_each_turn": [s["pids_after"] for s in samples + extra],
        "pid_identity_is_observational": True,
        "grades": [s["grade"] for s in samples],
        "additional_efforts": [s["effort"] for s in extra],
        "turns": len(samples) + len(extra),
        "pool_after_suite": {
            "idle": census.get("idle"),
            "live": census.get("live"),
            "leaked": census.get("leaked"),
            "halted": census.get("halted"),
        },
    }


def check_context_did_not_survive_recycling(run: Run) -> dict[str, Any]:
    """No extra turn. Grade 02 asked the instance that had just answered grade
    01 whether it had been given a nonce. `docs/path-b.md` sec.0.3 rule 5: this
    is evidence because a surviving context WOULD have shown a presence -- the
    same process, the same composer, one `/clear` apart."""

    first = next(s for s in run.turns if s["grade"] == GRADES[0].id)
    probe = next(s for s in run.turns if s["grade"] == GRADES[1].id)
    leaked = [
        needle
        for needle in (first["nonce"], first["expected"])
        if needle in probe["text"]
    ]
    if leaked:
        raise CheckFailed(f"turn 1's own bytes came back after `/clear`: {leaked}")
    return {
        "pids_after_nonce_turn": first["pids_after"],
        "pids_after_emptiness_probe": probe["pids_after"],
        "pid_identity_is_observational": True,
        "prior_nonce": first["nonce"],
        "probe_answered": probe["expected"],
    }


def check_no_tool_surface(run: Run) -> dict[str, Any]:
    """No extra turn. Every result's sidechain block is structurally zero and
    nothing was cached: a Path B cell has no tools, no CLAUDE.md and no skills,
    so there is nothing to cache and no subagent to bill."""

    offenders = []
    for sample in run.turns:
        usage = sample["usage"]
        sidechain = usage.get("sidechain") or {}
        if any(value for value in sidechain.values()):
            offenders.append((sample["grade"], "sidechain", sidechain))
        for field in ("cache_creation_input_tokens", "cache_read_input_tokens"):
            if usage.get(field):
                offenders.append((sample["grade"], field, usage[field]))
    if offenders:
        raise CheckFailed(
            f"the cell billed work a minified cell cannot do: {offenders}"
        )
    return {
        "turns_checked": len(run.turns),
        "sidechain_all_zero": True,
        "cache_creation_and_read_all_zero": True,
    }


def check_pool_never_halted(run: Run) -> dict[str, Any]:
    """No extra turn. `is_a_version_drift_signal` HALTS the whole pool when the
    transcript `/clear` opened is not the preamble pmux measured. The suite
    recycled the instance once per turn, so a preamble that moved at this
    version would have halted the pool before the last grade answered."""

    evidence = run.pool_evidence().get("evidence", {})
    if evidence.get("halted"):
        raise CheckFailed(f"the pool is HALTED: {evidence['halted']}")
    if evidence.get("leaked"):
        raise CheckFailed(f"the pool leaked {evidence['leaked']} slot(s)")
    return {
        "halted": None,
        "leaked": evidence.get("leaked"),
        "recycles_survived": len(run.turns),
    }


def check_drain_within_the_pooled_bound(run: Run) -> dict[str, Any]:
    """Free, and reads what the turns above already wrote. `Pool::destroy`
    mirrors every destroyed instance's transcript, pruned to
    `evidence::RETAINED_ROW_FIELDS`, into the daemon's evidence corpus. Running
    the drain tool over that mirror at the TARGET version is
    `RepromotionTrigger::ReachableArrivalAboveTheBound`'s own detector, and exit
    5 -- nothing to check -- is a FAILURE here, because a promotion that
    measured no arrival at the version it promotes has measured nothing."""

    assert run.sandbox is not None
    corpus = run.daemon_evidence_dir()
    run.stop()  # the mirror is written in `Pool::destroy`, at teardown.
    if not corpus.is_dir():
        raise CheckFailed(
            f"the daemon wrote no evidence mirror at {corpus}; there is no "
            "corpus at this version to measure the drain over"
        )
    run.evidence_dir = corpus
    if not run.real:
        return {
            "not_applicable": "the test double writes no `version` row, so a "
            "version-keyed corpus measurement would be measuring nothing",
            "mirrored_files": len(list(corpus.rglob("*.jsonl"))),
        }
    argv = [
        sys.executable,
        str(DRAIN_TOOL),
        "--corpus",
        str(corpus),
        "--version",
        run.version,
        "--bound-ms",
        str(run.bound_ms),
        "--json",
    ]
    done = subprocess.run(argv, capture_output=True, text=True, timeout=600)
    if done.returncode != 0:
        raise CheckFailed(
            f"{' '.join(argv)} exited {done.returncode}; exit 4 is an arrival "
            "above the bound, exit 5 is an empty corpus and exit 2/3 is an "
            f"unclassified row kind:\n{done.stdout[-4000:]}\n{done.stderr[-2000:]}"
        )
    measured = json.loads(done.stdout)
    reachable = measured["post_answer_arrivals"]["reachable_on_a_minified_cell"]
    if not reachable or not reachable.get("count"):
        raise CheckFailed(
            "the corpus at this version holds no reachable post-answer arrival, "
            "so the bound was not checked against anything"
        )
    return {
        "argv": argv[1:],
        "pooled_bound_ms": run.bound_ms,
        "pooled_bound_receipt": str(run.bound_receipt.relative_to(REPOSITORY_ROOT)),
        "files_at_this_version": measured["corpus"]["files_at_these_versions"],
        # The drain tool's own count of transcript turns in the mirror, which is
        # NOT this run's turn count and must not be quoted as one: a `/clear`
        # rotates the transcript, so one instance leaves more files than it
        # served turns. `real_claude_turns` is the turn count.
        "transcript_turns_in_the_mirror": measured["corpus"]["turns"],
        "reachable_post_answer_arrivals": reachable,
        "full_drain_binds_on": measured["full_drain_binds_on"],
        "per_version_recommendations_not_to_be_shipped": measured[
            "per_version_recommendations_not_to_be_shipped"
        ],
        "note": "the per-version fit above is published for the same reason "
        "measure_transcript_drain.py publishes it -- to be read and NOT "
        "shipped. This tool asserts only that the pooled bound was not exceeded.",
    }


def check_nothing_survived(run: Run) -> dict[str, Any]:
    """Free. The daemon is already stopped. Nothing of the executable under
    test may still be running, and the pool parent must hold no epoch tree."""

    assert run.sandbox is not None
    deadline = time.monotonic() + 15.0
    survivors = run.claude_pids()
    while survivors and time.monotonic() < deadline:
        time.sleep(0.2)
        survivors = run.claude_pids()
    if survivors:
        raise CheckFailed(
            f"{len(survivors)} process(es) of {run.claude} survived teardown"
        )
    trees = sorted(
        str(path.relative_to(run.sandbox.root))
        for path in (run.sandbox.root / "pool").glob("*/*")
    )
    if trees:
        raise CheckFailed(f"the pool parent still holds epoch tree(s): {trees}")
    return {"surviving_processes": 0, "pool_parent_epoch_trees": 0}


CHECKS: tuple[Check, ...] = (
    Check(
        id="version_identity",
        exercises=("major_or_minor_version_change",),
        costs_real_turns=False,
        criterion="the target is an exact x.y.z on the same major.minor line as "
        "the promoted floor, and at or above it",
        run=check_version_identity,
    ),
    Check(
        id="launch_bundle_parses",
        exercises=("launch_bundle_rejected",),
        costs_real_turns=False,
        criterion="every flag in `MINIFIED_LAUNCH_FLAGS` is parsed by the target "
        "binary, and a known-bad flag is still rejected",
        run=check_launch_bundle_parses,
    ),
    Check(
        id="minified_cell_is_admitted",
        exercises=("launch_bundle_rejected", "clear_screen_or_preamble_mismatch"),
        costs_real_turns=False,
        criterion="a declared warm `SessionCell::Minified` instance reaches "
        "`idle`, with the pool neither halted nor leaking",
        run=check_minified_cell_is_admitted,
    ),
    Check(
        id="grades_answer",
        exercises=(),
        costs_real_turns=True,
        criterion="every graded prompt returns the exact reply it specified; "
        "`/clear` recycle is the emptiness probe (grade 02), not pgrep",
        run=check_grades_answer,
    ),
    Check(
        id="context_did_not_survive_recycling",
        exercises=("clear_screen_or_preamble_mismatch",),
        costs_real_turns=False,
        criterion="the emptiness probe did not return the prior turn's nonce "
        "(pgrep pid-set is observational on linux)",
        run=check_context_did_not_survive_recycling,
    ),
    Check(
        id="no_tool_surface",
        exercises=(),
        costs_real_turns=False,
        criterion="`sidechain` is all-zero and `cache_creation`/`cache_read` are "
        "zero on every result",
        run=check_no_tool_surface,
    ),
    Check(
        id="pool_never_halted",
        exercises=("clear_screen_or_preamble_mismatch",),
        costs_real_turns=False,
        criterion="`halted` is null and `leaked` is 0 after one `/clear` per turn",
        run=check_pool_never_halted,
    ),
    Check(
        id="drain_within_the_pooled_bound",
        exercises=(
            "reachable_arrival_above_the_bound",
            "unclassified_transcript_row_kind",
        ),
        costs_real_turns=False,
        criterion="`measure_transcript_drain.py --version <target> --bound-ms "
        "<pooled>` exits 0 over the daemon's own evidence mirror AND the corpus "
        "holds at least one reachable arrival at that version",
        run=check_drain_within_the_pooled_bound,
    ),
    Check(
        id="nothing_survived",
        exercises=(),
        costs_real_turns=False,
        criterion="no process of the target binary survives and the pool parent "
        "holds no epoch tree",
        run=check_nothing_survived,
    ),
)


def _require_every_repromotion_trigger_is_exercised() -> list[str]:
    declared = repromotion_trigger_ids()
    exercised = {trigger for check in CHECKS for trigger in check.exercises}
    missing = sorted(set(declared) - exercised)
    invented = sorted(exercised - set(declared))
    if missing:
        raise PromotionRefused(
            "this promotion path exercises no check for re-promotion trigger(s) "
            f"{missing}. A trigger nothing here exercises is a condition a "
            "promotion would not have looked for"
        )
    if invented:
        raise PromotionRefused(
            f"CHECKS claim to exercise trigger(s) {invented} that "
            "`RepromotionTrigger::detector` does not define"
        )
    return declared


def describe() -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "algorithm": PROMOTION_ALGORITHM,
        "repromotion_triggers": _require_every_repromotion_trigger_is_exercised(),
        "grades": [
            {"id": grade.id, "proves": grade.proves, "requires_a_tool": False}
            for grade in GRADES
        ],
        "checks": [
            {
                "order": index,
                "id": check.id,
                "exercises": list(check.exercises),
                "costs_real_turns": check.costs_real_turns,
                "criterion": check.criterion,
            }
            for index, check in enumerate(CHECKS, start=1)
        ],
    }


# The FLOOR half of `range_provenance`, keyed by the cell this run is widening.
# A constant per cell rather than a generated sentence because this tool does
# not re-measure the floor: a run that widens a range forward has no evidence
# about where the range starts, and generating a fresh sentence about it would
# be this repository's own bug class -- prose asserting more than its predicate
# tested. Re-promoting the FLOOR is a different job with a different receipt.
# A first promotion on an os/arch (no shipped cell yet) uses FIRST_FLOOR_PROVENANCE.
FLOOR_PROVENANCE = {
    ("macos", "aarch64", "2.1.220"): (
        "floor 2.1.220: the version with a drain receipt, a Gate B campaign and the "
        "screen/preamble measurements; below it 2.1.201 and earlier have ZERO "
        "reachable cli arrivals, which is unestablished rather than safe."
    ),
    ("linux", "x86_64", "2.1.227"): (
        "floor 2.1.227: first linux/x86_64 Path B drain receipt "
        "(evidence/promoted-profile-2.1.227-linux-x86_64.json, max reachable 46 ms) "
        "pooled with 2.1.232/2.1.233 in evidence/pooled-transcript-drain-linux-x86_64.json; "
        "below it linux minified cells were not measured as a promotion floor."
    ),
}


def floor_provenance(run: Run) -> str:
    """The first sentence of `range_provenance` for this run's cell."""

    key = (run.os, run.arch, run.floor)
    text = FLOOR_PROVENANCE.get(key)
    if text is not None:
        return text
    shipped = any(
        profile["os"] == run.os and profile["arch"] == run.arch
        for profile in promoted_profiles()
    )
    if shipped:
        raise PromotionRefused(
            f"the promoted floor is {run.floor} on {run.os}/{run.arch} and "
            "FLOOR_PROVENANCE has no sentence for that cell, so this run would "
            "ship a range whose first half is about a version nobody measured"
        )
    return (
        f"floor {run.floor}: first promoted cell on {run.os}/{run.arch}; the "
        "version with a drain receipt on this OS. Not macos Gate B, not another "
        "OS's floor."
    )


def range_provenance(run: Run, results: list[dict[str, Any]]) -> str:
    """The `PromotedProfile::range_provenance` this run's evidence supports.

    GENERATED, because the two things that string has been wrong about are
    both the house bug class: it once claimed more than had been measured, and
    then -- after `docs/2.1.226-acceptance.md` measured the drain -- it claimed
    less. A sentence assembled from the check results cannot do either.
    """

    floor_text = floor_provenance(run)
    if f"floor {run.floor}:" not in floor_text:
        raise PromotionRefused(
            f"the promoted floor is {run.floor} and floor_provenance describes a "
            "different one, so this run would ship a range whose first half is "
            "about a version nobody measured"
        )
    by_id = {result["id"]: result for result in results}
    drain = by_id["drain_within_the_pooled_bound"]["detail"]
    grades = by_id["grades_answer"]["detail"]
    reachable = drain["reachable_post_answer_arrivals"]
    efforts = "/".join([grades["suite_effort"], *grades["additional_efforts"]])
    fit = drain["per_version_recommendations_not_to_be_shipped"].get(run.version)
    return (
        f"{floor_text} Tested through {run.version}: "
        f"promote_claude_version.py drove {len(run.turns)} minified-cell turns "
        f"through `pmux run` at {run.args.model} {efforts} -- every graded reply "
        "exact, the four-grade suite answered across a "
        "`/clear` per turn, sidechain and cache zero on every result, the pool "
        f"never halted -- and measured {reachable['count']} reachable post-answer "
        f"arrival(s) at this version, max {reachable['max_ms']} ms against the "
        f"pooled {drain['pooled_bound_ms']} ms bound. NOT measured at "
        f"{run.version}: anything outside a minified cell on {run.os}/{run.arch}, "
        f"and the per-version fit of {fit} ms, which is published to be read and "
        "NOT shipped."
    )


def execute(args: argparse.Namespace) -> tuple[dict[str, Any], int]:
    triggers = _require_every_repromotion_trigger_is_exercised()
    run = Run(args)
    results: list[dict[str, Any]] = []
    failure: str | None = None
    started = time.monotonic()
    try:
        run.start()
        for check in CHECKS:
            began = time.monotonic()
            try:
                detail = check.run(run)
            except CheckFailed as error:
                results.append(
                    {
                        "id": check.id,
                        "outcome": "FAILED",
                        "exercises": list(check.exercises),
                        "criterion": check.criterion,
                        "why": str(error),
                        "elapsed_ms": round((time.monotonic() - began) * 1000.0),
                    }
                )
                failure = check.id
                break
            results.append(
                {
                    "id": check.id,
                    "outcome": "passed",
                    "exercises": list(check.exercises),
                    "criterion": check.criterion,
                    "detail": detail,
                    "elapsed_ms": round((time.monotonic() - began) * 1000.0),
                }
            )
    finally:
        run.stop()
        run.cleanup()

    promotable = failure is None and run.real
    receipt: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "algorithm": PROMOTION_ALGORITHM,
        "measured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "elapsed_ms": round((time.monotonic() - started) * 1000.0),
        "verdict": (
            "promotable"
            if promotable
            else ("rehearsal" if failure is None else "REFUSED")
        ),
        "failed_check": failure,
        "host": {
            "os": platform.system().lower(),
            "arch": platform.machine(),
            "release": platform.release(),
            "cpu_count": os.cpu_count(),
            "load_average_1m": os.getloadavg()[0],
        },
        "driver": {
            "environment": args.driver_environment,
            "claude_executable": str(run.claude),
            "claude_version": run.version,
            "model": args.model,
            "efforts": list(args.efforts),
            "compatibility_probe_profile": getattr(run, "profile_probe", None),
            # Processes of this exact executable that were already running when
            # the run started, and are subtracted from every pid reading. A
            # non-empty set means the operator had their own Claude Code open;
            # it does not weaken any check, and recording it is what makes that
            # claim checkable rather than assumed.
            "pre_existing_pids_of_this_executable": sorted(run.baseline_pids),
        },
        "repromotion_triggers_this_path_exercises": triggers,
        "checks": results,
        # Real model round trips this run made. The historical attempt ledger
        # is frozen; this key is still published so a reader can see how many
        # turns this promotion spent.
        "real_claude_turns": {
            "count": len(run.turns) if run.real else 0,
            "reserved_ledger_ordinals": 0,
            "why": "`pmux run` reserves nothing on the frozen attempt ledger.",
        },
        "turns": run.turns,
        # Derived from the check table rather than written out: a check added
        # above appears here without anyone remembering, which is the whole
        # reason the criteria live on `Check` rather than in this receipt.
        "what_would_invalidate_it": [
            f"{check.id}: {check.criterion}" for check in CHECKS
        ]
        + [
            "a different `recommended_transcript_drain_ms` in "
            f"{run.bound_receipt.name}: the bound asserted against is that "
            "receipt's, never a fit taken here",
            "a different Claude executable, version, OS or arch: the identity is "
            "the key, exactly as it is for a compatibility profile",
            "a loaded host: `load_average_1m` is recorded for exactly this reason",
        ],
    }
    if promotable:
        receipt["profile"] = {
            "claude_version": run.floor,
            "claude_version_tested_through": run.version,
            "os": run.os,
            "arch": run.arch,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": run.bound_ms,
        }
        receipt["range_provenance"] = range_provenance(run, results)
    else:
        receipt["profile"] = None
        receipt["range_provenance"] = None
    return receipt, (0 if failure is None else 1)


def _parse(arguments: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--describe", action="store_true", help="print the ordered check list and exit"
    )
    parser.add_argument("--release-dir", type=pathlib.Path)
    parser.add_argument("--claude", type=pathlib.Path)
    parser.add_argument("--model", default="claude-sonnet-5")
    parser.add_argument(
        "--effort",
        dest="efforts",
        action="append",
        help="repeatable; the first runs the whole grade suite and each "
        "additional one runs grade 01. Default: low, high",
    )
    parser.add_argument(
        "--evidence-dir",
        type=pathlib.Path,
        default=REPOSITORY_ROOT / "evidence",
        help="where the POOLED drain receipt this tool reads its bound from lives",
    )
    parser.add_argument("--turn-deadline-ms", type=int, default=180_000)
    parser.add_argument("--warm-timeout-ms", type=int, default=180_000)
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument(
        "--floor",
        default=None,
        help="claude_version_floor for a first promotion on this os/arch. "
        "Required when PROMOTED_PROFILES has no cell for this host; must match "
        "the shipped floor when a cell already exists. Do not pass another "
        "OS's floor.",
    )
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--keep-sandbox", action="store_true")
    parser.add_argument(
        "--driver-environment",
        dest="driver_environment",
        choices=("double", "operator"),
        default="operator",
    )
    args = parser.parse_args(arguments)
    if args.efforts is None:
        args.efforts = ["low", "high"]
    if not args.describe:
        for name in ("release_dir", "claude"):
            if getattr(args, name) is None:
                parser.error(
                    f"--{name.replace('_', '-')} is required unless --describe"
                )
    return args


def main(arguments: list[str] | None = None) -> int:
    args = _parse(arguments)
    try:
        if args.describe:
            print(json.dumps(describe(), indent=1, sort_keys=True))
            return 0
        receipt, status = execute(args)
    except PromotionRefused as error:
        print(f"promotion refused: {error}", file=sys.stderr)
        return 2
    except MeasurementError as error:
        print(f"promotion refused: {error}", file=sys.stderr)
        return 2
    # See `measure_turn_latency.main`: the whole document, at the one point it
    # becomes bytes. This receipt reaches a path three ways -- the Claude
    # executable it drove, the drain tool it shelled out to, and the sandbox it
    # built -- and only the first two are obvious from the field names.
    encoded = json.dumps(
        portable_paths.render_document(receipt), indent=1, sort_keys=True
    )
    if args.output:
        args.output.write_text(encoded + "\n", encoding="utf-8")
    if args.json or not args.output:
        print(encoded)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
