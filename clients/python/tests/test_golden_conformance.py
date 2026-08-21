from __future__ import annotations

import copy
import json
import socket
import struct
import tempfile
import threading
import unittest
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import Any, Literal, get_args, get_origin, is_typeddict

from pmux_client import (
    PmuxClient,
    PmuxProtocolError,
    PmuxRequestIdMismatchError,
    PmuxSequenceError,
    PmuxServerError,
    PmuxVersionError,
)
from pmux_client import client as client_module
from pmux_client import protocol as protocol_module
from tests.durable_ids import turn_id_for_attempt

CONFORMANCE_ROOT = Path(__file__).resolve().parents[3] / "tests" / "conformance" / "v1"
GOLDEN = json.loads((CONFORMANCE_ROOT / "golden.json").read_text(encoding="utf-8"))
CASES = json.loads((CONFORMANCE_ROOT / "cases.json").read_text(encoding="utf-8"))
MANIFEST = json.loads((CONFORMANCE_ROOT / "manifest.json").read_text(encoding="utf-8"))
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


class FakeServer:
    def __init__(self, handlers: list[Handler]) -> None:
        self.handlers = handlers
        self.directory = tempfile.TemporaryDirectory(prefix="pmux-python-golden-")
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


def request_for(method: str) -> dict[str, Any]:
    return copy.deepcopy(
        next(
            exchange["request"]
            for exchange in GOLDEN["requests_and_results"]
            if exchange["method"] == method
        )
    )


def replace_request_id(value: object, request_id: str) -> object:
    if value == "$REQUEST_ID":
        return request_id
    if isinstance(value, list):
        return [replace_request_id(item, request_id) for item in value]
    if isinstance(value, dict):
        return {key: replace_request_id(item, request_id) for key, item in value.items()}
    return value


def remove_pointer(value: dict[str, Any], pointer: str) -> None:
    parts = [part.replace("~1", "/").replace("~0", "~") for part in pointer[1:].split("/")]
    parent: Any = value
    for part in parts[:-1]:
        parent = parent[int(part)] if isinstance(parent, list) else parent[part]
    field = parts[-1]
    if isinstance(parent, list):
        parent.pop(int(field))
    else:
        if field not in parent:
            raise AssertionError(f"shared deletion pointer {pointer} must exist")
        del parent[field]


def object_at_pointer(value: dict[str, Any], pointer: str) -> dict[str, Any]:
    if not pointer:
        return value
    current: Any = value
    for part in pointer[1:].split("/"):
        part = part.replace("~1", "/").replace("~0", "~")
        current = current[int(part)] if isinstance(current, list) else current[part]
    if not isinstance(current, dict):
        raise AssertionError(f"golden pointer {pointer!r} must identify an object")
    return current


def insert_additive_field(value: dict[str, Any], pointer: str) -> None:
    target = object_at_pointer(value, pointer)
    if "future_minor_field" in target:
        raise AssertionError(f"mutation field already exists at {pointer!r}")
    target["future_minor_field"] = {"opaque": True}


def object_pointers(value: object) -> list[str]:
    pointers: list[str] = []

    def escape(part: str) -> str:
        return part.replace("~", "~0").replace("/", "~1")

    def visit(current: object, pointer: str) -> None:
        if isinstance(current, dict):
            pointers.append(pointer)
            for key, child in current.items():
                visit(child, f"{pointer}/{escape(key)}")
        elif isinstance(current, list):
            for index, child in enumerate(current):
                visit(child, f"{pointer}/{index}")

    visit(value, "")
    return pointers


def invoke_typed(client: PmuxClient, request: dict[str, Any]) -> dict[str, Any]:
    method = request["method"]
    params = request.get("params")
    if method == "ping":
        return {"type": "pong", "data": client.ping()}
    if method == "start_session":
        return {"type": "session_started", "data": client.start_session(params)}
    if method == "run_turn":
        return {
            "type": "turn_accepted",
            "data": client.run_turn(params["session_id"], params["generation_id"], params["turn"]),
        }
    if method == "cancel_turn":
        return {
            "type": "turn_cancelled",
            "data": client.cancel_turn(
                params["session_id"], params["generation_id"], params["turn_id"]
            ),
        }
    if method == "inspect_session":
        return {
            "type": "session_snapshot",
            "data": client.inspect_session(params["session_id"], params["generation_id"]),
        }
    if method == "attach_session":
        return {"type": "attach_capability", "data": client.attach_session(params)}
    if method == "close_session":
        return {
            "type": "session_closed",
            "data": client.close_session(
                params["session_id"], params["generation_id"], params["policy"]
            ),
        }
    if method == "subscribe_events":
        return {
            "type": "events",
            "data": client.subscribe_events(
                params["session_id"],
                params["generation_id"],
                after_sequence=params["after_sequence"],
                wait_ms=params["wait_ms"],
                max_events=params["max_events"],
            ),
        }
    if method == "run_once":
        return {"type": "turn_result", "data": client.run_once(params)}
    if method == "clear_session":
        return {
            "type": "session_cleared",
            "data": client.clear_session(
                params["session_id"],
                params["generation_id"],
                params["expected_transcript_session_id"],
                params.get("deadline_unix_ms"),
            ),
        }
    if method == "diagnose":
        return {"type": "diagnosis", "data": client.diagnose()}
    if method == "run_stateless":
        return {"type": "stateless_result", "data": client.run_stateless(params)}
    if method == "create_agent":
        return {"type": "agent_created", "data": client.create_agent(params["spec"])}
    if method == "get_agent":
        return {
            "type": "agent",
            "data": client.get_agent(params["agent_id"], params.get("version")),
        }
    if method == "list_agents":
        return {"type": "agent_list", "data": client.list_agents()}
    if method == "update_agent":
        return {
            "type": "agent_updated",
            "data": client.update_agent(
                params["agent_id"], params["expected_version"], params["spec"]
            ),
        }
    raise AssertionError(f"unknown golden method {method}")


class GoldenConformanceTests(unittest.TestCase):
    def test_every_typed_method_sends_exact_requests_and_accepts_results(self) -> None:
        # DERIVED FROM THE MANIFEST, never written out. A hand-written count
        # freezes the corpus at the size it had the day it was typed: deleting
        # an entry reddens it, failing to ADD one does not, and MEASURED,
        # ``run_stateless`` -- all of Path B and the only producer of
        # ``stateless_result`` -- had no golden pair in any of the three
        # languages while this client implemented and validated it. Compared by
        # NAME so the failure says which method is uncovered.
        self.assertEqual(
            sorted(exchange["method"] for exchange in GOLDEN["requests_and_results"]),
            sorted(MANIFEST["methods"]),
            "golden.json must carry one complete request/result pair for every manifest method",
        )
        handlers: list[Handler] = []
        for exchange in GOLDEN["requests_and_results"]:

            def handler(stream: socket.socket, exchange: dict[str, Any] = exchange) -> None:
                actual = read_request(stream)
                generated_id = actual["request_id"]
                parsed = uuid.UUID(generated_id)
                self.assertEqual(str(parsed), generated_id)
                normalized = {**actual, "request_id": GOLDEN["ids"]["request_id"]}
                self.assertEqual(normalized, exchange["request"], exchange["method"])
                write_json(stream, {**exchange["response"], "request_id": generated_id})

            handlers.append(handler)

        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            for exchange in GOLDEN["requests_and_results"]:
                actual = invoke_typed(client, copy.deepcopy(exchange["request"]))
                self.assertEqual(actual, exchange["response"]["result"], exchange["method"])

    def test_shared_negative_identity_schema_sequence_cursor_gap_and_safe_max_matrix(self) -> None:
        self.assertEqual(len(CASES["client_negative_matrix"]), 17)
        handlers: list[Handler] = []
        for vector in CASES["client_negative_matrix"]:

            def handler(stream: socket.socket, vector: dict[str, Any] = vector) -> None:
                request = read_request(stream)
                response = replace_request_id(
                    copy.deepcopy(vector["response"]), request["request_id"]
                )
                write_json(stream, response)

            handlers.append(handler)

        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            for vector in CASES["client_negative_matrix"]:
                request = request_for(vector["operation"])
                if vector["operation"] == "subscribe_events":
                    request["params"]["after_sequence"] = vector["after_sequence"]
                with self.assertRaises(Exception, msg=vector["id"]) as raised:
                    invoke_typed(client, request)
                error = raised.exception
                category = vector["error_category"]
                if category == "response_identity":
                    self.assertIsInstance(error, PmuxRequestIdMismatchError, vector["id"])
                elif category == "schema_version":
                    self.assertIsInstance(error, PmuxVersionError, vector["id"])
                elif category == "schema":
                    self.assertIsInstance(error, PmuxProtocolError, vector["id"])
                elif category == "result_session":
                    self.assertRegex(str(error), r"result session_id .* does not match request")
                elif category == "result_generation":
                    self.assertRegex(str(error), r"result generation_id .* does not match request")
                elif category == "result_turn":
                    self.assertRegex(str(error), r"result turn_id .* does not match request")
                elif category == "event_session":
                    self.assertRegex(str(error), r"(event|snapshot) belongs to another session")
                elif category == "event_generation":
                    self.assertRegex(
                        str(error), r"(event|snapshot) belongs to another process generation"
                    )
                elif category in {"event_sequence", "batch_cursor"}:
                    self.assertIsInstance(error, PmuxSequenceError, vector["id"])
                elif category == "replay_gap":
                    self.assertRegex(str(error), r"replay-gap|replay gap")
                elif category == "cursor_exhaustion":
                    self.assertRegex(str(error), r"safe-integer|safe integer|between 0 and")
                else:
                    self.fail(f"unknown shared category {category}")

    def test_golden_carries_one_complete_frame_for_every_manifest_event(self) -> None:
        # DERIVED FROM THE MANIFEST, exactly as the method coverage above is.
        # That fix was applied to the method half and not to the event half: the
        # Rust checker kept two hand-written ``14``s in the same file and the
        # same commit that derived the method count, and neither client asserted
        # event coverage at all -- both compared the corpus to itself. MEASURED,
        # appending ``"future_event"`` to ``manifest.events`` left every golden
        # test in all three languages green.
        self.assertEqual(
            sorted(event["type"] for event in GOLDEN["events"]),
            sorted(MANIFEST["events"]),
            "golden.json must carry one complete frame for every manifest event",
        )

    def test_durable_uuid_goldens_share_the_complete_frame_corpus(self) -> None:
        self.assertEqual(
            GOLDEN["durable_ids"]["namespace"],
            "7ec46f2d-5f29-5ebc-9ac1-925b0a76f76d",
        )
        for vector in GOLDEN["durable_ids"]["cases"]:
            self.assertEqual(
                turn_id_for_attempt(vector["attempt"]), vector["turn_id"], vector["attempt"]
            )

    def test_shared_required_inventory_rejects_every_nested_field_deletion(self) -> None:
        deletions = CASES["client_required_field_deletions"]
        # DERIVED FROM THE CORPUS: a method appended to ``golden.json`` with no
        # required-field inventory of its own must redden here rather than pass
        # by having no cases.
        self.assertEqual(len(deletions["results"]), len(GOLDEN["requests_and_results"]))
        self.assertEqual(len(deletions["events"]), len(GOLDEN["events"]))
        result_cases = [
            {"method": fields["method"], "pointer": pointer}
            for fields in deletions["results"]
            for pointer in [*deletions["result_envelope"], *fields["pointers"]]
        ]
        event_cases = [
            {"event_type": fields["event_type"], "pointer": pointer}
            for fields in deletions["events"]
            for pointer in [*deletions["event_envelope"], *fields["pointers"]]
        ]
        # 187 before ``diagnose``; its nine required result pointers plus the
        # five shared envelope pointers add fourteen. 201 before
        # ``run_stateless``, whose twenty required result pointers plus the same
        # five add twenty-five. 226 before the four agent methods: three
        # descriptors of six plus five, and ``agent_list``'s six plus five.
        self.assertEqual(len(result_cases), 270)
        self.assertEqual(len(event_cases), 223)
        self.assertEqual(len(deletions["error"]), 6)
        handlers: list[Handler] = []
        for deletion in result_cases:

            def result_handler(stream: socket.socket, deletion: dict[str, Any] = deletion) -> None:
                request = read_request(stream)
                exchange = next(
                    candidate
                    for candidate in GOLDEN["requests_and_results"]
                    if candidate["method"] == deletion["method"]
                )
                response = copy.deepcopy(exchange["response"])
                response["request_id"] = request["request_id"]
                remove_pointer(response, deletion["pointer"])
                write_json(stream, response)

            handlers.append(result_handler)

        for deletion in event_cases:

            def event_handler(stream: socket.socket, deletion: dict[str, Any] = deletion) -> None:
                request = read_request(stream)
                frame = copy.deepcopy(
                    next(
                        candidate["frame"]
                        for candidate in GOLDEN["events"]
                        if candidate["type"] == deletion["event_type"]
                    )
                )
                frame["sequence"] = 1
                remove_pointer(frame, deletion["pointer"])
                write_json(
                    stream,
                    {
                        "version": 1,
                        "request_id": request["request_id"],
                        "result": {
                            "type": "events",
                            "data": {"events": [frame], "next_sequence": 2},
                        },
                    },
                )

            handlers.append(event_handler)

        for pointer in deletions["error"]:

            def error_handler(stream: socket.socket, pointer: str = pointer) -> None:
                request = read_request(stream)
                response = copy.deepcopy(GOLDEN["error"])
                response["request_id"] = request["request_id"]
                remove_pointer(response, pointer)
                write_json(stream, response)

            handlers.append(error_handler)
        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            for deletion in result_cases:
                with self.assertRaises(
                    PmuxProtocolError, msg=f"{deletion['method']} {deletion['pointer']}"
                ):
                    invoke_typed(client, request_for(deletion["method"]))
            subscription = request_for("subscribe_events")
            subscription["params"]["after_sequence"] = 0
            for deletion in event_cases:
                with self.assertRaises(
                    PmuxProtocolError,
                    msg=f"{deletion['event_type']} {deletion['pointer']}",
                ):
                    invoke_typed(client, copy.deepcopy(subscription))
            for pointer in deletions["error"]:
                with self.assertRaises(PmuxProtocolError, msg=f"error {pointer}"):
                    client.ping()

    def test_shared_goldens_accept_additions_at_every_object_boundary(self) -> None:
        successes: list[dict[str, Any]] = []
        result_boundaries = 0
        for exchange in GOLDEN["requests_and_results"]:
            for pointer in object_pointers(exchange["response"]):
                result_boundaries += 1
                response = copy.deepcopy(exchange["response"])
                insert_additive_field(response, pointer)
                successes.append(
                    {
                        "label": f"{exchange['method']} {pointer!r}",
                        "request": request_for(exchange["method"]),
                        "response": response,
                    }
                )
        # 58 before ``diagnose``; its exchange adds six object boundaries -- the
        # envelope, ``result``, ``result/data``, ``result/data/runtime``, and one
        # per entry of ``result/data/sessions`` -- and every one must still
        # tolerate an unknown field, because response DTOs evolve additively. 64
        # before ``run_stateless``, whose exchange adds eight: the envelope,
        # ``result``, ``result/data``, ``result/data/stop_reason``,
        # ``result/data/usage``, and one per usage scope. 72 before the agent
        # methods, whose four exchanges add 46; the echoed ``spec`` is OPAQUE on
        # a response, so its boundaries are additive like every other.
        self.assertEqual(result_boundaries, 118, "review new result object boundaries")

        event_boundaries = 0
        subscription = request_for("subscribe_events")
        subscription["params"]["after_sequence"] = 0
        for event in GOLDEN["events"]:
            base_frame = copy.deepcopy(event["frame"])
            base_frame["sequence"] = 1
            for pointer in object_pointers(base_frame):
                event_boundaries += 1
                frame = copy.deepcopy(base_frame)
                insert_additive_field(frame, pointer)
                successes.append(
                    {
                        "label": f"{event['type']} {pointer!r}",
                        "request": copy.deepcopy(subscription),
                        "response": {
                            "version": 1,
                            "request_id": GOLDEN["ids"]["request_id"],
                            "result": {
                                "type": "events",
                                "data": {"events": [frame], "next_sequence": 2},
                            },
                        },
                    }
                )
        self.assertEqual(event_boundaries, 67, "review new event object boundaries")

        additive_errors: list[dict[str, Any]] = []
        for pointer in object_pointers(GOLDEN["error"]):
            response = copy.deepcopy(GOLDEN["error"])
            insert_additive_field(response, pointer)
            additive_errors.append({"label": f"error {pointer!r}", "response": response})
        self.assertEqual(len(additive_errors), 3, "review new error object boundaries")

        handlers: list[Handler] = []
        for mutation in [*successes, *additive_errors]:

            def handler(stream: socket.socket, mutation: dict[str, Any] = mutation) -> None:
                request = read_request(stream)
                response = copy.deepcopy(mutation["response"])
                response["request_id"] = request["request_id"]
                write_json(stream, response)

            handlers.append(handler)

        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            for mutation in successes:
                try:
                    invoke_typed(client, copy.deepcopy(mutation["request"]))
                except Exception as error:  # pragma: no cover - assertion detail
                    self.fail(f"{mutation['label']} rejected additive field: {error}")
            for mutation in additive_errors:
                with self.assertRaises(PmuxServerError, msg=mutation["label"]):
                    client.ping()

    def test_reserved_turn_leases_are_sent_then_surface_unsupported_feature(self) -> None:
        reserved = CASES["reserved_turn_lease_cases"]
        self.assertEqual(
            reserved["expected_error"], {"code": "unsupported_feature", "retryable": False}
        )
        self.assertEqual(len(reserved["cases"]), 6)
        requests: list[dict[str, Any]] = []
        for vector in reserved["cases"]:
            request = request_for(vector["operation"])
            request["params"]["turn"]["lease"] = copy.deepcopy(vector["lease"])
            requests.append({**vector, "request": request})

        handlers: list[Handler] = []
        for vector in requests:

            def handler(stream: socket.socket, vector: dict[str, Any] = vector) -> None:
                actual = read_request(stream)
                self.assertEqual(
                    {**actual, "request_id": GOLDEN["ids"]["request_id"]},
                    vector["request"],
                    vector["id"],
                )
                write_json(
                    stream,
                    {
                        "version": 1,
                        "request_id": actual["request_id"],
                        "error": {
                            **reserved["expected_error"],
                            "message": (
                                "reserved turn lease values require a future leased connection API"
                            ),
                        },
                    },
                )

            handlers.append(handler)

        with FakeServer(handlers) as server:
            client = PmuxClient(server.path)
            for vector in requests:
                with self.assertRaises(PmuxServerError, msg=vector["id"]) as raised:
                    invoke_typed(client, copy.deepcopy(vector["request"]))
                self.assertEqual(raised.exception.code, "unsupported_feature", vector["id"])
                self.assertFalse(raised.exception.retryable, vector["id"])


class ValueEnumConformanceTest(unittest.TestCase):
    def test_shared_manifest_value_enums_match_the_python_literals(self) -> None:
        expected = {name: list(values) for name, values in MANIFEST["value_enums"].items()}
        actual = {name: list(values) for name, values in protocol_module.V1_VALUE_ENUMS.items()}
        self.assertEqual(expected, actual)

    def test_every_python_string_literal_alias_is_pinned(self) -> None:
        # ``PmuxErrorCode`` is pinned by the manifest's ``error_codes`` list instead.
        declared = {
            name
            for name, value in vars(protocol_module).items()
            if not name.startswith("_")
            and get_origin(value) is Literal
            and all(isinstance(arg, str) for arg in get_args(value))
        } - {"PmuxErrorCode"}
        self.assertEqual(declared, set(protocol_module.V1_VALUE_ENUMS))


class TaggedUnionConformanceTest(unittest.TestCase):
    def test_shared_manifest_tagged_unions_match_the_python_typed_dicts(self) -> None:
        self.assertEqual(MANIFEST["tagged_unions"], protocol_module.V1_TAGGED_UNIONS)

    def test_every_python_tagged_union_alias_is_pinned(self) -> None:
        """A seventh union added to ``protocol.py`` and to nothing else.

        The six above are a hand-written list, and this is what derives it: any
        module-level alias whose members are all ``TypedDict``s is an internally
        tagged union of this wire surface, and the manifest is the only thing
        the Rust and TypeScript sides see it through.
        """
        declared = {
            name
            for name, value in vars(protocol_module).items()
            if not name.startswith("_")
            and get_args(value)
            and all(is_typeddict(member) for member in get_args(value))
        }
        self.assertTrue(declared, "the scan found no union of TypedDicts at all")
        self.assertEqual(declared, set(protocol_module.V1_TAGGED_UNIONS))

    def test_message_block_validation_covers_every_pinned_variant(self) -> None:
        """The runtime validator's domain is the union's variant list, so a
        variant pinned and not validated must say so rather than be checked
        against the last branch's shape."""
        for variant in MANIFEST["tagged_unions"]["MessageBlock"]["variants"]:
            with self.subTest(variant=variant), self.assertRaises(PmuxProtocolError) as raised:
                client_module._validate_message_block({"kind": variant}, "turn.final_blocks[0]")
            self.assertNotIn("does not validate", str(raised.exception), variant)


if __name__ == "__main__":
    unittest.main()
