"""Redact the attempt ledger and RE-SEAL it with the machinery that owns the seal.

`tools/evidence_common/portable_paths.py --rewrite` refuses this file, and is
right to: substituting a placeholder into a hash-chained record does not redact
the record, it forges it. Every digest that covered the substituted bytes stops
verifying, and the file goes on claiming to be sealed. This program is the other
half of that refusal -- the writer that MAY touch a sealed file, because after it
substitutes it recomputes every binding, in the order the appender computes them,
with the appender's own functions.

## Nothing here re-implements a digest

`canonical_json_bytes`, `sha256_bytes`, `campaign_contract_sha256` and
`_revision_identity_sha256` are imported from `phase0_lib`. A second spelling of
a canonical encoding is a second encoding, and the copy is the one that drifts;
a re-seal computed by a copy would produce a file that verifies against the copy
and refuses the next live campaign, which is the exact failure this program
exists to avoid.

## The placeholder has to stay absolute, and that is not cosmetic

`_validate_reservation_record` reads `artifact_directory` back through
`Path(...).is_absolute()` (`phase0_lib.py:2567-2572`), `_validate_public_file_
identity` reads every recorded binary path back the same way, and
`source_digest._canonical_absolute_path_text` reads every Git control path back
through `is_absolute()` AND through `str(Path(value)) == value`. The project
placeholder is `<HOME>`, and `<HOME>/x` is a relative path. MEASURED, by
re-sealing the committed ledger against the DEFAULT map and reading the result
back: every one of its reservations comes out invalid -- 51 with a relative
`artifact_directory`, 2210 recorded `path` fields that stopped being absolute,
and the 52nd caught by its Claude binary alone. The map is asked for its
root-preserving form instead (`machine_identifiers(absolute_placeholders=True)`),
which spells an absolute needle's placeholder with that needle's own root, so
`/Users/somebody/campaign` becomes `/<HOME>/campaign` -- absolute, one component
per original component, `.name` and `.parent` unchanged.

## Which digests are recomputed is DERIVED, not listed

A list of "the fields that carry a digest of a sibling" is a list written against
the schema as it stands today, which is this repository's own bug class. The rule
here is a property instead: a key `k_sha256` beside a key `k` is recomputed if
and only if some known digest function REPRODUCES its pre-substitution value.
A digest the program cannot first reproduce is a digest it does not understand,
and it is left byte-identical -- which is why `path_sha256`, `canonical_path_
sha256`, `algorithm_sha256`, `stdout_sha256` and the rest are untouched, without
this file naming any of them.

Bottom-up, so an inner digest is recomputed before the outer digest that covers
it: `revision_identity_sha256` before `campaign_contract_sha256`, without either
being sequenced by hand.

## The chain fields, and the ONE rule for each

* `ledger_prefix_records` is the record's own and never changes: it is where the
  campaign run that wrote the record found the file, not a property of the file.
* `ledger_prefix_sha256` becomes the digest of the first `ledger_prefix_records`
  REWRITTEN lines.
* `previous_ledger_sha256` becomes the digest of every rewritten byte in front of
  the record.
* `previous_reservation_sha256` is `None` when `ledger_prefix_records` equals the
  record's own line index -- the record is the first its run appended -- and the
  previous line's new seal otherwise. This is `_append_reservation_locked`'s rule
  read back off the file: it is null in exactly the 20 committed records whose
  boundary is their own index, and in no other.
* `reservation_sha256` is computed LAST, over the canonical body of everything
  above, exactly as `phase0_lib.py:4126` computes it.

## What is lost, and why the file says so itself

A re-sealed chain is a valid chain over different bytes. The digests recorded at
reservation time are not recoverable from the published file, and the receipts
that pinned the old whole-file digest cannot be checked against it any more.
`--note` is refused by the schema (see `--explain-note`), so that statement lives
in `evidence/README.md`, and this program prints the before/after digests so the
statement can be written from a run rather than from memory.

Idempotent: a second run substitutes nothing and recomputes the same digests over
the same bytes, so it rewrites the file to itself.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import stat
import sys
from typing import Any, Callable, Sequence

WORKSPACE = pathlib.Path(__file__).resolve().parents[2]

sys.path.insert(0, str(WORKSPACE / "tools" / "phase0"))
sys.path.insert(0, str(WORKSPACE / "tools" / "evidence_common"))

import phase0_lib  # noqa: E402 -- the machinery that owns the seal, resolved above
import portable_paths  # noqa: E402 -- the one map, resolved above

# The digest functions this program is allowed to recognise, each one imported.
# A candidate `k_sha256` is only rewritten when one of these reproduces the value
# it already holds, so an unknown digest is left alone rather than guessed at.
DIGEST_FUNCTIONS: tuple[Callable[[Any], str], ...] = (
    lambda value: phase0_lib.sha256_bytes(phase0_lib.canonical_json_bytes(value)),
    phase0_lib._revision_identity_sha256,
)

SEAL = portable_paths.SEAL_FIELD
CHAIN = portable_paths.CHAIN_FIELD
RESERVATION_CHAIN = portable_paths.RESERVATION_CHAIN_FIELD
PREFIX_RECORDS = portable_paths.PREFIX_RECORDS_FIELD
PREFIX_DIGEST = portable_paths.PREFIX_DIGEST_FIELD

# Why `--note` cannot be honoured, kept beside the flag that refuses it so the
# refusal cites the predicates rather than asserting them.
NOTE_REFUSAL = """\
The ledger schema cannot carry a redaction note. Each line below was RUN against
`summarize_attempt_ledger` -- the derivation `phase0.py budget` publishes -- with
a two-field note record inserted into a re-sealed copy of the committed ledger:

  note first, no ordinal              REFUSED: an attempt ledger record spells
  note inside the prefix, no ordinal  its ordinal in none of
  note last, no ordinal               global_attempt_ordinal, ... ; the budget
                                      cannot be counted from it
  note with a duplicate ordinal 81    REFUSED: recognized immutable prefix
                                      attempts must be strictly increasing
  note with a fresh ordinal 82        ACCEPTED, and consumed goes 85 -> 86:
                                      the note spends an irreplaceable attempt
  note with ordinal 4 in front        ACCEPTED, and predating_the_file goes
                                      4 -> 3: the note forges an attempt the
                                      file's own first record attests predates
                                      it

The prefix region is not a loophole -- `summarize_attempt_ledger` reads every
line in the file, prefix included -- and `_validate_reservation_record` requires
`set(record) == required`, so a note cannot ride inside a reservation either.
Every form the schema accepts changes a published budget figure.

That is a finding, not a licence to relax `_validate`. The statement belongs in
`evidence/README.md`, which is where this program's operator is told to put it.
"""


def _recompute_sibling_digests(value: Any, *, original: Any) -> Any:
    """Rewrite every `k_sha256` whose pre-substitution value can be reproduced.

    `original` is the same node before substitution and is what decides whether
    a field is understood: a digest this program can recompute to the value the
    committed record already carries is a digest of that sibling, and one it
    cannot is a digest of something else and is not touched.
    """
    if isinstance(value, list):
        return [
            _recompute_sibling_digests(item, original=source)
            for item, source in zip(value, original, strict=True)
        ]
    if not isinstance(value, dict):
        return value
    rebuilt = {
        key: _recompute_sibling_digests(item, original=original[key])
        for key, item in value.items()
    }
    for key in value:
        sibling = key.removesuffix("_sha256")
        if sibling == key or sibling not in value:
            continue
        for digest in DIGEST_FUNCTIONS:
            try:
                reproduced = digest(original[sibling]) == original[key]
            except (TypeError, ValueError):
                continue
            if reproduced:
                rebuilt[key] = digest(rebuilt[sibling])
                break
    return rebuilt


def reseal(payload: bytes, *, against: Sequence[tuple[str, str]]) -> bytes:
    """The whole file, substituted and re-sealed, as bytes ready to write."""

    lines = phase0_lib._ledger_lines(payload)
    rendered: list[bytes] = []
    for index, line in enumerate(lines):
        original = phase0_lib.strict_json_loads(
            line, label=f"attempt ledger record {index + 1}"
        )
        record = _recompute_sibling_digests(
            portable_paths.render_document(original, against=against),
            original=original,
        )
        if SEAL in record:
            boundary = record[PREFIX_RECORDS]
            record[PREFIX_DIGEST] = phase0_lib.sha256_bytes(
                b"".join(rendered[:boundary])
            )
            record[CHAIN] = phase0_lib.sha256_bytes(b"".join(rendered))
            record[RESERVATION_CHAIN] = (
                None if boundary == index else json.loads(rendered[index - 1])[SEAL]
            )
            body = {key: item for key, item in record.items() if key != SEAL}
            record[SEAL] = phase0_lib.sha256_bytes(
                phase0_lib.canonical_json_bytes(body)
            )
        rendered.append(phase0_lib.canonical_json_bytes(record) + b"\n")
    return b"".join(rendered)


def _main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Substitute this machine's identifiers out of a hash-chained attempt "
            "ledger and recompute every seal with phase0_lib's own functions."
        )
    )
    parser.add_argument("ledger", nargs="?", type=pathlib.Path)
    parser.add_argument(
        "--write",
        action="store_true",
        help="write the result back in place; without it nothing is written",
    )
    parser.add_argument(
        "--explain-note",
        action="store_true",
        help="print why a redaction note cannot be a record of this file, and exit",
    )
    parser.add_argument(
        "--workspace",
        type=pathlib.Path,
        default=WORKSPACE,
        help="the checkout the map's needles are asked against",
    )
    arguments = parser.parse_args(argv)
    if arguments.explain_note:
        sys.stdout.write(NOTE_REFUSAL)
        return 0
    if arguments.ledger is None:
        parser.error("no ledger given")

    payload = arguments.ledger.read_bytes()
    identifiers = portable_paths.machine_identifiers(
        arguments.workspace, absolute_placeholders=True
    )
    against = sorted(identifiers.values(), key=lambda pair: len(pair[0]), reverse=True)
    before = portable_paths.offences(
        payload.decode("utf-8", portable_paths.BYTES_AS_TEXT), identifiers
    )
    resealed = reseal(payload, against=against)
    after = portable_paths.offences(
        resealed.decode("utf-8", portable_paths.BYTES_AS_TEXT), identifiers
    )
    sealed_before = portable_paths.sealed_records(
        payload.decode("utf-8", portable_paths.BYTES_AS_TEXT)
    )
    sealed_after = portable_paths.sealed_records(
        resealed.decode("utf-8", portable_paths.BYTES_AS_TEXT)
    )
    print(f"identifier occurrences  : {len(before)} -> {len(after)}")
    print(f"records verifying       : {len(sealed_before)} -> {len(sealed_after)}")
    print(f"sha256                  : {phase0_lib.sha256_bytes(payload)}")
    print(f"                     -> : {phase0_lib.sha256_bytes(resealed)}")
    print(f"bytes                   : {len(payload)} -> {len(resealed)}")
    if after:
        print("REFUSED: identifiers survived the substitution", file=sys.stderr)
        return 1
    if len(sealed_after) != len(sealed_before) or not sealed_after:
        print("REFUSED: the re-sealed chain does not verify", file=sys.stderr)
        return 1
    if not arguments.write:
        print("(nothing written; pass --write)")
        return 0
    if resealed == payload:
        print("already re-sealed; nothing written")
        return 0
    mode = stat.S_IMODE(arguments.ledger.stat().st_mode)
    arguments.ledger.write_bytes(resealed)
    arguments.ledger.chmod(mode)
    print(f"written, mode {mode:04o} preserved")
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
