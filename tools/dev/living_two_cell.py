#!/usr/bin/env python3
"""Living two-cell proof: two-account `pmux ask` plus Full `pmux run`.

Spends real subscription turns. Writes a receipt under evidence/ or --output.
Does not edit PROMOTED_PROFILES. Usage-limit and empty text are RED, not GREEN.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import time
import uuid
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]


def fail(message: str) -> None:
    print(f"RED: {message}", file=sys.stderr)
    raise SystemExit(2)


def wait_socket(path: pathlib.Path, timeout: float) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if path.exists():
            try:
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                    sock.settimeout(1)
                    sock.connect(str(path))
                return
            except OSError:
                pass
        time.sleep(0.2)
    fail(f"pmuxd never bound {path}")


def run_pmux(
    pmux: pathlib.Path,
    sock: pathlib.Path,
    args: list[str],
    timeout: float,
) -> dict[str, Any]:
    completed = subprocess.run(
        [str(pmux), "--socket", str(sock), "--output", "json", *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        fail(
            f"pmux {' '.join(args[:4])} exited {completed.returncode}: "
            f"{completed.stderr.strip() or completed.stdout.strip()}"
        )
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        fail(f"pmux json was not an object: {error}: {completed.stdout[:400]}")
    if not isinstance(payload, dict):
        fail(f"pmux json was not an object: {payload!r}")
    return payload


def stop_kind(stop: Any) -> str | None:
    if stop is None:
        return None
    if isinstance(stop, str):
        return stop
    if isinstance(stop, dict):
        kind = stop.get("kind") or stop.get("type")
        return kind if isinstance(kind, str) else None
    return str(stop)


def refuse_usage_limit(label: str, result: dict[str, Any]) -> None:
    text = (result.get("text") or "").strip()
    stop = stop_kind(result.get("stop_reason"))
    if not text:
        fail(f"{label}: empty text")
    if stop in {"max_tokens", "refusal"}:
        fail(f"{label}: stop_reason={stop!r} text={text[:200]!r}")
    lowered = text.lower()
    if "usage limit" in lowered or "rate limit" in lowered or "you've hit" in lowered:
        fail(f"{label}: usage-limit text is not success: {text[:200]!r}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir", type=pathlib.Path, required=True)
    parser.add_argument("--claude", type=pathlib.Path, required=True)
    parser.add_argument("--runtime", type=pathlib.Path, required=True)
    parser.add_argument("--task-cwd", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--account-pin", action="append", default=[])
    parser.add_argument("--tested-claude-profile")
    parser.add_argument("--model", default="claude-sonnet-5")
    parser.add_argument("--effort", default="low")
    parser.add_argument("--os", required=True)
    parser.add_argument("--arch", required=True)
    args = parser.parse_args()

    release = args.release_dir.resolve()
    pmux = release / "pmux"
    pmuxd = release / "pmuxd"
    for binary in (pmux, pmuxd, release / "pmux-rmuxd", release / "pmux-launcher"):
        if not binary.is_file():
            fail(f"missing {binary}")

    runtime = args.runtime.resolve()
    pool = runtime / "pool"
    private = runtime / "private"
    sock = runtime / "pmux.sock"
    task = args.task_cwd.resolve()
    for path in (runtime, pool, private, task):
        path.mkdir(parents=True, exist_ok=True)
        os.chmod(path, 0o700)

    hello = task / "hello.txt"
    if hello.exists():
        hello.unlink()

    accounts = [("default", None)]
    for spec in args.account_pin:
        if "=" not in spec:
            fail(f"--account-pin NAME=/abs/pin, got {spec!r}")
        name, pin = spec.split("=", 1)
        accounts.append((name, pin))

    argv = [
        str(pmuxd),
        "serve",
        "--socket",
        str(sock),
        "--rmuxd",
        str(release / "pmux-rmuxd"),
        "--launcher",
        str(release / "pmux-launcher"),
        "--runtime-parent",
        str(private),
        "--pool-parent",
        str(pool),
        "--pool-claude",
        str(args.claude.resolve()),
        "--pool-securestorage-dir",
        "empty",
        "--pool-size",
        str(max(2, len(accounts))),
        "--pool-no-evidence",
        "--stateful",
    ]
    for name, pin in accounts[1:]:
        argv += ["--pool-account", f"{name}={pin}"]
    for name, _ in accounts:
        warm = f"{args.model}/{args.effort}"
        if name != "default":
            warm += f"@{name}"
        argv += ["--pool-warm", f"{warm}=1"]
    if args.tested_claude_profile:
        argv += ["--tested-claude-profile", args.tested_claude_profile]

    log_path = runtime / "pmuxd.log"
    log = log_path.open("wb")
    daemon = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
    receipt: dict[str, Any] = {
        "schema": "pmux.living-two-cell.v1",
        "os": args.os,
        "arch": args.arch,
        "model": args.model,
        "effort": args.effort,
        "claude": str(args.claude),
        "accounts": [name for name, _ in accounts],
        "stateful": True,
        "pmuxd_argv": argv,
    }
    try:
        wait_socket(sock, 90)
        last_pool: dict[str, Any] = {}
        warm_deadline = time.time() + 120
        doctor: dict[str, Any] = {}
        while time.time() < warm_deadline:
            doctor = run_pmux(pmux, sock, ["doctor", "--claude", str(args.claude)], 60)
            layers = (doctor.get("diagnosis") or {}).get("layers") or []
            pool_ev = {}
            for item in layers:
                if item.get("layer") == "pool":
                    pool_ev = item.get("evidence") or {}
            last_pool = pool_ev if isinstance(pool_ev, dict) else {}
            if last_pool.get("halted"):
                fail(f"pool halted during warm: {last_pool['halted']}")
            if int(last_pool.get("idle") or 0) >= len(accounts):
                break
            time.sleep(0.5)
        else:
            fail(f"warm instances never reached idle: {last_pool}")
        receipt["doctor_status"] = doctor.get("status")
        receipt["pool_census"] = last_pool
        if doctor.get("status") != "healthy":
            fail(f"doctor was {doctor.get('status')!r}: {json.dumps(doctor)[:800]}")

        asks = []
        for name, _ in accounts:
            nonce = uuid.uuid4().hex[:8].upper()
            prompt = (
                f"Reply with exactly the token {name.upper()}-{nonce} and nothing else."
            )
            started = time.monotonic()
            ask_args = [
                "ask",
                "--model",
                args.model,
                "--effort",
                args.effort,
                "--account",
                name,
                prompt,
            ]
            try:
                result = run_pmux(pmux, sock, ask_args, 180)
            except SystemExit:
                time.sleep(2)
                result = run_pmux(pmux, sock, ask_args, 180)
            wall_ms = round((time.monotonic() - started) * 1000, 1)
            refuse_usage_limit(f"ask --account {name}", result)
            text = (result.get("text") or "").strip()
            expected = f"{name.upper()}-{nonce}"
            row = {
                "account": name,
                "nonce": nonce,
                "expected": expected,
                "text": text,
                "answered": expected in text or text == expected,
                "stop_reason": result.get("stop_reason"),
                "claude_version": result.get("claude_version"),
                "usage": result.get("usage") or {},
                "client_wall_ms": wall_ms,
            }
            asks.append(row)
            if not row["answered"]:
                fail(f"ask --account {name} expected {expected!r}, got {text!r}")
        receipt["asks"] = asks

        prompt = "Write hello.txt containing hi"
        started = time.monotonic()
        run_args = [
            "run",
            "--model",
            args.model,
            "--effort",
            args.effort,
            "--cwd",
            str(task),
            "--permission-mode",
            "dangerously-skip-permissions",
            prompt,
        ]
        time.sleep(2)
        try:
            full = run_pmux(pmux, sock, run_args, 300)
        except SystemExit:
            time.sleep(3)
            full = run_pmux(pmux, sock, run_args, 300)
        wall_ms = round((time.monotonic() - started) * 1000, 1)
        refuse_usage_limit("run Full hello.txt", full)
        hello_text = hello.read_text(encoding="utf-8") if hello.is_file() else None
        receipt["full"] = {
            "text": (full.get("text") or "").strip(),
            "stop_reason": full.get("stop_reason"),
            "claude_version": full.get("claude_version"),
            "usage": full.get("usage") or {},
            "client_wall_ms": wall_ms,
            "hello_path": str(hello),
            "hello_exists": hello.is_file(),
            "hello_text": hello_text,
        }
        if hello_text not in {"hi\n", "hi"}:
            fail(f"hello.txt contents were {hello_text!r}, not hi")
        stateful_left = list((pool / "stateful").glob("*")) if (pool / "stateful").exists() else []
        receipt["stateful_trees_after"] = [str(path) for path in stateful_left]
        if stateful_left:
            fail(f"stateful isolation trees leaked: {stateful_left}")
        receipt["verdict"] = "GREEN"
    except SystemExit:
        receipt["verdict"] = "RED"
        raise
    finally:
        daemon.send_signal(signal.SIGTERM)
        try:
            daemon.wait(timeout=20)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait(timeout=5)
        log.close()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"verdict": receipt.get("verdict"), "output": str(args.output)}))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.TimeoutExpired as error:
        fail(f"timed out: {error}")
