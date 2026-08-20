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

The placeholders PRESERVE STRUCTURE. A recorded binary path must still be named
after the binary and must still share one parent with its siblings:
`<REPO>/target/release/pmux` keeps `Path(...).name` and `Path(...).parent`.
A placeholder that is a digest or an elision would pass a redaction check and
break those readers.

## The scope is the tracked tree, and the set is asked of git

`tracked_files` is `git ls-files`, so a file added tomorrow is in scope tomorrow
without anybody remembering to add it, and `tree_offences` is the one scan both
`--check` and the living redaction test run. The alternative -- a directory
list, or a suffix list, or "the documents I found" -- is the same defect as a
literal needle wearing a different coat: it is written on the host that has
already been searched, and it is complete there and nowhere else.

Every file is read with `surrogateescape`, so a file this module cannot decode
is still scanned rather than skipped. A checker that skips what it cannot read
reports success over the bytes it did not look at.

The historical attempt ledger under `evidence/` is frozen. Do not reseal it.
`--rewrite` applies the same map to every tracked file; a file that already
spells placeholders is a no-op.

## What is not a machine identifier

`macos`, `aarch64` and `macOS-15.7.7` are not looked for: the compatibility
profile is keyed on them and the Linux handoff is entirely about that boundary.
Neither is `smithers`, which is a path component of this checkout's
tooling and not a machine identifier. `/private/tmp` is the
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
# re-running the emitter.
PLACEHOLDERS = ("<TMPDIR>", "<HOME>", "<WORKSPACES>", "<REPO>", "<USER>", "<DIGEST>")

# A needle shorter than this is a word before it is an identifier, and matching
# on one would report a document's prose rather than its provenance.
MINIMUM_NEEDLE_LENGTH = 5

# Arbitrary bytes survive this round trip unchanged, so a file that is not UTF-8
# is scanned rather than skipped, and rewriting one cannot corrupt it.
BYTES_AS_TEXT = "surrogateescape"


def _platform_default_temporary_directory() -> str:
    """The temporary directory this platform falls back to with no environment.

    Ask once with `TMPDIR`, `TMP` and `TEMP` removed and the cache reset.
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
    one directory. A map carrying only the resolved spelling silently misses
    every emitter that did not resolve, which is most of them.

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


def machine_identifiers(
    workspace: pathlib.Path | None = None,
) -> dict[str, tuple[str, str]]:
    """{description: (needle, placeholder)}, derived from this machine.

    Keyed by description so a failure says which identifier leaked rather than
    only that something did. `workspace` defaults to the checkout this file is
    in, which is what an emitter wants: a tool running inside a pinned worktree
    should render that worktree as `<REPO>`, because that is the repository the
    run it is describing actually had.
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
        description: (needle, placeholder)
        for description, (needle, placeholder) in found.items()
        if len(needle) >= MINIMUM_NEEDLE_LENGTH
    }


def substitutions(
    workspace: pathlib.Path | None = None,
) -> list[tuple[str, str]]:
    """The map as (needle, placeholder), longest needle first.

    Order is load-bearing and is derived rather than declared: the checkout
    path contains the home directory, which contains the login name, so a
    shorter needle applied first would leave the longer one half-substituted
    and unrecognisable to both this map and the check that it was applied.
    """
    pairs = list(machine_identifiers(workspace).values())
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


def tree_offences(
    workspace: pathlib.Path | None = None,
    *,
    identifiers: dict[str, tuple[str, str]] | None = None,
) -> dict[str, list[str]]:
    """{tracked path: offences}, over EVERY tracked file.

    The ONE scan. `--check` runs it and the living redaction test asserts on
    it, so the rule a reviewer reads and the rule the build enforces cannot
    come apart. No file is skipped.
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
            "--check reads every one of them"
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
    substituted = 0
    for path in arguments.files:
        text = read_text(path)
        found = offences(text, identifiers)
        if not found:
            continue
        if arguments.rewrite:
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
        print(f"{substituted} files rewritten")
    return status


if __name__ == "__main__":
    raise SystemExit(_main())
