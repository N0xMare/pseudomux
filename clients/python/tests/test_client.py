from __future__ import annotations

import json
import socket
import struct
import tempfile
import threading
import time
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any

from pmux_client import (
    DEFAULT_RUN_ONCE_TIMEOUT,
    MAX_SAFE_JSON_INTEGER,
    RUN_ONCE_RESPONSE_MARGIN,
    PmuxClient,
    PmuxFrameTooLargeError,
    PmuxProtocolError,
    PmuxRequestIdMismatchError,
    PmuxSequenceError,
    PmuxServerError,
    PmuxVersionError,
    ReplayGapItem,
    turn_id_for_attempt,
)
from pmux_client.client import KNOWN_ERROR_CODES, _timeout_for

SESSION_ID = "00000000-0000-4000-8000-000000000022"
GENERATION_ID = "00000000-0000-4000-8000-000000000044"
OTHER_ID = "00000000-0000-4000-8000-000000009999"
CONFORMANCE_ROOT = Path(__file__).resolve().parents[3] / "tests" / "conformance" / "v1"
CONFORMANCE_MANIFEST = json.loads((CONFORMANCE_ROOT / "manifest.json").read_text(encoding="utf-8"))
CONFORMANCE_CASES = json.loads((CONFORMANCE_ROOT / "cases.json").read_text(encoding="utf-8"))
CONFORMANCE_GOLDEN = json.loads((CONFORMANCE_ROOT / "golden.json").read_text(encoding="utf-8"))
Handler = Callable[[socket.socket], None]


def read_exact(stream: socket.socket, size: int) -> bytes:
    result = bytearray()
    while len(result) < size:
        chunk = stream.recv(size - len(result))
        if not chunk:
            raise RuntimeError("socket closed before request")
        result.extend(chunk)
    return bytes(result)


def read_request(stream: socket.socket) -> dict[str, Any]:
    length = struct.unpack("!I", read_exact(stream, 4))[0]
    return json.loads(read_exact(stream, length))


def write_json(stream: socket.socket, value: object) -> None:
    payload = json.dumps(value, separators=(",", ":")).encode()
    stream.sendall(struct.pack("!I", len(payload)))
    stream.sendall(payload)


def write_raw_json(stream: socket.socket, value: str) -> None:
    payload = value.encode()
    stream.sendall(struct.pack("!I", len(payload)))
    stream.sendall(payload)


def success(request: dict[str, Any], kind: str, data: object, version: int = 1) -> dict[str, Any]:
    return {
        "version": version,
        "request_id": request["request_id"],
        "result": {"type": kind, "data": data},
    }


class FakeServer:
    def __init__(self, handlers: list[Handler]) -> None:
        self.handlers = handlers
        self.directory = tempfile.TemporaryDirectory(prefix="pmux-python-")
        self.path = Path(self.directory.name) / "pmuxd.sock"
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(self.path))
        self.listener.listen()
        self.listener.settimeout(3)
        self.error: BaseException | None = None
        self.thread = threading.Thread(target=self._serve, daemon=True)

    def _serve(self) -> None:
        try:
            for handler in self.handlers:
                connection, _ = self.listener.accept()
                with connection:
                    handler(connection)
        except BaseException as error:  # propagated on context exit
            self.error = error

    def __enter__(self) -> FakeServer:
        self.thread.start()
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        self.thread.join(timeout=4)
        self.listener.close()
        self.directory.cleanup()
        if self.thread.is_alive() and exc is None:
            raise RuntimeError("fake server did not finish")
        if self.error is not None and exc is None:
            raise self.error


def start_request() -> dict[str, Any]:
    return {
        "identity": {"mode": "new", "session_id": SESSION_ID},
        "cwd": "/work/project",
        "claude": {
            "executable": "/usr/local/bin/claude",
            "settings": [
                {
                    "source": "inline",
                    "document": {
                        "hooks": {
                            "PostToolUse": [
                                {
                                    "matcher": "*",
                                    "hooks": [{"type": "command", "command": "snapshot"}],
                                }
                            ]
                        }
                    },
                }
            ],
        },
        "auth_policy": "subscription",
        "lifecycle": {"mode": "transcript"},
    }


def compatibility_report() -> dict[str, Any]:
    return {
        "claude_version": "2.1.207",
        "os": "macos",
        "arch": "aarch64",
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "tested": True,
        "transcript_drain_ms": 750,
    }


def snapshot(last_sequence: int) -> dict[str, Any]:
    return {
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "transcript_session_id": SESSION_ID,
        "cell": "full",
        "state": "ready",
        "cwd": "/work/project",
        "compatibility": compatibility_report(),
        "created_at_ms": 1,
        "updated_at_ms": 2,
        "resumable": True,
        "last_sequence": last_sequence,
    }


def heartbeat(sequence: int) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "sequence": sequence,
        "timestamp_ms": 100 + sequence,
        "event": {"type": "heartbeat", "data": {"session_state": "ready"}},
    }


def turn_result(turn_id: str) -> dict[str, Any]:
    usage = {
        "input_tokens": 10,
        "output_tokens": 2,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
    }
    return {
        "session_id": SESSION_ID,
        "generation_id": GENERATION_ID,
        "turn_id": turn_id,
        "outcome": "completed",
        "text": "done",
        "usage": {
            "main": usage,
            "sidechain": {**usage, "input_tokens": 0, "output_tokens": 0},
            "combined": usage,
        },
        "timings": {"submitted_at_ms": 1, "completed_at_ms": 2},
        "claude_version": "2.1.207",
        "compatibility": compatibility_report(),
        "completion": {
            "authority": "transcript",
            "prompt_acknowledged": True,
            "terminal_message_observed": True,
            "terminal_prompt_observed": True,
            "terminal_quiet_observed": True,
            "transcript_drained": True,
            "lifecycle_hook_observed": False,
        },
        "final_sequence": 9,
    }


class ClientTests(unittest.TestCase):
    def test_client_rejects_relative_socket_paths_before_connecting(self) -> None:
        with self.assertRaisesRegex(ValueError, "absolute"):
            PmuxClient("relative/pmux.sock")
        with self.assertRaisesRegex(ValueError, "max_frame_bytes"):
            PmuxClient("/tmp/pmux.sock", max_frame_bytes=8 * 1024 * 1024 + 1)
        with self.assertRaisesRegex(ValueError, "max_frame_bytes"):
            PmuxClient("/tmp/pmux.sock", max_frame_bytes=True)

    def test_start_uses_explicit_socket_and_preserves_settings(self) -> None:
        expected = start_request()

        def handler(stream: socket.socket) -> None:
            request = read_request(stream)
            self.assertEqual(request["method"], "start_session")
            self.assertEqual(
                request["params"]["claude"]["settings"], expected["claude"]["settings"]
            )
            write_json(
                stream,
                success(
                    request,
                    "session_started",
                    {
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "state": "booting",
                        "compatibility": compatibility_report(),
                        "created_at_ms": 1,
                        "last_sequence": 0,
                    },
                ),
            )

        with FakeServer([handler]) as server:
            client = PmuxClient(server.path)
            self.assertEqual(client.socket_path, str(server.path))
            session = client.start_session(expected)  # type: ignore[arg-type]
            self.assertEqual(session["session_id"], SESSION_ID)

    def test_native_typed_method_shapes(self) -> None:
        turn_id = "00000000-0000-4000-8000-000000000033"
        expected = [
            (
                "inspect_session",
                "session_snapshot",
                snapshot(0),
            ),
            (
                "run_turn",
                "turn_accepted",
                {
                    "session_id": SESSION_ID,
                    "generation_id": GENERATION_ID,
                    "turn_id": turn_id,
                    "replayed": False,
                    "state": "running",
                    "next_sequence": 1,
                },
            ),
            (
                "cancel_turn",
                "turn_cancelled",
                {
                    "session_id": SESSION_ID,
                    "generation_id": GENERATION_ID,
                    "turn_id": turn_id,
                    "outcome": "cancelled",
                    "session_state": "ready",
                },
            ),
            (
                "attach_session",
                "attach_capability",
                {
                    "session_id": SESSION_ID,
                    "generation_id": GENERATION_ID,
                    "token": "one-use-token",
                    "endpoint": "/runtime/attach.sock",
                    "expires_at_ms": 10,
                    "read_only": True,
                },
            ),
            (
                "close_session",
                "session_closed",
                {
                    "session_id": SESSION_ID,
                    "generation_id": GENERATION_ID,
                    "already_closed": False,
                    "process_reaped": True,
                },
            ),
            ("run_once", "turn_result", turn_result(turn_id)),
        ]

        def handler(method: str, result_type: str, data: object) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                self.assertEqual(request["method"], method)
                write_json(stream, success(request, result_type, data))

            return respond

        handlers = [handler(*entry) for entry in expected]
        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            self.assertEqual(client.inspect_session(SESSION_ID, GENERATION_ID)["state"], "ready")
            self.assertEqual(
                client.run_turn(
                    SESSION_ID,
                    GENERATION_ID,
                    {"turn_id": turn_id, "prompt": "review"},
                )["turn_id"],
                turn_id,
            )
            self.assertEqual(
                client.cancel_turn(SESSION_ID, GENERATION_ID, turn_id)["outcome"],
                "cancelled",
            )
            self.assertTrue(
                client.attach_session(
                    {
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "read_only": True,
                    }
                )["read_only"]
            )
            self.assertTrue(client.close_session(SESSION_ID, GENERATION_ID)["process_reaped"])
            self.assertEqual(
                client.run_once(
                    {
                        "session": start_request(),  # type: ignore[typeddict-item]
                        "turn": {"turn_id": turn_id, "prompt": "review"},
                    }
                )["text"],
                "done",
            )

    def test_every_typed_method_rejects_contextually_mismatched_results(self) -> None:
        turn_id = "00000000-0000-4000-8000-000000000033"
        inspect_data = snapshot(0)
        inspect_data["session_id"] = OTHER_ID
        mismatched_event = heartbeat(1)
        mismatched_event["session_id"] = OTHER_ID
        responses: list[tuple[str, object]] = [
            ("pong", {"server_version": "test", "protocol_version": 2}),
            (
                "session_started",
                {
                    "session_id": OTHER_ID,
                    "generation_id": GENERATION_ID,
                    "state": "booting",
                    "compatibility": compatibility_report(),
                    "created_at_ms": 1,
                    "last_sequence": 0,
                },
            ),
            ("session_snapshot", inspect_data),
            (
                "turn_accepted",
                {
                    "session_id": SESSION_ID,
                    "generation_id": OTHER_ID,
                    "turn_id": turn_id,
                    "replayed": False,
                    "state": "running",
                    "next_sequence": 1,
                },
            ),
            (
                "turn_cancelled",
                {
                    "session_id": SESSION_ID,
                    "generation_id": GENERATION_ID,
                    "turn_id": OTHER_ID,
                    "outcome": "cancelled",
                    "session_state": "ready",
                },
            ),
            (
                "attach_capability",
                {
                    "session_id": OTHER_ID,
                    "generation_id": GENERATION_ID,
                    "token": "opaque",
                    "endpoint": "/tmp/attach.sock",
                    "expires_at_ms": 10,
                    "read_only": True,
                },
            ),
            (
                "session_closed",
                {
                    "session_id": SESSION_ID,
                    "generation_id": OTHER_ID,
                    "already_closed": False,
                    "process_reaped": True,
                },
            ),
            ("turn_result", turn_result(OTHER_ID)),
            (
                "events",
                {"events": [mismatched_event], "next_sequence": 2},
            ),
        ]

        def handler(result_type: str, data: object) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                write_json(stream, success(request, result_type, data))

            return respond

        with FakeServer([handler(*response) for response in responses]) as server:
            client = PmuxClient(server.path)
            with self.assertRaises(PmuxVersionError):
                client.ping()
            with self.assertRaises(PmuxProtocolError):
                client.start_session(start_request())  # type: ignore[arg-type]
            with self.assertRaises(PmuxProtocolError):
                client.inspect_session(SESSION_ID, GENERATION_ID)
            with self.assertRaises(PmuxProtocolError):
                client.run_turn(
                    SESSION_ID,
                    GENERATION_ID,
                    {"turn_id": turn_id, "prompt": "test"},
                )
            with self.assertRaises(PmuxProtocolError):
                client.cancel_turn(SESSION_ID, GENERATION_ID, turn_id)
            with self.assertRaises(PmuxProtocolError):
                client.attach_session(
                    {
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "read_only": True,
                    }
                )
            with self.assertRaises(PmuxProtocolError):
                client.close_session(SESSION_ID, GENERATION_ID)
            with self.assertRaises(PmuxProtocolError):
                client.run_once(
                    {
                        "session": start_request(),  # type: ignore[typeddict-item]
                        "turn": {"turn_id": turn_id, "prompt": "test"},
                    }
                )
            with self.assertRaises(PmuxProtocolError):
                client.subscribe_events(
                    SESSION_ID,
                    GENERATION_ID,
                    max_events=8,
                )

    def test_structured_error_is_preserved(self) -> None:
        def handler(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(
                stream,
                {
                    "version": 1,
                    "request_id": request["request_id"],
                    "error": {
                        "code": "rate_limited",
                        "message": "quota exhausted",
                        "retryable": True,
                        "details": {"resets_at_ms": 42},
                    },
                },
            )

        with FakeServer([handler]) as server:
            with self.assertRaises(PmuxServerError) as raised:
                PmuxClient(server.path).ping()
            self.assertEqual(raised.exception.code, "rate_limited")
            self.assertEqual(raised.exception.details, {"resets_at_ms": 42})

    def test_shared_v1_manifest_and_durable_id_vectors_match_python(self) -> None:
        self.assertEqual(CONFORMANCE_MANIFEST["schema_version"], 1)
        self.assertEqual(CONFORMANCE_MANIFEST["protocol_version"], 1)
        self.assertEqual(set(CONFORMANCE_MANIFEST["error_codes"]), KNOWN_ERROR_CODES)
        self.assertEqual(
            CONFORMANCE_MANIFEST["methods"],
            [
                "ping",
                "start_session",
                "run_turn",
                "cancel_turn",
                "inspect_session",
                "attach_session",
                "close_session",
                "subscribe_events",
                "run_once",
                "clear_session",
                "diagnose",
                "run_stateless",
                "create_agent",
                "get_agent",
                "list_agents",
                "update_agent",
            ],
        )
        self.assertEqual(
            CONFORMANCE_MANIFEST["results"],
            [
                "pong",
                "session_started",
                "turn_accepted",
                "turn_cancelled",
                "session_snapshot",
                "attach_capability",
                "session_closed",
                "events",
                "turn_result",
                "session_cleared",
                "diagnosis",
                "stateless_result",
                "agent_created",
                "agent",
                "agent_list",
                "agent_updated",
            ],
        )
        self.assertEqual(
            CONFORMANCE_MANIFEST["events"],
            [
                "session_state_changed",
                "prompt_acknowledged",
                "logical_message",
                "tool_started",
                "tool_completed",
                "rate_limit",
                "needs_input",
                "terminal_candidate",
                "turn_completed",
                "turn_cancelled",
                "turn_failed",
                "warning",
                "replay_gap",
                "heartbeat",
            ],
        )
        for vector in CONFORMANCE_GOLDEN["durable_ids"]["cases"]:
            self.assertEqual(turn_id_for_attempt(vector["attempt"]), vector["turn_id"])
        with self.assertRaisesRegex(ValueError, "must not be empty"):
            turn_id_for_attempt("")

    def test_a_listing_surfaces_the_records_the_daemon_could_not_read(self) -> None:
        """``unreadable`` is omitted when empty and validated when present.

        The ordinary listing's bytes are unchanged; a client that dropped the
        field when it IS present would show a stored agent simply ceasing to
        exist, which is the reason pmuxd stopped answering the whole listing
        with the first bad record's refusal.
        """
        frames: list[object] = [
            {
                "agents": [],
                "unreadable": [
                    {"agent_id": OTHER_ID, "reason": "agent store ... has no head pointer"}
                ],
            },
            {"agents": []},
            {"unreadable": [{"agent_id": OTHER_ID}]},
            {"unreadable": [{"agent_id": "not-a-uuid", "reason": "x"}]},
            {"unreadable": {}},
        ]

        def handler(data: object) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                write_json(stream, success(request, "agent_list", data))

            return respond

        with FakeServer([handler(frame) for frame in frames]) as server:
            client = PmuxClient(server.path)
            self.assertEqual(
                client.list_agents()["unreadable"],
                [{"agent_id": OTHER_ID, "reason": "agent store ... has no head pointer"}],
            )
            self.assertNotIn("unreadable", client.list_agents())
            for label in ("missing reason", "non-canonical id", "not an array"):
                with self.assertRaises(PmuxProtocolError, msg=label):
                    client.list_agents()

    def test_shared_strict_error_body_vectors_are_enforced_at_top_level(self) -> None:
        vectors = CONFORMANCE_CASES["error_bodies"]

        def handler(vector: dict[str, Any]) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                write_json(
                    stream,
                    {
                        "version": 1,
                        "request_id": request["request_id"],
                        "error": vector["body"],
                    },
                )

            return respond

        with FakeServer([handler(vector) for vector in vectors]) as server:
            client = PmuxClient(server.path)
            for vector in vectors:
                if vector["valid"]:
                    with self.assertRaises(PmuxServerError) as raised:
                        client.ping()
                    self.assertEqual(raised.exception.code, vector["body"]["code"])
                else:
                    with self.assertRaises(PmuxProtocolError, msg=vector["id"]):
                        client.ping()

    def test_shared_replay_gap_vectors_require_exclusivity_and_exact_cursors(self) -> None:
        vectors = CONFORMANCE_CASES["replay_batches"]

        def handler(vector: dict[str, Any]) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                write_json(
                    stream,
                    success(
                        request,
                        "events",
                        {
                            "events": [
                                heartbeat(sequence) for sequence in vector["event_sequences"]
                            ],
                            "next_sequence": vector["batch_next"],
                            "replay_gap": {
                                "requested_after": vector["requested_after"],
                                "oldest_available": vector["oldest_available"],
                                "next_sequence": vector["gap_next"],
                                "snapshot": snapshot(vector["snapshot_last"]),
                            },
                        },
                    ),
                )

            return respond

        with FakeServer([handler(vector) for vector in vectors]) as server:
            client = PmuxClient(server.path)
            for vector in vectors:
                if vector["valid"]:
                    batch = client.subscribe_events(
                        SESSION_ID,
                        GENERATION_ID,
                        after_sequence=vector["requested_after"],
                        max_events=8,
                    )
                    self.assertEqual(
                        batch["replay_gap"]["snapshot"]["last_sequence"],
                        vector["snapshot_last"],
                    )
                else:
                    with self.assertRaises(PmuxProtocolError, msg=vector["id"]):
                        client.subscribe_events(
                            SESSION_ID,
                            GENERATION_ID,
                            after_sequence=vector["requested_after"],
                            max_events=8,
                        )

    def test_shared_canonical_uuid_vectors_validate_without_rewriting(self) -> None:
        valid = [vector for vector in CONFORMANCE_CASES["identities"] if vector["valid"]]

        def valid_handler(vector: dict[str, Any]) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                self.assertEqual(request["params"]["session_id"], vector["value"])
                data = snapshot(0)
                data["session_id"] = vector["value"]
                response = success(request, "session_snapshot", data)
                if vector["id"] == "canonical_upper":
                    response["request_id"] = response["request_id"].upper()
                write_json(stream, response)

            return respond

        with FakeServer([valid_handler(vector) for vector in valid]) as server:
            client = PmuxClient(server.path)
            for vector in valid:
                result = client.inspect_session(vector["value"], GENERATION_ID)
                self.assertEqual(result["session_id"], vector["value"])

        disconnected = PmuxClient("/tmp/pmux-conformance-does-not-exist.sock")
        invalid = [vector for vector in CONFORMANCE_CASES["identities"] if not vector["valid"]]
        for vector in invalid:
            with self.assertRaises(PmuxProtocolError, msg=vector["id"]):
                disconnected.inspect_session(vector["value"], GENERATION_ID)

        def invalid_handler(vector: dict[str, Any]) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                write_json(
                    stream,
                    success(
                        request,
                        "session_started",
                        {
                            "session_id": vector["value"],
                            "generation_id": GENERATION_ID,
                            "state": "booting",
                            "compatibility": compatibility_report(),
                            "created_at_ms": 1,
                            "last_sequence": 0,
                        },
                    ),
                )

            return respond

        with FakeServer([invalid_handler(vector) for vector in invalid]) as server:
            client = PmuxClient(server.path)
            for vector in invalid:
                with self.assertRaises(PmuxProtocolError, msg=vector["id"]):
                    client.request("ping")

    def test_nonstandard_json_constants_are_rejected_in_both_directions(self) -> None:
        constants = CONFORMANCE_CASES["nonstandard_json_constants"]

        def handler(constant: str) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                write_raw_json(
                    stream,
                    '{"version":1,"request_id":"'
                    + request["request_id"]
                    + '","result":{"type":"pong","data":{"server_version":"test",'
                    + '"protocol_version":'
                    + constant
                    + "}}}",
                )

            return respond

        with FakeServer([handler(constant) for constant in constants]) as server:
            client = PmuxClient(server.path)
            for constant in constants:
                with self.assertRaises(PmuxProtocolError, msg=constant):
                    client.ping()

        disconnected = PmuxClient("/tmp/pmux-conformance-does-not-exist.sock")
        for value in (float("nan"), float("inf"), float("-inf")):
            with self.assertRaises(PmuxProtocolError):
                disconnected.request("future_method", {"value": value})

    def test_shared_safe_integer_boundaries_are_enforced_on_public_client_input(self) -> None:
        self.assertEqual(MAX_SAFE_JSON_INTEGER, 9_007_199_254_740_991)
        boundaries = CONFORMANCE_CASES["numeric_boundaries"]

        def protocol_handler(vector: dict[str, Any]) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                response = success(
                    request,
                    "session_started",
                    {
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "state": "booting",
                        "compatibility": compatibility_report(),
                        "created_at_ms": "__PMUX_NUMBER__",
                        "last_sequence": 0,
                    },
                )
                write_raw_json(
                    stream,
                    json.dumps(response, separators=(",", ":")).replace(
                        '"__PMUX_NUMBER__"', vector["literal"]
                    ),
                )

            return respond

        with FakeServer([protocol_handler(vector) for vector in boundaries]) as server:
            client = PmuxClient(server.path)
            for vector in boundaries:
                if vector["protocol_owned_valid"]:
                    result = client.start_session(start_request())  # type: ignore[arg-type]
                    self.assertEqual(result["created_at_ms"], int(vector["literal"]))
                else:
                    with self.assertRaises(PmuxProtocolError, msg=vector["id"]):
                        client.start_session(start_request())  # type: ignore[arg-type]

        def opaque_handler(vector: dict[str, Any]) -> Handler:
            def respond(stream: socket.socket) -> None:
                request = read_request(stream)
                response = {
                    "version": 1,
                    "request_id": request["request_id"],
                    "error": {
                        "code": "internal",
                        "message": "synthetic",
                        "retryable": False,
                        "details": {"nested": ["__PMUX_NUMBER__"]},
                    },
                }
                write_raw_json(
                    stream,
                    json.dumps(response, separators=(",", ":")).replace(
                        '"__PMUX_NUMBER__"', vector["literal"]
                    ),
                )

            return respond

        with FakeServer([opaque_handler(vector) for vector in boundaries]) as server:
            client = PmuxClient(server.path)
            for vector in boundaries:
                expected = PmuxServerError if vector["opaque_json_valid"] else PmuxProtocolError
                with self.assertRaises(expected, msg=vector["id"]):
                    client.ping()

    def test_shared_safe_integer_boundaries_are_enforced_before_public_client_output(self) -> None:
        boundaries = CONFORMANCE_CASES["numeric_boundaries"]
        turn_id = "00000000-0000-4000-8000-000000000033"

        def turn_handler(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(
                stream,
                success(
                    request,
                    "turn_accepted",
                    {
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "turn_id": turn_id,
                        "replayed": False,
                        "state": "running",
                        "next_sequence": 1,
                    },
                ),
            )

        valid_protocol = [item for item in boundaries if item["protocol_owned_valid"]]
        with FakeServer([turn_handler for _ in valid_protocol]) as server:
            client = PmuxClient(server.path)
            for vector in boundaries:
                turn = {
                    "turn_id": turn_id,
                    "prompt": "numeric boundary",
                    "deadline_unix_ms": int(vector["literal"]),
                }
                if vector["protocol_owned_valid"]:
                    client.run_turn(SESSION_ID, GENERATION_ID, turn)  # type: ignore[arg-type]
                else:
                    with self.assertRaises(PmuxProtocolError, msg=vector["id"]):
                        client.run_turn(
                            SESSION_ID,
                            GENERATION_ID,
                            turn,  # type: ignore[arg-type]
                        )

        def start_handler(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(
                stream,
                success(
                    request,
                    "session_started",
                    {
                        "session_id": SESSION_ID,
                        "generation_id": GENERATION_ID,
                        "state": "booting",
                        "compatibility": compatibility_report(),
                        "created_at_ms": 1,
                        "last_sequence": 0,
                    },
                ),
            )

        valid_opaque = [item for item in boundaries if item["opaque_json_valid"]]
        with FakeServer([start_handler for _ in valid_opaque]) as server:
            client = PmuxClient(server.path)
            for vector in boundaries:
                request = start_request()
                request["claude"]["settings"] = [
                    {
                        "source": "inline",
                        "document": {"nested": [int(vector["literal"])]},
                    }
                ]
                if vector["opaque_json_valid"]:
                    client.start_session(request)  # type: ignore[arg-type]
                else:
                    with self.assertRaises(PmuxProtocolError, msg=vector["id"]):
                        client.start_session(request)  # type: ignore[arg-type]

    def test_minor_v1_additive_response_and_event_fields_are_tolerated(self) -> None:
        def ping(stream: socket.socket) -> None:
            request = read_request(stream)
            response = success(
                request,
                "pong",
                {
                    "server_version": "future-minor",
                    "protocol_version": 1,
                    "future_pong_field": {"opaque": True},
                },
            )
            response["future_envelope_field"] = True
            write_json(stream, response)

        def events(stream: socket.socket) -> None:
            request = read_request(stream)
            event = heartbeat(1)
            event["future_event_envelope_field"] = True
            event["event"] = {
                "type": "heartbeat",
                "future_event_wrapper_field": "opaque",
                "data": {"session_state": "ready", "future_heartbeat_field": 1},
            }
            write_json(
                stream,
                success(
                    request,
                    "events",
                    {
                        "events": [event],
                        "next_sequence": 2,
                        "future_batch_field": {"opaque": True},
                    },
                ),
            )

        with FakeServer([ping, events]) as server:
            client = PmuxClient(server.path)
            pong = client.ping()
            self.assertEqual(pong["server_version"], "future-minor")
            pong_with_additions: Any = pong
            self.assertEqual(pong_with_additions["future_pong_field"], {"opaque": True})

            batch = client.subscribe_events(SESSION_ID, GENERATION_ID, max_events=8)
            batch_with_additions: Any = batch
            self.assertEqual(batch_with_additions["future_batch_field"], {"opaque": True})
            self.assertEqual(
                batch_with_additions["events"][0]["event"]["data"]["future_heartbeat_field"],
                1,
            )

    def test_known_response_and_event_payloads_require_v1_fields(self) -> None:
        def malformed_pong(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(stream, success(request, "pong", {"protocol_version": 1}))

        def malformed_event(stream: socket.socket) -> None:
            request = read_request(stream)
            event = heartbeat(1)
            event["event"] = {"type": "heartbeat", "data": {}}
            write_json(
                stream,
                success(request, "events", {"events": [event], "next_sequence": 2}),
            )

        with FakeServer([malformed_pong, malformed_event]) as server:
            client = PmuxClient(server.path)
            with self.assertRaises(PmuxProtocolError):
                client.ping()
            with self.assertRaises(PmuxProtocolError):
                client.subscribe_events(SESSION_ID, GENERATION_ID, max_events=8)

    def test_compatibility_reports_are_required_bounded_and_resolved(self) -> None:
        def handler_for(mutation: str) -> Callable[[socket.socket], None]:
            def handler(stream: socket.socket) -> None:
                request = read_request(stream)
                data: dict[str, Any] = {
                    "session_id": SESSION_ID,
                    "generation_id": GENERATION_ID,
                    "state": "booting",
                    "compatibility": compatibility_report(),
                    "created_at_ms": 1,
                    "last_sequence": 0,
                }
                if mutation == "missing":
                    del data["compatibility"]
                elif mutation == "auto":
                    data["compatibility"]["input_transport"] = "auto"
                else:
                    data["compatibility"]["transcript_drain_ms"] = 60_001
                write_json(stream, success(request, "session_started", data))

            return handler

        handlers = [handler_for("missing"), handler_for("auto"), handler_for("oversized")]
        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            for _ in handlers:
                with self.assertRaises(PmuxProtocolError):
                    client.start_session(start_request())  # type: ignore[arg-type]

    def test_optional_turn_timings_are_validated_as_protocol_owned_integers(self) -> None:
        turn_id = "00000000-0000-4000-8000-000000000033"

        def handler_for(field: str, value: object) -> Handler:
            def handler(stream: socket.socket) -> None:
                request = read_request(stream)
                data = turn_result(turn_id)
                data["timings"]["drain_ms"] = 10
                data["timings"][field] = value
                write_json(stream, success(request, "turn_result", data))

            return handler

        fields = ("last_transcript_activity_at_ms", "stop_hook_at_ms")
        mutations = (1, "190", -1, MAX_SAFE_JSON_INTEGER + 1)
        handlers = [handler_for(field, value) for field in fields for value in mutations]
        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            call = {
                "session": start_request(),
                "turn": {"turn_id": turn_id, "prompt": "timings"},
            }
            for field in fields:
                result = client.run_once(call)  # type: ignore[arg-type]
                self.assertEqual(result["timings"][field], 1)
                for other in fields:
                    if other != field:
                        self.assertNotIn(other, result["timings"])
                for _ in mutations[1:]:
                    with self.assertRaises(PmuxProtocolError):
                        client.run_once(call)  # type: ignore[arg-type]

    def test_unknown_v1_result_and_event_discriminants_are_rejected(self) -> None:
        def unknown_result(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(stream, success(request, "future_result", {}))

        def unknown_event(stream: socket.socket) -> None:
            request = read_request(stream)
            event = heartbeat(1)
            event["event"] = {"type": "future_event", "data": {}}
            write_json(
                stream,
                success(request, "events", {"events": [event], "next_sequence": 2}),
            )

        with FakeServer([unknown_result, unknown_event]) as server:
            client = PmuxClient(server.path)
            with self.assertRaises(PmuxProtocolError):
                client.request("ping")
            with self.assertRaises(PmuxProtocolError):
                client.subscribe_events(SESSION_ID, GENERATION_ID, max_events=8)

    def test_run_once_uses_the_turn_window_not_the_short_rpc_timeout(self) -> None:
        turn_id = "00000000-0000-4000-8000-000000000033"

        def handler(stream: socket.socket) -> None:
            request = read_request(stream)
            self.assertEqual(request["method"], "run_once")
            time.sleep(0.075)
            write_json(stream, success(request, "turn_result", turn_result(turn_id)))

        with FakeServer([handler]) as server:
            result = PmuxClient(server.path, request_timeout=0.02).run_once(
                {
                    "session": start_request(),  # type: ignore[typeddict-item]
                    "turn": {"turn_id": turn_id, "prompt": "wait past RPC timeout"},
                }
            )
            self.assertEqual(result["text"], "done")

    def test_run_once_timeout_budget_contains_recovery_drain_and_cleanup(self) -> None:
        self.assertEqual(DEFAULT_RUN_ONCE_TIMEOUT, 15 * 60)
        self.assertEqual(RUN_ONCE_RESPONSE_MARGIN, 120)
        self.assertEqual(
            _timeout_for(
                "run_once",
                {"turn": {"deadline_unix_ms": 101_000}},
                1,
                now=100,
            ),
            121,
        )
        self.assertEqual(
            _timeout_for("run_once", {"turn": {}}, 1, now=100),
            DEFAULT_RUN_ONCE_TIMEOUT,
        )

    def test_version_is_validated_before_result(self) -> None:
        def handler(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(stream, success(request, "future_result", {"future": True}, version=2))

        with FakeServer([handler]) as server, self.assertRaises(PmuxVersionError):
            PmuxClient(server.path).ping()

    def test_boolean_protocol_version_is_not_integer_version_one(self) -> None:
        def handler(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(
                stream,
                success(
                    request,
                    "pong",
                    {"server_version": "test", "protocol_version": 1},
                    version=True,
                ),
            )

        with FakeServer([handler]) as server, self.assertRaises(PmuxVersionError):
            PmuxClient(server.path).ping()

    def test_request_id_mismatch_is_rejected(self) -> None:
        def handler(stream: socket.socket) -> None:
            read_request(stream)
            write_json(
                stream,
                {
                    "version": 1,
                    "request_id": "00000000-0000-4000-8000-000000009999",
                    "result": {
                        "type": "pong",
                        "data": {"server_version": "test", "protocol_version": 1},
                    },
                },
            )

        with FakeServer([handler]) as server, self.assertRaises(PmuxRequestIdMismatchError):
            PmuxClient(server.path).ping()

    def test_oversized_advertised_frame_is_rejected(self) -> None:
        def handler(stream: socket.socket) -> None:
            read_request(stream)
            stream.sendall(struct.pack("!I", 1_025))

        with FakeServer([handler]) as server, self.assertRaises(PmuxFrameTooLargeError):
            PmuxClient(server.path, max_frame_bytes=1_024).ping()

    def test_subscription_reconnects_and_surfaces_replay_gap(self) -> None:
        reconnects: list[tuple[int, int]] = []

        def disconnect(stream: socket.socket) -> None:
            request = read_request(stream)
            self.assertEqual(request["params"]["after_sequence"], 0)

        def events(stream: socket.socket) -> None:
            request = read_request(stream)
            self.assertEqual(request["params"]["after_sequence"], 0)
            write_json(
                stream,
                success(
                    request,
                    "events",
                    {"events": [heartbeat(1), heartbeat(2)], "next_sequence": 3},
                ),
            )

        def gap(stream: socket.socket) -> None:
            request = read_request(stream)
            self.assertEqual(request["params"]["after_sequence"], 2)
            write_json(
                stream,
                success(
                    request,
                    "events",
                    {
                        "next_sequence": 10,
                        "replay_gap": {
                            "requested_after": 2,
                            "oldest_available": 8,
                            "next_sequence": 10,
                            "snapshot": snapshot(9),
                        },
                    },
                ),
            )

        with FakeServer([disconnect, events, gap]) as server:
            subscription = PmuxClient(server.path).events(
                SESSION_ID,
                GENERATION_ID,
                reconnect_delay=0.001,
                on_reconnect=lambda _error, attempt, cursor: reconnects.append((attempt, cursor)),
            )
            self.assertEqual(next(subscription).event["sequence"], 1)  # type: ignore[union-attr]
            self.assertEqual(next(subscription).event["sequence"], 2)  # type: ignore[union-attr]
            gap_item = next(subscription)
            self.assertIsInstance(gap_item, ReplayGapItem)
            self.assertEqual(gap_item.gap["snapshot"]["last_sequence"], 9)  # type: ignore[union-attr]
            self.assertEqual(subscription.after_sequence, 9)
            self.assertEqual(reconnects, [(1, 0)])
            subscription.close()

    def test_sequence_error_does_not_advance_cursor(self) -> None:
        def handler(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(
                stream,
                success(
                    request,
                    "events",
                    {"events": [heartbeat(2)], "next_sequence": 3},
                ),
            )

        with FakeServer([handler]) as server:
            subscription = PmuxClient(server.path).events(
                SESSION_ID, GENERATION_ID, max_reconnect_attempts=0
            )
            with self.assertRaises(PmuxSequenceError):
                next(subscription)
            self.assertEqual(subscription.after_sequence, 0)

    def test_safe_max_event_cursor_fails_closed(self) -> None:
        def handler(stream: socket.socket) -> None:
            request = read_request(stream)
            write_json(
                stream,
                success(
                    request,
                    "events",
                    {"events": [], "next_sequence": MAX_SAFE_JSON_INTEGER},
                ),
            )

        with FakeServer([handler]) as server, self.assertRaises(PmuxProtocolError):
            PmuxClient(server.path).subscribe_events(
                SESSION_ID,
                GENERATION_ID,
                after_sequence=MAX_SAFE_JSON_INTEGER,
            )

    def test_durable_attempt_uuid_is_cross_language_golden(self) -> None:
        turn_id = turn_id_for_attempt("workflow-7/task-review/attempt-2")
        self.assertEqual(turn_id, "6e77d57e-7ee5-51e8-8476-4ed12c876154")


if __name__ == "__main__":
    unittest.main()
