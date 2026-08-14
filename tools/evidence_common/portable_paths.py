"""Every path an artefact records, written against a root that names no machine.

An artefact that spells `/Users/somebody/checkout/target/release/pmux` is worth
less than one that spells `<REPO>/target/release/pmux`: the second says what
the first says, plus it is true on the machine the reader is sitting at. This
module is how a receipt gets the second form, AT THE POINT OF GENERATION. A
scrub applied afterwards fixes the files that exist; the next campaign writes
the absolute paths straight back.

NOTHING HERE IS WRITTEN DOWN. The roots are asked of the running environment --
home, login name, checkout, the distance between home and the checkout, and the
temporary directory when it is this machine's rather than this platform's. A
substitution map whose needles are literals is this repository's own bug class
in its most inviting shape: the list is composed on the host that has nothing
left to find, so it passes there, and it keeps passing on the next host for the
same reason.

The placeholders PRESERVE STRUCTURE, and that is load-bearing rather than
decorative. `_validate_campaign_contract` in `tools/phase0/phase0_lib.py` reads
a recorded binary path back and checks that its file name is the binary's name
and that every release binary shares one parent directory; both survive
`<REPO>/target/release/pmux` and neither survives a digest or an elision. A
placeholder that keeps the shape keeps the checks that read the shape.

STRUCTURE INCLUDES THE ROOT, and the default map drops it. `<REPO>/target/
release/pmux` keeps `Path(...).name` and `Path(...).parent`, which is every
check a rendered receipt is read by today -- and it does NOT keep
`Path(...).is_absolute()`, which is what `_validate_public_file_identity` and
`source_digest._canonical_absolute_path_text` test. The gap did not matter
while the one file holding validated absolute paths was exempt from the map.
`absolute_placeholders=True` closes it: a needle that is an absolute path gets
a placeholder that is an absolute path, spelled with the needle's own root, so
`/<REPO>/target/release/pmux` renders and validates. The flag is not the
default because every receipt already committed spells the rootless form, and
one map with two spellings of one placeholder would be two maps.

## The scope is the tracked tree, and the set is asked of git

`tracked_files` is `git ls-files`, so a file added tomorrow is in scope tomorrow
without anybody remembering to add it, and `tree_offences` is the one scan both
`--check` and the gate test run. The alternative -- a directory list, or a
suffix list, or "the documents I found" -- is the same defect as a literal
needle wearing a different coat: it is written on the host that has already been
searched, and it is complete there and nowhere else.

Every file is read with `surrogateescape`, so a file this module cannot decode
is still scanned rather than skipped. A checker that skips what it cannot read
reports success over the bytes it did not look at.

## Which writer may touch a file, and how that is decided

It is a PROPERTY OF THE FILE, checked against the file, and never a name on a
list -- `keeps_its_paths` asks whether every record the file holds is sealed
against its own canonical body, chained to every byte in front of it, chained
to the seal of the record before it, and bound to the digest of its own
immutable prefix. A file cannot buy the refusal by writing the field names,
because every binding present must verify and there must be at least one.

THE EXEMPTION IS NOW ONLY THE WRITER'S, NOT THE CHECKER'S. `--rewrite` still
refuses a sealed file -- a blind substitution does not redact a sealed record,
it forges it -- but `--check` and `tree_offences` read one. A checker that
excused the one file holding the most occurrences reported zero over a file it
never opened, which is this repository's own bug class: the message said "no
tracked file names this machine" and the predicate said "no unsealed tracked
file does". The remedy for a sealed file is not a hand edit and not `--rewrite`;
it is `tools/phase0/reseal_ledger.py`, which substitutes and then RE-SEALS with
the machinery that owns the seal, so the chain the checker verifies is the chain
the appender will verify.

The pinned gate receipt (`scripts/gate-in-worktree.sh`) still records absolute
paths and is not sealed: `scripts/path_b_done.py` opens `artefacts[].path`,
re-hashes it, and compares the gate receipt's `workspace` to the pinned
receipt's `worktree`. Those two are written by two processes whose checkouts
differ, so one would render against the pinned worktree and the other against
this one, and two spellings of one directory would stop comparing equal.

## What is not a machine identifier

`macos`, `aarch64` and `macOS-15.7.7` are not looked for: the compatibility
profile is keyed on them and the Linux handoff is entirely about that boundary.
Neither is `smithers`, which is a shipped product module. `/private/tmp` is the
same string on every host of this platform, so the temporary directory is taken
only where it DIFFERS from the platform default -- a rule found by running the
check rather than by reasoning about it, when taking both spellings reported
seven offences and every one of them was substantive.

`pseudomux` is deliberately not derivable even though it is a path component of
this checkout, because it is also the crate namespace. The ancestors between
home and the checkout are taken as ONE needle rather than as one needle each,
which is the difference.
"""

from __future__ import annotations

import argparse
import getpass
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
from typing import Any, Sequence

WORKSPACE = pathlib.Path(__file__).resolve().parents[2]

# The tokens the map substitutes in. Structure-preserving by design.
#
# `<DIGEST>` was missing here from the commit that introduced the needle, and
# `test_every_placeholder_the_map_can_emit_is_declared` has been red ever since:
# a token the map can emit and this tuple does not name is invisible to the
# idempotence check, which is the one property a reader can verify without
# re-running the emitter. `absolute_placeholders=True` emits `/<HOME>` and the
# rest, and those are not separate tokens -- each one CONTAINS the token named
# here, so every check that looks for a declared token still finds one.
PLACEHOLDERS = ("<TMPDIR>", "<HOME>", "<WORKSPACES>", "<REPO>", "<USER>", "<DIGEST>")

# A needle shorter than this is a word before it is an identifier, and matching
# on one would report a document's prose rather than its provenance.
MINIMUM_NEEDLE_LENGTH = 5

# Every binding a sealed record carries, and there are FOUR: the digest of its
# own canonical body, the digest of every byte in front of it, the seal of the
# record before it, and the boundary and digest of the immutable prefix it was
# reserved against. All four are written by `_append_reservation_locked` in
# `tools/phase0/phase0_lib.py` and all four are re-verified there before the
# next reservation is appended (`:4024-4040`).
#
# The last two arrived here because `tools/phase0/reseal_ledger.py` RECOMPUTES
# them. A re-seal that recomputed a binding nothing re-verifies would be a chain
# that looks sealed and is not checked, which is worse than no seal: the
# artefact would carry four digests and this repository would test two.
SEAL_FIELD = "reservation_sha256"
CHAIN_FIELD = "previous_ledger_sha256"
RESERVATION_CHAIN_FIELD = "previous_reservation_sha256"
PREFIX_RECORDS_FIELD = "ledger_prefix_records"
PREFIX_DIGEST_FIELD = "ledger_prefix_sha256"

# Arbitrary bytes survive this round trip unchanged, so a file that is not UTF-8
# is scanned rather than skipped, and rewriting one cannot corrupt it.
BYTES_AS_TEXT = "surrogateescape"


def _platform_default_temporary_directory() -> str:
    """The temporary directory this platform falls back to with no environment.

    The derivation `scripts/gate-in-worktree.sh` already uses: ask once with
    `TMPDIR`, `TMP` and `TEMP` removed and the cache reset.
    """
    stashed = {name: os.environ.pop(name, None) for name in ("TMPDIR", "TMP", "TEMP")}
    try:
        tempfile.tempdir = None
        return tempfile.gettempdir()
    finally:
        for name, value in stashed.items():
            if value is not None:
                os.environ[name] = value
        tempfile.tempdir = None


def _private_temporary_directory() -> str | None:
    """The temporary directory, only if it is THIS machine's not this platform's.

    Asked as the environment stands and kept only where it differs from the
    platform default. What is machine-specific is the hashed per-user path a
    shell points `TMPDIR` at; `/private/tmp` identifies the platform and names
    no machine, and this repository carries a real finding about `/tmp` being a
    symlink to it that a scrub would destroy.
    """
    configured = pathlib.Path(tempfile.gettempdir())
    default = pathlib.Path(_platform_default_temporary_directory())
    # Compared resolved, so `/tmp` and `/private/tmp` are one answer; RETURNED
    # as the environment spells it, so `_spellings` can offer both.
    if configured.resolve() == default.resolve():
        return None
    return str(configured)


def _spellings(path: pathlib.Path) -> list[str]:
    """One directory, every way this platform spells it, resolved form first.

    macOS answers `/tmp` and `/var` as symlinks into `/private`, so the path a
    tool RESOLVES and the path it merely makes absolute are two strings naming
    one directory. `scripts/path_b_done.py` already says so about the gate
    receipts -- "two spellings of one directory are the ordinary case here" --
    and a map carrying only the resolved spelling silently misses every emitter
    that did not resolve, which is most of them.

    FOUND BY RUNNING IT, not by reasoning about it: `measure_transcript_drain.py`
    was driven against a corpus under a temporary `HOME`, and the root came back
    out of the receipt unrendered, because the needle was `/private/var/...` and
    the receipt said `/var/...`.
    """
    seen: list[str] = []
    for candidate in (path.resolve(), path.absolute()):
        text = str(candidate)
        if text not in seen:
            seen.append(text)
    return seen


def _rooted(needle: str, placeholder: str) -> str:
    """A placeholder that is absolute exactly when the needle it replaces is.

    The root is taken from the NEEDLE rather than spelled here, so the rule is
    "keep whatever root this path had" rather than "prepend a slash", and a
    needle that has no root keeps none: `<USER>` is a login name and
    `<WORKSPACES>` is the relative distance from home to the checkout, and
    giving either one a root would invent a directory that never existed.
    """
    path = pathlib.PurePath(needle)
    return f"{path.root}{placeholder}" if path.is_absolute() else placeholder


def machine_identifiers(
    workspace: pathlib.Path | None = None,
    *,
    absolute_placeholders: bool = False,
) -> dict[str, tuple[str, str]]:
    """{description: (needle, placeholder)}, derived from this machine.

    Keyed by description so a failure says which identifier leaked rather than
    only that something did. `workspace` defaults to the checkout this file is
    in, which is what an emitter wants: a tool running inside a pinned worktree
    should render that worktree as `<REPO>`, because that is the repository the
    run it is describing actually had.

    `absolute_placeholders` renders an absolute needle as an absolute
    placeholder -- `/Users/somebody` becomes `/<HOME>` rather than `<HOME>` --
    for the one artefact whose paths are read back through
    `Path(...).is_absolute()`. It is a parameter of THIS derivation rather than
    a second map beside it: the needles are the same needles, asked of the same
    machine, and a second map is the one that goes stale.
    """
    given = workspace or WORKSPACE
    root = given.resolve()
    home = pathlib.Path.home().resolve()
    found: dict[str, tuple[str, str]] = {
        "worktree directory name": (root.name, "<REPO>"),
        "login name": (getpass.getuser(), "<USER>"),
    }
    for label, path, placeholder in (
        ("checkout path", given, "<REPO>"),
        ("home directory", pathlib.Path.home(), "<HOME>"),
    ):
        for index, spelling in enumerate(_spellings(path)):
            description = label if index == 0 else f"{label}, unresolved"
            found[description] = (spelling, placeholder)
    try:
        # The whole distance from home to this checkout as one needle: taken
        # component by component it would include the project's own name.
        found["path from home to checkout"] = (
            root.relative_to(home).as_posix(),
            "<WORKSPACES>",
        )
    except ValueError:
        pass
    private_temporary = _private_temporary_directory()
    if private_temporary is not None:
        for index, spelling in enumerate(_spellings(pathlib.Path(private_temporary))):
            description = (
                "private temporary directory"
                if index == 0
                else "private temporary directory, unresolved"
            )
            found[description] = (spelling, "<TMPDIR>")
    # A TRUNCATED DIGEST OF A NEEDLE IS NOT A NEEDLE, and no substitution over
    # path spellings can ever catch one. pmux namespaces the keychain service
    # name and the daemon socket directory by `sha256(config_dir)[0..8]`
    # (`crates/service/src/config_isolation.rs`), so those eight characters are
    # a one-way function of a path that contains the operator's account name --
    # published beside the command that produces them, as this repository's own
    # documentation published them, they make that name recoverable by
    # dictionary attack over low-entropy account names.
    #
    # Derived, never written down: the digest of a literal in a committed map
    # would publish the very value the map exists to remove. Every config root
    # this machine actually has is hashed the way the product hashes it.
    for config_root in sorted(home.glob(".claude*")):
        if not config_root.is_dir():
            continue
        for spelling in _spellings(config_root):
            digest = hashlib.sha256(spelling.encode()).hexdigest()[:8]
            found[f"config-root digest, {config_root.name}"] = (digest, "<DIGEST>")
    return {
        description: (
            needle,
            _rooted(needle, placeholder) if absolute_placeholders else placeholder,
        )
        for description, (needle, placeholder) in found.items()
        if len(needle) >= MINIMUM_NEEDLE_LENGTH
    }


def substitutions(
    workspace: pathlib.Path | None = None,
    *,
    absolute_placeholders: bool = False,
) -> list[tuple[str, str]]:
    """The map as (needle, placeholder), longest needle first.

    Order is load-bearing and is derived rather than declared: the checkout
    path contains the home directory, which contains the login name, so a
    shorter needle applied first would leave the longer one half-substituted
    and unrecognisable to both this map and the check that it was applied.
    """
    pairs = list(
        machine_identifiers(
            workspace, absolute_placeholders=absolute_placeholders
        ).values()
    )
    return sorted(pairs, key=lambda pair: len(pair[0]), reverse=True)


def offences(text: str, identifiers: dict[str, tuple[str, str]]) -> list[str]:
    """Every identifier the text still carries, as reportable lines."""
    found = []
    for number, line in enumerate(text.splitlines(), 1):
        for description, (needle, _) in identifiers.items():
            if needle in line:
                found.append(
                    f"{number}: {description} ({needle!r}) in {line.strip()!r}"
                )
    return found


def render(value: str, *, against: Sequence[tuple[str, str]] | None = None) -> str:
    """One string, written against the named roots.

    Idempotent: no placeholder contains a needle, so a second pass is a no-op.
    """
    for needle, placeholder in against if against is not None else substitutions():
        value = value.replace(needle, placeholder)
    return value


def render_document(
    document: Any, *, against: Sequence[tuple[str, str]] | None = None
) -> Any:
    """A whole JSON-able receipt, rendered at its serialisation choke point.

    Applied to the document rather than field by field ON PURPOSE. A renderer
    with a hand-written list of path-valued fields is the bug class this
    repository names: the list is written against the receipt as it is today
    and says nothing about the field somebody adds next week. Keys are rendered
    as well as values, because a receipt that keys a map by file name keys it by
    a path.
    """
    resolved = substitutions() if against is None else against
    if isinstance(document, str):
        return render(document, against=resolved)
    if isinstance(document, dict):
        return {
            render_document(key, against=resolved): render_document(
                value, against=resolved
            )
            for key, value in document.items()
        }
    if isinstance(document, (list, tuple)):
        return [render_document(item, against=resolved) for item in document]
    return document


def nested_placeholders(text: str) -> list[str]:
    """Placeholders substituted into placeholders: the map applied twice.

    Idempotence is the property that makes re-running the transformation safe,
    and this is the form the artefact itself can witness without re-running it.
    """
    return [token for token in PLACEHOLDERS if f"<{token}>" in text]


def tracked_files(workspace: pathlib.Path | None = None) -> list[pathlib.Path]:
    """Every file git tracks, asked of git.

    The set is DERIVED and total. A hand-written list of directories to scrub is
    the same defect as a hand-written list of identifiers to scrub for: it is
    composed against the tree as it stands, it is complete on the host that
    composed it, and the file somebody adds next week is outside it silently.
    """
    root = (workspace or WORKSPACE).resolve()
    listed = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [root / name for name in listed.split("\0") if name]


def read_text(path: pathlib.Path) -> str:
    """A file's bytes as text, losslessly, whatever they are.

    Nothing tracked here fails to decode as UTF-8 today, and that is exactly why
    the reader must not assume it: the first binary fixture somebody commits
    would otherwise leave the scan reporting success over a file it never
    opened.
    """
    return path.read_bytes().decode("utf-8", BYTES_AS_TEXT)


def sealed_records(text: str) -> list[dict]:
    """The records of a file no byte of which can be changed unnoticed.

    FOUR bindings, because such a file carries two kinds of record and the
    sealed kind is pinned four ways. A reservation pins its own canonical body
    through `reservation_sha256`; it pins every byte in front of it through
    `previous_ledger_sha256` -- which is what covers the legacy records that
    predate the reservation schema and carry no digest of their own; it pins the
    reservation before it through `previous_reservation_sha256`; and it pins the
    boundary and content of its immutable prefix through `ledger_prefix_records`
    and `ledger_prefix_sha256`. Requiring the LAST record to be sealed is what
    stops the coverage running out at the end of the file.

    All four are checked HERE and not only where they are written, because this
    is the verifier that runs on the committed artefact on every gate.
    `phase0_lib._append_reservation_locked` checks the same four before it
    appends, but only over the records after the prefix boundary the driver
    hands it -- and the driver derives that boundary as the whole file
    (`records=len(lines)`), so on the committed ledger it checks none of them.

    Empty on anything that is not that shape, so a file cannot buy the exemption
    by writing the field names: every binding present must verify and there must
    be at least one record carrying them.

    The canonical encoding and the digest are imported from
    `tools/phase0/phase0_lib.py` rather than restated, because a second spelling
    of a canonical encoding is a second encoding and the copy is the one that
    drifts. Imported HERE rather than at module scope: the emitters that render
    a receipt in flight must not pull in the campaign library to do it.
    """
    phase0 = WORKSPACE / "tools" / "phase0"
    if str(phase0) not in sys.path:
        sys.path.insert(0, str(phase0))
    import phase0_lib  # noqa: PLC0415

    text_lines = text.splitlines(keepends=True)
    lines = [line.encode("utf-8", BYTES_AS_TEXT) for line in text_lines]
    seals: list[str | None] = [None] * len(lines)
    records: list[dict] = []
    preceding = b""
    last_was_sealed = False
    for index, line in enumerate(text_lines):
        if not line.strip():
            preceding += lines[index]
            continue
        try:
            record = json.loads(line)
        except ValueError:
            return []
        if not isinstance(record, dict):
            return []
        claimed = record.get(SEAL_FIELD)
        last_was_sealed = isinstance(claimed, str)
        if not last_was_sealed:
            preceding += lines[index]
            continue
        body = {key: value for key, value in record.items() if key != SEAL_FIELD}
        if phase0_lib.sha256_bytes(phase0_lib.canonical_json_bytes(body)) != claimed:
            return []
        if record.get(CHAIN_FIELD) != phase0_lib.sha256_bytes(preceding):
            return []
        # The prefix boundary is the record's OWN, not the file's: each campaign
        # run hands `phase0` the ledger it found as its immutable prefix, so
        # `ledger_prefix_records` is where that run started and the reservation
        # chain restarts at `None` there. Measured on the committed ledger --
        # `previous_reservation_sha256` is null in exactly the 20 records whose
        # boundary equals their own line index, and in no other.
        boundary = record.get(PREFIX_RECORDS_FIELD)
        if type(boundary) is not int or not 0 <= boundary <= index:
            return []
        if record.get(PREFIX_DIGEST_FIELD) != phase0_lib.sha256_bytes(
            b"".join(lines[:boundary])
        ):
            return []
        expected = None if boundary == index else seals[index - 1]
        if record.get(RESERVATION_CHAIN_FIELD) != expected:
            return []
        records.append(record)
        seals[index] = claimed
        preceding += lines[index]
    return records if last_was_sealed else []


def keeps_its_paths(text: str) -> bool:
    """Whether a BLIND substitution into this file is forgery, from the file.

    Not an exemption from the check, and it was one: a sealed record cannot be
    substituted into by `render` alone -- the substitution does not redact it,
    it forges it -- so `--rewrite` refuses, and `tools/phase0/reseal_ledger.py`
    is the writer that may touch it, because it re-seals afterwards. Any other
    file claiming the same refusal has to carry the same four bindings to get
    it.
    """
    return bool(sealed_records(text))


def tree_offences(
    workspace: pathlib.Path | None = None,
    *,
    identifiers: dict[str, tuple[str, str]] | None = None,
) -> dict[str, list[str]]:
    """{tracked path: offences}, over EVERY tracked file.

    The ONE scan. `--check` runs it and the gate test asserts on it, so the rule
    a reviewer reads and the rule the build enforces cannot come apart.

    No file is skipped here, and one was: while a sealed file was excused, this
    returned `{}` over a tree whose largest single concentration of identifiers
    -- 113 of them, in the one file nothing opened -- was inside the file it
    excused. Which writer may fix a file is a question for the writer;
    `keeps_its_paths` answers it in `_main` and in nothing else.
    """
    root = (workspace or WORKSPACE).resolve()
    derived = machine_identifiers(root) if identifiers is None else identifiers
    found: dict[str, list[str]] = {}
    for path in tracked_files(root):
        lines = offences(read_text(path), derived)
        if lines:
            found[path.relative_to(root).as_posix()] = lines
    return found


def _main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Render machine-specific roots in a file as <REPO>/<HOME>/<TMPDIR>. "
            "Emitters call render_document at generation time; this entry point "
            "is for the artefacts that were written before they did, and for "
            "the shell emitters that cannot import Python."
        )
    )
    parser.add_argument("files", nargs="*", type=pathlib.Path)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--check",
        action="store_true",
        help="report every identifier the files still carry; exit 1 if any",
    )
    mode.add_argument(
        "--rewrite",
        action="store_true",
        help="apply the map to the files in place",
    )
    parser.add_argument(
        "--stdin",
        action="store_true",
        help="render standard input to standard output",
    )
    parser.add_argument(
        "--tracked",
        action="store_true",
        help=(
            "take the files from `git ls-files` instead of the command line; "
            "--check reads every one of them and --rewrite leaves the sealed "
            "ones alone"
        ),
    )
    parser.add_argument(
        "--workspace",
        type=pathlib.Path,
        default=WORKSPACE,
        help=(
            "the checkout `--tracked` reads and `<REPO>` names; defaults to the "
            "one this file is in, which is what a run inside a pinned worktree "
            "wants"
        ),
    )
    arguments = parser.parse_args(argv)
    if arguments.stdin:
        sys.stdout.write(render(sys.stdin.read()))
        return 0
    if arguments.tracked:
        if arguments.files:
            parser.error("--tracked derives the file set; do not also name files")
        arguments.files = tracked_files(arguments.workspace)
    if not arguments.files:
        parser.error("no files given")
    identifiers = machine_identifiers(arguments.workspace)
    resolved = sorted(identifiers.values(), key=lambda pair: len(pair[0]), reverse=True)
    status = 0
    substituted = exempt = 0
    for path in arguments.files:
        text = read_text(path)
        found = offences(text, identifiers)
        if not found:
            continue
        if keeps_its_paths(text):
            # Refused by the WRITER, reported by the checker. Substituting here
            # would forge the seal rather than redact the record, so `--rewrite`
            # must not be able to do it even when a file is named on the command
            # line -- but a sealed file that still names this machine still
            # names this machine, and a check that stayed quiet about it was
            # reporting success over the file it did not open.
            if arguments.rewrite:
                print(
                    f"{path}: sealed, left alone ({len(found)} occurrences); "
                    "re-seal it with tools/phase0/reseal_ledger.py"
                )
                exempt += 1
                continue
        elif arguments.rewrite:
            rendered = render(text, against=resolved)
            if rendered != text:
                path.write_bytes(rendered.encode("utf-8", BYTES_AS_TEXT))
                substituted += 1
                print(f"{path}: {len(found)} substituted")
            continue
        for line in found:
            print(f"{path}:{line}")
        status = 1
    if arguments.rewrite:
        print(f"{substituted} files rewritten, {exempt} sealed and left alone")
    return status


if __name__ == "__main__":
    raise SystemExit(_main())
