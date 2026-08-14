"""Actual-daemon public-client exercise invoked by the Rust release E2E harness."""

from __future__ import annotations

import hashlib
import importlib
import json
import os
import socket
import struct
import sys
import threading
import time
import uuid
from pathlib import Path
from typing import Any

MANIFEST_PATHS = (
    "pyproject.toml",
    "pmux_client/__init__.py",
    "pmux_client/client.py",
    "pmux_client/protocol.py",
    "pmux_client/py.typed",
    "pmux_client/smithers.py",
    "tests/actual_daemon_e2e.py",
)


def check(condition: bool, label: str) -> None:
    if not condition:
        raise AssertionError(f"cross-client assertion failed: {label}")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def identity(path: Path) -> dict[str, str]:
    canonical = path.resolve(strict=True)
    check(canonical.is_file(), "identity target is a regular file")
    return {"path": str(canonical), "sha256": digest(canonical)}


def source_manifest(root: Path) -> list[dict[str, str]]:
    records = []
    for name in MANIFEST_PATHS:
        asset = identity(root / name)
        check(Path(asset["path"]).relative_to(root).as_posix() == name, f"source path {name}")
        records.append({"relative_path": name, "sha256": asset["sha256"]})
    return records


def make_start(
    config: dict[str, Any], identity_value: dict[str, str], retention: dict[str, Any]
) -> dict[str, Any]:
    return {
        "identity": identity_value,
        "cwd": config["cwd"],
        "claude": {
            "executable": config["claude_executable"],
            "model": "test-model",
            "permission_mode": "default",
        },
        "environment": config["environment"],
        "auth_policy": "subscription",
        "terminal": {
            "rows": 24,
            "cols": 120,
            "profile": "transparent",
            "input_transport": "sdk",
        },
        "lifecycle": {"mode": "transcript"},
        "retention": retention,
        "compatibility": "require_tested",
    }


def make_turn(turn_id: str, prompt: str) -> dict[str, Any]:
    return {
        "turn_id": turn_id,
        "prompt": prompt,
        "deadline_unix_ms": int(time.time() * 1_000) + 30_000,
        "lease": {"on_disconnect": "continue"},
    }


def validate_completed(result: dict[str, Any], session_id: str, turn_id: str) -> None:
    check(result["session_id"] == session_id, "completed session identity")
    check(result["turn_id"] == turn_id, "completed turn identity")
    check(result["outcome"] == "completed", "completed outcome")
    check(result["text"] == "pmux-test-ok", "completed text")
    check(result["model"] == "pmux-test-model", "completed model")
    check(result["usage"]["main"]["input_tokens"] == 3, "completed input usage")
    check(result["usage"]["main"]["output_tokens"] == 1, "completed output usage")
    completion = result["completion"]
    check(completion["authority"] == "transcript", "completion authority")
    check(completion["prompt_acknowledged"], "prompt acknowledgement provenance")
    check(completion["terminal_message_observed"], "terminal message provenance")
    check(completion["terminal_prompt_observed"], "terminal prompt provenance")
    check(completion["terminal_quiet_observed"], "terminal quiet provenance")
    check(completion["transcript_drained"], "transcript drain provenance")
    check(result["compatibility"]["tested"], "tested compatibility")


def submit_when_ready(
    api: Any, client: Any, handle: dict[str, Any], turn: dict[str, Any]
) -> dict[str, Any]:
    for _attempt in range(200):
        try:
            return client.run_turn(handle["session_id"], handle["generation_id"], turn)
        except api.PmuxServerError as error:
            if error.code != "session_busy":
                raise
            time.sleep(0.025)
    raise AssertionError("session remained busy after bounded attach reconciliation")


def observe_until(
    client: Any,
    handle: dict[str, Any],
    after_sequence: int,
    turn_id: str,
    event_types: set[str],
    label: str,
) -> dict[str, Any]:
    stop_event = threading.Event()
    timer = threading.Timer(35.0, stop_event.set)
    timer.daemon = True
    timer.start()
    cursor = after_sequence
    count = 0
    reconnects = 0

    def on_reconnect(_error: object, _attempt: int, _cursor: int) -> None:
        nonlocal reconnects
        reconnects += 1

    subscription = client.events(
        handle["session_id"],
        handle["generation_id"],
        after_sequence=after_sequence,
        wait_ms=1_000,
        max_events=128,
        reconnect_delay=0.01,
        max_reconnect_attempts=3,
        stop_event=stop_event,
        on_reconnect=on_reconnect,
    )
    try:
        for item in subscription:
            check(item.kind == "event", f"{label} did not cross a replay gap")
            event = item.event
            check(event["sequence"] == cursor + 1, f"{label} sequence continuity")
            cursor = event["sequence"]
            count += 1
            if event.get("turn_id") == turn_id and event["event"]["type"] in event_types:
                return {
                    "event": event,
                    "first_sequence": after_sequence + 1,
                    "last_sequence": cursor,
                    "count": count,
                    "reconnects": reconnects,
                }
    finally:
        timer.cancel()
        subscription.close()
    raise AssertionError(f"{label} ended without the requested event")


def expect_server_code(api: Any, operation: Any, expected: str) -> str:
    try:
        operation()
    except api.PmuxServerError as error:
        if error.code == expected:
            return expected
        raise
    raise AssertionError(f"operation unexpectedly succeeded instead of returning {expected}")


def attach_exchange(endpoint: str, token: str, require_bytes: bool) -> int:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(3.0)
        try:
            stream.connect(endpoint)
            token_bytes = token.encode()
            stream.sendall(struct.pack("!I", len(token_bytes)) + token_bytes)
            payload = stream.recv(64)
        except (OSError, TimeoutError):
            if require_bytes:
                raise
            return 0
    if require_bytes:
        check(bool(payload), "first attach stream returned terminal bytes")
    else:
        check(not payload, "one-use attach capability returned bytes twice")
    return len(payload)


def consume_one_use_capability(
    capability: dict[str, Any], handle: dict[str, Any]
) -> dict[str, Any]:
    check(capability["session_id"] == handle["session_id"], "attach session identity")
    check(capability["generation_id"] == handle["generation_id"], "attach generation identity")
    check(capability["read_only"] is False, "attach writable metadata")
    check(os.path.isabs(capability["endpoint"]), "attach endpoint is absolute")
    check(capability["expires_at_ms"] > int(time.time() * 1_000), "attach future expiry")
    first_bytes = attach_exchange(capability["endpoint"], capability["token"], True)
    time.sleep(0.05)
    second_bytes = attach_exchange(capability["endpoint"], capability["token"], False)
    check(second_bytes == 0, "attach capability is one use")
    return {"metadata_valid": True, "first_stream_bytes": first_bytes, "reuse_rejected": True}


def canonical_uuid(value: str) -> str:
    parsed = str(uuid.UUID(value))
    check(parsed == value, "configuration UUID is canonical")
    return parsed


def main() -> None:
    check(len(sys.argv) == 3, "expected config and client-root arguments")
    config_path = Path(sys.argv[1]).resolve(strict=True)
    client_root = Path(sys.argv[2]).resolve(strict=True)
    check(config_path.is_absolute() and client_root.is_absolute(), "invocation paths are absolute")
    config = json.loads(config_path.read_text(encoding="utf-8"))
    check(config["schema_version"] == 1, "configuration schema version")
    for value in config["ids"].values():
        canonical_uuid(value)

    helper_identity = identity(Path(__file__))
    check(
        helper_identity["path"] == str(client_root / "tests/actual_daemon_e2e.py"),
        "helper loaded from the expected source root",
    )
    sys.path.insert(0, str(client_root))
    api = importlib.import_module("pmux_client")
    module_path = Path(api.__file__).resolve(strict=True)
    check(module_path == client_root / "pmux_client/__init__.py", "client import source")
    check(api.PROTOCOL_VERSION == 1, "client protocol version")

    client = api.PmuxClient(config["socket_path"], connect_timeout=5.0, request_timeout=45.0)
    pong = client.ping()
    check(pong["protocol_version"] == 1, "ping protocol version")

    persistent = client.start_session(
        make_start(
            config,
            {"mode": "new", "session_id": config["ids"]["persistent_session"]},
            {"mode": "persistent", "idle_ttl_ms": 60_000},
        )
    )
    check(
        persistent["session_id"] == config["ids"]["persistent_session"],
        "persistent session identity",
    )
    check(persistent["compatibility"]["tested"], "persistent tested compatibility")

    first_turn_request = make_turn(config["ids"]["first_turn"], config["prompts"]["first"])
    first_accepted = submit_when_ready(api, client, persistent, first_turn_request)
    check(first_accepted["replayed"] is False, "first turn was not replayed")
    first_observed = observe_until(
        client,
        persistent,
        first_accepted["next_sequence"] - 1,
        config["ids"]["first_turn"],
        {"turn_completed", "turn_failed"},
        "first turn",
    )
    check(first_observed["event"]["event"]["type"] == "turn_completed", "first completion")
    first_result = first_observed["event"]["event"]["data"]
    validate_completed(first_result, persistent["session_id"], config["ids"]["first_turn"])
    check(
        first_observed["event"]["sequence"] == first_result["final_sequence"],
        "first final sequence",
    )

    replay_accepted = client.run_turn(
        persistent["session_id"], persistent["generation_id"], first_turn_request
    )
    check(replay_accepted["session_id"] == persistent["session_id"], "replay session identity")
    check(
        replay_accepted["generation_id"] == persistent["generation_id"],
        "replay generation identity",
    )
    check(replay_accepted["turn_id"] == config["ids"]["first_turn"], "replay turn identity")
    check(replay_accepted["replayed"] is True, "completed turn retry was replayed")
    check(replay_accepted["state"] == "ready", "replay preserved ready state")
    replay_observed = observe_until(
        client,
        persistent,
        replay_accepted["next_sequence"] - 1,
        config["ids"]["first_turn"],
        {"turn_completed", "turn_failed"},
        "replayed first turn",
    )
    check(replay_observed["event"]["event"]["type"] == "turn_completed", "replay completion")
    replay_result = replay_observed["event"]["event"]["data"]
    validate_completed(replay_result, persistent["session_id"], config["ids"]["first_turn"])
    check(
        replay_observed["event"]["sequence"] == replay_result["final_sequence"],
        "replay final sequence",
    )
    conflicting_turn = dict(first_turn_request)
    conflicting_turn["prompt"] = f"{config['prompts']['first']}_CONFLICT"
    conflict_code = expect_server_code(
        api,
        lambda: client.run_turn(
            persistent["session_id"], persistent["generation_id"], conflicting_turn
        ),
        "id_conflict",
    )

    snapshot = client.inspect_session(persistent["session_id"], persistent["generation_id"])
    check(snapshot["last_turn"]["turn_id"] == config["ids"]["first_turn"], "inspect turn")
    check(snapshot["last_sequence"] == replay_result["final_sequence"], "inspect replay cursor")

    capability = client.attach_session(
        {
            "session_id": persistent["session_id"],
            "generation_id": persistent["generation_id"],
            "read_only": False,
        }
    )
    attach = consume_one_use_capability(capability, persistent)

    cancel_accepted = submit_when_ready(
        api,
        client,
        persistent,
        make_turn(config["ids"]["cancel_turn"], config["prompts"]["cancel"]),
    )
    acknowledged = observe_until(
        client,
        persistent,
        cancel_accepted["next_sequence"] - 1,
        config["ids"]["cancel_turn"],
        {"prompt_acknowledged"},
        "cancel prompt acknowledgement",
    )
    cancel_result = client.cancel_turn(
        persistent["session_id"],
        persistent["generation_id"],
        config["ids"]["cancel_turn"],
    )
    check(cancel_result["outcome"] == "cancelled", "cancel result")
    cancel_observed = observe_until(
        client,
        persistent,
        acknowledged["last_sequence"],
        config["ids"]["cancel_turn"],
        {"turn_cancelled", "turn_failed"},
        "cancel terminal event",
    )
    cancel_event = cancel_observed["event"]["event"]
    check(cancel_event["type"] == "turn_cancelled", "cancel event type")
    check(cancel_event["data"]["outcome"] == "cancelled", "cancel event outcome")
    check(cancel_event["data"]["recovered_to_ready"], "cancel recovery")

    recovery_accepted = submit_when_ready(
        api,
        client,
        persistent,
        make_turn(config["ids"]["recovery_turn"], config["prompts"]["recovery"]),
    )
    recovery_observed = observe_until(
        client,
        persistent,
        recovery_accepted["next_sequence"] - 1,
        config["ids"]["recovery_turn"],
        {"turn_completed", "turn_failed"},
        "recovery turn",
    )
    recovery_event = recovery_observed["event"]["event"]
    check(recovery_event["type"] == "turn_completed", "recovery terminal event")
    validate_completed(
        recovery_event["data"], persistent["session_id"], config["ids"]["recovery_turn"]
    )

    first_close = client.close_session(
        persistent["session_id"], persistent["generation_id"], "graceful"
    )
    check(first_close["process_reaped"] and not first_close["already_closed"], "first close")

    resumed = client.start_session(
        make_start(
            config,
            {"mode": "resume", "session_id": persistent["session_id"]},
            {"mode": "persistent", "idle_ttl_ms": 60_000},
        )
    )
    check(resumed["generation_id"] != persistent["generation_id"], "resume generation")
    stale_code = expect_server_code(
        api,
        lambda: client.inspect_session(persistent["session_id"], persistent["generation_id"]),
        "stale_session_generation",
    )
    old_close = client.close_session(persistent["session_id"], persistent["generation_id"], "force")
    check(old_close["already_closed"] and old_close["process_reaped"], "old close replay")

    resumed_accepted = submit_when_ready(
        api,
        client,
        resumed,
        make_turn(config["ids"]["resumed_turn"], config["prompts"]["resumed"]),
    )
    resumed_observed = observe_until(
        client,
        resumed,
        resumed_accepted["next_sequence"] - 1,
        config["ids"]["resumed_turn"],
        {"turn_completed", "turn_failed"},
        "resumed turn",
    )
    resumed_event = resumed_observed["event"]["event"]
    check(resumed_event["type"] == "turn_completed", "resumed terminal event")
    validate_completed(resumed_event["data"], resumed["session_id"], config["ids"]["resumed_turn"])
    resumed_close = client.close_session(
        resumed["session_id"], resumed["generation_id"], "graceful"
    )
    check(resumed_close["process_reaped"], "resumed close")

    once_result = client.run_once(
        {
            "session": make_start(
                config,
                {"mode": "new", "session_id": config["ids"]["once_session"]},
                {"mode": "one_shot"},
            ),
            "turn": make_turn(config["ids"]["once_turn"], config["prompts"]["once"]),
        }
    )
    validate_completed(once_result, config["ids"]["once_session"], config["ids"]["once_turn"])

    missing_client = api.PmuxClient(
        f"{config['socket_path']}.python-missing", connect_timeout=0.25, request_timeout=0.25
    )
    transport_error = False
    try:
        missing_client.ping()
    except api.PmuxTransportError:
        transport_error = True
    check(transport_error, "missing socket maps to a transport error")

    runtime_path = Path(sys.executable).resolve(strict=True)
    report = {
        "schema_version": 1,
        "language": "python",
        "runtime": {
            "path": str(runtime_path),
            "sha256": digest(runtime_path),
            "version": sys.version.split()[0],
        },
        "helper": helper_identity,
        "client": {
            "package_name": "pmux-client",
            "package_version": api.__version__,
            "protocol_version": api.PROTOCOL_VERSION,
            "entry_path": str(module_path),
            "manifest": source_manifest(client_root),
        },
        "ping_protocol_version": pong["protocol_version"],
        "persistent": {
            "session_id": persistent["session_id"],
            "generation_id": persistent["generation_id"],
            "first_turn_id": config["ids"]["first_turn"],
            "first_final_sequence": first_result["final_sequence"],
            "first_event_count": first_observed["count"],
            "reconnects": first_observed["reconnects"],
            "inspected": True,
            "closed_and_reaped": first_close["process_reaped"],
        },
        "idempotency": {
            "turn_id": config["ids"]["first_turn"],
            "initial_replayed": first_accepted["replayed"],
            "replayed": replay_accepted["replayed"],
            "replay_final_sequence": replay_result["final_sequence"],
            "replay_event_count": replay_observed["count"],
            "reconnects": replay_observed["reconnects"],
            "conflict_error_code": conflict_code,
            "conflict_preserved_cursor": snapshot["last_sequence"]
            == replay_result["final_sequence"],
        },
        "attach": attach,
        "cancellation": {
            "turn_id": config["ids"]["cancel_turn"],
            "outcome": cancel_result["outcome"],
            "recovered_to_ready": cancel_event["data"]["recovered_to_ready"],
            "recovery_turn_id": config["ids"]["recovery_turn"],
            "recovery_outcome": recovery_event["data"]["outcome"],
        },
        "resume": {
            "generation_id": resumed["generation_id"],
            "stale_error_code": stale_code,
            "old_close_replayed": old_close["already_closed"],
            "turn_id": config["ids"]["resumed_turn"],
            "outcome": resumed_event["data"]["outcome"],
            "closed_and_reaped": resumed_close["process_reaped"],
        },
        "run_once": {
            "session_id": once_result["session_id"],
            "generation_id": once_result["generation_id"],
            "turn_id": once_result["turn_id"],
            "outcome": once_result["outcome"],
            "text": once_result["text"],
        },
        "missing_socket_transport_error": transport_error,
    }
    sys.stdout.write(json.dumps(report, separators=(",", ":"), sort_keys=True) + "\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        code = getattr(error, "code", "")
        sys.stderr.write(f"pmux Python cross-client E2E failed: {type(error).__name__}:{code}\n")
        raise SystemExit(1) from None
