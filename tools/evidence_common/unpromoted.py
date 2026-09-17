"""Refuse anything a `--allow-unpromoted-claude` daemon produced.

`pmuxd --allow-unpromoted-claude` admits a Claude compatibility tuple nothing
measured. That is a legitimate exploratory mode, and it is the exact opposite of
a promotion input: a receipt built from it would state a drain, a version range
and a platform that no campaign ever established.

So the daemon labels everything it emits and this module is the one place the
promotion tools read the label. Two artifacts carry it:

* the retained drain corpus, through a `pmux-unpromoted.json` sibling file that
  `crate::pool::evidence::mark_unpromoted` writes into the evidence directory at
  daemon start (the mirrors themselves are pruned to the fields
  `measure_transcript_drain.py` reads, so the label cannot live in a row); and
* `pmux doctor`, whose `configuration` layer publishes
  `unpromoted_claude_opt_in` and whose `compatibility_profile` layer publishes
  `pool_claude_unpromoted`.

Every function here is a REFUSAL and never a warning. A tool that measured an
unpromoted daemon and said so in a footnote would still have written the
receipt.
"""

from __future__ import annotations

import json
import pathlib
from typing import Any, Iterable

#: The sibling file `crate::pool::evidence::UNPROMOTED_MARKER` writes. Spelled
#: here and there; `evidence::tests::the_unpromoted_marker_is_the_name_the_promotion_tools_refuse`
#: reads THIS file and fails if the two names drift apart.
UNPROMOTED_MARKER = "pmux-unpromoted.json"

#: The `pmux doctor` configuration-layer key that says the same thing.
DOCTOR_OPT_IN_KEY = "unpromoted_claude_opt_in"


class UnpromotedArtifact(Exception):
    """A promotion input came off a daemon nobody measured."""


def marker_in(roots: Iterable[pathlib.Path]) -> pathlib.Path | None:
    """The first `pmux-unpromoted.json` under any of `roots`, or `None`.

    Recursive, because a corpus root is normally a parent of several evidence
    directories and only one of them need be unpromoted for the pooled
    measurement to be contaminated.
    """

    for root in roots:
        for found in sorted(pathlib.Path(root).rglob(UNPROMOTED_MARKER)):
            if found.is_file():
                return found
    return None


def refuse_unpromoted_corpus(roots: Iterable[pathlib.Path]) -> None:
    """# Raises

    [`UnpromotedArtifact`] when any corpus root holds the marker.
    """

    found = marker_in(roots)
    if found is None:
        return
    detail = ""
    try:
        detail = json.loads(found.read_text(encoding="utf-8")).get("why", "")
    except (OSError, ValueError, AttributeError):
        detail = ""
    raise UnpromotedArtifact(
        f"{found} marks this corpus as produced by a pmuxd started with "
        f"--allow-unpromoted-claude, so it cannot back a promotion. {detail} "
        f"Measure this OS/arch on a daemon without that flag."
    )


def doctor_opt_in(report: dict[str, Any]) -> bool:
    """Whether a `pmux doctor` report came from an unpromoted daemon.

    Reads the configuration layer's own key rather than any prose, and treats a
    missing layer as `False`: a daemon older than the flag cannot have it on.
    """

    for layer in (report.get("diagnosis") or {}).get("layers") or []:
        evidence = layer.get("evidence") or {}
        if evidence.get(DOCTOR_OPT_IN_KEY) is True:
            return True
        if evidence.get("pool_claude_unpromoted") is True:
            return True
    return False


def refuse_unpromoted_daemon(report: dict[str, Any]) -> None:
    """# Raises

    [`UnpromotedArtifact`] when `report` is an unpromoted daemon's `doctor`.
    """

    if not doctor_opt_in(report):
        return
    raise UnpromotedArtifact(
        "`pmux doctor` reports this daemon was started with "
        "--allow-unpromoted-claude, so its Claude cell is unmeasured and "
        "nothing it produces may back a promotion or an operator pin. Restart "
        "pmuxd without that flag."
    )
