#!/usr/bin/env python3
"""n=50 minified drain campaign on this OS.

Starts one warm pool cell, drives 50 `pmux ask` turns (40 arithmetic + 10
long-reply), then measures reachable post-answer JSONL gaps on the daemon's
own evidence mirror.

The probe cell uses a 1000 ms drain so arrivals up to the shipped macos bound
can be observed. Linux's shipped 250 ms would censor anything slower than that.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import random
import shutil
import subprocess
import sys
import time
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "promotion"))
sys.path.insert(0, str(ROOT / "tools" / "evidence_common"))

from measure_turn_latency import (  # noqa: E402
    Daemon,
    MeasurementError,
    Sandbox,
    claude_version,
    host_identity,
    resolve_binaries,
    run_client,
)
from promote_claude_version import GRADES, _nonce  # noqa: E402
import portable_paths  # noqa: E402

SHORT = next(g for g in GRADES if g.id == "01-arithmetic-nonce")
LONG = next(g for g in GRADES if g.id == "04-ordered-long-reply")
N_SHORT = 40
N_LONG = 10
TURN_DEADLINE_MS = 180_000
OBSERVE_DRAIN_MS = 1000
SCHEMA = "pmux.drain-n50.v1"


def doctor(binaries: dict[str, pathlib.Path], sandbox: Sandbox, claude: pathlib.Path) -> dict[str, Any]:
    done = subprocess.run(
        [
            str(binaries["pmux"]),
            "--socket",
            str(sandbox.socket),
            "--output",
            "json",
            "doctor",
            "--claude",
            str(claude),
        ],
        env=sandbox.environment(),
        capture_output=True,
        text=True,
        timeout=180,
    )
    payloads = [line for line in done.stdout.splitlines() if line.startswith("{")]
    if not payloads:
        raise MeasurementError(f"doctor printed no JSON:\n{done.stdout}\n{done.stderr}")
    return json.loads(payloads[-1])


def pool_layer(report: dict[str, Any]) -> dict[str, Any]:
    for item in (report.get("diagnosis") or {}).get("layers") or []:
        if item.get("layer") == "pool":
            return item
    raise MeasurementError("doctor has no pool layer")


def evidence_dir_of(report: dict[str, Any]) -> pathlib.Path:
    for item in (report.get("diagnosis") or {}).get("layers") or []:
        if item.get("layer") == "configuration":
            directory = ((item.get("evidence") or {}).get("path_b") or {}).get(
                "evidence_dir"
            )
            if directory:
                return pathlib.Path(directory)
    raise MeasurementError("daemon published no path_b.evidence_dir")


def wait_idle(
    binaries: dict[str, pathlib.Path], sandbox: Sandbox, claude: pathlib.Path
) -> dict[str, Any]:
    last: dict[str, Any] = {}
    deadline = time.monotonic() + 180.0
    while time.monotonic() < deadline:
        report = doctor(binaries, sandbox, claude)
        last = pool_layer(report).get("evidence") or {}
        if last.get("halted"):
            raise MeasurementError(f"pool halted during warm: {last['halted']}")
        if int(last.get("idle") or 0) >= 1:
            return last
        time.sleep(0.25)
    raise MeasurementError(f"warm instance never reached idle: {last}")


def refuse_usage(text: str) -> None:
    lowered = (text or "").lower()
    if "usage limit" in lowered or "rate limit" in lowered or "you've hit" in lowered:
        raise MeasurementError(f"usage-limit is not a drain sample: {text[:200]!r}")


def ask(
    binaries: dict[str, pathlib.Path],
    sandbox: Sandbox,
    grade: Any,
    effort: str,
    nonce: str,
    model: str,
) -> dict[str, Any]:
    prompt = grade.render(nonce)
    expected = grade.expected(nonce)
    deadline = int(time.time() * 1000) + TURN_DEADLINE_MS
    result, wall_ms = run_client(
        binaries,
        sandbox,
        [
            "ask",
            "--model",
            model,
            "--effort",
            effort,
            "--deadline-unix-ms",
            str(deadline),
            prompt,
        ],
        timeout=TURN_DEADLINE_MS / 1000.0 + 60.0,
    )
    text = (result.get("text") or "").strip()
    refuse_usage(text)
    return {
        "grade": grade.id,
        "effort": effort,
        "nonce": nonce,
        "expected": expected,
        "text": text,
        "answered": text == expected,
        "stop_reason": result.get("stop_reason"),
        "claude_version": result.get("claude_version"),
        "usage": result.get("usage") or {},
        "client_wall_ms": round(wall_ms, 1),
    }


def measure_drain(corpus: pathlib.Path, version: str, host_os: str, host_arch: str) -> dict[str, Any]:
    argv = [
        sys.executable,
        str(ROOT / "tools" / "promotion" / "measure_transcript_drain.py"),
        "--corpus",
        str(corpus),
        "--version",
        version,
        "--os",
        host_os,
        "--arch",
        host_arch,
        "--bound-ms",
        str(OBSERVE_DRAIN_MS),
        "--json",
    ]
    done = subprocess.run(argv, capture_output=True, text=True, timeout=600)
    body = done.stdout.strip()
    try:
        measured = json.loads(body) if body.startswith("{") else {}
    except json.JSONDecodeError:
        measured = {}
    if done.returncode != 0:
        return {
            "ok": False,
            "exit": done.returncode,
            "stderr": done.stderr[-2000:],
            "stdout_tail": body[-2000:],
            "measured": measured,
        }
    return {"ok": True, "exit": 0, "measured": measured, "argv": argv[1:]}


def keep_corpus(corpus: pathlib.Path, destination: pathlib.Path) -> dict[str, Any]:
    """Copy this campaign's transcripts out before the sandbox is removed.

    WHY THIS EXISTS. The corpus a campaign produces is the daemon's Path B
    evidence mirror, which is a sibling of the socket
    (`bin/pmuxd/src/main.rs::settings.evidence_dir`) and therefore lives inside
    `Sandbox.root` -- which `Sandbox.remove()` deletes in this tool's `finally`.
    So the transcripts are gone the moment the run ends and the receipt keeps
    only summary numbers.

    That is fine for a per-version campaign and fatal for a POOLED one. The
    bound pmux ships is pooled over every version measured
    (`docs/engineering/version-drift.md` P1), and
    `compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit`
    refuses a pool that names fewer than two versions. Pooling two versions
    means one `measure_transcript_drain.py` run over BOTH campaigns' corpora,
    and no committed tool could produce that corpus because each campaign ate
    its own.

    This copies only `*.jsonl`, at the same relative paths, so the destination
    is a corpus root `measure_transcript_drain.py` can be pointed at directly.
    It is NOT evidence: it is host-local transcripts carrying real prompts, it
    is never committed, and the receipt remains the durable artifact.
    """

    destination.mkdir(parents=True, exist_ok=True)
    copied = 0
    for source in sorted(corpus.rglob("*.jsonl")):
        target = destination / source.relative_to(corpus)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        copied += 1
    return {"root": str(destination), "jsonl_files": copied}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir", type=pathlib.Path, required=True)
    parser.add_argument("--claude", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument(
        "--corpus-out",
        type=pathlib.Path,
        default=None,
        help="copy this campaign's transcripts here before the sandbox is "
        "removed, so a later measure_transcript_drain.py run can POOL this "
        "version with another. Host-local and never committed: the files "
        "carry real prompts.",
    )
    parser.add_argument("--model", default="claude-sonnet-5")
    parser.add_argument("--effort", default="low")
    parser.add_argument("--seed", type=int, default=27250)
    args = parser.parse_args()

    binaries = resolve_binaries(args.release_dir)
    claude = args.claude.resolve(strict=True)
    version = claude_version(claude)
    host_os, host_arch = host_identity()
    rng = random.Random(args.seed)
    plan = [SHORT] * N_SHORT + [LONG] * N_LONG
    rng.shuffle(plan)

    sandbox = Sandbox("operator")
    profile = {
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "transcript_drain_ms": OBSERVE_DRAIN_MS,
    }
    daemon = Daemon(
        binaries,
        sandbox,
        profile,
        claude,
        f"{args.model}/{args.effort}=1",
    )
    turns: list[dict[str, Any]] = []
    drain: dict[str, Any] = {}
    census: dict[str, Any] = {}
    kept: dict[str, Any] | None = None
    started = time.monotonic()
    try:
        census = wait_idle(binaries, sandbox, claude)
        for grade in plan:
            nonce = _nonce(rng)
            sample = None
            last_error: str | None = None
            for _attempt in range(2):
                try:
                    sample = ask(binaries, sandbox, grade, args.effort, nonce, args.model)
                    break
                except MeasurementError as error:
                    last_error = str(error)[:600]
                    time.sleep(1.5)
            if sample is None:
                turns.append(
                    {
                        "grade": grade.id,
                        "nonce": nonce,
                        "answered": False,
                        "error": last_error,
                    }
                )
                continue
            turns.append(sample)
        report = doctor(binaries, sandbox, claude)
        corpus = evidence_dir_of(report)
        daemon.stop()
        daemon = None  # type: ignore[assignment]
        time.sleep(1)
        drain = measure_drain(corpus, version, host_os, host_arch)
        if args.corpus_out is not None:
            kept = keep_corpus(corpus, args.corpus_out)
    finally:
        if daemon is not None:
            daemon.stop()
        sandbox.remove()

    reachable = ((drain.get("measured") or {}).get("post_answer_arrivals") or {}).get(
        "reachable_on_a_minified_cell"
    )
    recommended = (drain.get("measured") or {}).get("recommended_transcript_drain_ms")
    binds = (drain.get("measured") or {}).get("full_drain_binds_on") or {}
    answered = sum(1 for row in turns if row.get("answered"))
    receipt = {
        "schema": SCHEMA,
        "os": host_os,
        "arch": host_arch,
        "claude_version": version,
        "model": args.model,
        "effort": args.effort,
        "observe_drain_ms": OBSERVE_DRAIN_MS,
        "n_planned": N_SHORT + N_LONG,
        "n_answered": answered,
        "n_short": N_SHORT,
        "n_long": N_LONG,
        "warm_census": census,
        "drain_ok": drain.get("ok"),
        "drain_exit": drain.get("exit"),
        "recommended_transcript_drain_ms": recommended,
        "reachable_post_answer_arrivals": reachable,
        "full_drain_binds_on": binds,
        "per_version_recommendations_not_to_be_shipped": (drain.get("measured") or {}).get(
            "per_version_recommendations_not_to_be_shipped"
        ),
        "elapsed_ms": round((time.monotonic() - started) * 1000.0, 1),
        # Where this campaign's transcripts were kept, when they were kept. Not
        # a measurement and not committed with the corpus: a pointer, so a
        # pooled receipt taken later can be traced back to the run that fed it.
        "corpus_kept": kept,
        "turns": turns,
    }
    encoded = json.dumps(portable_paths.render_document(receipt), indent=2, sort_keys=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(encoded + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "output": str(args.output),
                "answered": answered,
                "drain_ok": drain.get("ok"),
                "recommended": recommended,
                "reachable": reachable,
                "full_drain_binds_on": binds,
                "corpus_kept": kept,
            },
            indent=2,
        )
    )
    if answered < N_SHORT + N_LONG:
        return 1
    if not drain.get("ok"):
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except MeasurementError as error:
        print(f"RED: {error}", file=sys.stderr)
        raise SystemExit(2)
