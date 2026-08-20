#!/usr/bin/env python3
"""Drop `--tested-claude-profile` on one OS/arch. Not “this Claude works.”

A missing `evidence/pooled-transcript-drain-<os>-<arch>.json` means this OS
cannot drop the operator flag. Confirm the binary with `operator_eval.py`
instead. Never invent a drain receipt. Never use another OS's bound.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "promotion"))

from measure_turn_latency import host_identity  # noqa: E402
from promote_claude_version import (  # noqa: E402
    PromotionRefused,
    main as promotion_main,
    pooled_bound,
)


def drain_path(evidence_dir: pathlib.Path, host_os: str, host_arch: str) -> pathlib.Path:
    return evidence_dir / f"pooled-transcript-drain-{host_os}-{host_arch}.json"


def evidence_dir_from(raw: list[str]) -> pathlib.Path | None:
    """Last --evidence-dir wins. A missing or flag-shaped value is None."""

    found = False
    value: str | None = None
    index = 0
    while index < len(raw):
        arg = raw[index]
        if arg.startswith("--evidence-dir="):
            found = True
            value = arg.split("=", 1)[1]
            index += 1
            continue
        if arg == "--evidence-dir":
            found = True
            if index + 1 >= len(raw) or raw[index + 1].startswith("-"):
                value = None
                index += 1
                continue
            value = raw[index + 1]
            index += 2
            continue
        index += 1
    if not found:
        return ROOT / "evidence"
    return pathlib.Path(value) if value else None


def main(arguments: list[str] | None = None) -> int:
    raw = list(sys.argv[1:] if arguments is None else arguments)
    if any(arg in ("-h", "--help") for arg in raw):
        print(__doc__.strip(), end="\n\n")
        try:
            return promotion_main(["--help"])
        except SystemExit as raised:
            return 0 if raised.code in (0, None) else int(raised.code)
    host_os, host_arch = host_identity()
    if "--describe" in raw:
        evidence_dir = evidence_dir_from(raw) or ROOT / "evidence"
        path = drain_path(evidence_dir, host_os, host_arch)
        print(
            "promote drops --tested-claude-profile for one os/arch.\n"
            "operator_eval.py confirms a pin without a drain receipt.\n"
            f"this host would read: {path}"
        )
        return promotion_main(["--describe"])
    evidence_dir = evidence_dir_from(raw)
    if evidence_dir is None:
        print("--evidence-dir needs a path", file=sys.stderr)
        return 2
    path = drain_path(evidence_dir, host_os, host_arch)
    if not path.is_file():
        print(
            f"cannot drop the operator flag on {host_os}/{host_arch}: {path} "
            "does not exist.\n"
            "That does not mean this Claude failed. Run "
            "tools/dev/operator_eval.py to pin it.\n"
            "Do not copy another OS's pooled-drain receipt.",
            file=sys.stderr,
        )
        return 2
    try:
        pooled_bound(evidence_dir, host_os, host_arch)
    except PromotionRefused as error:
        print(f"cannot drop the operator flag: {error}", file=sys.stderr)
        return 2
    try:
        return promotion_main(raw)
    except SystemExit as raised:
        return 0 if raised.code in (0, None) else int(raised.code)


if __name__ == "__main__":
    raise SystemExit(main())
