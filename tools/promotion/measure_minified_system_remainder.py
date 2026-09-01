#!/usr/bin/env python3
"""Usage-infer what a minified TUI cell still bills besides REPLACE + user prompt.

Public Claude Code does not emit `[API REQUEST DETAIL]` on `--debug-file`, so
this is not a dump of the API body. It is two TUI `pmux run` turns (cold, then
after `/clear`) plus an optional process-log census. `claude --print` is a
different API shape and is refused here if cache_creation is non-zero.

Usage:

    python3 tools/promotion/measure_minified_system_remainder.py \\
        --release-dir target/debug \\
        --claude "$HOME/.local/share/pmux/claude/2.1.236/claude" \\
        --output evidence/linux-minified-system-remainder-2.1.236-x86_64.json
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import shlex
import stat
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "evidence_common"))

import portable_paths
from measure_turn_latency import (
    Daemon,
    MeasurementError,
    Sandbox,
    claude_version,
    host_identity,
    resolve_binaries,
    run_client,
)

SCHEMA = "pmux.minified-system-remainder.v1"
USER_PROMPT = "Reply with exactly the word OK and nothing else."
CONFIG_RS = (
    pathlib.Path(__file__).resolve().parents[2]
    / "crates"
    / "service"
    / "src"
    / "pool"
    / "config.rs"
)
DEFAULT_SYSTEM_PROMPT_RE = re.compile(
    r'pub const DEFAULT_SYSTEM_PROMPT: &str = "([^"]+)";'
)
CENSUS_KEYS = (
    "idle",
    "live",
    "clearing",
    "tearing_down",
    "registered_instances",
    "in_flight",
    "leaked",
    "halted",
)


def default_system_prompt_from_rust(source: str) -> str:
    match = DEFAULT_SYSTEM_PROMPT_RE.search(source)
    if match is None:
        raise MeasurementError("could not parse DEFAULT_SYSTEM_PROMPT from config.rs")
    return match.group(1)


def load_displacer() -> str:
    return default_system_prompt_from_rust(CONFIG_RS.read_text(encoding="utf-8"))


def billed_input(main_usage: dict) -> int:
    """Prefix the model billed: tail + cache write + cache read.

    `input_tokens` alone collapses when the mass moves into cache_creation
    (pool_concurrency.rs S-45: a long prompt reported input=2 cache_creation=1214).
    """

    def need(name: str) -> int:
        value = main_usage.get(name)
        if not isinstance(value, int):
            raise MeasurementError(f"usage.main.{name} is not an int: {value!r}")
        return value

    return (
        need("input_tokens")
        + need("cache_creation_input_tokens")
        + need("cache_read_input_tokens")
    )


def chars_over_4(text: str) -> int:
    return max(1, round(len(text) / 4))


def compact_census(census: dict) -> dict[str, object]:
    return {key: census.get(key) for key in CENSUS_KEYS}


def refuse_destroyed_floor(post: dict) -> None:
    """A drained floor after the cold turn is destroy, not `/clear` recycle.

    The next `pmux run` would mint a new epoch. Do not publish that as after_clear.
    """

    if "error" in post:
        raise MeasurementError(
            "post-cold census failed; cannot prove after_clear: "
            f"{post.get('error')}"
        )
    live = int(post.get("live") or 0)
    clearing = int(post.get("clearing") or 0)
    tearing = int(post.get("tearing_down") or 0)
    if tearing >= 1:
        raise MeasurementError(
            "tearing_down after the cold turn; a second pmux run would be "
            f"a remint, not after_clear: {post}"
        )
    if live == 0 and clearing == 0:
        raise MeasurementError(
            "slot destroyed after the cold turn; a second pmux run would be "
            f"a remint, not after_clear: {post}"
        )


def extract_debug(debug_text: str) -> dict[str, object]:
    lines = debug_text.splitlines()
    tags: dict[str, int] = {}
    api_sources: list[str] = []
    notes: list[str] = []
    for line in lines:
        match = re.search(r"\[DEBUG\] \[([^\]]+)\]", line)
        if match:
            tags[match.group(1)] = tags.get(match.group(1), 0) + 1
        source = re.search(r"source=([A-Za-z0-9_]+)", line)
        if "API REQUEST" in line and source:
            api_sources.append(source.group(1))
        if (
            "No CLAUDE.md" in line
            or "Git remote URL" in line
            or "No git remote" in line
        ):
            snippet = line.split(" [DEBUG] ", 1)[-1][:240]
            notes.append(re.sub(r"/tmp/[^ ]+", "<TMPDIR>", snippet))
    return {
        "line_count": len(lines),
        "bytes": len(debug_text.encode("utf-8")),
        "process_side_debug_tags": dict(sorted(tags.items())),
        "process_side_only": True,
        "api_sources": api_sources,
        "api_request_detail_emitted": any(
            "API REQUEST DETAIL" in line for line in lines
        ),
        "json_bodies_found": 0,
        "bodies_classified": [],
        "notes": notes[:20],
        "notes_are_process_log_not_model_visible": True,
    }


def write_wrapper(path: pathlib.Path, real: pathlib.Path, debug_file: pathlib.Path) -> None:
    path.write_text(
        "#!/bin/bash\n"
        f"exec {shlex.quote(str(real))} --debug-file {shlex.quote(str(debug_file))} \"$@\"\n",
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def pool_census(binaries: dict, sandbox: Sandbox) -> dict:
    done = subprocess.run(
        [
            str(binaries["pmux"]),
            "--socket",
            str(sandbox.socket),
            "--output",
            "json",
            "doctor",
        ],
        env=sandbox.environment(),
        capture_output=True,
        text=True,
        timeout=60,
    )
    payloads = [line for line in done.stdout.splitlines() if line.startswith("{")]
    if not payloads:
        raise MeasurementError(f"doctor printed no JSON:\n{done.stdout}\n{done.stderr}")
    report = json.loads(payloads[-1])
    layers = (report.get("diagnosis") or {}).get("layers") or []
    pool = next((layer for layer in layers if layer.get("layer") == "pool"), None)
    if pool is None:
        raise MeasurementError("doctor has no pool layer")
    census = pool.get("evidence") or {}
    if census.get("halted"):
        raise MeasurementError(f"pool halted: {census['halted']}")
    return census


def wait_idle(binaries: dict, sandbox: Sandbox, seconds: float) -> dict[str, object]:
    """Wait until a warm instance is idle. Used only before the first turn.

    After a turn, do not wait on doctor: linux 2.1.236 replaced the Claude pid
    across `/clear` (a refused `/clear` plus remint, fixed 2026-09-01), so a census can read live=0 while recycle or remint is
    in flight. The next `pmux run` is the product wait (`admit` on Clearing
    or a free slot).
    """

    deadline = time.monotonic() + seconds
    last: dict[str, object] = {}
    while time.monotonic() < deadline:
        census = pool_census(binaries, sandbox)
        last = compact_census(census)
        if int(census.get("idle") or 0) >= 1:
            return last
        time.sleep(0.4)
    raise MeasurementError(f"warm instance never reached idle; last={last}")


def summarize_turn(
    result: dict, wall_ms: float, label: str, displacer: str
) -> dict[str, object]:
    usage = result.get("usage") or {}
    main_usage = usage.get("main")
    if not isinstance(main_usage, dict):
        raise MeasurementError(f"{label} usage.main is missing")
    side = usage.get("sidechain") if isinstance(usage.get("sidechain"), dict) else {}
    combined = usage.get("combined") if isinstance(usage.get("combined"), dict) else {}
    if result.get("text", "").strip() != "OK":
        raise MeasurementError(f"{label} text was {result.get('text')!r}, not OK")
    billed = billed_input(main_usage)
    cache_creation = main_usage["cache_creation_input_tokens"]
    if cache_creation != 0:
        raise MeasurementError(
            f"{label} billed cache_creation={cache_creation}; this is not the TUI "
            "minified shape (cache_creation must be 0). Do not mix with --print."
        )
    for key in (
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ):
        value = side.get(key, 0)
        if isinstance(value, int) and value != 0:
            raise MeasurementError(
                f"{label} usage.sidechain.{key}={value}; a minified cell "
                "must not carry a sidechain row"
            )
    displacer_est = chars_over_4(displacer)
    user_est = chars_over_4(USER_PROMPT)
    return {
        "label": label,
        "wall_ms": round(wall_ms, 1),
        "text": result.get("text"),
        "stop_reason": result.get("stop_reason"),
        "input_tokens": main_usage.get("input_tokens"),
        "output_tokens": main_usage.get("output_tokens"),
        "cache_creation_input_tokens": cache_creation,
        "cache_read_input_tokens": main_usage.get("cache_read_input_tokens"),
        "sidechain": side,
        "combined": combined,
        "billed_input_tokens": billed,
        "displacer_tokens_est_chars_over_4": displacer_est,
        "user_prompt_tokens_est_chars_over_4": user_est,
        "remainder_tokens_est_chars_over_4": max(0, billed - displacer_est - user_est),
        "remainder_method": (
            "billed_input_tokens - round(len(displacer)/4) - round(len(user)/4)"
        ),
    }


def scrub_sandbox_paths(text: str) -> str:
    return re.sub(
        r"/tmp/qv4-turn-latency-[^/\s\"']+",
        "<TMPDIR>/qv4-turn-latency-<SANDBOX>",
        text,
    )


def encode_receipt(receipt: dict[str, object]) -> str:
    encoded = (
        json.dumps(portable_paths.render_document(receipt), indent=1, sort_keys=True)
        + "\n"
    )
    encoded = scrub_sandbox_paths(encoded)
    leaked = portable_paths.offences(encoded, portable_paths.machine_identifiers())
    if leaked:
        raise MeasurementError(
            "receipt still carries machine identifiers: " + "; ".join(leaked[:5])
        )
    return encoded


def run_campaign(
    binaries: dict,
    claude: pathlib.Path,
    profile: dict,
    displacer: str,
    wrap_debug: bool,
    keep_sandbox: bool,
) -> dict[str, object]:
    sandbox = Sandbox("operator")
    debug_file = sandbox.root / "claude-debug.txt"
    child = claude
    if wrap_debug:
        wrapper = sandbox.root / "claude-wrapper"
        write_wrapper(wrapper, claude, debug_file)
        child = wrapper
    receipt: dict[str, object] = {
        "schema": SCHEMA,
        "kind": "minified_system_remainder",
        "claude_version": profile["claude_version"],
        "os": profile["os"],
        "arch": profile["arch"],
        "displacer": displacer,
        "user_prompt": USER_PROMPT,
        "minified_cell_flags_unchanged": True,
        "debug_file_prepended_by_wrapper": wrap_debug,
        "note": (
            "TUI minified cell through pmux run. Remainder is usage-inferred; "
            "public --debug-file does not dump API bodies. Process debug tags "
            "are not model-visible tokens."
        ),
        "what_would_invalidate_it": [
            "cache_creation_input_tokens != 0 (print-shaped surface, not TUI)",
            "text != OK",
            "dropping REPLACE, which restores Claude Code's default agent prompt",
        ],
    }
    daemon: Daemon | None = None
    try:
        daemon = Daemon(
            binaries,
            sandbox,
            profile,
            child,
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
        try:
            receipt["post_cold_census"] = compact_census(pool_census(binaries, sandbox))
        except MeasurementError as error:
            receipt["post_cold_census"] = {"error": str(error)}
        if isinstance(receipt["post_cold_census"], dict):
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
        if wrap_debug:
            if not debug_file.is_file():
                raise MeasurementError("claude --debug-file was not written")
            extracted = extract_debug(
                debug_file.read_text(encoding="utf-8", errors="replace")
            )
            if "repl_main_thread" not in extracted["api_sources"]:
                raise MeasurementError(
                    "debug api_sources missing repl_main_thread: "
                    f"{extracted['api_sources']}"
                )
            receipt["debug"] = extracted
        else:
            receipt["debug"] = {
                "skipped": (
                    "wrapper omitted so /clear recycle is the product path; "
                    "remainder is still usage-inferred from StatelessResult"
                )
            }
        receipt["usage_interpretation"] = {
            "repl_main_thread_is_the_armed_turn": True,
            "quota_check_is_startup_not_in_usage": True,
            "generate_session_title_is_a_separate_api_call_tokens_unknown": True,
            "api_body_dump": (
                "[API REQUEST DETAIL] is not emitted on this public linux "
                "binary at --debug-file; remainder is usage-inferred, not dumped as text"
            ),
            "hundreds_not_29k": (
                "after_clear.remainder_tokens_est_chars_over_4 is a chars/4 lower "
                "bound on leftover Claude Code envelope after displacer+user prompt, "
                "not a tokenizer remainder and not the 29k tool surface"
            ),
        }
        return receipt
    finally:
        if daemon is not None:
            daemon.stop()
        if not keep_sandbox:
            sandbox.remove()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir", type=pathlib.Path, required=True)
    parser.add_argument("--claude", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--keep-sandbox", action="store_true")
    parser.add_argument(
        "--no-debug-file",
        action="store_true",
        help="Do not prepend --debug-file; still a TUI pmux run remainder.",
    )
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
    wrap = not args.no_debug_file
    receipt: dict[str, object]
    try:
        receipt = run_campaign(
            binaries, claude, profile, displacer, wrap, args.keep_sandbox
        )
        encoded = encode_receipt(receipt)
    except Exception as error:
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
        failed = {
            "schema": SCHEMA,
            "kind": "minified_system_remainder",
            "claude_version": version,
            "os": host_os,
            "arch": host_arch,
            "displacer": displacer,
            "user_prompt": USER_PROMPT,
            "error": str(error),
        }
        encoded = encode_receipt(failed)
        args.output.write_text(encoded, encoding="utf-8")
        print(encoded, end="")
        return 1
    args.output.write_text(encoded, encoding="utf-8")
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
