#!/usr/bin/env python3
"""Human-readable breakdown of pmux's own per-turn overhead.

Reads one or more `pmux-performance-diagnostics-v2` records emitted by
`crates/service/tests/performance_diagnostics.rs` and prints where a turn's
wall time goes.

WHAT THIS CLAIMS
    Every number here was measured against zero model latency: the emitting
    test drives the production `SessionRegistry` with doubles that answer
    instantly, and the launch/close phases are measured around a real
    `pmux-rmuxd` sidecar and a real PTY pane. So the totals are pmux's own
    structural overhead on the host that produced the record.

WHAT THIS DOES NOT CLAIM
    Nothing about real-Claude latency, and nothing about any other host. A
    real turn additionally contains model time, which pmux neither controls
    nor measures here. The record is host-sensitive and diagnostic-only: the
    emitting test contains no threshold, and neither does this report. The
    `admission` phase excludes the real two-gate editor fence in
    `driver_io.rs` (a double stands in for the terminal), so its production
    floor is constants-derived, not measured. Each record carries its own
    `gaps` list; it is printed verbatim below.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

RECORD_PREFIX = "pmux_performance_diagnostics "
SUPPORTED_SCHEMAS = frozenset({"pmux-performance-diagnostics-v2"})
STAT_KEYS = ("count", "min_ms", "mean_ms", "p50_ms", "p95_ms", "max_ms")
TURN_PHASES = ("admission", "execution", "completion")
SESSION_PHASES = ("launch", "close")


class ReportError(Exception):
    """A record could not be read as a performance-diagnostics record."""


def _records_from_text(text: str, origin: str) -> list[dict[str, Any]]:
    """Accept a bare JSON record, a JSON array, or captured test stderr."""
    stripped = text.strip()
    if not stripped:
        raise ReportError(f"{origin}: empty input")
    if stripped.startswith(("{", "[")):
        try:
            loaded = json.loads(stripped)
        except json.JSONDecodeError as error:
            raise ReportError(f"{origin}: {error}") from error
        return list(loaded) if isinstance(loaded, list) else [loaded]

    records: list[dict[str, Any]] = []
    for number, line in enumerate(stripped.splitlines(), start=1):
        index = line.find(RECORD_PREFIX)
        if index < 0:
            continue
        payload = line[index + len(RECORD_PREFIX) :].strip()
        try:
            records.append(json.loads(payload))
        except json.JSONDecodeError as error:
            raise ReportError(f"{origin}:{number}: {error}") from error
    if not records:
        raise ReportError(f"{origin}: no {RECORD_PREFIX.strip()} record found")
    return records


def load_records(paths: list[str]) -> list[tuple[str, dict[str, Any]]]:
    loaded: list[tuple[str, dict[str, Any]]] = []
    sources = paths or ["-"]
    for source in sources:
        if source == "-":
            text, origin = sys.stdin.read(), "<stdin>"
        else:
            path = Path(source)
            text, origin = path.read_text(encoding="utf-8"), str(path)
        for record in _records_from_text(text, origin):
            loaded.append((origin, record))
    return loaded


def _require(record: dict[str, Any], key: str, origin: str) -> Any:
    if key not in record:
        raise ReportError(f"{origin}: record is missing {key!r}")
    return record[key]


def _stats(entry: dict[str, Any], origin: str, label: str) -> dict[str, float]:
    stats = entry.get("stats", entry)
    missing = [key for key in STAT_KEYS if key not in stats]
    if missing:
        raise ReportError(f"{origin}: {label} is missing {', '.join(missing)}")
    return {key: float(stats[key]) for key in STAT_KEYS}


def _fmt_ms(value: float) -> str:
    return f"{value:,.1f}"


def _share(part: float, whole: float) -> str:
    if whole <= 0:
        return "n/a"
    return f"{100.0 * part / whole:5.1f}%"


def _table(header: list[str], rows: list[list[str]]) -> list[str]:
    widths = [len(cell) for cell in header]
    for row in rows:
        for index, cell in enumerate(row):
            widths[index] = max(widths[index], len(cell))

    def render(cells: list[str]) -> str:
        parts = [cells[0].ljust(widths[0])]
        parts += [cell.rjust(widths[index + 1]) for index, cell in enumerate(cells[1:])]
        return "  ".join(parts).rstrip()

    lines = [render(header), "  ".join("-" * width for width in widths)]
    lines += [render(row) for row in rows]
    return lines


def _wrap(text: str, width: int, indent: str) -> list[str]:
    words = text.split()
    lines: list[str] = []
    current = indent
    for word in words:
        candidate = f"{current}{word} "
        if len(candidate.rstrip()) > width and current.strip():
            lines.append(current.rstrip())
            current = f"{indent}{word} "
        else:
            current = candidate
    if current.strip():
        lines.append(current.rstrip())
    return lines


def render_record(origin: str, record: dict[str, Any]) -> list[str]:
    schema = _require(record, "schema", origin)
    if schema not in SUPPORTED_SCHEMAS:
        supported = ", ".join(sorted(SUPPORTED_SCHEMAS))
        raise ReportError(f"{origin}: schema {schema!r} is not one of {supported}")
    breakdown = _require(record, "phase_breakdown", origin)
    phases = {entry["phase"]: entry for entry in breakdown.get("phases", [])}
    unknown = set(TURN_PHASES + SESSION_PHASES) - set(phases)
    if unknown:
        raise ReportError(f"{origin}: record is missing phases {sorted(unknown)}")

    totals = breakdown.get("totals", {})
    turn_total = _stats(totals.get("turn_total", {}), origin, "turn_total")
    configured = breakdown.get("configured", {})
    constants = breakdown.get("product_constants", {})
    session_p50 = sum(
        _stats(phases[name], origin, name)["p50_ms"]
        for name in SESSION_PHASES + TURN_PHASES
    )

    lines = [
        "=" * 78,
        f"record   {origin}",
        f"schema   {schema}   policy {record.get('policy', 'unknown')}",
        (
            f"host     {record.get('os', '?')}/{record.get('arch', '?')}"
            f"   profile {record.get('profile', '?')}"
        ),
        f"latency  model: {breakdown.get('model_latency', 'unstated')}",
        (
            f"config   turns={configured.get('turns', '?')}"
            f"  transcript_drain_ms={configured.get('transcript_drain_ms', '?')}"
            f"  actor_poll_interval_ms="
            f"{configured.get('actor_poll_interval_ms', '?')}"
        ),
        "",
        "PER-PHASE WALL TIME (ms)",
    ]

    rows: list[list[str]] = []
    for name in SESSION_PHASES[:1] + TURN_PHASES + SESSION_PHASES[1:]:
        stats = _stats(phases[name], origin, name)
        scope = "turn" if name in TURN_PHASES else "session"
        rows.append(
            [
                name,
                scope,
                f"{int(stats['count'])}",
                _fmt_ms(stats["min_ms"]),
                _fmt_ms(stats["mean_ms"]),
                _fmt_ms(stats["p50_ms"]),
                _fmt_ms(stats["p95_ms"]),
                _fmt_ms(stats["max_ms"]),
                _share(stats["p50_ms"], session_p50),
            ]
        )
    rows.append(
        [
            "turn total",
            "turn",
            f"{int(turn_total['count'])}",
            _fmt_ms(turn_total["min_ms"]),
            _fmt_ms(turn_total["mean_ms"]),
            _fmt_ms(turn_total["p50_ms"]),
            _fmt_ms(turn_total["p95_ms"]),
            _fmt_ms(turn_total["max_ms"]),
            _share(turn_total["p50_ms"], session_p50),
        ]
    )
    lines += _table(
        ["phase", "scope", "n", "min", "mean", "p50", "p95", "max", "share@p50"],
        rows,
    )
    lines.append("")
    lines += _wrap(
        "share@p50 is each phase's p50 over the composed one-turn session "
        "(launch + admission + execution + completion + close). launch and close "
        "come from a different sample set than the turn phases, so the "
        "composition is arithmetic, not a single observed timeline.",
        78,
        "",
    )
    lines += ["", "STRUCTURAL FLOOR vs MEASURED"]
    lines += _floor_lines(breakdown, constants, configured, phases, turn_total, origin)

    gaps = breakdown.get("gaps", [])
    if gaps:
        lines += ["", "GAPS THE RECORD DECLARES"]
        for gap in gaps:
            observable = "observable" if gap.get("observable") else "NOT observable"
            lines.append(f"  - {gap.get('boundary', '?')} [{observable}]")
            lines += _wrap(str(gap.get("why", "")), 78, "      ")
    return lines


def _floor_lines(
    breakdown: dict[str, Any],
    constants: dict[str, Any],
    configured: dict[str, Any],
    phases: dict[str, Any],
    turn_total: dict[str, float],
    origin: str,
) -> list[str]:
    editor = float(constants.get("editor_stability_ms", 0))
    render = float(constants.get("post_paste_render_stability_ms", 0))
    fallback_drain = float(constants.get("transcript_drain_fallback_ms", 0))
    configured_drain = float(configured.get("transcript_drain_ms", 0))
    completion = _stats(phases["completion"], origin, "completion")
    admission = _stats(phases["admission"], origin, "admission")
    execution = _stats(phases["execution"], origin, "execution")
    slack = completion["p50_ms"] - configured_drain
    production_floor = editor + render + fallback_drain
    diagnostic_floor = editor + render + configured_drain
    projected = (
        editor
        + render
        + fallback_drain
        + max(slack, 0.0)
        + admission["p50_ms"]
        + execution["p50_ms"]
    )
    source = breakdown.get("product_constants", {}).get("source", "")
    lines = [
        f"  constants-only floor, production defaults   {_fmt_ms(production_floor)} ms",
        (
            f"    = editor stability {_fmt_ms(editor)}"
            f" + post-paste render stability {_fmt_ms(render)}"
            f" + transcript drain fallback {_fmt_ms(fallback_drain)}"
        ),
        f"  constants-only floor, as configured here    {_fmt_ms(diagnostic_floor)} ms",
        f"    (this record configured transcript_drain_ms={_fmt_ms(configured_drain)})",
        f"  measured turn total, p50                    {_fmt_ms(turn_total['p50_ms'])} ms",
        f"  measured completion phase, p50              {_fmt_ms(completion['p50_ms'])} ms",
        f"  observation slack over the configured drain {_fmt_ms(slack)} ms",
        (
            f"  projected per-turn floor at drain={_fmt_ms(fallback_drain)} ms:"
            f" {_fmt_ms(projected)} ms"
        ),
        (
            "    = the two editor-fence windows (not exercised here) + the "
            "production drain + this host's measured slack, admission and "
            "execution."
        ),
        "",
        "  The drain is the one tunable: it dominates the floor, and it is a per-cell",
        "  policy constant, not work pmux is doing. Everything else in the budget is a",
        "  stability window measured in hundreds of milliseconds.",
    ]
    if source:
        lines += ["", f"  constants provenance: {source}"]
    citations = constants.get("citations", {})
    for name in sorted(citations):
        lines.append(f"    {name}: {citations[name]}")
    return lines


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Print a pmux per-turn overhead breakdown from one or more "
            "performance-diagnostics records."
        ),
        epilog=(
            "Records are read from files, or from stdin when no path is given. "
            "Captured test stderr is accepted directly: lines carrying the "
            "'pmux_performance_diagnostics' prefix are extracted."
        ),
    )
    parser.add_argument("--version", action="version", version="pmux-perf-report 1")
    parser.add_argument(
        "paths",
        nargs="*",
        help="record files ('-' for stdin; default: stdin)",
    )
    parser.add_argument(
        "--no-preamble",
        action="store_true",
        help="omit the claims header",
    )
    args = parser.parse_args(argv)

    try:
        records = load_records(args.paths)
    except (ReportError, OSError) as error:
        print(f"perf_report: {error}", file=sys.stderr)
        return 1

    out: list[str] = []
    if not args.no_preamble:
        out += [line.rstrip() for line in (__doc__ or "").strip().splitlines()]
        out.append("")
    try:
        for origin, record in records:
            out += render_record(origin, record)
            out.append("")
    except ReportError as error:
        print(f"perf_report: {error}", file=sys.stderr)
        return 1
    out.append(f"records read: {len(records)}")
    print("\n".join(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
