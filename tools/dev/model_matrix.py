#!/usr/bin/env python3
"""Probe every `(model, effort)` cell `MODEL_TABLE` admits against real Claude.

`MODEL_TABLE` (crates/service/src/pool/class.rs) says of itself: "CHOSEN, not
MEASURED ... they must be probed before this table is pinned to a Claude
version -- one `--model <M> --effort <E>` probe per cell, recorded with the
version." This tool is that probe. It spends one real turn per cell, plus one
per model through that model's first alias, and writes a receipt.

It is NOT a promotion. It does not read or write a pooled-drain receipt, it
does not edit PROMOTED_PROFILES, and it does not edit MODEL_TABLE. A cell that
answers proves the row is launchable on this Claude on this OS; it proves
nothing about latency, stickiness or the flagless lane.

The rows are DERIVED from class.rs, never restated here: a model added to the
table is probed by the next run without anybody editing this file.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import random
import re
import subprocess
import sys
import time
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "promotion"))
sys.path.insert(0, str(ROOT / "tools" / "evidence_common"))
# Explicit, because this module is also imported by path from the tests,
# where its own directory is not on `sys.path`.
sys.path.insert(0, str(ROOT / "tools" / "dev"))

from measure_turn_latency import (  # noqa: E402
    MeasurementError,
    Sandbox,
    claude_version,
    host_identity,
    resolve_binaries,
    run_client,
)
from promote_claude_version import GRADES, _nonce  # noqa: E402
from operator_eval import EvalDaemon, doctor, layer  # noqa: E402
import portable_paths  # noqa: E402

CLASS_RS = ROOT / "crates" / "service" / "src" / "pool" / "class.rs"
SCHEMA = "pmux.model-matrix.v1"
GREEN = "GREEN_MATRIX"
RED = "RED"
MATRIX_DRAIN_MS = 250
TURN_DEADLINE_MS = 180_000
# The one grade whose answer is exact-checkable without a judge: the nonce is
# unique to the turn and the sum is not in the prompt.
GRADE = GRADES[0]


class MatrixRefused(Exception):
    """The matrix cannot honestly be derived or run. Exit 2."""


def file_sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_model_table(source: str) -> list[dict[str, Any]]:
    """`MODEL_TABLE`, read out of the Rust that defines it.

    Three reads, in the order the Rust composes them: the argv token each
    `AdmittedEffort` constant renders, the members of each `EFFORTS_*` set, and
    the entries of the table. Restating any of the three here would be the copy
    the table's own doc comment says must not exist.
    """

    # `const LOW: AdmittedEffort = AdmittedEffort { level: .., argv: "low" };`
    argv_of = dict(
        re.findall(
            r"const (\w+): AdmittedEffort = AdmittedEffort \{[^}]*argv: \"([a-z]+)\"",
            source,
            re.DOTALL,
        )
    )
    if not argv_of:
        raise MatrixRefused(
            f"{CLASS_RS} named no AdmittedEffort constant, so this tool cannot "
            "derive which `--effort` spellings a cell renders"
        )
    # `const EFFORTS_ALL: &[AdmittedEffort] = &[LOW, MEDIUM, ..];`
    tiers_of: dict[str, list[str]] = {}
    for name, members in re.findall(
        r"const (EFFORTS_\w+): &\[AdmittedEffort\] = &\[([^\]]*)\];", source
    ):
        tiers = []
        for member in members.replace(" ", "").split(","):
            if not member:
                continue
            if member not in argv_of:
                raise MatrixRefused(
                    f"{name} names {member}, which is not an AdmittedEffort "
                    f"constant in {CLASS_RS}"
                )
            tiers.append(argv_of[member])
        tiers_of[name] = tiers
    if not tiers_of:
        raise MatrixRefused(f"{CLASS_RS} named no EFFORTS_* constant")

    start = source.find("\npub static MODEL_TABLE: &[ModelEntry] = &[\n")
    if start < 0:
        table = ""
    else:
        end = source.find("\n];", start)
        table = source[start:] if end < 0 else source[start : end + len("\n];")]
    entries = re.findall(
        r"canonical: \"([^\"]+)\",\s*aliases: &\[([^\]]*)\],\s*efforts: (\w+),",
        table,
    )
    if not entries:
        raise MatrixRefused(
            f"{CLASS_RS} yielded no ModelEntry; refusing to probe an empty matrix"
        )
    # A doc comment or a reordered field inside one entry would make the regex
    # skip that entry silently, and a matrix that quietly drops a row is worse
    # than one that refuses to run.
    written = table.count("ModelEntry {")
    if written != len(entries):
        raise MatrixRefused(
            f"MODEL_TABLE in {CLASS_RS} writes {written} ModelEntry but this tool "
            f"could parse {len(entries)}; refusing to probe a matrix that silently "
            "dropped a row"
        )
    parsed: list[dict[str, Any]] = []
    for canonical, aliases, efforts in entries:
        if efforts not in tiers_of:
            raise MatrixRefused(
                f"model {canonical} names effort set {efforts}, which is not an "
                f"EFFORTS_* constant in {CLASS_RS}"
            )
        parsed.append(
            {
                "canonical": canonical,
                "aliases": [
                    alias.strip().strip('"')
                    for alias in aliases.split(",")
                    if alias.strip()
                ],
                "efforts": tiers_of[efforts],
            }
        )
    return parsed


def derive_rows(
    table: list[dict[str, Any]],
    only: list[str] | None = None,
    skip_efforts: list[str] | None = None,
) -> list[dict[str, Any]]:
    """One row per admitted cell, plus one alias row per model.

    The alias row is the cheapest end-to-end proof that two spellings of one
    model reach one class: it is a real turn through `--model fable`, not a
    unit test of the resolver.
    """

    wanted = {name.lower() for name in only or []}
    skipped = {name.lower() for name in skip_efforts or []}
    if wanted:
        canonical = {entry["canonical"].lower() for entry in table}
        unknown = sorted(wanted - canonical)
        if unknown:
            raise MatrixRefused(
                f"--only named {', '.join(unknown)}, which MODEL_TABLE does not "
                f"carry as a canonical model; it admits {', '.join(sorted(canonical))}"
            )
    if skipped:
        admitted = {tier.lower() for entry in table for tier in entry["efforts"]}
        unknown = sorted(skipped - admitted)
        if unknown:
            raise MatrixRefused(
                f"--skip-effort named {', '.join(unknown)}, which MODEL_TABLE does "
                f"not admit as an effort tier; it admits "
                f"{', '.join(sorted(admitted))}"
            )
    rows: list[dict[str, Any]] = []
    for entry in table:
        canonical = entry["canonical"]
        if wanted and canonical.lower() not in wanted:
            continue
        efforts = [tier for tier in entry["efforts"] if tier.lower() not in skipped]
        if entry["efforts"] and not efforts:
            # Every tier this model admits was skipped. Probing it bare would
            # test a cell the table does not have.
            continue
        for effort in efforts:
            rows.append(
                {
                    "model": canonical,
                    "spelling": canonical,
                    "alias_used": None,
                    "via_alias": False,
                    "effort": effort,
                }
            )
        if not efforts:
            rows.append(
                {
                    "model": canonical,
                    "spelling": canonical,
                    "alias_used": None,
                    "via_alias": False,
                    "effort": None,
                }
            )
        if entry["aliases"]:
            rows.append(
                {
                    "model": canonical,
                    "spelling": entry["aliases"][0],
                    "alias_used": entry["aliases"][0],
                    "via_alias": True,
                    "effort": efforts[0] if efforts else None,
                }
            )
    return rows


def reported_model_matches(reported: Any, model: str) -> bool:
    """Does `reported_model` name the model pmux launched?

    `reported_model` is either the canonical id verbatim (the 2.1.257 receipts
    show exact equality for every undated id) or that id plus a build date
    (`claude-opus-4-5-20251101`). A bare prefix test is too loose: it would let
    `claude-opus-5` accept a turn served by `claude-opus-5-1`.
    """

    if not reported:
        return False
    reported = str(reported)
    return reported == model or reported.startswith(f"{model}-20")


def label(row: dict[str, Any]) -> str:
    return f"{row['spelling']}/{row['effort'] or '-'}"


def describe(rows: list[dict[str, Any]]) -> str:
    lines = [
        "model_matrix probes every (model, effort) cell MODEL_TABLE admits.",
        f"schema: {SCHEMA}",
        f"green verdict: {GREEN}",
        "not a promotion: no pooled-drain receipt is read or written, and it\n"
        "does not edit PROMOTED_PROFILES, does not edit MODEL_TABLE.",
        f"rows derived from {CLASS_RS.relative_to(ROOT)}: {len(rows)} "
        f"(one real turn each)",
    ]
    for row in rows:
        argv = f"pmux run --model {row['spelling']}"
        if row["effort"]:
            argv += f" --effort {row['effort']}"
        suffix = f"  # alias of {row['model']}" if row["via_alias"] else ""
        lines.append(f"  - {argv}{suffix}")
    return "".join(f"{line}\n" for line in lines)


def probe_row(
    binaries: dict[str, pathlib.Path],
    sandbox: Sandbox,
    row: dict[str, Any],
    nonce: str,
    turn_deadline_ms: int,
) -> dict[str, Any]:
    """One cell, one turn. A cell that fails is recorded, not raised: the point
    of a matrix is the cells you did not expect to fail, and aborting on the
    first one hides every cell after it."""

    prompt = GRADE.render(nonce)
    expected = GRADE.expected(nonce)
    deadline_ms = int(time.time() * 1000) + turn_deadline_ms
    arguments = ["run", "--model", row["spelling"]]
    if row["effort"]:
        arguments += ["--effort", row["effort"]]
    arguments += ["--deadline-unix-ms", str(deadline_ms), prompt]
    record: dict[str, Any] = {
        "model": row["model"],
        "spelling": row["spelling"],
        "alias_used": row["alias_used"],
        "via_alias": row["via_alias"],
        "effort": row["effort"],
        "nonce": nonce,
        "expected": expected,
    }
    try:
        result, wall_ms = run_client(
            binaries,
            sandbox,
            arguments,
            timeout=turn_deadline_ms / 1000.0 + 60.0,
        )
    except (MeasurementError, subprocess.SubprocessError) as error:
        record.update(
            {
                "text": "",
                "answered": False,
                "reported_model": None,
                "reported_model_matches": False,
                "claude_version": None,
                "stop_reason": None,
                "usage": {},
                "client_wall_ms": None,
                "error": str(error)[:600],
            }
        )
        return record
    text = (result.get("text") or "").strip()
    reported = result.get("reported_model")
    record.update(
        {
            "text": text,
            "answered": text == expected,
            "reported_model": reported,
            "reported_model_matches": reported_model_matches(reported, row["model"]),
            "claude_version": result.get("claude_version"),
            "stop_reason": result.get("stop_reason"),
            "usage": result.get("usage") or {},
            "client_wall_ms": round(wall_ms, 1),
            "error": None,
        }
    )
    return record


def verdict(rows: list[dict[str, Any]], pool_after: dict[str, Any]) -> str:
    if not rows:
        return RED
    if pool_after.get("halted") or pool_after.get("leaked"):
        return RED
    for row in rows:
        if not row.get("answered") or not row.get("reported_model_matches"):
            return RED
    return GREEN


def summarize(rows: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "rows": len(rows),
        "answered": sum(1 for row in rows if row.get("answered")),
        "failed": sum(1 for row in rows if not row.get("answered")),
        "reported_model_mismatches": [
            label(row) for row in rows if not row.get("reported_model_matches")
        ],
    }


def execute(args: argparse.Namespace, rows: list[dict[str, Any]]) -> dict[str, Any]:
    binaries = resolve_binaries(args.release_dir)
    claude = args.claude.resolve(strict=True)
    version = claude_version(claude)
    host_os, host_arch = host_identity()
    receipt: dict[str, Any] = {
        "schema": SCHEMA,
        "kind": "model_effort_matrix",
        "not_a_promotion": True,
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "pin": {
            "path": str(claude),
            "sha256": file_sha256(claude),
            "bytes": claude.stat().st_size,
            "version": version,
        },
    }
    profile = {
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "transcript_drain_ms": args.drain_ms,
    }
    sandbox = Sandbox("operator")
    daemon = EvalDaemon(
        binaries,
        sandbox,
        profile,
        claude,
        None,
        rows[0]["model"],
        rows[0]["effort"],
    )
    probed: list[dict[str, Any]] = []
    try:
        # Warm the first row's class before the first turn, so a matrix that
        # goes red says the cell failed rather than that the pool never came up.
        census: dict[str, Any] = {}
        deadline = time.monotonic() + args.warm_timeout_ms / 1000.0
        while time.monotonic() < deadline:
            census = layer(doctor(binaries, sandbox, claude), "pool").get("evidence") or {}
            if census.get("halted"):
                raise MeasurementError(f"pool halted during warm: {census['halted']}")
            if census.get("idle", 0) >= 1:
                break
            time.sleep(0.2)
        else:
            raise MeasurementError(f"warm instance never reached idle: {census}")
        receipt["warm"] = census

        rng = random.Random(args.seed)
        # No daemon restart between rows: `pmux run` on a class the pool has
        # never served cold-mints one, which is the behaviour a caller gets.
        for row in rows:
            probed.append(
                probe_row(binaries, sandbox, row, _nonce(rng), args.turn_deadline_ms)
            )
        pool_after = layer(doctor(binaries, sandbox, claude), "pool").get("evidence") or {}
    finally:
        daemon.stop()
        if not args.keep_sandbox:
            sandbox.remove()

    receipt["rows"] = probed
    receipt["summary"] = summarize(probed)
    receipt["pool_after"] = {
        "halted": pool_after.get("halted"),
        "leaked": pool_after.get("leaked"),
    }
    receipt["verdict"] = verdict(probed, pool_after)
    return receipt


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir")
    parser.add_argument("--claude")
    parser.add_argument(
        "--describe",
        action="store_true",
        help="print the derived row list and exit; spends no turns",
    )
    parser.add_argument(
        "--only",
        dest="only",
        action="append",
        help="repeatable; restrict the matrix to these canonical models",
    )
    parser.add_argument(
        "--skip-effort",
        dest="skip_efforts",
        action="append",
        help="repeatable; drop these effort tiers from every model",
    )
    parser.add_argument("--drain-ms", type=int, default=MATRIX_DRAIN_MS)
    parser.add_argument("--turn-deadline-ms", type=int, default=TURN_DEADLINE_MS)
    parser.add_argument("--warm-timeout-ms", type=int, default=180_000)
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--keep-sandbox", action="store_true")
    args = parser.parse_args(arguments)
    try:
        table = parse_model_table(CLASS_RS.read_text(encoding="utf-8"))
        rows = derive_rows(table, args.only, args.skip_efforts)
        if not rows:
            raise MatrixRefused("the filters left no cell to probe")
    except (MatrixRefused, OSError) as error:
        print(f"model-matrix refused: {error}", file=sys.stderr)
        return 2
    if args.describe:
        print(describe(rows), end="")
        return 0
    if not args.release_dir or not args.claude:
        parser.error("--release-dir and --claude are required unless --describe")
    args.release_dir = pathlib.Path(args.release_dir)
    args.claude = pathlib.Path(args.claude)
    if args.seed is None:
        args.seed = int(time.time())
    try:
        receipt = execute(args, rows)
    except (FileNotFoundError, MeasurementError, MatrixRefused) as error:
        print(f"model-matrix failed: {error}", file=sys.stderr)
        return 2
    except Exception as error:  # noqa: BLE001 - a crash is not a RED matrix
        print(f"model-matrix crashed: {error!r}", file=sys.stderr)
        return 3
    text = json.dumps(portable_paths.render_document(receipt), indent=2) + "\n"
    if args.output:
        args.output.write_text(text)
    print(text, end="")
    return 0 if receipt.get("verdict") == GREEN else 1


if __name__ == "__main__":
    raise SystemExit(main())
