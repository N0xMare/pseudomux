#!/usr/bin/env python3
"""When a survivor-register row stops being true -- computed, not assumed.

WHAT THIS REPLACES
------------------

`evidence/mutation-survivor-register.json` describes one tree. The check that
decided whether it still described the tree in front of it read
`git diff --name-only <register head> <commit>` over `FULL_GLOBS` and refused if
anything came back, so a moved comment in `crates/service/src/driver_io.rs`
declared all 144 rows stale and demanded the full campaign again: 3 h 16 m,
measured, for a change that touched a handful of functions.

That check was also blind to the thing that actually falsifies a row. A KILLED
row is a claim about TWO pieces of the tree -- the code the mutant patches, and
THE TEST THAT DECIDED IT -- and the test lives outside `FULL_GLOBS`, so nothing
in it could see the test move. `docs/register-currency.md` section 4.1 builds
that hole in a throwaway clone: delete one test, and the done-gate reports
criterion 1 MET, at zero drift, over a row that has stopped being true.

THE THREE RULES, AND WHERE EACH ONE'S DATA COMES FROM
-----------------------------------------------------

RULE 1, the code the mutant patches. A row is stale when a changed hunk overlaps
the span of the item its mutant lives in. The spans come from
`cargo mutants --list --json` at the commit being judged -- the tool's own
answer, not a parse of Rust here -- intersected with `git diff -U0`'s new-side
hunk ranges. That enumeration is 4.4 s over `FULL_GLOBS`, measured, and it is
NOT run at all when nothing under those globs changed between the two commits:
identical bytes and an identical pinned tool enumerate identically, so there is
nothing for it to tell us.

RULE 2, the test that decided it. Every KILLED row carries `caught_by`, distilled
from the deciding run's own per-mutant log, and the row is stale when the source
of that test changed. A target's sources are DERIVED -- a `--test NAME` target is
`tests/NAME.rs` plus the subdirectories of `tests/`, which is Cargo's own layout
rule; a `--lib` test's module path names the file it is in -- so this is not a
table anybody has to maintain. A row whose catcher is `undetermined` -- the
timeout-decided mutants, which name no test and no target -- is stale on a change
to ANY FILE OF ANY TEST PACKAGE, source included, because "nobody knows what
caught this" has exactly one safe reading.

RULE 3, what forces the full run anyway. A function-scoped re-run is an
approximation: changing function A can flip a mutant in B through a callee, a
type or a constant. So [`assess`] carries a second list beside the stale one --
the escalations -- and every trigger in it is derived from the tree rather than
judged. Seven come from `docs/register-currency.md` section 5: a hunk in no
enumerated item, a frame that moved, a file that arrived, a row whose function is
gone, a callee outside the scope, the arithmetic backstop and the age backstop.
The rest come from this implementation's own limits and are escalations for the
same reason -- a missing pinned cargo-mutants, a stale row whose mutant has no
function for `-F` to select, a `#[cfg(test)]` region this could not place, and a
mutant the recorded campaign never enumerated. When any of them fires this names
the full-scope invocation instead of the filtered one.

FEWER UNNECESSARY REFUSALS, NOT FEWER REFUSALS
----------------------------------------------

Every decision this makes errs one way. A row it cannot place is STALE, not
current. A hunk it cannot attribute ESCALATES. A catcher it cannot resolve to a
file invalidates on the whole package.

AND ONE THING NO RULE HERE WATCHES, MEASURED RATHER THAN ASSUMED AWAY
---------------------------------------------------------------------

Rule 2 watches the test that decided a KILLED row. An ACCEPTED or EQUIVALENT row
names no test -- its claim is that NOTHING catches the mutant -- so the change
that falsifies it is a test being ADDED, and no rule above can see that.
`docs/register-currency.md` section 9 replays the register recorded at `23e81db`
against the full campaign at `c94612d`: 21 of its 143 rows had stopped being
true, every one of them a survivor that had since been caught, 20 of the 21 by
eight test functions one commit added, and the rules named one of the 21. The
direction is the safe one -- the register OVER-states the surviving set, and
[`mutation_register.check`] reports such rows as `retired` and never fails on
them -- but it is under-invalidation, so [`assess`] counts it on every run as
`survivor_register_rows_a_new_test_could_falsify` rather than leaving it in a
document. Closing it is not cheap for THIS register: two of its forty-eight
survivor rows are module-level items `cargo mutants -F` cannot select, so a rule
that invalidated survivor rows on a test change would escalate to the full scope
on every commit that touches a test.
"""

import dataclasses
import datetime as dt
import fnmatch
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import mutation_register  # noqa: E402 -- this module's own directory, resolved above

# The register's key, restated nowhere: `scripts/mutation_register.py` owns it
# and this reads it from there, so a sixth field added to one is a sixth field in
# both or an import error, never a silent disagreement.
KEY_FIELDS = mutation_register.KEY_FIELDS
SURVIVOR_REGISTER = "evidence/mutation-survivor-register.json"
ENUMERATION_CENSUS = "evidence/mutation-enumeration.json"
MUTATION_GATE = "scripts/gate-a-mutants.sh"
REFILTER_TOOL = "scripts/mutation_refilter.py"
TOOLCHAIN_PIN = "rust-toolchain.toml"
CARGO_MUTANTS_BIN = ".context/tools/cargo-mutants/bin/cargo-mutants"
FILTERED_RECEIPT_SCHEMA = "pmux.mutation-filtered-run.v1"
HUNK = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")
# A top-level `#[cfg(test)] mod NAME {` and the `}` that closes it in column one.
# Column one is not a guess: `cargo fmt --all --check` is a Gate A cell, so a
# top-level module's closing brace is at column zero or the tree is already red.
# The derivation is checked anyway -- see [`cfg_test_regions`].
TEST_MODULE_OPEN = re.compile(r"^(?:pub(?:\([^)]*\))? )?mod [A-Za-z_][A-Za-z0-9_]* \{$")
TEST_TARGET = re.compile(r"^-p (\S+)(?: --(lib|test|bin|bench|example)(?: (\S+))?)?$")


class CurrencyError(RuntimeError):
    """The currency of the register could not be decided at all."""


@dataclasses.dataclass
class Currency:
    """What is stale, what would refresh it, and what refuses to be refreshed."""

    head: str = ""
    commit: str = ""
    stale_functions: list[tuple[str, str]] = dataclasses.field(default_factory=list)
    stale_rows: list[tuple] = dataclasses.field(default_factory=list)
    reasons: list[str] = dataclasses.field(default_factory=list)
    escalations: list[str] = dataclasses.field(default_factory=list)
    notes: list[tuple[str, object]] = dataclasses.field(default_factory=list)

    def note(self, key: str, value: object) -> None:
        self.notes.append((key, value))

    @property
    def current(self) -> bool:
        return not self.reasons and not self.escalations


# ---------------------------------------------------------------------------
# Reading the gate's own declarations, so nothing here is a second copy
# ---------------------------------------------------------------------------


def gate_array(script: str, name: str) -> list[str]:
    """One `readonly NAME=( ... )` array out of the mutation gate.

    Both spellings that script uses: one line for the short arrays, one entry a
    line for `FULL_GLOBS`. Matching only the shape an array happens to have today
    is how this comes back empty the day somebody reflows it.
    """

    block = re.search(rf"^readonly {name}=\((.*?)\)$", script, re.MULTILINE | re.DOTALL)
    if block is None:
        raise CurrencyError(f"{MUTATION_GATE} no longer declares {name}")
    values = []
    for line in block.group(1).splitlines():
        values.extend(word.strip("'") for word in line.split("#", 1)[0].split())
    if not values:
        raise CurrencyError(f"{MUTATION_GATE} declares an empty {name}")
    return values


def gate_scalar(script: str, name: str) -> str:
    match = re.search(rf'^readonly {name}="([^"]*)"$', script, re.MULTILINE)
    if match is None:
        raise CurrencyError(f"{MUTATION_GATE} no longer declares {name}")
    return match.group(1)


def pathspecs(globs: list[str]) -> list[str]:
    """`FULL_GLOBS` as `git diff` pathspecs: a leading path is a whole subtree."""

    return [glob.rstrip("*").rstrip("/") for glob in globs]


def matches_globs(path: str, globs: list[str]) -> bool:
    return any(
        fnmatch.fnmatch(path, glob)
        or path.startswith(glob.rstrip("*").rstrip("/") + "/")
        for glob in globs
    )


# ---------------------------------------------------------------------------
# git
# ---------------------------------------------------------------------------


def git(repo: pathlib.Path, *arguments: str) -> str:
    done = subprocess.run(
        ["git", "-C", str(repo), *arguments],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    if done.returncode != 0:
        raise CurrencyError(
            f"git {' '.join(arguments)} failed: {done.stderr.strip() or done.returncode}"
        )
    return done.stdout


def changed_hunks(repo: pathlib.Path, head: str, commit: str, path: str):
    """New-side line ranges of every hunk that touched one file.

    A pure deletion has no new-side lines, and is reported as the pair of lines
    it landed between: whatever the deletion changed, it changed the meaning of
    the code on one side of it, and half a rule is not a rule.
    """

    ranges = []
    for line in git(repo, "diff", "-U0", head, commit, "--", path).splitlines():
        match = HUNK.match(line)
        if match is None:
            continue
        start = int(match.group(1))
        count = 1 if match.group(2) is None else int(match.group(2))
        ranges.append((start, start + count - 1) if count else (start, start + 1))
    return ranges


# ---------------------------------------------------------------------------
# The tree's own structure, derived rather than parsed
# ---------------------------------------------------------------------------


def cfg_test_regions(text: str) -> list[tuple[int, int]]:
    """Line ranges of the top-level `#[cfg(test)] mod ... { ... }` blocks.

    Not a Rust parser. It reads a `#[cfg(test)]` attribute in column one, the
    module it opens, and the first `}` in column one after it -- which is where
    rustfmt puts the close of a top-level item, and `cargo fmt --all --check` is
    a Gate A cell, so a tree where that is false is already red. Every caller
    validates the answer against the enumeration anyway (see
    [`attribute_hunks`]): a region that contains a mutant is a region this got
    wrong, and a wrong region is treated as no region at all.
    """

    lines = text.splitlines()
    regions = []
    index = 0
    while index < len(lines):
        if lines[index] == "#[cfg(test)]":
            opener = index + 1
            while opener < len(lines) and not lines[opener].strip():
                opener += 1
            if opener < len(lines) and TEST_MODULE_OPEN.match(lines[opener]):
                closer = opener + 1
                while closer < len(lines) and lines[closer] != "}":
                    closer += 1
                if closer < len(lines):
                    regions.append((index + 1, closer + 1))
                    index = closer
        index += 1
    return regions


def within(line_range: tuple[int, int], span: tuple[int, int]) -> bool:
    return line_range[0] <= span[1] and span[0] <= line_range[1]


def workspace_packages(repo: pathlib.Path) -> dict[str, pathlib.Path]:
    """{package name -> directory}, from the workspace manifest and no other list."""

    manifest = tomllib.loads((repo / "Cargo.toml").read_text(encoding="utf-8"))
    packages = {}
    for member in manifest.get("workspace", {}).get("members", []):
        member_manifest = repo / member / "Cargo.toml"
        if not member_manifest.is_file():
            continue
        name = tomllib.loads(member_manifest.read_text(encoding="utf-8"))
        name = name.get("package", {}).get("name")
        if name:
            packages[name] = repo / member
    if not packages:
        raise CurrencyError("Cargo.toml names no workspace members with packages")
    return packages


def declared_under_cfg_test(repo: pathlib.Path, path: str) -> bool:
    """Whether a source file is reached only through a `#[cfg(test)]` module.

    `crates/service/src/native/tests/seam.rs` is one: `mod seam;` sits inside
    `native.rs`'s `#[cfg(test)] mod tests { ... }`. Without this, every edit to a
    test module that happens to live in its own file would escalate to the
    three-hour run -- and adding a test is how a survivor gets closed. Anything
    this cannot place is NOT test-only, so an unplaceable file escalates.
    """

    file = pathlib.PurePosixPath(path)
    declaration = re.compile(
        rf"^\s*(?:pub(?:\([^)]*\))? )?mod {re.escape(file.stem)};", re.MULTILINE
    )
    parent = file.parent
    while str(parent) not in (".", "/"):
        for candidate in (parent.with_suffix(".rs"), parent / "mod.rs"):
            source = repo / candidate
            if not source.is_file():
                continue
            text = source.read_text(encoding="utf-8", errors="replace")
            match = declaration.search(text)
            if match is None:
                return False
            line = text[: match.start()].count("\n") + 1
            regions = cfg_test_regions(text)
            return any(start <= line <= end for start, end in regions)
        parent = parent.parent
    return False


# ---------------------------------------------------------------------------
# Rule 2 -- what sources a catching test is compiled from
# ---------------------------------------------------------------------------


def catcher_sources(
    repo: pathlib.Path, packages: dict, caught_by: dict
) -> tuple[str | None, list[str]]:
    """(the file that DEFINES the catching test, everything else it compiles).

    Derived from Cargo's layout and from the test's own module path, never from a
    table:

      * `--test NAME` is defined in `tests/NAME.rs` and also compiles every
        SUBDIRECTORY of `tests/` -- files directly under `tests/` are each their
        own target and cannot be modules of this one, and only a subdirectory can
        hold shared helpers;
      * `--lib` is defined in the file the test's module path names, walked from
        `src/`;
      * anything else, or a module path that resolves to nothing, has no defining
        file and invalidates on the whole package. Fail closed: an unresolvable
        catcher invalidates more, not less.

    The split matters because the defining file can be narrowed further -- to the
    test function's own span -- and the rest cannot.
    """

    everything = sorted(
        str(directory.relative_to(repo)) for directory in packages.values()
    )
    if not isinstance(caught_by, dict) or caught_by.get("undetermined"):
        # Nobody knows what caught it: every test source can falsify the row.
        return None, everything
    match = TEST_TARGET.match(caught_by.get("target") or "")
    if match is None:
        return None, everything
    package, kind, name = match.groups()
    directory = packages.get(package)
    if directory is None:
        return None, everything
    relative = directory.relative_to(repo)
    if kind == "test" and name:
        shared = (
            sorted(
                f"{relative}/tests/{child.name}"
                for child in (directory / "tests").iterdir()
                if child.is_dir()
            )
            if (directory / "tests").is_dir()
            else []
        )
        return f"{relative}/tests/{name}.rs", shared
    if kind == "lib":
        resolved = lib_module_source(directory, caught_by.get("test") or "")
        if resolved is not None:
            return str(resolved.relative_to(repo)), []
    return None, [f"{relative}/src"]


def catcher_moved(
    repo: pathlib.Path,
    head: str,
    commit: str,
    defining: str | None,
    shared: list[str],
    caught_by: dict,
) -> str | None:
    """Why one recorded catching test may no longer be the test it was, or None.

    Narrowest first, each step failing closed into the next:

      1. anything the test compiles besides the file that defines it changed;
      2. the defining file is gone, or the test's name is no longer in it, or
         this cannot find the end of the function -- the row is stale;
      3. a changed hunk overlaps the function's own span -- the row is stale;
      4. otherwise the file moved around the test and the test did not.

    Step 4 is the whole point. `docs/register-currency.md` section 4.2 records
    that 751 of 1,078 caught mutants in one campaign were caught by a `--lib`
    test, which lives in the same file as the code it tests; invalidating those
    rows on any change to that file would put the whole-file rule back.
    """

    if shared:
        moved = git(repo, "diff", "--name-only", head, commit, "--", *shared).split()
        if moved:
            return f"{moved[0]}, which its target also compiles, changed"
    if defining is None:
        return None
    if not git(repo, "diff", "--name-only", head, commit, "--", defining).split():
        return None
    source = repo / defining
    if not source.is_file():
        return f"{defining}, which defines it, is gone at {commit[:7]}"
    text = source.read_text(encoding="utf-8", errors="replace")
    span = test_span(text, caught_by.get("test") or "")
    if span is None:
        return (
            f"{defining} changed and the test is no longer a function this can find "
            "in it"
        )
    for hunk in changed_hunks(repo, head, commit, defining):
        if within(hunk, span):
            return (
                f"{defining} lines {hunk[0]}-{hunk[1]} changed inside it "
                f"(lines {span[0]}-{span[1]})"
            )
    return None


def test_span(text: str, name: str) -> tuple[int, int] | None:
    """The lines of one test function, attributes included, or None.

    Rustfmt again, and only rustfmt: a function's closing brace sits at the
    function's own indentation, so the span is the `fn` line -- with any `#[...]`
    attributes stacked above it -- down to the first line that is exactly that
    indentation and a brace. `cargo fmt --all --check` is a Gate A cell, so a
    tree where that is false is already red, and a name this cannot find or close
    returns None and invalidates the whole file instead of half of one.

    This is what makes ADDING a test cost nothing. An insertion inside a test
    module lands between two test functions, inside no recorded catcher's span,
    and adding a test cannot falsify a KILLED row in any case -- it can only add
    catchers. Deleting or weakening one lands inside the span, which is exactly
    the change that can.
    """

    leaf = name.split("::")[-1]
    opener = re.compile(
        rf"^(\s*)(?:pub(?:\([^)]*\))? )?(?:const )?(?:async )?fn {re.escape(leaf)}\s*[(<]"
    )
    lines = text.splitlines()
    for index, line in enumerate(lines):
        match = opener.match(line)
        if match is None:
            continue
        indent = match.group(1)
        # A body rustfmt kept on the signature's own line -- `fn wire() {}` --
        # closes where it opens, and looking for a brace on a line of its own
        # would run to the end of the file and give up on a function that is
        # perfectly findable.
        closer = index
        if not line.rstrip().endswith("}"):
            while closer < len(lines) and lines[closer] != f"{indent}}}":
                closer += 1
        if closer >= len(lines):
            return None
        start = index
        while start > 0 and lines[start - 1].strip().startswith("#["):
            start -= 1
        return start + 1, closer + 1
    return None


def lib_module_source(package: pathlib.Path, test_path: str) -> pathlib.Path | None:
    """The file a `--lib` test's module path names, or None if it names nothing.

    `driver_io::tests::a_fence_that_is_not_the_proven_frame_never_sends_enter` is
    `src/driver_io.rs`; `pool::refusal::tests::x` is `src/pool/refusal.rs`;
    `native::tests::seam::x` is `src/native/tests/seam.rs`, because an inline
    `mod tests` inside `native.rs` opens the directory `native/tests/`.
    """

    segments = test_path.split("::")[:-1]
    if not segments:
        return None
    here = package / "src"
    resolved = None
    for segment in segments:
        file = here / f"{segment}.rs"
        directory = here / segment
        if file.is_file():
            resolved, here = file, directory
        elif (directory / "mod.rs").is_file():
            resolved, here = directory / "mod.rs", directory
        elif directory.is_dir():
            here = directory
        else:
            break
    return resolved


# ---------------------------------------------------------------------------
# Rule 1 -- attributing hunks to items
# ---------------------------------------------------------------------------


def item_spans(run: dict) -> dict[str, list[tuple[int, int, str]]]:
    """{file -> [(start, end, function)]} for every item the enumeration named."""

    spans: dict[str, list[tuple[int, int, str]]] = {}
    for row in run.values():
        if row["item"] is None:
            continue
        entry = (row["item"][0], row["item"][1], row["function"])
        spans.setdefault(row["file"], [])
        if entry not in spans[row["file"]]:
            spans[row["file"]].append(entry)
    for file in spans:
        spans[file].sort()
    return spans


TEST_CODE = ("#[cfg(test)]",)


def hunk_rule(
    hunk: tuple[int, int],
    spans: list[tuple[int, int, str]],
    regions: list[tuple[int, int]],
) -> object:
    """Which rule one changed hunk falls under, given one file's structure.

    The functions it overlaps, if any -- rule 1. Otherwise [`TEST_CODE`] if the
    hunk is CONTAINED in a `#[cfg(test)]` module -- rule 2's business. Otherwise
    the empty list, which is rule 3(a): a declaration, an import, a type or a new
    item, whose effect is not bounded by one function.

    CONTAINED, and not merely overlapping. One hunk can be a product change and a
    test change at once -- a rewrite that runs from the last item in a file down
    into the test module below it is exactly that -- and reading overlap as
    containment lets the product half through as a test change, which is an
    under-invalidation and the one direction this may not err in.

    The order of the two questions is NOT load-bearing, and saying so is cheaper
    than a comment that implies it is: the caller hands over only the regions it
    has confirmed hold no enumerated item, so a hunk contained in one overlaps no
    span and the two answers can never both apply.
    """

    touched = [function for start, end, function in spans if within(hunk, (start, end))]
    if touched:
        return touched
    if any(start <= hunk[0] and hunk[1] <= end for start, end in regions):
        return TEST_CODE
    return []


def gap_of(
    spans: list[tuple[int, int, str]], line: int, last_line: int
) -> tuple[int, int]:
    """The span between the enumerated items that surround one line.

    THE FALLBACK FOR A MUTANT WITH NO FUNCTION, STATED WHERE IT IS USED. Twenty-
    one of the 1,661 mutants this tree enumerates carry `"function": null` -- all
    of them the initializer of a module-level `const` -- and for those the tool
    reports no item span to widen the mutant's own span to. Widening it to the
    whole gap between the two items that surround it is a SUPERSET of the
    declaration: the item is somewhere in that gap, and treating the gap as the
    item can only over-invalidate. It is never the mutant's own span, because a
    `const` whose initializer wraps over two lines would then be checked against
    half of itself.

    A hunk landing in a gap escalates under rule 3(a) in any case -- a gap is
    where declarations, imports, types and attributes live -- so this widening
    decides which ROWS are named, not whether the run is refused.
    """

    start, end = 1, last_line
    for span_start, span_end, _ in spans:
        if span_end < line:
            start = max(start, span_end + 1)
        elif span_start > line:
            end = min(end, span_start - 1)
    return start, end


# ---------------------------------------------------------------------------
# Enumerating the tree in front of us
# ---------------------------------------------------------------------------


def enumerate_now(
    repo: pathlib.Path, cargo_mutants: pathlib.Path, globs: list[str], cargo: str = ""
) -> list[dict]:
    """`cargo mutants --list --json` over `FULL_GLOBS`. Measured at 4.4 s.

    Run ONLY when something under those globs moved. Identical bytes and an
    identical pinned tool enumerate identically, so a commit that changed none of
    the mutated files has nothing for this to say, and a check that pays for the
    answer anyway on every green run is a check that gets disabled.
    """

    argv = [
        str(cargo_mutants), "mutants",
        "--no-config", "--gitignore=true", "--copy-vcs=false",
        "--test-workspace=false", "--no-times", "--list", "--json",
    ]  # fmt: skip
    for glob in globs:
        argv += ["--file", glob]
    environment = {
        "CARGO_TERM_COLOR": "never",
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    }
    if cargo:
        environment["CARGO"] = cargo
        environment["PATH"] = f"{pathlib.Path(cargo).parent}:{environment['PATH']}"
    done = subprocess.run(
        argv, cwd=str(repo), capture_output=True, text=True, timeout=1800,
        env=environment, check=False,
    )  # fmt: skip
    if done.returncode != 0:
        raise CurrencyError(
            f"the mutant enumeration failed (exit {done.returncode}): "
            f"{done.stderr.strip()[:400]}"
        )
    return json.loads(done.stdout)


# ---------------------------------------------------------------------------
# The assessment
# ---------------------------------------------------------------------------


def assess(
    repo: pathlib.Path,
    commit: str,
    *,
    register: dict,
    census: dict,
    now: dt.datetime,
    max_receipt_age_days: float,
    cargo_mutants: pathlib.Path | None = None,
    cargo: str = "",
    enumerate_mutants=None,
) -> Currency:
    """Everything that makes the register stale, and what would refresh it.

    `enumerate_mutants` is how the current tree is enumerated, injectable so that
    the tests can drive every rule below without an 86-second `cargo mutants` for
    each one. It defaults to the real thing.
    """

    enumerate_mutants = enumerate_mutants or enumerate_now

    script = (repo / MUTATION_GATE).read_text(encoding="utf-8")
    globs = gate_array(script, "FULL_GLOBS")
    packages = workspace_packages(repo)
    recorded = register.get("recorded_at", {})
    head = recorded.get("head") or ""
    currency = Currency(head=head, commit=commit)

    # --- rule 3(g): the age backstop, tied to the number criterion 4 already
    # uses. Two freshness rules for one commit is how the two drift apart.
    recorded_date = recorded.get("date") or ""
    try:
        age = (
            now
            - dt.datetime.fromisoformat(recorded_date).replace(tzinfo=dt.timezone.utc)
        ).total_seconds() / 86400.0
    except ValueError:
        currency.escalations.append(
            f"the register records date {recorded_date!r}, which is not a date, so "
            "its age cannot be checked"
        )
        age = 0.0
    currency.note("survivor_register_age_days", f"{age:.1f}")
    if age > max_receipt_age_days:
        currency.escalations.append(
            f"rule 3(g): the full-scope recording is {age:.1f} days old, past the "
            f"{max_receipt_age_days:g}-day bound a Gate A receipt is held to"
        )

    # --- rule 3(b): the measurement's own frame. Compared against what the
    # census RECORDED, not against a second copy of the script's literals.
    frame = census.get("recorded_at", {})
    if frame.get("head") != head:
        currency.escalations.append(
            f"rule 3(b): the enumeration census was recorded at {frame.get('head')!r} "
            f"and the register at {head!r}; they describe two trees"
        )
    if frame.get("scope") != "full":
        currency.escalations.append(
            f"rule 3(b): the enumeration census was recorded at scope "
            f"{frame.get('scope')!r}, and only a full-scope one covers the register"
        )
    for name, recorded_value, live_value in (
        ("FULL_GLOBS", frame.get("globs"), globs),
        (
            "TEST_PACKAGES",
            frame.get("test_packages"),
            gate_array(script, "TEST_PACKAGES"),
        ),
        (
            "MUTATION_PROFILE",
            frame.get("mutation_profile"),
            gate_scalar(script, "MUTATION_PROFILE"),
        ),
        (
            "REQUIRED_CARGO_MUTANTS_VERSION",
            frame.get("cargo_mutants_version"),
            gate_scalar(script, "REQUIRED_CARGO_MUTANTS_VERSION"),
        ),
        (
            "rust-toolchain.toml channel",
            frame.get("toolchain_channel"),
            tomllib.loads((repo / TOOLCHAIN_PIN).read_text(encoding="utf-8"))
            .get("toolchain", {})
            .get("channel"),
        ),
    ):
        if recorded_value != live_value:
            currency.escalations.append(
                f"rule 3(b): {name} was {recorded_value!r} when the register was "
                f"recorded and is {live_value!r} now, so the measurement's own frame "
                "moved and no filtered run can re-decide it"
            )

    # --- what moved between the two commits, over the mutated files.
    status = git(repo, "diff", "--name-status", head, commit, "--", *pathspecs(globs))
    changed, arrivals = [], []
    for line in status.splitlines():
        parts = line.split("\t")
        if parts[0].startswith(("A", "R", "C")):
            arrivals.append(parts[-1])
        changed.append(parts[-1])
    currency.note("survivor_register_files_drifted", len(changed))
    for path in sorted(set(arrivals)):
        currency.escalations.append(
            f"rule 3(c): {path} matches FULL_GLOBS and was added or renamed since the "
            "register was recorded, so it has no rows and no row of it can be stale"
        )

    # --- rule 3(e): a callee the mutated crates compile that FULL_GLOBS misses.
    mutated_packages = sorted(
        {
            name
            for name, directory in packages.items()
            if any(glob.startswith(f"{directory.relative_to(repo)}/") for glob in globs)
        }
    )
    outside = []
    for name in mutated_packages:
        relative = packages[name].relative_to(repo)
        for path in git(
            repo, "diff", "--name-only", head, commit, "--", f"{relative}/src"
        ).split():
            if matches_globs(path, globs) or declared_under_cfg_test(repo, path):
                continue
            outside.append(path)
    for path in sorted(set(outside)):
        currency.escalations.append(
            f"rule 3(e): {path} changed, is compiled into a mutated crate and is not "
            "covered by FULL_GLOBS, so a mutant inside the scope can have flipped "
            "with nothing in the enumeration able to see it"
        )

    # --- rule 3(f): the arithmetic backstop. Once the filtered runs since the
    # last full one have cost more mutants than one full run, take the full one.
    spent, receipts = filtered_spend(repo, head)
    enumerated_count = frame.get("enumerated") or 0
    currency.note("filtered_mutants_since_full_scope", spent)
    if enumerated_count and spent >= enumerated_count:
        currency.escalations.append(
            f"rule 3(f): {spent} mutant(s) have been graded by filtered runs "
            f"({', '.join(receipts)}) since the last full-scope one, which enumerated "
            f"{enumerated_count}; the cheap path has already cost more than the sound one"
        )

    # --- rule 2: the test that decided each KILLED row. This needs no
    # enumeration and runs whether or not the mutated files moved, because the
    # tests do not live under FULL_GLOBS and never did.
    undetermined = 0
    for entry in register.get("entries", []):
        if entry.get("disposition") != "KILLED":
            continue
        caught_by = entry.get("caught_by") or {}
        if caught_by.get("undetermined"):
            undetermined += 1
        defining, shared = catcher_sources(repo, packages, caught_by)
        why = catcher_moved(repo, head, commit, defining, shared, caught_by)
        if why is None:
            continue
        currency.stale_rows.append(tuple(entry[field] for field in KEY_FIELDS))
        currency.stale_functions.append((entry["file"], entry["function"]))
        currency.reasons.append(
            f"rule 2: {entry['file']}::{entry['function']} is KILLED by "
            f"{caught_by.get('test') or caught_by.get('undetermined') or 'an unrecorded test'}"
            f" ({caught_by.get('target') or 'no recorded target'}), and {why}"
        )
    currency.note("survivor_register_undetermined_catchers", undetermined)

    # --- rule 1, and the escalations that need the enumeration.
    if changed:
        if cargo_mutants is None or not cargo_mutants.is_file():
            currency.escalations.append(
                f"{len(changed)} mutated file(s) changed and the pinned cargo-mutants "
                f"is not at {CARGO_MUTANTS_BIN}, so which functions moved cannot be "
                "decided here"
            )
        else:
            run = mutation_register.enumerated(
                enumerate_mutants(repo, cargo_mutants, globs, cargo)
            )
            attribute_hunks(
                repo, head, commit, changed, run, register, census, currency
            )
    currency.stale_functions = sorted(set(currency.stale_functions))
    # Every row of a stale function is stale, whatever its disposition: an
    # ACCEPTED row says this mutant survives THIS code, and rule 1 has just said
    # the code is not that code. Collected here rather than at each rule so the
    # two lists cannot disagree about which rows the named functions hold.
    for entry in register.get("entries", []):
        if (entry["file"], entry["function"]) in set(currency.stale_functions):
            currency.stale_rows.append(tuple(entry[field] for field in KEY_FIELDS))
    currency.stale_rows = sorted(set(currency.stale_rows))
    currency.note("survivor_register_stale_rows", len(currency.stale_rows))
    currency.note("survivor_register_stale_functions", len(currency.stale_functions))
    # THE EXPOSURE THIS DOES NOT COVER, COUNTED ON EVERY RUN. A survivor row is
    # falsified by a test being ADDED, and it names no test for rule 2 to watch,
    # so the rows above are the ones this run would re-decide and these are the
    # ones it would not. Printed as a number by criterion 1 rather than left as a
    # paragraph in `docs/register-currency.md`, which is where a limit goes to be
    # forgotten. Derived from the register in front of it, so it shrinks when a
    # survivor is closed and grows when one is accepted.
    unwatched = [
        entry
        for entry in register.get("entries", [])
        if entry.get("disposition") in ("ACCEPTED", "EQUIVALENT")
        and tuple(entry[field] for field in KEY_FIELDS) not in set(currency.stale_rows)
    ]
    currency.note("survivor_register_rows_a_new_test_could_falsify", len(unwatched))
    # A stale row whose mutant has no function cannot be selected by `-F`, so a
    # filtered run cannot re-decide it. Stated as an escalation rather than
    # silently dropped from the command that claims to refresh everything.
    for file, function in currency.stale_functions:
        if not function:
            currency.escalations.append(
                f"a stale row in {file} has no function -- a module-level item -- and "
                "`cargo mutants -F` selects by function name, so a filtered run cannot "
                "re-decide it"
            )
    return currency


def filtered_spend(repo: pathlib.Path, head: str) -> tuple[int, list[str]]:
    """Mutants graded by filtered runs since the register's full-scope one."""

    spent, named = 0, []
    for path in sorted((repo / "evidence").glob("*.json")):
        try:
            receipt = json.loads(path.read_text(encoding="utf-8"))
        except ValueError:
            continue
        if (
            not isinstance(receipt, dict)
            or receipt.get("schema") != FILTERED_RECEIPT_SCHEMA
        ):
            continue
        commit = receipt.get("commit")
        if commit:
            ancestor = subprocess.run(
                ["git", "-C", str(repo), "merge-base", "--is-ancestor", commit, head],
                capture_output=True, timeout=120, check=False,
            )  # fmt: skip
            if ancestor.returncode == 0:
                continue
        spent += (receipt.get("counts") or {}).get("total_mutants") or 0
        named.append(path.name)
    return spent, named


def attribute_hunks(
    repo: pathlib.Path,
    head: str,
    commit: str,
    changed: list[str],
    run: dict,
    register: dict,
    census: dict,
    currency: Currency,
) -> None:
    """Rule 1, then the three escalations that need the enumeration: 3(a) for a
    hunk in no item, 3(d) for a row the enumeration no longer names, and a mutant
    the register's own campaign never saw."""

    spans = item_spans(run)
    for path in sorted(set(changed)):
        source = repo / path
        if not source.is_file():
            currency.escalations.append(
                f"rule 3(c): {path} changed and is not a file at {commit[:7]}"
            )
            continue
        text = source.read_text(encoding="utf-8", errors="replace")
        last_line = text.count("\n") + 1
        file_spans = spans.get(path, [])
        regions = cfg_test_regions(text)
        # The derivation checked, not trusted: a `#[cfg(test)]` region holding an
        # enumerated mutant is a region this got wrong, and a wrong region would
        # excuse a product change as a test change.
        confirmed = [
            region
            for region in regions
            if not any(within(region, (start, end)) for start, end, _ in file_spans)
        ]
        for region in sorted(set(regions) - set(confirmed)):
            currency.escalations.append(
                f"rule 3(a): {path} lines {region[0]}-{region[1]} read as a "
                "`#[cfg(test)]` module and hold an enumerated mutant, so this cannot "
                "tell that file's test code from its product code"
            )
        for hunk in changed_hunks(repo, head, commit, path):
            touched = hunk_rule(hunk, file_spans, confirmed)
            if touched is TEST_CODE:
                continue  # rule 2 is what decides the rows those can falsify
            if touched:
                for function in touched:
                    currency.stale_functions.append((path, function))
                    currency.reasons.append(
                        f"rule 1: {path}::{function} changed at lines "
                        f"{hunk[0]}-{hunk[1]}"
                    )
                continue
            gap = gap_of(file_spans, hunk[0], last_line)
            currency.escalations.append(
                f"rule 3(a): {path} lines {hunk[0]}-{hunk[1]} changed inside no "
                f"enumerated item (the gap {gap[0]}-{gap[1]} between two of them) and "
                "outside the file's own `#[cfg(test)]` modules -- a declaration, an "
                "import, a type or a new item, whose effect is not bounded by one "
                "function"
            )
    # --- rule 3(d): a row whose function the enumeration no longer names. A
    # renamed or deleted function makes the row un-re-decidable -- `-F` has
    # nothing to select and the row's claim is about code that is gone. REMOVED
    # rows are exempt: "this mutant is no longer enumerated" is what they assert.
    placed = {(row["file"], row["function"]) for row in run.values()}
    for entry in register.get("entries", []):
        if entry.get("disposition") == "REMOVED":
            continue
        if (entry["file"], entry["function"]) not in placed:
            currency.escalations.append(
                f"rule 3(d): the register holds a row for {entry['file']}::"
                f"{entry['function']}, which the enumeration at {commit[:7]} no longer "
                "names, so no run can re-decide it"
            )

    counts = census.get("counts", {})
    stale = set(currency.stale_functions)
    for key, row in run.items():
        file, function = key[0], key[1]
        # `covers` and not a second walk of the same nesting: the census is
        # written by `scripts/mutation_register.py` and read back through the
        # function that file exports for the purpose.
        if mutation_register.covers(counts, key):
            continue
        if (file, function) in stale:
            continue  # a mutant of a function this run is about to re-decide
        currency.escalations.append(
            f"the enumeration at {commit[:7]} holds {row['name']}, which the register's "
            "own campaign never enumerated and no re-run of a stale function would "
            "reach"
        )


# ---------------------------------------------------------------------------
# What would refresh it -- printed, because "N files changed" is not a remedy
# ---------------------------------------------------------------------------


def toolchain_channel(repo: pathlib.Path) -> str:
    return (
        tomllib.loads((repo / TOOLCHAIN_PIN).read_text(encoding="utf-8"))
        .get("toolchain", {})
        .get("channel", "")
    )


def filtered_command(repo: pathlib.Path) -> str:
    """The one command that re-decides exactly the stale set.

    `--stale` and not a copied-out list of functions: the command re-derives the
    set from this same code, so the remedy cannot name a different set from the
    refusal that printed it.
    """

    return (
        f"PYTHONDONTWRITEBYTECODE=1 python3 {REFILTER_TOOL} --repo . "
        f'--cargo "$(rustup which --toolchain {toolchain_channel(repo)} cargo)" '
        f"--cargo-mutants {CARGO_MUTANTS_BIN} "
        "--work-dir /tmp/refilter --receipt evidence/mutation-filtered-run-NAME.json "
        "--stale"
    )


def full_command(repo: pathlib.Path, register: dict) -> str:
    """The full-scope campaign, with the floor read out of the register."""

    floor = register.get("recorded_at", {}).get("floor_percent")
    return (
        f'env PMUX_MUTANTS_CARGO="$(rustup which --toolchain '
        f'{toolchain_channel(repo)} cargo)" '
        f"PMUX_CARGO_MUTANTS_BIN={CARGO_MUTANTS_BIN} "
        f"PMUX_MUTANTS_MINIMUM_SCORE={floor} PMUX_MUTANTS_SCOPE=full "
        "PMUX_MUTANTS_JOBS=4 PMUX_MUTANTS_WORK_DIR=/tmp/mut/work "
        f"PMUX_MUTANTS_EVIDENCE_ROOT=/tmp/mut/evidence bash {MUTATION_GATE}"
    )
