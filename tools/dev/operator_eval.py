#!/usr/bin/env python3
"""Confirm a Claude binary on this OS against this pmux tree.

This is the Linux (and macos operator) pin path. It spends real turns.
It does not read or write a pooled-drain receipt. It does not edit
PROMOTED_PROFILES. Messages same-cell + cache hit is the product identity;
pgrep pid-set equality is not.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import random
import signal
import socket
import subprocess
import sys
import time
import unicodedata
from http.client import HTTPConnection
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "promotion"))
sys.path.insert(0, str(ROOT / "tools" / "evidence_common"))

from measure_turn_latency import (  # noqa: E402
    MeasurementError,
    Sandbox,
    claude_version,
    host_identity,
    resolve_binaries,
    run_client,
)
from promote_claude_version import (  # noqa: E402
    GRADES,
    PROBE_SENTINEL,
    PROBE_VALUES,
    PromotionRefused,
    _nonce,
    valueless_bundle_flags,
)
from measure_transcript_drain import MINIFIED_LAUNCH_FLAGS  # noqa: E402
import portable_paths  # noqa: E402

OPERATOR_DRAIN_MS = 250
MODEL = "claude-sonnet-5"
TURN_DEADLINE_MS = 180_000
SCHEMA = "pmux.operator-eval.v1"
GREEN = "GREEN_OPERATOR"
RED = "RED"
CHECK_ORDER = (
    "launch_bundle_parses",
    "minified_cell_is_admitted",
    "grades_answer",
    "context_did_not_survive_recycling",
    "no_tool_surface",
    "pool_never_halted",
    "messages_sticky",
)
PRE_MESSAGES_CHECKS = CHECK_ORDER[1:-1]


def file_sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def describe() -> str:
    return (
        "operator_eval confirms a Claude binary on this OS against this pmux.\n"
        f"schema: {SCHEMA}\n"
        f"green verdict: {GREEN}\n"
        "not a promotion: does not read or write a pooled-drain receipt and "
        "does not edit PROMOTED_PROFILES.\n"
        "product identity: Messages same-cell + cache hit (not pgrep pid-set).\n"
        "checks:\n"
        + "".join(f"  - {name}\n" for name in CHECK_ORDER)
    )


def free_loopback() -> str:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return f"127.0.0.1:{port}"


class EvalDaemon:
    def __init__(
        self,
        binaries: dict[str, pathlib.Path],
        sandbox: Sandbox,
        profile: dict[str, Any],
        claude: pathlib.Path,
        messages_bind: str | None,
        model: str,
        effort: str | None,
    ) -> None:
        # `messages_bind` and `effort` are optional so model_matrix.py can reuse
        # this daemon: it needs no Messages listener, and it warms a class whose
        # model takes no `--effort` at all. operator_eval always passes both.
        self.sandbox = sandbox
        self.log_path = sandbox.root / "pmuxd.log"
        self.log = self.log_path.open("wb")
        argv = [
            str(binaries["pmuxd"]),
            "serve",
            "--socket",
            str(sandbox.socket),
            "--rmuxd",
            str(binaries["pmux-rmuxd"]),
            "--launcher",
            str(binaries["pmux-launcher"]),
            "--runtime-parent",
            str(sandbox.root / "private"),
            "--tested-claude-profile",
            json.dumps(profile, sort_keys=True),
            "--pool-parent",
            str(sandbox.root / "pool"),
            "--pool-claude",
            str(claude),
            "--pool-size",
            "3",
            "--pool-recycle-turns",
            "250",
            "--pool-idle-ttl-ms",
            "600000",
            "--pool-turn-timeout-ms",
            "180000",
            "--pool-warm",
            f"{model}/{effort}=1" if effort else f"{model}=1",
        ]
        if messages_bind:
            argv += ["--messages-bind", messages_bind]
        self.process = subprocess.Popen(
            argv,
            env=sandbox.environment(),
            stdin=subprocess.DEVNULL,
            stdout=self.log,
            stderr=self.log,
            start_new_session=True,
        )
        deadline = time.monotonic() + 180.0
        while time.monotonic() < deadline:
            if sandbox.socket.is_socket():
                return
            if self.process.poll() is not None:
                raise MeasurementError(
                    "pmuxd exited during startup:\n"
                    + self.log_path.read_text(errors="replace")
                )
            time.sleep(0.05)
        raise MeasurementError("pmuxd never bound its socket")

    def stop(self) -> None:
        for number in (signal.SIGTERM, signal.SIGKILL):
            if self.process.poll() is not None:
                break
            try:
                os.killpg(os.getpgid(self.process.pid), number)
            except (ProcessLookupError, PermissionError):
                break
            try:
                self.process.wait(timeout=20)
                break
            except subprocess.TimeoutExpired:
                continue
        self.process.wait()
        self.log.close()


def probe_launch_bundle(claude: pathlib.Path) -> dict[str, Any]:
    valueless = valueless_bundle_flags()
    probed: dict[str, str] = {}
    for flag in MINIFIED_LAUNCH_FLAGS:
        argv = [str(claude), flag]
        if flag in PROBE_VALUES:
            argv.append(PROBE_VALUES[flag])
        elif flag not in valueless:
            raise MeasurementError(f"no probe value for {flag}")
        argv += [PROBE_SENTINEL, "doctor"]
        done = subprocess.run(argv, capture_output=True, text=True, timeout=120)
        reply = (done.stdout + done.stderr).strip()
        if PROBE_SENTINEL in reply and flag not in reply:
            probed[flag] = "accepted"
        elif flag in reply:
            probed[flag] = "REJECTED"
        else:
            probed[flag] = f"unreadable: {reply[:200]!r}"
    rejected = {key: value for key, value in probed.items() if value != "accepted"}
    control = subprocess.run(
        [str(claude), "--definitely-not-a-flag", PROBE_SENTINEL, "doctor"],
        capture_output=True,
        text=True,
        timeout=120,
    )
    control_reply = (control.stdout + control.stderr).strip()
    return {
        "probed": probed,
        "rejected": rejected,
        "negative_control_rejected": "--definitely-not-a-flag" in control_reply,
        "ok": not rejected and "--definitely-not-a-flag" in control_reply,
    }


def doctor(
    binaries: dict[str, pathlib.Path], sandbox: Sandbox, claude: pathlib.Path
) -> dict[str, Any]:
    argv = [
        str(binaries["pmux"]),
        "--socket",
        str(sandbox.socket),
        "--output",
        "json",
        "doctor",
        "--claude",
        str(claude),
    ]
    done = subprocess.run(
        argv,
        env=sandbox.environment(),
        capture_output=True,
        text=True,
        timeout=180,
    )
    payloads = [line for line in done.stdout.splitlines() if line.startswith("{")]
    if not payloads:
        raise MeasurementError(f"doctor printed no JSON:\n{done.stdout}\n{done.stderr}")
    return json.loads(payloads[-1])


def layer(report: dict[str, Any], name: str) -> dict[str, Any]:
    for item in (report.get("diagnosis") or {}).get("layers") or []:
        if item.get("layer") == name:
            return item
    raise MeasurementError(f"no {name} layer")


def http_messages(
    bind: str,
    method: str,
    path: str,
    body: str = "",
    extra_headers: dict[str, str] | None = None,
) -> tuple[int, dict[str, str], str]:
    host, port = bind.rsplit(":", 1)
    conn = HTTPConnection(host, int(port), timeout=180)
    headers = {
        "x-api-key": "pmux-eval",
        "content-type": "application/json",
        "connection": "close",
    }
    if extra_headers:
        headers.update(extra_headers)
    conn.request(method, path, body=body.encode() if body else None, headers=headers)
    response = conn.getresponse()
    raw = response.read().decode("utf-8", errors="replace")
    hdrs = {k.lower(): v for k, v in response.getheaders()}
    conn.close()
    return response.status, hdrs, raw


def sticky_messages_payloads(
    model: str, first_user: str, assistant_text: str, token: str
) -> tuple[dict[str, Any], dict[str, Any]]:
    """T1 primer and T2 full-history continue. A suffix-only T2 reprimes."""

    t1 = {
        "model": model,
        "max_tokens": 128,
        "messages": [{"role": "user", "content": first_user}],
    }
    t2 = {
        "model": model,
        "max_tokens": 128,
        "messages": [
            {"role": "user", "content": first_user},
            {"role": "assistant", "content": assistant_text},
            {
                "role": "user",
                "content": f"Reply with exactly {token} and nothing else.",
            },
        ],
    }
    return t1, t2


def parse_message_body(raw: str) -> tuple[str, dict[str, Any]]:
    try:
        body = json.loads(raw)
    except json.JSONDecodeError:
        return "", {}
    text = ""
    for block in body.get("content") or []:
        if isinstance(block, dict) and block.get("type") == "text":
            text += str(block.get("text") or "")
    usage = body.get("usage") if isinstance(body.get("usage"), dict) else {}
    return text.strip(), usage


def messages_turn(
    bind: str, conversation: str, payload: dict[str, Any]
) -> dict[str, Any]:
    started = time.monotonic()
    try:
        status, headers, raw = http_messages(
            bind,
            "POST",
            "/v1/messages",
            json.dumps(payload),
            extra_headers={"x-pmux-conversation": conversation},
        )
    except OSError as error:
        return {
            "status": 0,
            "elapsed_s": round(time.monotonic() - started, 2),
            "lease": None,
            "cell": None,
            "conversation": conversation,
            "usage": {},
            "text": "",
            "raw_head": f"connection failed: {error}",
        }
    text, usage = parse_message_body(raw)
    return {
        "status": status,
        "elapsed_s": round(time.monotonic() - started, 2),
        "lease": headers.get("x-pmux-lease"),
        "cell": headers.get("x-pmux-cell"),
        "conversation": headers.get("x-pmux-conversation") or conversation,
        "usage": usage,
        "text": text,
        "raw_head": raw[:240],
    }


def execute(args: argparse.Namespace) -> dict[str, Any]:
    binaries = resolve_binaries(args.release_dir)
    claude = args.claude.resolve(strict=True)
    version = claude_version(claude)
    host_os, host_arch = host_identity()
    efforts = args.efforts
    pin = {
        "path": str(claude),
        "sha256": file_sha256(claude),
        "bytes": claude.stat().st_size,
        "version": version,
    }
    receipt: dict[str, Any] = {
        "schema": SCHEMA,
        "kind": "operator_compatibility",
        "not_a_promotion": True,
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "pin": pin,
        "promotion_status": {
            "may_replace_operator_pin": False,
            "may_ship_without_flag": False,
            "requires_for_flagless": (
                f"evidence/pooled-transcript-drain-{host_os}-{host_arch}.json "
                "and tools/dev/promote.py"
            ),
        },
        "operator_profile": {
            "claude_version": version,
            "os": host_os,
            "arch": host_arch,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": args.drain_ms,
        },
    }
    receipt["launch_bundle_parses"] = probe_launch_bundle(claude)
    if not receipt["launch_bundle_parses"]["ok"]:
        receipt["verdict"] = RED
        return receipt

    sandbox = Sandbox("operator")
    bind = free_loopback()
    daemon = EvalDaemon(
        binaries,
        sandbox,
        receipt["operator_profile"],
        claude,
        bind,
        args.model,
        efforts[0],
    )
    checks: dict[str, Any] = {}
    turns: list[dict[str, Any]] = []
    try:
        last: dict[str, Any] = {}
        deadline = time.monotonic() + args.warm_timeout_ms / 1000.0
        while time.monotonic() < deadline:
            report = doctor(binaries, sandbox, claude)
            last = layer(report, "pool").get("evidence") or {}
            if last.get("halted"):
                raise MeasurementError(f"pool halted during warm: {last['halted']}")
            if last.get("idle", 0) >= 1:
                break
            time.sleep(0.2)
        else:
            raise MeasurementError(f"warm instance never reached idle: {last}")
        checks["minified_cell_is_admitted"] = {
            "ok": True,
            "census": last,
            "doctor_status": doctor(binaries, sandbox, claude).get("status"),
        }

        rng = random.Random(args.seed)
        for grade in GRADES:
            nonce = _nonce(rng)
            prompt = grade.render(nonce)
            if unicodedata.normalize("NFC", prompt) != prompt:
                raise MeasurementError(f"{grade.id} is not NFC")
            expected = grade.expected(nonce)
            deadline_ms = int(time.time() * 1000) + args.turn_deadline_ms
            result, wall_ms = run_client(
                binaries,
                sandbox,
                [
                    "run",
                    "--model",
                    args.model,
                    "--effort",
                    efforts[0],
                    "--deadline-unix-ms",
                    str(deadline_ms),
                    prompt,
                ],
                timeout=args.turn_deadline_ms / 1000.0 + 60.0,
            )
            text = (result.get("text") or "").strip()
            turns.append(
                {
                    "grade": grade.id,
                    "effort": efforts[0],
                    "nonce": nonce,
                    "expected": expected,
                    "text": text,
                    "answered": text == expected,
                    "client_wall_ms": round(wall_ms, 1),
                    "claude_version": result.get("claude_version"),
                    "usage": result.get("usage") or {},
                }
            )
        if len(efforts) > 1:
            nonce = _nonce(rng)
            expected = GRADES[0].expected(nonce)
            deadline_ms = int(time.time() * 1000) + args.turn_deadline_ms
            result, wall_ms = run_client(
                binaries,
                sandbox,
                [
                    "run",
                    "--model",
                    args.model,
                    "--effort",
                    efforts[1],
                    "--deadline-unix-ms",
                    str(deadline_ms),
                    GRADES[0].render(nonce),
                ],
                timeout=args.turn_deadline_ms / 1000.0 + 60.0,
            )
            text = (result.get("text") or "").strip()
            turns.append(
                {
                    "grade": GRADES[0].id,
                    "effort": efforts[1],
                    "nonce": nonce,
                    "expected": expected,
                    "text": text,
                    "answered": text == expected,
                    "client_wall_ms": round(wall_ms, 1),
                    "claude_version": result.get("claude_version"),
                    "usage": result.get("usage") or {},
                }
            )

        wrong = [turn["grade"] for turn in turns if not turn["answered"]]
        first = next(turn for turn in turns if turn["grade"] == GRADES[0].id)
        probe = next(turn for turn in turns if turn["grade"] == GRADES[1].id)
        leaked = [
            needle
            for needle in (first["nonce"], first["expected"])
            if needle in probe["text"]
        ]
        offenders: list[Any] = []
        for sample in turns:
            usage = sample["usage"] if isinstance(sample["usage"], dict) else {}
            side = usage.get("sidechain") if isinstance(usage.get("sidechain"), dict) else {}
            if any(side.values()):
                offenders.append((sample["grade"], "sidechain", side))
            main = usage.get("main") if isinstance(usage.get("main"), dict) else usage
            for field in ("cache_creation_input_tokens", "cache_read_input_tokens"):
                if main.get(field):
                    offenders.append((sample["grade"], field, main[field]))
        pool_after = layer(doctor(binaries, sandbox, claude), "pool").get("evidence") or {}
        checks["grades_answer"] = {"ok": not wrong, "wrong": wrong}
        checks["context_did_not_survive_recycling"] = {
            "ok": not leaked,
            "leaked": leaked,
        }
        checks["no_tool_surface"] = {"ok": not offenders, "offenders": offenders}
        checks["pool_never_halted"] = {
            "ok": not pool_after.get("halted") and not pool_after.get("leaked"),
            "halted": pool_after.get("halted"),
            "leaked": pool_after.get("leaked"),
        }

        if not all(
            checks[name]["ok"] for name in PRE_MESSAGES_CHECKS if name in checks
        ):
            checks["messages_sticky"] = {
                "ok": False,
                "skipped": True,
                "reason": "earlier check failed; not spending Messages turns",
            }
        else:
            filler = "cache-primer " + ("alpha bravo charlie delta echo foxtrot " * 80)
            token = f"Otter-Cache-{int(time.time()) % 10000}"
            first_user = (
                f"{filler}\nReply with exactly ACK {token} and nothing else."
            )
            conversation = f"operator-eval-{int(time.time())}"
            model_id = f"{args.model}-{efforts[0]}"
            t1_payload, _ = sticky_messages_payloads(model_id, first_user, "", token)
            t1 = messages_turn(bind, conversation, t1_payload)
            _, t2_payload = sticky_messages_payloads(
                model_id, first_user, t1["text"], token
            )
            t2 = messages_turn(bind, t1["conversation"], t2_payload)
            try:
                rel_status, _, rel_body = http_messages(
                    bind, "POST", f"/v1/conversations/{t1['conversation']}/release"
                )
            except OSError as error:
                rel_status, rel_body = 0, f"connection failed: {error}"
            cache_write = t1["usage"].get("cache_creation_input_tokens")
            cache_read = t2["usage"].get("cache_read_input_tokens")
            checks["messages_sticky"] = {
                "ok": (
                    t1["status"] == 200
                    and t2["status"] == 200
                    and t1["lease"] == "primed"
                    and t2["lease"] == "continued"
                    and t1.get("cell")
                    and t1.get("cell") == t2.get("cell")
                    and bool(cache_read)
                    and cache_read == cache_write
                    and rel_status == 200
                ),
                "turn1": t1,
                "turn2": t2,
                "release_status": rel_status,
                "release_body": rel_body[:300],
                "same_cell": t1.get("cell") == t2.get("cell"),
                "cache_write_t1": cache_write,
                "cache_read_t2": cache_read,
                "cache_hit": bool(cache_read) and cache_read == cache_write,
            }
        receipt["turns"] = turns
        receipt["checks"] = checks
    finally:
        daemon.stop()
        if not args.keep_sandbox:
            sandbox.remove()

    core = [
        receipt["launch_bundle_parses"]["ok"],
        *[checks.get(name, {}).get("ok") for name in CHECK_ORDER[1:]],
    ]
    receipt["verdict"] = GREEN if all(core) else RED
    receipt["promotion_status"]["may_replace_operator_pin"] = receipt["verdict"] == GREEN
    return receipt


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir")
    parser.add_argument("--claude")
    parser.add_argument(
        "--describe",
        action="store_true",
        help="print the check list and exit; spends no turns",
    )
    parser.add_argument("--model", default=MODEL)
    parser.add_argument(
        "--effort",
        dest="efforts",
        action="append",
        help="repeatable; default low then one extra high grade",
    )
    parser.add_argument("--drain-ms", type=int, default=OPERATOR_DRAIN_MS)
    parser.add_argument("--turn-deadline-ms", type=int, default=TURN_DEADLINE_MS)
    parser.add_argument("--warm-timeout-ms", type=int, default=180_000)
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--keep-sandbox", action="store_true")
    args = parser.parse_args(arguments)
    if args.describe:
        print(describe(), end="")
        return 0
    if not args.release_dir or not args.claude:
        parser.error("--release-dir and --claude are required unless --describe")
    args.release_dir = pathlib.Path(args.release_dir)
    args.claude = pathlib.Path(args.claude)
    if args.efforts is None:
        args.efforts = ["low", "high"]
    if args.seed is None:
        args.seed = int(time.time())
    try:
        receipt = execute(args)
    except FileNotFoundError as error:
        print(f"operator-eval failed: {error}", file=sys.stderr)
        return 2
    except (MeasurementError, PromotionRefused) as error:
        print(f"operator-eval failed: {error}", file=sys.stderr)
        return 2
    text = json.dumps(portable_paths.render_document(receipt), indent=2) + "\n"
    if args.output:
        args.output.write_text(text)
    print(text, end="")
    return 0 if receipt.get("verdict") == GREEN else 1


if __name__ == "__main__":
    raise SystemExit(main())
