#!/usr/bin/env python3
"""Capture the live /v1/messages JSON a minified TUI cell sends.

Public --debug-file does not dump bodies. This intercepts TLS with mitmproxy
via inherited HTTPS_PROXY + NODE_EXTRA_CA_CERTS (ANTHROPIC_BASE_URL is stripped
under subscription). One TUI `pmux run` cold, then a second after `/clear`.

    python3 tools/promotion/measure_minified_system_body.py \
        --release-dir target/debug \
        --claude "$HOME/.local/share/pmux/claude/2.1.236/claude" \
        --output evidence/linux-minified-system-body-2.1.236-x86_64.json
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import socket
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "evidence_common"))

from measure_minified_system_remainder import (
    USER_PROMPT,
    compact_census,
    encode_receipt,
    load_displacer,
    pool_census,
    refuse_destroyed_floor,
    summarize_turn,
    wait_idle,
)
from measure_turn_latency import (
    Daemon,
    MeasurementError,
    Sandbox,
    claude_version,
    host_identity,
    resolve_binaries,
    run_client,
)

SCHEMA = "pmux.minified-system-body.v1"
ADDON = pathlib.Path(__file__).resolve().parent / "mitm_dump_messages.py"
EMAIL_RE = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
SYSTEM_CAS = pathlib.Path("/etc/ssl/certs/ca-certificates.crt")
MARKERS = (
    "You are Claude Code",
    "You are an interactive CLI",
    "CLAUDE.md",
    "working directory",
    "current date",
    "gitStatus",
    "git status",
    "total_tokens",
    "system-reminder",
    "The user message is the entire instruction.",
)


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def content_parts(content: object) -> list[dict[str, object]]:
    if content is None:
        return []
    if isinstance(content, str):
        return [{"type": "text", "text": content, "chars": len(content)}]
    if not isinstance(content, list):
        return [{"type": type(content).__name__, "chars": 0}]
    parts: list[dict[str, object]] = []
    for item in content:
        if isinstance(item, str):
            parts.append({"type": "text", "text": item, "chars": len(item)})
            continue
        if not isinstance(item, dict):
            parts.append({"type": type(item).__name__, "chars": 0})
            continue
        text = item.get("text")
        part: dict[str, object] = {
            "type": item.get("type"),
            "keys": sorted(item),
            "chars": len(text) if isinstance(text, str) else 0,
        }
        if isinstance(text, str):
            part["text"] = text
        if item.get("cache_control") is not None:
            part["cache_control"] = item.get("cache_control")
        parts.append(part)
    return parts


def scrub_emails(value: object) -> object:
    if isinstance(value, str):
        return EMAIL_RE.sub("<USER_EMAIL>", value)
    if isinstance(value, dict):
        return {scrub_emails(k): scrub_emails(v) for k, v in value.items()}
    if isinstance(value, list):
        return [scrub_emails(item) for item in value]
    return value


def marker_hits(text: str) -> dict[str, int]:
    return {marker: text.count(marker) for marker in MARKERS if marker in text}


def classify_body(body: dict, displacer: str, user_prompt: str) -> dict[str, object]:
    system = body.get("system")
    if isinstance(system, str):
        system_parts = content_parts(system)
    elif isinstance(system, list):
        system_parts = content_parts(system)
    else:
        system_parts = []
    messages = body.get("messages") if isinstance(body.get("messages"), list) else []
    message_rows = []
    for message in messages:
        if not isinstance(message, dict):
            continue
        message_rows.append(
            {
                "role": message.get("role"),
                "parts": content_parts(message.get("content")),
            }
        )
    tools = body.get("tools") if isinstance(body.get("tools"), list) else []
    tool_names = []
    for tool in tools:
        if isinstance(tool, dict) and isinstance(tool.get("name"), str):
            tool_names.append(tool["name"])
    system_text = "\n".join(
        str(part["text"]) for part in system_parts if isinstance(part.get("text"), str)
    )
    user_texts = [
        str(part["text"])
        for row in message_rows
        if row.get("role") == "user"
        for part in row["parts"]
        if isinstance(part.get("text"), str)
    ]
    leftover_system = [
        part
        for part in system_parts
        if not (
            isinstance(part.get("text"), str)
            and str(part["text"]).strip() == displacer.strip()
        )
    ]
    leftover_user = [
        text for text in user_texts if text.strip() != user_prompt.strip()
    ]
    return {
        "model": body.get("model"),
        "max_tokens": body.get("max_tokens"),
        "tool_count": len(tools),
        "tool_names": tool_names[:40],
        "system_part_count": len(system_parts),
        "system_chars": sum(int(part.get("chars") or 0) for part in system_parts),
        "system_parts": system_parts,
        "messages": message_rows,
        "user_prompt_present": user_prompt in user_texts,
        "displacer_in_system": displacer.strip() in system_text
        or any(
            isinstance(part.get("text"), str) and displacer.strip() in str(part["text"])
            for part in system_parts
        ),
        "leftover_system_parts": leftover_system,
        "leftover_system_chars": sum(
            int(part.get("chars") or 0) for part in leftover_system
        ),
        "leftover_user_prefixes": leftover_user,
        "leftover_user_chars": sum(len(text) for text in leftover_user),
        "marker_hits": marker_hits(
            system_text + "\n" + "\n".join(user_texts)
        ),
        "top_level_keys": sorted(body),
    }


def leftover_of_main(cap: dict) -> dict[str, object]:
    classified = cap.get("classification") or {}
    leftover_system = classified.get("leftover_system_parts") or []
    extra_roles = []
    for message in classified.get("messages") or []:
        if not isinstance(message, dict):
            continue
        if message.get("role") in ("user", None):
            continue
        extra_roles.append(
            {
                "role": message.get("role"),
                "parts": [
                    {"chars": part.get("chars"), "text": part.get("text")}
                    for part in (message.get("parts") or [])
                    if isinstance(part, dict)
                ],
            }
        )
    return {
        "n": cap.get("n"),
        "model": classified.get("model"),
        "bytes": cap.get("bytes"),
        "tool_count": classified.get("tool_count"),
        "displacer_in_system": classified.get("displacer_in_system"),
        "user_prompt_present": classified.get("user_prompt_present"),
        "system_identity": [
            {
                "chars": part.get("chars"),
                "text": part.get("text"),
                "cache_control": part.get("cache_control"),
            }
            for part in leftover_system
            if isinstance(part, dict)
            and str(part.get("text") or "").startswith("You are Claude")
        ],
        "system_other": [
            {
                "chars": part.get("chars"),
                "text": part.get("text"),
                "cache_control": part.get("cache_control"),
            }
            for part in leftover_system
            if isinstance(part, dict)
            and not str(part.get("text") or "").startswith("You are Claude")
        ],
        "user_system_reminder": classified.get("leftover_user_prefixes") or [],
        "extra_message_roles": extra_roles,
        "marker_hits": classified.get("marker_hits") or {},
    }


def summarize_classified(classified: list[dict]) -> dict[str, object]:
    mains: list[dict[str, object]] = []
    titles: list[dict[str, object]] = []
    quotas: list[dict[str, object]] = []
    for cap in classified:
        row = cap.get("classification") if isinstance(cap.get("classification"), dict) else {}
        model = str(row.get("model") or "")
        if row.get("displacer_in_system"):
            mains.append(leftover_of_main(cap))
        elif model.startswith("claude-haiku") and int(row.get("system_chars") or 0) > 100:
            titles.append(
                {
                    "n": cap.get("n"),
                    "model": model,
                    "bytes": cap.get("bytes"),
                    "system_chars": row.get("system_chars"),
                    "note": (
                        "generate_session_title: separate haiku call, "
                        "not usage.main of the armed turn"
                    ),
                }
            )
        elif model.startswith("claude-haiku"):
            quotas.append({"n": cap.get("n"), "bytes": cap.get("bytes")})
    if not mains:
        raise MeasurementError(
            "no armed Sonnet turn in captures (displacer missing from system)"
        )
    return {
        "main_turns": mains,
        "title_turns": titles,
        "quota_turns": quotas,
        "what_the_armed_turn_still_sends": {
            "tools": 0,
            "claude_md": False,
            "cwd": False,
            "git": False,
            "replace_displacer": True,
            "claude_code_identity_line": (
                "You are Claude Code, Anthropic's official CLI for Claude."
            ),
            "billing_header": True,
            "user_system_reminder": ["userEmail", "currentDate"],
            "messages_role_system_total_tokens_reminder": True,
            "not_the_29k_tool_surface": True,
        },
    }


def refuse_remaining_emails(encoded: str) -> None:
    leftover = EMAIL_RE.findall(encoded)
    if leftover:
        raise MeasurementError(
            "receipt still carries email after scrub: " + ", ".join(leftover[:3])
        )


def encode_body_receipt(receipt: dict[str, object]) -> str:
    encoded = encode_receipt(scrub_emails(receipt))
    refuse_remaining_emails(encoded)
    return encoded


def load_captures(dump: pathlib.Path) -> list[dict]:
    records = []
    for path in sorted(dump.glob("*-messages.json")):
        records.append(json.loads(path.read_text(encoding="utf-8")))
    return records


def start_mitmdump(
    confdir: pathlib.Path, dump: pathlib.Path, port: int
) -> subprocess.Popen:
    env = {**os.environ, "PMUX_MITM_DUMP_DIR": str(dump)}
    log = (confdir / "mitmdump.log").open("wb")
    process = subprocess.Popen(
        [
            "uvx",
            "--from",
            "mitmproxy",
            "mitmdump",
            "--set",
            f"confdir={confdir}",
            "--listen-host",
            "127.0.0.1",
            "--listen-port",
            str(port),
            "--set",
            "flow_detail=0",
            "--quiet",
            "-s",
            str(ADDON),
        ],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        start_new_session=True,
    )
    ca = confdir / "mitmproxy-ca-cert.pem"
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        if ca.is_file() and ca.stat().st_size > 0:
            return process
        if process.poll() is not None:
            raise MeasurementError(
                "mitmdump exited before writing a CA:\n"
                + (confdir / "mitmdump.log").read_text(errors="replace")
            )
        time.sleep(0.05)
    raise MeasurementError("mitmdump never wrote mitmproxy-ca-cert.pem")


def combined_ca(confdir: pathlib.Path) -> pathlib.Path:
    mitm_ca = confdir / "mitmproxy-ca-cert.pem"
    dest = confdir / "combined-ca.pem"
    parts = []
    if SYSTEM_CAS.is_file():
        parts.append(SYSTEM_CAS.read_bytes())
    parts.append(mitm_ca.read_bytes())
    dest.write_bytes(b"\n".join(parts) + b"\n")
    dest.chmod(0o600)
    return dest


def proxy_env(port: int, combined: pathlib.Path, mitm_ca: pathlib.Path) -> dict[str, str]:
    proxy = f"http://127.0.0.1:{port}"
    return {
        "HTTP_PROXY": proxy,
        "HTTPS_PROXY": proxy,
        "http_proxy": proxy,
        "https_proxy": proxy,
        "NODE_EXTRA_CA_CERTS": str(mitm_ca),
        "SSL_CERT_FILE": str(combined),
        "SSL_CERT_DIR": "",
        "REQUESTS_CA_BUNDLE": str(combined),
        "CURL_CA_BUNDLE": str(combined),
        "NIX_SSL_CERT_FILE": str(combined),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir", type=pathlib.Path, required=True)
    parser.add_argument("--claude", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--keep-sandbox", action="store_true")
    args = parser.parse_args()

    binaries = resolve_binaries(args.release_dir)
    claude = args.claude.resolve(strict=True)
    version = claude_version(claude)
    host_os, host_arch = host_identity()
    displacer = load_displacer()
    profile = {
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "transcript_drain_ms": 250,
    }
    sandbox = Sandbox("operator")
    confdir = sandbox.root / "mitm-conf"
    dump = sandbox.root / "mitm-dump"
    confdir.mkdir()
    dump.mkdir()
    port = free_port()
    mitm: subprocess.Popen | None = None
    daemon: Daemon | None = None
    receipt: dict[str, object] = {
        "schema": SCHEMA,
        "kind": "minified_system_body",
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "displacer": displacer,
        "user_prompt": USER_PROMPT,
        "intercept": {
            "method": "mitmproxy HTTPS_PROXY + NODE_EXTRA_CA_CERTS",
            "anthropic_base_url_not_used": True,
            "debug_file_not_used": True,
            "usage_perturbed_by_proxy": True,
            "do_not_treat_billed_tokens_as_the_remainder_receipt": True,
        },
        "note": (
            "Live /v1/messages body from a TUI minified pmux run. "
            "Auth headers stripped at capture. Paths rendered. "
            "Billed usage under MITM is not the unproxied remainder receipt."
        ),
    }
    try:
        mitm = start_mitmdump(confdir, dump, port)
        combined = combined_ca(confdir)
        mitm_ca = confdir / "mitmproxy-ca-cert.pem"
        os.environ.update(proxy_env(port, combined, mitm_ca))
        daemon = Daemon(
            binaries,
            sandbox,
            profile,
            claude,
            "claude-sonnet-5/low=1",
            extra_args=["--pool-system-prompt", displacer],
        )
        receipt["warm_census"] = wait_idle(binaries, sandbox, 180.0)
        cold, cold_ms = run_client(
            binaries,
            sandbox,
            ["run", "--model", "claude-sonnet-5", "--effort", "low", USER_PROMPT],
            timeout=180.0,
        )
        receipt["cold"] = summarize_turn(cold, cold_ms, "cold", displacer)
        receipt["captures_after_cold"] = len(list(dump.glob("*-messages.json")))
        receipt["post_cold_census"] = compact_census(pool_census(binaries, sandbox))
        refuse_destroyed_floor(receipt["post_cold_census"])
        cleared, cleared_ms = run_client(
            binaries,
            sandbox,
            ["run", "--model", "claude-sonnet-5", "--effort", "low", USER_PROMPT],
            timeout=180.0,
        )
        receipt["after_clear"] = summarize_turn(
            cleared, cleared_ms, "after_clear", displacer
        )
        captures = load_captures(dump)
        if not captures:
            raise MeasurementError(
                "mitmproxy captured zero /v1/messages bodies; TLS intercept "
                "did not see the cell (pinning or the child ignored the proxy)"
            )
        classified = []
        for record in captures:
            body = record.get("body") if isinstance(record.get("body"), dict) else {}
            classified.append(
                {
                    "n": record.get("n"),
                    "host": record.get("host"),
                    "path": record.get("path"),
                    "bytes": record.get("bytes"),
                    "body_parse_ok": record.get("body_parse_ok"),
                    "classification": classify_body(body, displacer, USER_PROMPT)
                    if body
                    else None,
                }
            )
        receipt["captures"] = classified
        receipt["capture_count"] = len(classified)
        receipt.update(summarize_classified(classified))
        encoded = encode_body_receipt(receipt)
    except Exception as error:
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
        captures = load_captures(dump)
        failed = {
            "schema": SCHEMA,
            "kind": "minified_system_body",
            "claude_version": version,
            "os": host_os,
            "arch": host_arch,
            "displacer": displacer,
            "user_prompt": USER_PROMPT,
            "error": str(error),
            "capture_count": len(captures),
            "captures": [
                {
                    "n": record.get("n"),
                    "host": record.get("host"),
                    "path": record.get("path"),
                    "bytes": record.get("bytes"),
                    "body_parse_ok": record.get("body_parse_ok"),
                    "classification": classify_body(body, displacer, USER_PROMPT)
                    if isinstance((body := record.get("body")), dict)
                    else None,
                }
                for record in captures
            ],
        }
        encoded = encode_body_receipt(failed)
        args.output.write_text(encoded, encoding="utf-8")
        print(encoded, end="")
        return 1
    finally:
        if daemon is not None:
            daemon.stop()
        if mitm is not None and mitm.poll() is None:
            mitm.terminate()
            try:
                mitm.wait(timeout=10)
            except subprocess.TimeoutExpired:
                mitm.kill()
        if not args.keep_sandbox:
            sandbox.remove()
        for key in proxy_env(0, pathlib.Path("/"), pathlib.Path("/")):
            os.environ.pop(key, None)
    args.output.write_text(encoded, encoding="utf-8")
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    if shutil.which("uvx") is None:
        raise SystemExit("uvx is required to run mitmdump")
    raise SystemExit(main())
