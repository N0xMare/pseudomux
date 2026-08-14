#!/usr/bin/env python3
"""The owner's five Path B criteria, executed instead of read.

WHY THIS EXISTS
---------------

The five criteria have been "verified" three times by an agent reading the tree
and writing a paragraph, and the three answers disagreed. A criterion checked by
reading drifts the moment the thing it describes moves, and nothing says so. So
each one is a function here, each function READS EVIDENCE -- a receipt, a
register, a version string, a test run -- and the whole thing exits 0 only when
all five hold.

WHERE THE SET OF CRITERIA COMES FROM
------------------------------------

Not from this file. `docs/path-b-verdict.md` section 1 is where the criteria are
written down, and [`criteria_in`] reads them out of it: the ordinal and the title
of every `###` heading under that section. [`CRITERIA`] below binds each of those
titles to the function that measures it, and [`bind`] refuses -- exit 2, before a
single check runs -- if the two sets differ in either direction. A sixth
criterion added to the document is therefore a refusal, not a silent omission,
and a check here that no longer matches a title in the document is the same
refusal from the other side. The section heading's own count word ("The five
criteria") is checked against the number of headings under it, because a census
nobody recomputes is the first thing to go stale.

The verdict text in those headings -- `**MET**`, `**NOT MET at 28bd6b2**` -- is a
dated record of what was true when the paragraph under it was written. Nothing
here reads it. The verdict comes from the measurement.

FAIL CLOSED
-----------

A criterion whose evidence is missing, unreadable, or stale is NOT MET. It is
never "assumed fine" and never skipped: the tool this replaces exited 0 meaning
"there was nothing to check", at exactly the version nobody had measured. The
one thing that is not a verdict is a fault that stops the gate deciding at all
(a criteria set that does not bind, a document whose structure this cannot
parse); those exit 2 and say so.

EXIT CODES

    0  every criterion MET
    1  at least one NOT MET, each named with why
    2  the gate could not decide (drift, malformed input, no repository)
    3  a partial run under `--only`, which is never a verdict
"""

import argparse
import dataclasses
import datetime as dt
import hashlib
import importlib.util
import json
import os
import pathlib
import platform
import re
import shlex
import subprocess
import sys
from collections.abc import Callable

CRITERIA_DOCUMENT = "docs/path-b-verdict.md"
DEFECT_REGISTER = "evidence/path-b-defect-register.json"
DEFECT_REGISTER_SCHEMA = "pmux.path-b-defect-register.v1"
SURVIVOR_REGISTER = "evidence/mutation-survivor-register.json"
ENUMERATION_CENSUS = "evidence/mutation-enumeration.json"
MUTATION_REGISTER_TOOL = "scripts/mutation_register.py"
REGISTER_CURRENCY_TOOL = "scripts/register_currency.py"
DEBT_DOCUMENT = "docs/current-state.md"
ADVERSARIAL_DOCUMENT = "docs/path-b-adversarial.md"
COMPATIBILITY_SOURCE = "crates/service/src/compatibility.rs"
PROMOTION_TOOL = "tools/promotion/promote_claude_version.py"
MANIFEST = "tools/gate-a-candidate/phase-manifest.json"
CITATION_GRADER = "crates/service/tests/path_b_doc_citations.rs"
GATE_DRIVER = "tools/gate-a/run_gate.py"
PINNED_RUNNER = "scripts/gate-in-worktree.sh"
PINNED_RUN_SCHEMA = "pmux.pinned-worktree-run.v1"

# Rust's `std::env::consts` spelling of this host, which is what a promoted
# profile is written in. Python spells the same two things differently, so the
# translation is stated once and an unmapped host REFUSES rather than guessing:
# a profile matched against the wrong spelling of an architecture is a promotion
# for a machine nobody ran.
RUST_OS = {"darwin": "macos", "linux": "linux"}
RUST_ARCH = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}
COUNT_WORDS = {
    "one": 1, "two": 2, "three": 3, "four": 4, "five": 5,
    "six": 6, "seven": 7, "eight": 8, "nine": 9, "ten": 10,
}  # fmt: skip
DEFECT_STATUSES = ("OPEN", "CLOSED", "ACCEPTED")
COMMAND_TIMEOUT_SECONDS = 3600.0
VERSION_TIMEOUT_SECONDS = 60.0


class DoneGateError(RuntimeError):
    """The gate could not decide. Never a verdict; always exit 2."""


@dataclasses.dataclass
class Verdict:
    """One criterion's measurement: whether it holds, and what was read."""

    met: bool = True
    failures: list[str] = dataclasses.field(default_factory=list)
    evidence: list[str] = dataclasses.field(default_factory=list)
    # Printed after the refusals, because a remedy above the reason it answers
    # reads as an instruction nobody asked for. Never a measurement: nothing
    # here decides `met`.
    remedy: list[str] = dataclasses.field(default_factory=list)

    def refuse(self, why: str) -> None:
        self.met = False
        self.failures.append(why)

    def note(self, key: str, value: object) -> None:
        self.evidence.append(f"{key}={value}")

    def suggest(self, line: str) -> None:
        self.remedy.append(line)


@dataclasses.dataclass(frozen=True)
class Context:
    """Everything a check may read, resolved once and never re-derived."""

    repo: pathlib.Path
    commit: str
    cargo: str
    claude: str
    receipts: tuple[pathlib.Path, ...]
    max_receipt_age_days: float
    now: dt.datetime


@dataclasses.dataclass(frozen=True)
class Criterion:
    ordinal: int
    title: str
    check: Callable[[Context], Verdict]


# ---------------------------------------------------------------------------
# Reading the tree
# ---------------------------------------------------------------------------


def read_text(repo: pathlib.Path, relative: str) -> str:
    path = repo / relative
    try:
        return path.read_text(encoding="utf-8")
    except OSError as error:
        raise DoneGateError(f"could not read {relative}: {error}") from error


def load_json(path: pathlib.Path) -> dict:
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def sha256_of(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def git(repo: pathlib.Path, *arguments: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *arguments],
        capture_output=True,
        text=True,
        timeout=120,
    )


def git_output(repo: pathlib.Path, *arguments: str) -> str:
    done = git(repo, *arguments)
    if done.returncode != 0:
        raise DoneGateError(
            f"git {' '.join(arguments)} failed: {done.stderr.strip() or done.returncode}"
        )
    return done.stdout


def strip_code_fences(text: str) -> str:
    """Fenced blocks quote transcripts, so their backticks name nothing."""

    return re.sub(r"^```.*?^```", "", text, flags=re.MULTILINE | re.DOTALL)


def inline_code(text: str) -> set[str]:
    return set(re.findall(r"`([^`\n]+)`", strip_code_fences(text)))


def section_of(text: str, opener: str, closer: str, where: str) -> str:
    """The body between one heading and the next one at its level."""

    start = re.search(opener, text, re.MULTILINE)
    if start is None:
        raise DoneGateError(f"{where} has no heading matching {opener!r}")
    rest = text[start.end() :]
    stop = re.search(closer, rest, re.MULTILINE)
    return rest if stop is None else rest[: stop.start()]


def markdown_rows(text: str) -> list[list[str]]:
    """Every table row, as its cells, from a document with fences removed."""

    rows = []
    for line in strip_code_fences(text).splitlines():
        line = line.strip()
        if not line.startswith("|") or not line.endswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if all(set(cell) <= set("-: ") for cell in cells):
            continue
        rows.append(cells)
    return rows


# ---------------------------------------------------------------------------
# The criteria set, read out of the document that states it
# ---------------------------------------------------------------------------


def criteria_section(document: str) -> str:
    return section_of(document, r"^## 1\. .*$", r"^## \d", CRITERIA_DOCUMENT)


def criteria_in(document: str) -> list[tuple[int, str]]:
    """The ordinal and title of every criterion the document publishes.

    A heading is `### <n>. <title> — <a dated verdict>`; only the ordinal and
    the title are read. The em dash and everything after it is the record of
    what was true on the day the section under it was written, and reading a
    verdict out of a document is the thing this tool exists to stop doing.
    """

    heading = re.search(r"^## 1\. (.+)$", document, re.MULTILINE)
    if heading is None:
        raise DoneGateError(f"{CRITERIA_DOCUMENT} has no section 1 heading")
    body = criteria_section(document)
    found = []
    for match in re.finditer(r"^### (\d+)\. (.+)$", body, re.MULTILINE):
        title = match.group(2).split(" — ")[0].strip()
        found.append((int(match.group(1)), title))
    if not found:
        raise DoneGateError(f"{CRITERIA_DOCUMENT} section 1 publishes no criteria")
    if [ordinal for ordinal, _ in found] != list(range(1, len(found) + 1)):
        raise DoneGateError(
            "the criteria are numbered "
            f"{[ordinal for ordinal, _ in found]}, which is not 1..n"
        )
    published = [
        COUNT_WORDS[word]
        for word in re.findall(r"[a-z]+", heading.group(1).lower())
        if word in COUNT_WORDS
    ]
    for count in published:
        if count != len(found):
            raise DoneGateError(
                f"{CRITERIA_DOCUMENT} section 1 is titled {heading.group(1)!r} and "
                f"holds {len(found)} criteria"
            )
    return found


def bind(document: str, implemented: list[Criterion]) -> list[Criterion]:
    """Every published criterion has a check here, and every check a criterion."""

    published = criteria_in(document)
    here = [(criterion.ordinal, criterion.title) for criterion in implemented]
    unchecked = [entry for entry in published if entry not in here]
    invented = [entry for entry in here if entry not in published]
    if unchecked or invented:
        for ordinal, title in unchecked:
            print(
                f"criterion {ordinal} ({title!r}) is published by {CRITERIA_DOCUMENT} "
                "and nothing here measures it",
                file=sys.stderr,
            )
        for ordinal, title in invented:
            print(
                f"this tool measures a criterion {CRITERIA_DOCUMENT} does not "
                f"publish: {ordinal} ({title!r})",
                file=sys.stderr,
            )
        raise DoneGateError(
            "the criteria this tool checks and the criteria the document states "
            "must be the same set"
        )
    order = {entry: position for position, entry in enumerate(published)}
    return sorted(implemented, key=lambda c: order[(c.ordinal, c.title)])


# ---------------------------------------------------------------------------
# Criterion 1 -- no known unfixed defect in the Path B path
# ---------------------------------------------------------------------------


def lettered_defects(document: str) -> set[str]:
    """The `**(a)**`-style defect list inside criterion 1's own section."""

    body = criteria_section(document)
    parts = re.split(r"^### \d+\. .+$", body, flags=re.MULTILINE)
    if len(parts) < 2:
        raise DoneGateError(f"{CRITERIA_DOCUMENT} section 1 has no subsections")
    return set(re.findall(r"^\*\*\(([a-z])\)", parts[1], re.MULTILINE))


def import_module(path: pathlib.Path, name: str):
    specification = importlib.util.spec_from_file_location(name, path)
    if specification is None or specification.loader is None:
        raise DoneGateError(f"could not import {path}")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def check_defect_register(context: Context, verdict: Verdict) -> None:
    """The register of defects, held to the document that lists four of them."""

    path = context.repo / DEFECT_REGISTER
    if not path.exists():
        verdict.refuse(
            f"there is no open-defect register at {DEFECT_REGISTER}; a criterion "
            "with no evidence is not met"
        )
        return
    try:
        register = load_json(path)
    except (OSError, ValueError) as error:
        verdict.refuse(f"{DEFECT_REGISTER} is unreadable: {error}")
        return
    if register.get("schema") != DEFECT_REGISTER_SCHEMA:
        verdict.refuse(
            f"{DEFECT_REGISTER} declares schema {register.get('schema')!r}, not "
            f"{DEFECT_REGISTER_SCHEMA!r}"
        )
        return
    entries = register.get("entries")
    if not isinstance(entries, list) or not entries:
        verdict.refuse(f"{DEFECT_REGISTER} holds no entries")
        return

    census = {status: 0 for status in DEFECT_STATUSES}
    for entry in entries:
        identifier = entry.get("id", "<an entry with no id>")
        status = entry.get("status")
        if status not in DEFECT_STATUSES:
            verdict.refuse(f"{identifier} carries status {status!r}")
            continue
        census[status] += 1
        for field in ("title", "where", "anchor", "reason"):
            if not (entry.get(field) or "").strip():
                verdict.refuse(f"{identifier} has no {field}")
        # The register's own citations are held to the rule the citation grader
        # holds a document to. A defect register whose rows point at nothing is
        # this repository's own bug class, aimed at the place defects go to be
        # remembered.
        cited = entry.get("where") or ""
        anchor = entry.get("anchor") or ""
        source = context.repo / cited
        if not cited or not source.is_file():
            verdict.refuse(f"{identifier} cites {cited!r}, which is not a file")
        elif anchor and anchor not in source.read_text(encoding="utf-8"):
            verdict.refuse(
                f"{identifier} cites {cited} for {anchor!r}, which is not in it"
            )
        if status == "CLOSED":
            # Held to the rule the survivor register's head already gets: a
            # commit here must be one the judged commit REACHES, not merely one
            # this object database happens to hold. `cat-file -e` alone was the
            # weaker predicate under a message that promised the stronger one,
            # and the case it could not see is the case it is most needed for --
            # a squash for publication leaves every replaced commit resolvable
            # on the host that did the squashing, because that host is where the
            # pre-squash tip is deliberately kept, and unresolvable in the only
            # repository anyone else will ever clone.
            closed_by = entry.get("closed_by") or ""
            if git(
                context.repo, "cat-file", "-e", f"{closed_by}^{{commit}}"
            ).returncode:
                verdict.refuse(
                    f"{identifier} is CLOSED by {closed_by!r}, which is not a commit "
                    "in this repository"
                )
            elif git(
                context.repo, "merge-base", "--is-ancestor", closed_by, context.commit
            ).returncode:
                verdict.refuse(
                    f"{identifier} is CLOSED by {closed_by}, which {context.commit[:7]} "
                    "does not reach: it names a commit outside this history"
                )
        if status == "ACCEPTED" and not (entry.get("decision") or "").strip():
            verdict.refuse(f"{identifier} is ACCEPTED and records no decision")
        if status == "OPEN":
            verdict.refuse(
                f"an OPEN defect in the Path B path: {identifier} -- {entry.get('title')}"
            )

    published = lettered_defects(read_text(context.repo, CRITERIA_DOCUMENT))
    held = {entry.get("letter") for entry in entries if entry.get("letter")}
    for letter in sorted(published - held):
        verdict.refuse(
            f"{CRITERIA_DOCUMENT} criterion 1 lists a defect ({letter}) the register "
            "does not hold"
        )
    for letter in sorted(held - published):
        verdict.refuse(
            f"{DEFECT_REGISTER} holds a defect lettered ({letter}) that criterion 1 "
            "does not list"
        )
    verdict.note("defect_register_entries", len(entries))
    for status in DEFECT_STATUSES:
        verdict.note(f"defect_register_{status.lower()}", census[status])
    verdict.note("defect_register_letters_reconciled", len(published & held))


def check_survivor_disposition(context: Context, verdict: Verdict) -> None:
    """Every mutation survivor dispositioned, at a head this commit still is."""

    path = context.repo / SURVIVOR_REGISTER
    if not path.exists():
        verdict.refuse(f"there is no survivor register at {SURVIVOR_REGISTER}")
        return
    register = load_json(path)
    tool = import_module(
        context.repo / MUTATION_REGISTER_TOOL, "pmux_mutation_register"
    )
    problems = tool.validate(register)
    for problem in problems:
        verdict.refuse(f"{SURVIVOR_REGISTER} is not well formed: {problem}")
    if problems:
        # A register this cannot read is a register this cannot date. Measuring
        # its currency past that point would report which of a malformed file's
        # rows are stale, which is a number about nothing.
        return
    recorded = register.get("recorded_at", {})
    if recorded.get("scope") != "full":
        verdict.refuse(
            f"the survivor register was recorded at scope {recorded.get('scope')!r}; "
            "only a full-scope run covers the Path B path"
        )
    for entry in register.get("entries", []):
        if entry.get("disposition") == "ACCEPTED" and not entry.get("closeable"):
            verdict.refuse(
                f"{entry.get('function')} carries an ACCEPTED survivor with no "
                "`closeable`, so nothing states what closing it costs"
            )
    head = recorded.get("head") or ""
    if git(context.repo, "cat-file", "-e", f"{head}^{{commit}}").returncode:
        verdict.refuse(
            f"the survivor register was recorded at {head!r}, not a commit here"
        )
        return
    if git(
        context.repo, "merge-base", "--is-ancestor", head, context.commit
    ).returncode:
        verdict.refuse(
            f"the survivor register was recorded at {head}, which is not an ancestor "
            f"of {context.commit[:7]}: it describes a different tree"
        )
        return
    verdict.note("survivor_register_head", head)
    verdict.note("survivor_register_entries", len(register.get("entries", [])))
    verdict.note("survivor_register_scope", recorded.get("scope"))

    # WHICH ROWS STOPPED BEING TRUE, and not which files moved. The check this
    # replaces asked `git diff --name-only` over `FULL_GLOBS` and refused on any
    # answer, so one moved comment in `driver_io.rs` called all 144 rows stale
    # and demanded the 3 h 16 m campaign again -- and it could not see a test
    # change at all, which is what actually falsifies a KILLED row.
    # `scripts/register_currency.py` decides both, and `docs/register-currency.md`
    # is where the three rules and the seven escalations are written down.
    census_path = context.repo / ENUMERATION_CENSUS
    if not census_path.exists():
        verdict.refuse(
            f"there is no enumeration census at {ENUMERATION_CENSUS}, so nothing "
            "records what the register's campaign enumerated and a mutant that never "
            "existed before cannot be told from one it dispositioned"
        )
        return
    currency = import_module(
        context.repo / REGISTER_CURRENCY_TOOL, "pmux_register_currency"
    )
    binary = context.repo / currency.CARGO_MUTANTS_BIN
    try:
        state = currency.assess(
            context.repo,
            context.commit,
            register=register,
            census=load_json(census_path),
            now=context.now,
            max_receipt_age_days=context.max_receipt_age_days,
            cargo_mutants=binary,
            cargo=context.cargo,
        )
    except currency.CurrencyError as error:
        raise DoneGateError(f"the survivor register's currency: {error}") from error
    for key, value in state.notes:
        verdict.note(key, value)
    for escalation in state.escalations:
        verdict.refuse(
            f"{escalation}. A FULL-scope run is what refreshes it: "
            f"{currency.full_command(context.repo, register)}"
        )
    if state.reasons:
        named = ", ".join(
            f"{file}::{function}" for file, function in state.stale_functions[:5]
        )
        verdict.refuse(
            f"{len(state.stale_rows)} survivor-register row(s) in "
            f"{len(state.stale_functions)} function(s) stopped describing this commit "
            f"({named}{', ...' if len(state.stale_functions) > 5 else ''}). "
            f"{state.reasons[0]}. Re-decide exactly those, and nothing else, with: "
            f"{currency.filtered_command(context.repo)}"
        )


def criterion_no_known_unfixed_defect(context: Context) -> Verdict:
    verdict = Verdict()
    check_defect_register(context, verdict)
    check_survivor_disposition(context, verdict)
    return verdict


# ---------------------------------------------------------------------------
# Criterion 2 -- the adversarial suite passes
# ---------------------------------------------------------------------------


def adversarial_commands(document: str) -> tuple[list[list[str]], list[str]]:
    """What the adversarial document's own verification tables name, in two parts.

    Not a list here. Every section headed "Verification at this commit" holds a
    table whose first column says what was checked, and each row is one of three
    kinds:

      * a single backticked `cargo test ...` -- the suite, returned first and
        run by criterion 2;
      * a single backticked something else (`cargo fmt`, `cargo clippy`, `ruff
        check`, the residue script) -- a Gate A cell, which criterion 4 reads a
        receipt for rather than running twice;
      * a row whose first cell is not one backticked command at all -- a check
        that is not a command. **Nothing in this file measures those**, and
        they are returned second so the criterion PRINTS them.

    That third kind is not a rounding error and the accounting here used to omit
    it. In this document it is where the LIVE half of the adversarial suite
    lives: section 10 and section 11.6 each carry a "live re-verification,
    rebuilt release binaries" row recording real model turns, and both were
    dropped silently by a derivation that then reported a criterion titled "the
    adversarial suite passes". A criterion that runs seven offline commands may
    say so; it may not say so in the name of a suite whose live rows it never
    looked at.
    """

    commands: list[list[str]] = []
    unmeasured: list[str] = []
    headings = list(
        re.finditer(r"^#+ .*Verification at this commit.*$", document, re.MULTILINE)
    )
    if not headings:
        raise DoneGateError(
            f"{ADVERSARIAL_DOCUMENT} publishes no 'Verification at this commit' "
            "section, so the suite cannot be derived from it"
        )
    for heading in headings:
        rest = document[heading.end() :]
        stop = re.search(r"^#+ ", rest, re.MULTILINE)
        body = rest if stop is None else rest[: stop.start()]
        found = 0
        for cells in markdown_rows(body):
            named = re.fullmatch(r"`([^`]+)`", cells[0])
            if named is None:
                # The header row of every one of these tables is the word
                # `check`, which names nothing and is not a row.
                label = cells[0].strip()
                if label and label.lower() != "check" and label not in unmeasured:
                    unmeasured.append(label)
                continue
            if not named.group(1).startswith("cargo test"):
                continue
            argv = shlex.split(named.group(1))
            found += 1
            if argv not in commands:
                commands.append(argv)
        if found == 0:
            raise DoneGateError(
                f"{ADVERSARIAL_DOCUMENT}'s {heading.group(0).strip()!r} names no "
                "`cargo test` command; the derivation is broken"
            )
    return commands, unmeasured


def run_test_command(context: Context, argv: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        [context.cargo, *argv[1:]],
        cwd=context.repo,
        capture_output=True,
        text=True,
        timeout=COMMAND_TIMEOUT_SECONDS,
        env={**os.environ, "CARGO_TERM_COLOR": "never"},
    )


def failing_lines(done: subprocess.CompletedProcess) -> str:
    lines = [
        line
        for line in (done.stdout + done.stderr).splitlines()
        if "FAILED" in line or line.startswith("error") or line.startswith("failures:")
    ]
    return "; ".join(lines[:4]) if lines else "no failing line was printed"


def criterion_adversarial_suite(context: Context) -> Verdict:
    verdict = Verdict()
    commands, unmeasured = adversarial_commands(
        read_text(context.repo, ADVERSARIAL_DOCUMENT)
    )
    verdict.note("adversarial_commands_derived", len(commands))
    # Printed, not refused: which rows of those tables are live turns is the
    # owner's reading of the owner's criterion, and a script that promoted a
    # dropped row to a failure would be legislating it. What it may not do is
    # stay quiet about them.
    for label in unmeasured:
        verdict.note("named_by_the_document_and_not_measured_here", label)
    if not context.cargo:
        verdict.refuse("no cargo was resolved, so the adversarial suite was not run")
        return verdict
    for argv in commands:
        printed = " ".join(argv)
        try:
            done = run_test_command(context, argv)
        except (OSError, subprocess.SubprocessError) as error:
            verdict.refuse(f"`{printed}` could not be run: {error}")
            continue
        if done.returncode != 0:
            verdict.refuse(
                f"`{printed}` exited {done.returncode}: {failing_lines(done)}"
            )
        verdict.note(
            f"suite[{printed}]", "passed" if done.returncode == 0 else "FAILED"
        )
    return verdict


# ---------------------------------------------------------------------------
# Criterion 3 -- a promoted profile for the installed version
# ---------------------------------------------------------------------------


def promoted_profiles(source: str) -> list[dict[str, str]]:
    """`PROMOTED_PROFILES` read out of the Rust that ships it."""

    block = re.search(
        r"pub const PROMOTED_PROFILES: &\[PromotedProfile\] = &\[(.*?)\n\}\];",
        source,
        re.DOTALL,
    )
    if block is None:
        raise DoneGateError(
            f"{COMPATIBILITY_SOURCE} no longer declares PROMOTED_PROFILES"
        )
    profiles = []
    for entry in re.split(r"PromotedProfile \{", block.group(1))[1:]:
        fields = dict(re.findall(r'(\w+): "([^"]*)"', entry))
        wanted = ("claude_version_floor", "claude_version_tested_through", "os", "arch")
        if all(name in fields for name in wanted):
            profiles.append({name: fields[name] for name in wanted})
    if not profiles:
        raise DoneGateError(
            f"{COMPATIBILITY_SOURCE} declares PROMOTED_PROFILES and this read none of "
            "them; the parse is broken"
        )
    return profiles


def version_tuple(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in version.split("."))


def promotion_checks(source: str) -> dict[str, str]:
    """Every check the promotion tool defines, and the criterion it states.

    The evidence file is required to hold all of them. A check added to the
    tool therefore makes older promotion evidence insufficient rather than
    silently unexercised, which is the direction that keeps a promoted profile
    honest as the machinery grows.
    """

    block = re.search(
        r"CHECKS: tuple\[Check, \.\.\.\] = \((.*?)\n\)\n", source, re.DOTALL
    )
    if block is None:
        raise DoneGateError(f"{PROMOTION_TOOL} no longer declares CHECKS")
    checks = {}
    for entry in re.split(r"\n    Check\(", block.group(1)):
        identifier = re.search(r'id="([^"]+)"', entry)
        criterion = re.search(r'criterion=((?:\s*"[^"]*")+)', entry)
        if identifier is None or criterion is None:
            continue
        checks[identifier.group(1)] = "".join(
            re.findall(r'"([^"]*)"', criterion.group(1))
        )
    if not checks:
        raise DoneGateError(f"{PROMOTION_TOOL} declares CHECKS and this read none")
    return checks


def criterion_promoted_profile(context: Context) -> Verdict:
    verdict = Verdict()
    system = platform.system().lower()
    machine = platform.machine().lower()
    if system not in RUST_OS or machine not in RUST_ARCH:
        verdict.refuse(
            f"this host is {system}/{machine}, which this tool cannot spell the way a "
            "promoted profile is written; it refuses rather than guess"
        )
        return verdict
    host_os, host_arch = RUST_OS[system], RUST_ARCH[machine]
    verdict.note("host", f"{host_os}/{host_arch}")

    if not context.claude:
        verdict.refuse(
            "no `claude` executable was resolved, so the installed version is unknown"
        )
        return verdict
    try:
        done = subprocess.run(
            [context.claude, "--version"],
            capture_output=True,
            text=True,
            timeout=VERSION_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError) as error:
        verdict.refuse(f"`{context.claude} --version` could not be run: {error}")
        return verdict
    installed = re.match(r"\s*(\d+\.\d+\.\d+)", done.stdout)
    if done.returncode != 0 or installed is None:
        verdict.refuse(
            f"`{context.claude} --version` printed {done.stdout.strip()!r}, which is "
            "not a version"
        )
        return verdict
    version = installed.group(1)
    verdict.note("claude_version_installed", version)

    profiles = promoted_profiles(read_text(context.repo, COMPATIBILITY_SOURCE))
    verdict.note("promoted_profiles", len(profiles))
    covering = [
        profile
        for profile in profiles
        if profile["os"] == host_os
        and profile["arch"] == host_arch
        and version_tuple(profile["claude_version_floor"])
        <= version_tuple(version)
        <= version_tuple(profile["claude_version_tested_through"])
    ]
    if not covering:
        verdict.refuse(
            f"no promoted profile covers Claude Code {version} on {host_os}/{host_arch}; "
            "the ranges are "
            + ", ".join(
                f"{p['os']}/{p['arch']} {p['claude_version_floor']}"
                f"..={p['claude_version_tested_through']}"
                for p in profiles
            )
        )
        return verdict
    profile = covering[0]
    verdict.note(
        "promoted_range",
        f"{profile['claude_version_floor']}..={profile['claude_version_tested_through']}",
    )

    # The evidence for the installed version itself if there is one, and
    # otherwise the evidence for the top of the range that covers it -- which is
    # what a promoted RANGE means. Either way it is a file, read here.
    candidates = list(
        dict.fromkeys([version, profile["claude_version_tested_through"]])
    )
    evidence = None
    for candidate in candidates:
        path = (
            context.repo / f"evidence/promotion-{candidate}-{host_os}-{host_arch}.json"
        )
        if path.is_file():
            evidence = (candidate, path, load_json(path))
            break
    if evidence is None:
        verdict.refuse(
            "no promotion evidence exists for "
            + " or ".join(
                f"evidence/promotion-{candidate}-{host_os}-{host_arch}.json"
                for candidate in candidates
            )
        )
        return verdict
    measured_version, path, record = evidence
    verdict.note("promotion_evidence", path.name)
    if measured_version != version:
        verdict.note("covered_by_range_not_by_its_own_run", measured_version)
    if record.get("verdict") != "promotable":
        verdict.refuse(f"{path.name} records verdict {record.get('verdict')!r}")
    if record.get("failed_check") is not None:
        verdict.refuse(
            f"{path.name} records a failed check: {record.get('failed_check')}"
        )

    required = promotion_checks(read_text(context.repo, PROMOTION_TOOL))
    observed = {
        check.get("id"): check.get("outcome") for check in record.get("checks", [])
    }
    for identifier in sorted(set(required) - set(observed)):
        verdict.refuse(
            f"{PROMOTION_TOOL} defines the check {identifier!r} and {path.name} does "
            "not hold it, so this profile was promoted by machinery that no longer "
            "exists"
        )
    for identifier in sorted(set(required) & set(observed)):
        if observed[identifier] != "passed":
            verdict.refuse(
                f"{path.name} records {identifier} as {observed[identifier]!r}"
            )
    # "from machinery that exercises minified cells" is the criterion's own
    # words, so the check that does it is found by what the promotion tool says
    # each check is FOR, not by a name written here.
    minified = sorted(
        identifier
        for identifier, criterion in required.items()
        if "Minified" in criterion
    )
    if not minified:
        verdict.refuse(
            f"no check in {PROMOTION_TOOL} states a criterion about a minified cell, "
            "so nothing in the promotion path exercises one"
        )
    for identifier in minified:
        if observed.get(identifier) != "passed":
            verdict.refuse(
                f"the minified-cell check {identifier} is {observed.get(identifier)!r} "
                f"in {path.name}"
            )
    verdict.note("promotion_checks_required", len(required))
    verdict.note("promotion_checks_exercising_a_minified_cell", ",".join(minified))
    return verdict


# ---------------------------------------------------------------------------
# Criterion 4 -- Gate A green except the deliberate Linux cell
# ---------------------------------------------------------------------------


def manifest_at(context: Context) -> tuple[dict, str]:
    """The phase manifest AS OF the commit under judgement, and its digest.

    Read out of the commit and not off the disk, because the cells a receipt
    was asked to run are the cells that commit declared. The digest is over the
    bytes for the same reason the driver takes it over the bytes: a receipt
    whose `manifest.sha256` is not this one graded a different list.
    """

    done = subprocess.run(
        ["git", "-C", str(context.repo), "show", f"{context.commit}:{MANIFEST}"],
        capture_output=True,
        timeout=120,
    )
    if done.returncode != 0:
        raise DoneGateError(
            f"{MANIFEST} is not in {context.commit[:7]}: "
            f"{done.stderr.decode('utf-8', 'replace').strip()}"
        )
    return json.loads(done.stdout), hashlib.sha256(done.stdout).hexdigest()


def deliberate_red_cells(
    context: Context, cells: set[str]
) -> tuple[set[str], list[str]]:
    """The cells a red receipt may name, granted by two documents at once.

    A cell is admissible red only if BOTH the criterion that grants the
    exception names it AND an open debt row does. Either side alone
    over-derives, and this is measured rather than argued: criterion 4's own
    section also names `gate_f/phase0_self_tests` (in the sentence saying it
    passed), and section 9.4's rows also name `release_full_stack_e2e` --
    inside row **C6**, in the sentence about the ordering `test_runner.py:821`
    forbids, and not inside C10 as this comment claimed until the sets were
    printed and read back. Requiring both leaves exactly the Linux cell, and
    widening it takes an edit to two documents that agree.

    A row whose disposition says CLOSED grants nothing, which is what keeps the
    phase0 cell out: row C11 names it and is closed.
    """

    verdict_section = section_of(
        read_text(context.repo, CRITERIA_DOCUMENT),
        r"^### 4\. .*$",
        r"^#{2,3} ",
        CRITERIA_DOCUMENT,
    )
    granted = {name.split("/")[-1] for name in inline_code(verdict_section)} & cells
    debt = section_of(
        read_text(context.repo, DEBT_DOCUMENT),
        r"^### 9\.4 .*$",
        r"^### 9\.5 ",
        DEBT_DOCUMENT,
    )
    provenance = []
    open_rows: set[str] = set()
    for row in markdown_rows(debt):
        identifier = row[0].strip("* ")
        if not re.fullmatch(r"C?\d+", identifier):
            continue
        line = " | ".join(row)
        named = {name.split("/")[-1] for name in inline_code(line)} & cells
        if not named:
            continue
        if "CLOSED" in line:
            continue
        open_rows |= named
        for name in sorted(named & granted):
            provenance.append(f"{name} is granted by {DEBT_DOCUMENT} row {identifier}")
    return granted & open_rows, provenance


def gate_receipts(
    context: Context, verdict: Verdict
) -> list[tuple[pathlib.Path, dict]]:
    """Every receipt named on the command line, resolved to a gate receipt.

    A receipt written by the pinned-worktree runner NAMES THE COMMIT IT GRADED,
    so it is accepted only for that commit, and the gate receipt inside it is
    found by digest rather than by path. A bare gate receipt names no commit at
    all, so the only thing that can bind it to one is content: it must describe
    the tree that is here, which means a clean tree at the judged commit and a
    source digest recomputed now that equals the one it recorded.
    """

    resolved = []
    driver = import_module(context.repo / GATE_DRIVER, "pmux_gate_a_run_gate")
    for path in context.receipts:
        if not path.is_file():
            verdict.refuse(f"no receipt at {path}")
            continue
        try:
            record = load_json(path)
        except (OSError, ValueError) as error:
            verdict.refuse(f"{path} is not readable JSON: {error}")
            continue
        if record.get("schema") == PINNED_RUN_SCHEMA:
            graded = record.get("describes_commit") or ""
            if graded != context.commit:
                verdict.refuse(
                    f"{path.name} describes commit {graded[:7] or '<none>'} and this "
                    f"gate is judging {context.commit[:7]}"
                )
                continue
            tree = git_output(
                context.repo, "rev-parse", f"{context.commit}^{{tree}}"
            ).strip()
            if record.get("tree_sha") != tree:
                verdict.refuse(
                    f"{path.name} records tree {record.get('tree_sha')} for a commit "
                    f"whose tree is {tree}"
                )
                continue
            found = 0
            for artefact in record.get("artefacts", []):
                inner = pathlib.Path(artefact.get("path", ""))
                if not inner.is_file():
                    verdict.refuse(
                        f"{path.name} names an artefact that is gone: {inner}"
                    )
                    continue
                # Present and unreadable is its own outcome, and it stopped
                # being hypothetical when the runner started recording a null
                # digest for a file it could not hash: an uncaught `OSError`
                # here would end the run in a traceback rather than in a
                # verdict, which is the one thing this gate may not do.
                try:
                    here = sha256_of(inner)
                except OSError as error:
                    verdict.refuse(f"{inner} cannot be re-read: {error}")
                    continue
                if here != artefact.get("sha256"):
                    verdict.refuse(
                        f"{inner} no longer matches the digest {path.name} recorded"
                    )
                    continue
                try:
                    candidate = load_json(inner)
                except (OSError, ValueError):
                    continue
                if candidate.get("driver") != GATE_DRIVER:
                    continue
                # Compared as paths and not as strings: `/tmp` is a symlink on
                # macOS and the gate driver resolves what it records, so two
                # spellings of one directory are the ordinary case here.
                if (
                    pathlib.Path(candidate.get("workspace", "")).resolve()
                    != pathlib.Path(record.get("worktree", "")).resolve()
                ):
                    verdict.refuse(
                        f"{inner.name} was written for workspace "
                        f"{candidate.get('workspace')}, not the pinned worktree "
                        f"{record.get('worktree')}"
                    )
                    continue
                found += 1
                resolved.append((inner, candidate))
            if found == 0:
                verdict.refuse(
                    f"{path.name} carries no Gate A receipt among its artefacts"
                )
            continue
        if record.get("driver") != GATE_DRIVER:
            verdict.refuse(
                f"{path.name} was not written by {GATE_DRIVER} and is not a pinned run"
            )
            continue
        head = git_output(context.repo, "rev-parse", "HEAD").strip()
        dirty = git_output(context.repo, "status", "--porcelain").strip()
        if head != context.commit or dirty:
            verdict.refuse(
                f"{path.name} names no commit, so it can only be read against the tree "
                f"in front of it -- and this tree is "
                + ("dirty" if dirty else f"at {head[:7]}, not {context.commit[:7]}")
            )
            continue
        here = driver.source_digest(context.repo)
        for when in ("source_digest_before", "source_digest_after"):
            if record.get(when, {}).get("sha256") != here["sha256"]:
                verdict.refuse(
                    f"{path.name}'s {when} is not the digest of this tree "
                    f"({here['sha256'][:12]}, {here['file_count']} files)"
                )
                break
        else:
            resolved.append((path, record))
    return resolved


def pinned_receipt_remedy(context: Context, missing: list[str]) -> list[str]:
    """The path a pinned run would write, and the run that would write it.

    ASKED OF THE RUNNER RATHER THAN RESTATED. `scripts/gate-in-worktree.sh`
    owns where its receipt goes; `--print-receipt-path` answers that for the
    same `--commit` the printed command carries, so the path named here and the
    path the command produces are one derivation read twice. A convention
    spelled out again in this file would be a second author for it, and this
    refusal is the one place a reader is most entitled to a path that is
    actually the path.

    MEASURED, and the reason this function exists: two receipts recording 62
    and 8 cells were rejected -- correctly, because a bare `run_gate.py` receipt
    names no commit -- and criterion 4 said `cells_executed=0` and stopped
    there, leaving the reader to work out that the pinned receipt for that
    commit had died with the worktree and that nothing on disk could revive it.
    """

    runner = context.repo / PINNED_RUNNER
    if not runner.is_file():
        return [f"the pinned-worktree runner is gone from {PINNED_RUNNER}"]
    arguments = ["--commit", context.commit]
    try:
        done = subprocess.run(
            ["bash", str(runner), "--print-receipt-path", *arguments],
            capture_output=True,
            text=True,
            cwd=context.repo,
            timeout=VERSION_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError) as error:
        return [f"{PINNED_RUNNER} could not be asked where its receipt goes: {error}"]
    if done.returncode != 0 or not done.stdout.strip():
        return [
            f"{PINNED_RUNNER} --print-receipt-path exited {done.returncode}: "
            f"{done.stderr.strip() or '<no output>'}"
        ]
    expected = pathlib.Path(done.stdout.strip())
    lines = [f"a pinned receipt for {context.commit[:7]} belongs at {expected}"]
    # Already run and merely not named is a different mistake from never run,
    # and which one it is can be read off the disk instead of guessed at.
    for candidate in sorted(expected.parent.glob("pinned-receipt-*.json")):
        if candidate in context.receipts:
            continue
        try:
            record = load_json(candidate)
        except (OSError, ValueError):
            continue
        if (
            record.get("schema") == PINNED_RUN_SCHEMA
            and record.get("describes_commit") == context.commit
        ):
            lines.append(
                f"one for this commit exists already: --gate-a-receipt {candidate}"
            )
    phases = sorted({name.split("/")[0] for name in missing})
    # `--phase` is derived from the cells nothing graded, so it narrows as
    # receipts accumulate. The two preparations are NOT derived and cannot be:
    # nothing declares them in a form a program can read -- the manifest states
    # cells, not preconditions, and the release build and the locked `npm ci`
    # are named in prose by `tools/gate-a/README.md` and by the runner's own
    # header. Stated here as the third copy, and named as such rather than left
    # to look derived.
    command = [
        "bash",
        PINNED_RUNNER,
        *arguments,
        "--release-build",
        "--prepare",
        "cd clients/typescript && npm ci",
        "--",
        "python3",
        "{worktree}/" + GATE_DRIVER,
        "--manifest",
        "{worktree}/" + MANIFEST,
        "--workspace",
        "{worktree}",
        "--release-dir",
        "{worktree}/target/release",
        "--validation-root",
        "{validation}",
        "--receipt",
        "{artefacts}/gate-a-receipt.json",
    ]
    for phase in phases:
        command += ["--phase", phase]
    lines.append(f"the {len(phases)} phase(s) nothing here graded: {' '.join(phases)}")
    lines.append(shlex.join(command))
    # Stated rather than derived, because the set of tools a phase cannot
    # resolve is a fact about this host's PATH and not about the manifest:
    # `run_gate.py` names every unresolved placeholder before its first cell.
    lines.append(
        f"{GATE_DRIVER} refuses before cell one for any tool placeholder it cannot "
        "resolve, naming each; pass --tool NAME=PATH for those"
    )
    return lines


def criterion_gate_a_green(context: Context) -> Verdict:
    verdict = Verdict()
    manifest, manifest_digest = manifest_at(context)
    required = {
        f"{phase}/{cell['id']}"
        for phase, cells in manifest["phases"].items()
        for cell in cells
    }
    if not context.receipts:
        verdict.refuse(
            "no Gate A receipt was named (--gate-a-receipt), and a criterion with no "
            "evidence is not met"
        )
        # The same remedy the missing-cell refusal below prints, because naming
        # no receipt and naming one that covers nothing leave the reader in the
        # identical position and the manifest is already read.
        for line in pinned_receipt_remedy(context, sorted(required)):
            verdict.suggest(line)
        return verdict
    admissible, provenance = deliberate_red_cells(
        context, {name.split("/")[-1] for name in required}
    )
    verdict.note("manifest_cells", len(required))
    verdict.note("deliberately_red_cells", ",".join(sorted(admissible)) or "none")
    for line in provenance:
        verdict.note("granted_by", line)

    executed: set[str] = set()
    failed: set[str] = set()
    for path, receipt in gate_receipts(context, verdict):
        verdict.note("receipt", path.name)
        if receipt.get("manifest", {}).get("sha256") != manifest_digest:
            verdict.refuse(
                f"{path.name} graded a manifest whose digest is not this commit's "
                f"({MANIFEST})"
            )
            continue
        if not receipt.get("source_unchanged"):
            verdict.refuse(f"{path.name} records source_unchanged=false")
        completed = receipt.get("completed_at", "")
        try:
            when = dt.datetime.fromisoformat(completed.replace("Z", "+00:00"))
        except ValueError:
            verdict.refuse(f"{path.name} has no readable completed_at ({completed!r})")
            continue
        age = (context.now - when).total_seconds() / 86400.0
        verdict.note(f"receipt_age_days[{path.name}]", f"{age:.1f}")
        if age > context.max_receipt_age_days:
            verdict.refuse(
                f"{path.name} completed {age:.1f} days ago, past the "
                f"{context.max_receipt_age_days:g}-day freshness bound: the tree it "
                "describes is not the environment this runs in"
            )
        for cell in receipt.get("cells", []):
            name = f"{cell['phase']}/{cell['id']}"
            executed.add(name)
            if not cell.get("passed"):
                failed.add(name)
    verdict.note("cells_executed", len(executed))
    missing = sorted(required - executed)
    if missing:
        verdict.refuse(
            f"{len(missing)} manifest cell(s) were graded by no receipt named here, so "
            f"Gate A is not green over them: {', '.join(missing[:6])}"
            + (" ..." if len(missing) > 6 else "")
        )
        for line in pinned_receipt_remedy(context, missing):
            verdict.suggest(line)
    for name in sorted(failed):
        if name.split("/")[-1] not in admissible:
            verdict.refuse(f"{name} is red and no open debt row grants it")
        else:
            verdict.note("red_and_deliberate", name)
    return verdict


# ---------------------------------------------------------------------------
# Criterion 5 -- Path B doc claims reconciled to measurement
# ---------------------------------------------------------------------------


def package_of(context: Context, crate: pathlib.Path) -> str:
    manifest = (crate / "Cargo.toml").read_text(encoding="utf-8")
    named = re.search(r'^name\s*=\s*"([^"]+)"', manifest, re.MULTILINE)
    if named is None:
        raise DoneGateError(f"{crate}/Cargo.toml declares no package name")
    return named.group(1)


def criterion_doc_claims_reconciled(context: Context) -> Verdict:
    verdict = Verdict()
    grader = context.repo / CITATION_GRADER
    if not grader.is_file():
        verdict.refuse(f"the citation grader is gone from {CITATION_GRADER}")
        return verdict
    package = package_of(context, grader.parent.parent)
    argv = ["cargo", "test", "-p", package, "--test", grader.stem]
    verdict.note("citation_grader", " ".join(argv))
    if not context.cargo:
        verdict.refuse("no cargo was resolved, so the citation grader was not run")
        return verdict
    try:
        done = run_test_command(context, argv)
    except (OSError, subprocess.SubprocessError) as error:
        verdict.refuse(f"the citation grader could not be run: {error}")
        return verdict
    if done.returncode != 0:
        verdict.refuse(
            f"the citation grader exited {done.returncode}: {failing_lines(done)}"
        )
    result = re.search(r"test result: \S+\. (\d+) passed; (\d+) failed", done.stdout)
    if result is None:
        verdict.refuse("the citation grader printed no test result line")
        return verdict
    verdict.note("citation_rules_passed", result.group(1))
    verdict.note("citation_rules_failed", result.group(2))
    if result.group(1) == "0":
        verdict.refuse("the citation grader ran no rules, which is not a pass")
    # An observation and not a refusal: the document this gate reads its
    # criteria out of is not itself in the Path B reading order, so its own
    # citations are ungraded. `docs/path-b-verdict.md`'s remaining-work item on
    # the non-Path-B documents and the reading order states why -- cited by what
    # it says and not by the ordinal it happened to carry, because that list is
    # reordered every time something on it is finished, and a comment pinned to
    # "item 8" is a citation that rots on the next edit to a file this one does
    # not import.
    order = read_text(context.repo, "docs/path-b.md")
    verdict.note(
        "criteria_document_is_graded",
        str(f"`{CRITERIA_DOCUMENT}`" in order),
    )
    return verdict


CRITERIA = [
    Criterion(
        1,
        "No known unfixed defect in the Path B path",
        criterion_no_known_unfixed_defect,
    ),
    Criterion(2, "The adversarial suite passes", criterion_adversarial_suite),
    Criterion(
        3,
        "A promoted profile for the installed version, from machinery that "
        "exercises minified cells",
        criterion_promoted_profile,
    ),
    Criterion(
        4, "Gate A green except the deliberate Linux cell", criterion_gate_a_green
    ),
    Criterion(
        5,
        "Path B doc claims reconciled to measurement",
        criterion_doc_claims_reconciled,
    ),
]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", required=True, type=pathlib.Path)
    parser.add_argument("--commit", default="HEAD")
    parser.add_argument("--cargo", default="")
    parser.add_argument("--claude", default="")
    parser.add_argument(
        "--gate-a-receipt",
        action="append",
        default=[],
        type=pathlib.Path,
        dest="receipts",
    )
    parser.add_argument("--max-receipt-age-days", type=float, default=14.0)
    parser.add_argument(
        "--only",
        action="append",
        default=[],
        type=int,
        help="measure only these criteria; never a verdict, always exit 3",
    )
    arguments = parser.parse_args(argv)

    repo = arguments.repo.resolve()
    try:
        commit = git_output(repo, "rev-parse", f"{arguments.commit}^{{commit}}").strip()
        criteria = bind(read_text(repo, CRITERIA_DOCUMENT), CRITERIA)
    except DoneGateError as error:
        print(f"path-b-done: {error}", file=sys.stderr)
        return 2
    # An `--only 9` that quietly measures nothing is the shape of refusal this
    # whole tool exists to replace.
    unknown = sorted(set(arguments.only) - {c.ordinal for c in criteria})
    if unknown:
        print(
            f"path-b-done: --only names {unknown}, and the criteria are "
            f"{[c.ordinal for c in criteria]}",
            file=sys.stderr,
        )
        return 2
    context = Context(
        repo=repo,
        commit=commit,
        cargo=arguments.cargo,
        claude=arguments.claude,
        receipts=tuple(path.resolve() for path in arguments.receipts),
        max_receipt_age_days=arguments.max_receipt_age_days,
        now=dt.datetime.now(dt.timezone.utc),
    )
    print(f"commit={commit}")
    print(f"criteria={len(criteria)} (read from {CRITERIA_DOCUMENT})")

    # Criteria 2 and 5 RUN things, and what they run is the tree in front of
    # them. A verdict is about a commit, so a verdict from a tree that is not
    # that commit would attribute one tree's test run to another -- the same
    # confusion the pinned-worktree receipt exists to remove, one level up. A
    # partial run may be taken from anywhere, because it is never a verdict.
    head = git_output(repo, "rev-parse", "HEAD").strip()
    dirty = git_output(repo, "status", "--porcelain").strip()
    standing = (
        "clean"
        if not dirty and head == commit
        else ("dirty" if dirty else f"at {head[:7]}")
    )
    print(f"working_tree={standing}")
    if standing != "clean":
        if not arguments.only:
            print(
                f"path-b-done: this working tree is {standing} and the commit being "
                f"judged is {commit[:7]}. Criteria 2 and 5 run tests against the tree "
                "in front of them, so a verdict taken here would be a verdict about "
                "neither. Commit or clean the tree, or run --only for a partial "
                "measurement that is not a verdict.",
                file=sys.stderr,
            )
            return 2
        print(
            f"    warning: measurements below come from a {standing} tree, not from "
            f"{commit[:7]}"
        )

    unmet = []
    ran = 0
    for criterion in criteria:
        if arguments.only and criterion.ordinal not in arguments.only:
            print(f"[{criterion.ordinal}/{len(criteria)}] {criterion.title} -- NOT RUN")
            continue
        try:
            verdict = criterion.check(context)
        except DoneGateError as error:
            print(
                f"path-b-done: criterion {criterion.ordinal}: {error}", file=sys.stderr
            )
            return 2
        ran += 1
        state = "MET" if verdict.met else "NOT MET"
        print(f"[{criterion.ordinal}/{len(criteria)}] {criterion.title} -- {state}")
        for line in verdict.evidence:
            print(f"    {line}")
        for line in verdict.failures:
            print(f"    because: {line}")
        for line in verdict.remedy:
            print(f"    remedy: {line}")
        if not verdict.met:
            unmet.append(criterion.ordinal)
    if arguments.only:
        print(
            f"PARTIAL {ran} of {len(criteria)} criteria measured; this is not a verdict"
        )
        return 3
    if unmet:
        print(
            f"NOT DONE {len(criteria) - len(unmet)}/{len(criteria)} criteria met; "
            f"not met: {', '.join(str(ordinal) for ordinal in unmet)}"
        )
        return 1
    print(f"DONE {len(criteria)}/{len(criteria)} criteria met at {commit[:7]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
