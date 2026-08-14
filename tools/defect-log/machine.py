"""Every identifier that names THIS machine, asked of the machine.

`docs/defect-log.md` is the pre-squash commit log, preserved because each of
its messages names the defect found rather than the change made. It was
produced by applying one declared substitution map to every message and to
nothing else. This module is the map's first half -- what to look for -- and it
is what the generator that applies it (`tools/defect-log/generate.py`) and the
check that a run of it left nothing behind (`tools/gate-a/tests/
test_redaction.py`) both read.

THE DERIVATION ITSELF NOW LIVES IN `tools/evidence_common/portable_paths.py`,
beside the emitters that write receipts, and this module is the name the defect
log's two halves already knew it by. It moved when the same map acquired a
third and fourth caller: a scrub fixes the artefacts that exist, and the
emitters are what stop the next campaign writing the absolute paths back. Two
derivations of one map is two maps, and the second one is the one that goes
stale -- which is the whole reason there is one module rather than a copy in
each tool.

NOTHING IS WRITTEN DOWN, there or here. A scrubber whose set-of-things-to-scrub
is a literal is this repository's own bug class, and it is the shape a
redaction tool falls into most easily: the list is written on the host that has
nothing left to find, so it passes, and it keeps passing on the next host for
the same reason. Every needle is derived from the running environment.

`pseudomux` is deliberately NOT derivable from that set even though it is a
path component of this checkout, because it is also the crate namespace the
log names on hundreds of lines. The ancestors between home and the checkout are
taken as ONE needle rather than as one needle each, which is the difference.

`macos`, `aarch64` and `macOS-15.7.7` are not looked for. They are not
machine-specific: the compatibility profile is keyed on them and the Linux
handoff is entirely about that boundary. Neither is `smithers`, which is a
shipped product module.
"""

from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "evidence_common"))

from portable_paths import (  # noqa: E402 -- the shared derivation, resolved above
    MINIMUM_NEEDLE_LENGTH,
    PLACEHOLDERS,
    WORKSPACE,
    machine_identifiers,
    offences,
    render,
    render_document,
    substitutions,
)

__all__ = [
    "MINIMUM_NEEDLE_LENGTH",
    "PLACEHOLDERS",
    "WORKSPACE",
    "machine_identifiers",
    "offences",
    "render",
    "render_document",
    "substitutions",
]
