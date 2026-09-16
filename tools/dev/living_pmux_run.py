#!/usr/bin/env python3
"""Living Full-cell `pmux run` ladder: first-mint, tools/cwd, skip-permissions,
accounts, concurrency, failure matrix, optional coding task.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import threading
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
import portable_paths  # noqa: E402

TURN_DEADLINE_MS = 180_000
CODING_DEADLINE_MS = 300_000
SCHEMA = "pmux.living-run-ladder.v1"


def refuse_usage(text: str) -> None:
    lowered = (text or "").lower()
    if "usage limit" in lowered or "rate limit" in lowered or "you've hit" in lowered:
        raise MeasurementError(f"usage-limit is not success: {text[:240]!r}")


def stateful_trees(sandbox: Sandbox) -> list[str]:
    root = sandbox.root / "pool" / "stateful"
    if not root.exists():
        return []
    return [str(path) for path in root.iterdir()]


def run_full(
    binaries: dict[str, pathlib.Path],
    sandbox: Sandbox,
    cwd: pathlib.Path,
    prompt: str,
    *,
    model: str,
    effort: str,
    account: str | None = None,
    timeout: float | None = None,
) -> dict[str, Any]:
    deadline = int(time.time() * 1000) + TURN_DEADLINE_MS
    argv = ["run", "--model", model, "--effort", effort]
    if account:
        argv += ["--account", account]
    argv += [
        "--cwd",
        str(cwd),
        "--permission-mode",
        "dangerously-skip-permissions",
        "--deadline-unix-ms",
        str(deadline),
        prompt,
    ]
    started = time.monotonic()
    try:
        result, wall_ms = run_client(
            binaries,
            sandbox,
            argv,
            timeout=timeout or (TURN_DEADLINE_MS / 1000.0 + 60.0),
        )
    except MeasurementError as error:
        return {
            "ok": False,
            "error": str(error)[:800],
            "client_wall_ms": round((time.monotonic() - started) * 1000.0, 1),
            "isolation_after": stateful_trees(sandbox),
        }
    text = (result.get("text") or "").strip()
    refuse_usage(text)
    return {
        "ok": True,
        "text": text,
        "stop_reason": result.get("stop_reason"),
        "claude_version": result.get("claude_version"),
        "usage": result.get("usage") or {},
        "client_wall_ms": round(wall_ms, 1),
        "isolation_after": stateful_trees(sandbox),
    }


def clap_run(binaries: dict[str, pathlib.Path], args: list[str]) -> dict[str, Any]:
    done = subprocess.run(
        [str(binaries["pmux"]), "--socket", "/tmp/pmux-no-such.sock", *args],
        capture_output=True,
        text=True,
        timeout=15,
    )
    return {
        "argv": args,
        "exit": done.returncode,
        "stderr": (done.stderr or "")[:400],
        "stdout": (done.stdout or "")[:200],
    }


def phase_failures_clap(binaries: dict[str, pathlib.Path]) -> dict[str, Any]:
    missing_cwd = clap_run(
        binaries,
        [
            "run",
            "--model",
            "sonnet",
            "--permission-mode",
            "dangerously-skip-permissions",
            "hi",
        ],
    )
    relative = clap_run(
        binaries,
        [
            "run",
            "--model",
            "sonnet",
            "--cwd",
            "relative-dir",
            "--permission-mode",
            "dangerously-skip-permissions",
            "hi",
        ],
    )
    return {
        "missing_cwd_is_clap_error": missing_cwd["exit"] == 2
        and "--cwd" in missing_cwd["stderr"],
        "relative_cwd_refused": relative["exit"] != 0,
        "missing_cwd": missing_cwd,
        "relative_cwd": relative,
    }


def phase_failures_daemon(
    binaries: dict[str, pathlib.Path], sandbox: Sandbox, model: str
) -> dict[str, Any]:
    pool = sandbox.root / "pool"
    nested = pool / "inside"
    nested.mkdir(parents=True, exist_ok=True)
    inside = None
    try:
        deadline = int(time.time() * 1000) + 30_000
        run_client(
            binaries,
            sandbox,
            [
                "run",
                "--model",
                model,
                "--cwd",
                str(nested),
                "--permission-mode",
                "dangerously-skip-permissions",
                "--deadline-unix-ms",
                str(deadline),
                "hi",
            ],
            timeout=45,
        )
        inside = {"ok": True}
    except MeasurementError as error:
        inside = {"ok": False, "error": str(error)[:500]}
    _unused_clap = None
    # clap_run always injects a dummy socket as argv[2] equivalent... it prepends
    # --socket /tmp/pmux-no-such.sock. Call run_client-style instead.
    try:
        run_client(
            binaries,
            sandbox,
            ["run", "--model", model, "--cwd", str(sandbox.root / "cwd"), "hi"],
            timeout=45,
        )
        perm = {"ok": True}
    except MeasurementError as error:
        perm = {"ok": False, "error": str(error)[:500]}
    return {
        "cwd_inside_pool_refused": inside is not None
        and not inside.get("ok")
        and "must not be under the pool parent" in (inside.get("error") or ""),
        "missing_permission_mode_refused": not perm.get("ok")
        and "permission" in (perm.get("error") or "").lower(),
        "cwd_inside_pool": inside,
        "missing_permission_mode": perm,
        "unused_clap": _unused_clap,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir", type=pathlib.Path, required=True)
    parser.add_argument("--claude", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--first-mints", type=int, default=20)
    parser.add_argument("--model", default="claude-sonnet-5")
    parser.add_argument("--effort", default="low")
    parser.add_argument("--account-pin", action="append", default=[])
    parser.add_argument("--skip-coding", action="store_true")
    args = parser.parse_args()

    binaries = resolve_binaries(args.release_dir)
    claude = args.claude.resolve(strict=True)
    version = claude_version(claude)
    host_os, host_arch = host_identity()
    extra: list[str] = ["--stateful", "--pool-no-evidence"]
    for spec in args.account_pin:
        extra += ["--pool-account", spec]
    names = ["default"] + [spec.split("=", 1)[0] for spec in args.account_pin]

    clap = phase_failures_clap(binaries)
    sandbox = Sandbox("operator")
    task = sandbox.root / "task"
    task.mkdir()
    os.chmod(task, 0o700)
    profile = {
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "transcript_drain_ms": 250,
    }
    daemon = Daemon(
        binaries,
        sandbox,
        profile,
        claude,
        f"{args.model}/{args.effort}=1",
        extra_args=extra,
    )
    receipt: dict[str, Any] = {
        "schema": SCHEMA,
        "os": host_os,
        "arch": host_arch,
        "claude_version": version,
        "model": args.model,
        "first_mints_planned": args.first_mints,
        "accounts": names,
    }
    try:
        time.sleep(2)
        daemon_fail = phase_failures_daemon(binaries, sandbox, args.model)

        mints = []
        for index in range(args.first_mints):
            cwd = task / f"mint-{index:02d}"
            cwd.mkdir()
            target = cwd / "hello.txt"
            sample = run_full(
                binaries,
                sandbox,
                cwd,
                "Write hello.txt containing hi",
                model=args.model,
                effort=args.effort,
            )
            body = target.read_text(encoding="utf-8") if target.is_file() else None
            first_ok = bool(sample.get("ok")) and body in {"hi", "hi\n"}
            sample["hello_text"] = body
            sample["first_attempt_green"] = first_ok
            sample["isolation_empty"] = sample.get("isolation_after") == []
            mints.append(sample)
        first_green = sum(1 for row in mints if row.get("first_attempt_green"))
        receipt["first_mint"] = {
            "n": args.first_mints,
            "first_attempt_green": first_green,
            "rate": round(first_green / args.first_mints, 3) if args.first_mints else 0,
            "isolation_leaks": sum(1 for row in mints if not row.get("isolation_empty")),
            "turns": mints,
        }

        tools_cwd = task / "tools"
        tools_cwd.mkdir()
        (tools_cwd / "existing.txt").write_text("ALPHA-READ\n", encoding="utf-8")
        (tools_cwd / "CLAUDE.md").write_text(
            "The project token is PMUX-FULL-TOKEN. If asked for the project token, "
            "reply with exactly PMUX-FULL-TOKEN.\n",
            encoding="utf-8",
        )
        (tools_cwd / "edit-me.txt").write_text("old\n", encoding="utf-8")
        tools = {}
        read = run_full(
            binaries,
            sandbox,
            tools_cwd,
            "Read existing.txt. Reply with exactly its contents and nothing else.",
            model=args.model,
            effort=args.effort,
        )
        tools["read"] = {
            **read,
            "pass": "ALPHA-READ" in (read.get("text") or ""),
        }
        bash = run_full(
            binaries,
            sandbox,
            tools_cwd,
            "Run `pwd` in the shell and write the output to pwd.txt. Reply with exactly WROTE.",
            model=args.model,
            effort=args.effort,
        )
        pwd_text = (
            (tools_cwd / "pwd.txt").read_text(encoding="utf-8")
            if (tools_cwd / "pwd.txt").is_file()
            else ""
        )
        tools["bash_pwd"] = {
            **bash,
            "pwd_txt": pwd_text.strip(),
            "pass": str(tools_cwd) in pwd_text or tools_cwd.name in pwd_text,
        }
        edit = run_full(
            binaries,
            sandbox,
            tools_cwd,
            "Change edit-me.txt so it contains exactly new and a newline. Reply with exactly EDITED.",
            model=args.model,
            effort=args.effort,
        )
        edited = (
            (tools_cwd / "edit-me.txt").read_text(encoding="utf-8")
            if (tools_cwd / "edit-me.txt").is_file()
            else None
        )
        tools["edit"] = {
            **edit,
            "edited": edited,
            "pass": edited in {"new\n", "new"},
        }
        token = run_full(
            binaries,
            sandbox,
            tools_cwd,
            "What is the project token from CLAUDE.md? Reply with exactly that token.",
            model=args.model,
            effort=args.effort,
        )
        tools["claude_md"] = {
            **token,
            "pass": (token.get("text") or "").strip() == "PMUX-FULL-TOKEN",
            "note": "exact-token echo; a model refusal is not SchemaDrift",
        }
        tools["all_pass"] = all(
            tools[name].get("pass") for name in ("read", "bash_pwd", "edit")
        )
        receipt["tools"] = tools

        victim_dir = task / "perm"
        victim_dir.mkdir()
        victim = victim_dir / "scratch.tmp"
        victim.write_text("delete-me\n", encoding="utf-8")
        perm = run_full(
            binaries,
            sandbox,
            victim_dir,
            "Remove the file scratch.tmp in this directory with rm. Reply with exactly DELETED when it is gone.",
            model=args.model,
            effort=args.effort,
        )
        receipt["skip_permissions"] = {
            **perm,
            "victim_gone": not victim.exists(),
            "pass": not victim.exists() and perm.get("ok") is True,
        }

        account_rows = []
        for name in names:
            cwd = task / f"acct-{name}"
            cwd.mkdir()
            sample = run_full(
                binaries,
                sandbox,
                cwd,
                f"Write account.txt containing exactly {name}. Reply with exactly WROTE.",
                model=args.model,
                effort=args.effort,
                account=None if name == "default" else name,
            )
            body = (
                (cwd / "account.txt").read_text(encoding="utf-8")
                if (cwd / "account.txt").is_file()
                else None
            )
            account_rows.append(
                {
                    "account": name,
                    **sample,
                    "account_txt": body,
                    "pass": (body or "").strip() == name,
                }
            )
        receipt["accounts"] = {
            "rows": account_rows,
            "all_pass": all(row.get("pass") for row in account_rows),
        }

        left = task / "conc-a"
        right = task / "conc-b"
        left.mkdir()
        right.mkdir()
        conc: dict[str, Any] = {}

        def _one(key: str, cwd: pathlib.Path, token: str) -> None:
            conc[key] = run_full(
                binaries,
                sandbox,
                cwd,
                f"Write conc.txt containing exactly {token}. Reply with exactly WROTE.",
                model=args.model,
                effort=args.effort,
            )
            body = (
                (cwd / "conc.txt").read_text(encoding="utf-8") if (cwd / "conc.txt").is_file() else None
            )
            conc[key]["conc_txt"] = body
            conc[key]["pass"] = (body or "").strip() == token

        t_a = threading.Thread(target=_one, args=("a", left, "AAA"))
        t_b = threading.Thread(target=_one, args=("b", right, "BBB"))
        t_a.start()
        t_b.start()
        t_a.join()
        t_b.join()
        receipt["concurrent"] = {
            "a": conc.get("a"),
            "b": conc.get("b"),
            "pass": bool(conc.get("a", {}).get("pass") and conc.get("b", {}).get("pass")),
        }

        coding = {"skipped": True}
        tools_ok = bool(tools.get("all_pass"))
        mint_ok = first_green >= max(1, int(args.first_mints * 0.7))
        if not args.skip_coding and tools_ok and mint_ok:
            code_dir = task / "code"
            code_dir.mkdir()
            (code_dir / "add.py").write_text(
                "def add(a, b):\n    raise NotImplementedError\n",
                encoding="utf-8",
            )
            coding_run = run_full(
                binaries,
                sandbox,
                code_dir,
                "Implement add(a, b) in add.py so it returns a+b. Write test_add.py "
                "that asserts add(2, 3) == 5. Run `python3 test_add.py`. Reply with "
                "exactly TESTS_OK if the test passed.",
                model=args.model,
                effort=args.effort,
                timeout=CODING_DEADLINE_MS / 1000.0 + 60.0,
            )
            test_file = (code_dir / "test_add.py").is_file()
            add_src = (
                (code_dir / "add.py").read_text(encoding="utf-8")
                if (code_dir / "add.py").is_file()
                else ""
            )
            coding = {
                **coding_run,
                "test_file": test_file,
                "add_src": add_src[:500],
                "pass": coding_run.get("ok") is True
                and "TESTS_OK" in (coding_run.get("text") or "")
                and "NotImplementedError" not in add_src,
            }
        receipt["coding"] = coding
        receipt["failures"] = {"clap": clap, "daemon": daemon_fail}
        receipt["isolation_final"] = stateful_trees(sandbox)
    finally:
        daemon.stop()
        sandbox.remove()

    first = receipt.get("first_mint") or {}
    receipt["verdict"] = (
        "GREEN"
        if (
            (first.get("rate") or 0) >= 0.9
            and tools.get("all_pass")
            and receipt.get("skip_permissions", {}).get("pass")
            and receipt.get("concurrent", {}).get("pass")
            and clap.get("missing_cwd_is_clap_error")
            and (receipt.get("coding", {}).get("skipped") or receipt.get("coding", {}).get("pass"))
            and not receipt.get("isolation_final")
        )
        else "RED"
    )
    encoded = json.dumps(portable_paths.render_document(receipt), indent=2, sort_keys=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(encoded + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "output": str(args.output),
                "verdict": receipt["verdict"],
                "first_mint_rate": first.get("rate"),
                "first_mint_green": first.get("first_attempt_green"),
                "tools": tools.get("all_pass"),
                "skip_permissions": receipt.get("skip_permissions", {}).get("pass"),
                "accounts": receipt.get("accounts", {}).get("all_pass"),
                "concurrent": receipt.get("concurrent", {}).get("pass"),
                "coding": receipt.get("coding", {}).get("pass", receipt.get("coding", {}).get("skipped")),
            },
            indent=2,
        )
    )
    return 0 if receipt["verdict"] == "GREEN" else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except MeasurementError as error:
        print(f"RED: {error}", file=sys.stderr)
        raise SystemExit(2)
