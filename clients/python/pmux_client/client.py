"""Synchronous native pmux protocol-v1 Unix-socket client."""

from __future__ import annotations

import json
import math
import os
import re
import socket
import struct
import threading
import time
import uuid
from collections import deque
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from os import PathLike
from typing import Any, Final, cast, get_args

from .protocol import (
    MAX_NATIVE_FRAME_BYTES,
    MAX_SAFE_JSON_INTEGER,
    PROTOCOL_VERSION,
    V1_TAGGED_UNIONS,
    V1_VALUE_ENUMS,
    AgentDescriptor,
    AgentList,
    AgentSpec,
    AttachCapability,
    AttachSessionRequest,
    CancelTurnResult,
    ClearSessionResult,
    ClosePolicy,
    CloseSessionResult,
    DaemonDiagnosis,
    ErrorBody,
    EventBatch,
    EventEnvelope,
    HealthLayerName,
    Pong,
    ReplayGap,
    ResponseResult,
    RunOnceRequest,
    RunStatelessRequest,
    SessionGenerationId,
    SessionHandle,
    SessionId,
    SessionSnapshot,
    StartSessionRequest,
    StatelessResult,
    TurnAccepted,
    TurnId,
    TurnRequest,
    TurnResult,
)

DEFAULT_MAX_FRAME_BYTES: Final = MAX_NATIVE_FRAME_BYTES
DEFAULT_CONNECT_TIMEOUT: Final = 5.0
DEFAULT_REQUEST_TIMEOUT: Final = 45.0
DEFAULT_RUN_ONCE_TIMEOUT: Final = 15 * 60.0
RUN_ONCE_RESPONSE_MARGIN: Final = 120.0
CANONICAL_UUID_PATTERN: Final = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)
KNOWN_ERROR_CODES: Final = frozenset(
    {
        "invalid_config",
        "unsupported_feature",
        "unsupported_claude_version",
        "claude_not_found",
        "rmux_unavailable",
        "rmux_incompatible",
        "persistence_disabled",
        "transcript_unavailable",
        "schema_drift",
        "prompt_not_acknowledged",
        "result_too_large",
        "turn_history_capacity_exceeded",
        "session_busy",
        "id_conflict",
        "id_collision",
        "session_not_found",
        "stale_session_generation",
        "needs_trust",
        "needs_login",
        "needs_permission",
        "needs_update",
        "needs_input",
        "rate_limited",
        "authentication_failed",
        "billing_failed",
        "permission_denied",
        "turn_timeout",
        "cancelled",
        "recovery_failed",
        "claude_exited",
        "daemon_lost",
        "replay_gap",
        "protocol_version_mismatch",
        "internal",
    }
)


def _timeout_for(
    method: str,
    params: dict[str, Any] | None,
    configured: float,
    *,
    now: float | None = None,
) -> float:
    if method == "subscribe_events" and params is not None:
        return max(configured, float(params.get("wait_ms", 0)) / 1000 + 5)
    if method == "run_once" and params is not None:
        turn = cast(dict[str, Any], params.get("turn", {}))
        deadline = turn.get("deadline_unix_ms")
        turn_window = (
            DEFAULT_RUN_ONCE_TIMEOUT
            if deadline is None
            else max(0.0, float(deadline) / 1000 - (time.time() if now is None else now))
            + RUN_ONCE_RESPONSE_MARGIN
        )
        return max(configured, turn_window)
    # Same shape as run_once, and for a stronger reason: a stateless call may
    # have to mint a cold class (TUI launch) before the model is asked. The
    # default 45s request timeout gave up first.
    if method == "run_stateless" and params is not None:
        deadline = params.get("deadline_unix_ms")
        answer_window = (
            DEFAULT_RUN_ONCE_TIMEOUT
            if deadline is None
            else max(0.0, float(deadline) / 1000 - (time.time() if now is None else now))
            + RUN_ONCE_RESPONSE_MARGIN
        )
        return max(configured, answer_window)
    # A caller-supplied submission deadline widens the client's patience the
    # same way ``run_once`` does, so asking for a longer input window cannot
    # make the client give up while the daemon is still typing.
    if method == "clear_session" and params is not None:
        deadline = params.get("deadline_unix_ms")
        if deadline is None:
            return configured
        clear_window = (
            max(0.0, float(deadline) / 1000 - (time.time() if now is None else now))
            + RUN_ONCE_RESPONSE_MARGIN
        )
        return max(configured, clear_window)
    return configured


class PmuxError(Exception):
    """Base class for native pmux client failures."""


class PmuxTransportError(PmuxError):
    """Unix socket I/O failed."""


class PmuxTimeoutError(PmuxTransportError):
    def __init__(self, operation: str, timeout: float) -> None:
        super().__init__(f"{operation} timed out after {timeout}s")
        self.operation = operation
        self.timeout = timeout


class PmuxProtocolError(PmuxError):
    """A frame or protocol invariant was invalid."""


class PmuxFrameTooLargeError(PmuxProtocolError):
    def __init__(self, advertised: int, maximum: int) -> None:
        super().__init__(f"frame size {advertised} exceeds configured maximum {maximum}")
        self.advertised = advertised
        self.maximum = maximum


class PmuxVersionError(PmuxProtocolError):
    def __init__(self, actual: object) -> None:
        super().__init__(f"unsupported protocol version {actual!r}; expected {PROTOCOL_VERSION}")
        self.expected = PROTOCOL_VERSION
        self.actual = actual


class PmuxRequestIdMismatchError(PmuxProtocolError):
    def __init__(self, expected: str, actual: object) -> None:
        super().__init__(f"response request id {actual!r} does not match {expected}")
        self.expected = expected
        self.actual = actual


class PmuxUnexpectedResultError(PmuxProtocolError):
    def __init__(self, expected: str, actual: object) -> None:
        super().__init__(f"pmuxd returned {actual!r}, expected {expected}")
        self.expected = expected
        self.actual = actual


class PmuxServerError(PmuxError):
    def __init__(self, body: ErrorBody) -> None:
        super().__init__(f"pmuxd error {body['code']}: {body['message']}")
        self.body = body
        self.code = body["code"]
        self.retryable = body["retryable"]
        self.details = body.get("details")


class PmuxSequenceError(PmuxProtocolError):
    def __init__(self, expected: int, actual: int) -> None:
        super().__init__(f"invalid event sequence {actual}; expected {expected}")
        self.expected = expected
        self.actual = actual


@dataclass(frozen=True, slots=True)
class EventItem:
    event: EventEnvelope
    kind: str = "event"


@dataclass(frozen=True, slots=True)
class ReplayGapItem:
    gap: ReplayGap
    kind: str = "replay_gap"


EventStreamItem = EventItem | ReplayGapItem


class PmuxClient:
    """Client bound to one caller-supplied Unix-domain socket path.

    The product one-shot on this socket is ``run_stateless``. Interactive
    session methods remain compiled and are refused by current daemons.

    Every operation creates a fresh connection. There is no daemon discovery,
    auto-start, HTTP fallback, subprocess invocation, or claude-p integration.
    """

    def __init__(
        self,
        socket_path: str | PathLike[str],
        *,
        max_frame_bytes: int = DEFAULT_MAX_FRAME_BYTES,
        connect_timeout: float = DEFAULT_CONNECT_TIMEOUT,
        request_timeout: float = DEFAULT_REQUEST_TIMEOUT,
    ) -> None:
        path = str(socket_path)
        if not path:
            raise ValueError("socket_path must not be empty")
        if not os.path.isabs(path):
            raise ValueError("socket_path must be absolute")
        if (
            not isinstance(max_frame_bytes, int)
            or isinstance(max_frame_bytes, bool)
            or not 1 <= max_frame_bytes <= MAX_NATIVE_FRAME_BYTES
        ):
            raise ValueError(f"max_frame_bytes must be between 1 and {MAX_NATIVE_FRAME_BYTES}")
        if connect_timeout <= 0 or request_timeout <= 0:
            raise ValueError("timeouts must be greater than zero")
        self.socket_path = path
        self.max_frame_bytes = max_frame_bytes
        self.connect_timeout = connect_timeout
        self.request_timeout = request_timeout

    def request(self, method: str, params: dict[str, Any] | None = None) -> ResponseResult:
        _validate_request_identities(method, params)
        request_id = str(uuid.uuid4())
        envelope: dict[str, Any] = {
            "version": PROTOCOL_VERSION,
            "request_id": request_id,
            "method": method,
        }
        if params is not None:
            envelope["params"] = params
        try:
            _validate_json_numeric_domain(envelope, "request")
            payload = json.dumps(
                envelope,
                separators=(",", ":"),
                ensure_ascii=False,
                allow_nan=False,
            ).encode()
        except (TypeError, ValueError) as error:
            raise PmuxProtocolError("request is not JSON serializable") from error
        self._ensure_frame_size(len(payload))

        response_timeout = _timeout_for(method, params, self.request_timeout)

        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.settimeout(self.connect_timeout)
            try:
                stream.connect(self.socket_path)
            except TimeoutError as error:
                raise PmuxTimeoutError("connect", self.connect_timeout) from error
            except OSError as error:
                raise PmuxTransportError(f"socket connect failed: {error}") from error

            stream.settimeout(response_timeout)
            try:
                stream.sendall(struct.pack("!I", len(payload)))
                stream.sendall(payload)
                header = self._read_exact(stream, 4)
                advertised = struct.unpack("!I", header)[0]
                self._ensure_frame_size(advertised)
                response_payload = self._read_exact(stream, advertised)
            except TimeoutError as error:
                raise PmuxTimeoutError("request", response_timeout) from error
            except PmuxError:
                raise
            except OSError as error:
                raise PmuxTransportError(f"socket request failed: {error}") from error

        result = self._decode_response(response_payload, request_id)
        _validate_result_for_request(method, params, result)
        return result

    def ping(self) -> Pong:
        return cast(Pong, self._expect(self.request("ping"), "pong"))

    def diagnose(self) -> DaemonDiagnosis:
        """Completes one real operation against the daemon's private runtime.

        Costs one rmux round trip in the daemon regardless of how many sessions
        it holds, and never starts a Claude turn. ``ping`` cannot answer any of
        this: it is served without touching the private runtime, the session
        registry, the launch broker or the rmux sidecar.
        """
        return cast(DaemonDiagnosis, self._expect(self.request("diagnose"), "diagnosis"))

    def create_agent(self, spec: AgentSpec) -> AgentDescriptor:
        """Protocol method kept for goldens.

        Current daemons refuse every agent Request with
        ``session_surface_removed``. Do not build new callers on this.
        """
        return cast(
            AgentDescriptor,
            self._expect(
                self.request("create_agent", cast(dict[str, Any], {"spec": spec})),
                "agent_created",
            ),
        )

    def get_agent(self, agent_id: str, version: int | None = None) -> AgentDescriptor:
        """Protocol method kept for goldens.

        Current daemons refuse every agent Request with
        ``session_surface_removed``. Do not build new callers on this.

        Reads one stored agent version, or the current head.

        Environment values and inline settings/MCP document bodies come back as
        ``sha256:`` digests and never in the clear; ``config_digest`` still
        identifies the configuration exactly.
        """
        params: dict[str, Any] = {"agent_id": agent_id}
        if version is not None:
            params["version"] = version
        return cast(
            AgentDescriptor,
            self._expect(self.request("get_agent", params), "agent"),
        )

    def list_agents(self) -> AgentList:
        """Protocol method kept for goldens.

        Current daemons refuse every agent Request with
        ``session_surface_removed``. Do not build new callers on this.

        Lists every stored agent's id, current version, digest, name and
        cell. Deliberately not full specs."""
        return cast(
            AgentList,
            self._expect(self.request("list_agents", {}), "agent_list"),
        )

    def update_agent(
        self, agent_id: str, expected_version: int, spec: AgentSpec
    ) -> AgentDescriptor:
        """Protocol method kept for goldens.

        Current daemons refuse every agent Request with
        ``session_surface_removed``. Do not build new callers on this.

        Stores a new immutable version of one agent and returns it.

        ``expected_version`` is a fence: any value that is not the current head
        is refused with ``id_conflict``, including one stale by exactly one
        revision, and no update is ever answered as "already landed". ``spec``
        is a COMPLETE replacement. Running sessions are unaffected -- each
        pinned its version at start.
        """
        return cast(
            AgentDescriptor,
            self._expect(
                self.request(
                    "update_agent",
                    cast(
                        dict[str, Any],
                        {
                            "agent_id": agent_id,
                            "expected_version": expected_version,
                            "spec": spec,
                        },
                    ),
                ),
                "agent_updated",
            ),
        )

    def run_stateless(self, request: RunStatelessRequest) -> StatelessResult:
        """Unix-socket one-shot: ``(model, effort, prompt)`` in, text and usage out.

        THE CALLER NAMES NO RESOURCE. ``RunStatelessRequest`` carries a model, an
        optional effort, a prompt and an optional deadline, and nothing else: no
        cwd, no configuration root, no system prompt and no session id. The
        daemon mints every one of those from its own configuration plus a slot
        identity, and the request DTO is ``deny_unknown_fields`` on the Rust
        side, so a caller that believes it set one of them is told so rather than
        silently not having set it.
        """
        return cast(
            StatelessResult,
            self._expect(
                self.request("run_stateless", cast(dict[str, Any], request)),
                "stateless_result",
            ),
        )

    def start_session(self, request: StartSessionRequest) -> SessionHandle:
        """Protocol method kept for goldens.

        Current daemons refuse ``start_session`` with
        ``session_surface_removed``.
        """
        return cast(
            SessionHandle,
            self._expect(
                self.request("start_session", cast(dict[str, Any], request)),
                "session_started",
            ),
        )

    def inspect_session(
        self, session_id: SessionId, generation_id: SessionGenerationId
    ) -> SessionSnapshot:
        """Protocol method kept for goldens.

        Current daemons refuse ``inspect_session`` with
        ``session_surface_removed``.
        """
        return cast(
            SessionSnapshot,
            self._expect(
                self.request(
                    "inspect_session",
                    {"session_id": session_id, "generation_id": generation_id},
                ),
                "session_snapshot",
            ),
        )

    def run_turn(
        self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        turn: TurnRequest,
    ) -> TurnAccepted:
        """Protocol method kept for goldens.

        Current daemons refuse ``run_turn`` with ``session_surface_removed``.
        """
        return cast(
            TurnAccepted,
            self._expect(
                self.request(
                    "run_turn",
                    {
                        "session_id": session_id,
                        "generation_id": generation_id,
                        "turn": turn,
                    },
                ),
                "turn_accepted",
            ),
        )

    def cancel_turn(
        self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        turn_id: TurnId,
    ) -> CancelTurnResult:
        """Protocol method kept for goldens.

        Current daemons refuse ``cancel_turn`` with ``session_surface_removed``.
        """
        return cast(
            CancelTurnResult,
            self._expect(
                self.request(
                    "cancel_turn",
                    {
                        "session_id": session_id,
                        "generation_id": generation_id,
                        "turn_id": turn_id,
                    },
                ),
                "turn_cancelled",
            ),
        )

    def attach_session(self, request: AttachSessionRequest) -> AttachCapability:
        """Protocol method kept for goldens.

        Current daemons refuse ``attach_session`` with
        ``session_surface_removed``.
        """
        return cast(
            AttachCapability,
            self._expect(
                self.request("attach_session", cast(dict[str, Any], request)),
                "attach_capability",
            ),
        )

    def close_session(
        self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        policy: ClosePolicy = "graceful",
    ) -> CloseSessionResult:
        """Protocol method kept for goldens.

        Current daemons refuse ``close_session`` with
        ``session_surface_removed``.
        """
        return cast(
            CloseSessionResult,
            self._expect(
                self.request(
                    "close_session",
                    {
                        "session_id": session_id,
                        "generation_id": generation_id,
                        "policy": policy,
                    },
                ),
                "session_closed",
            ),
        )

    def clear_session(
        self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        expected_transcript_session_id: SessionId,
        deadline_unix_ms: int | None = None,
    ) -> ClearSessionResult:
        """Protocol method kept for goldens.

        Current daemons refuse ``clear_session`` with
        ``session_surface_removed``.

        Living recovery for a Messages lease is ``x-pmux-cell`` /
        ``pmux doctor`` conversation leases.
        """
        params: dict[str, Any] = {
            "session_id": session_id,
            "generation_id": generation_id,
            "expected_transcript_session_id": expected_transcript_session_id,
        }
        if deadline_unix_ms is not None:
            params["deadline_unix_ms"] = deadline_unix_ms
        return cast(
            ClearSessionResult,
            self._expect(self.request("clear_session", params), "session_cleared"),
        )

    def run_once(self, request: RunOnceRequest) -> TurnResult:
        """Protocol method kept for goldens.

        Current daemons refuse ``run_once`` with ``session_surface_removed``.
        """
        return cast(
            TurnResult,
            self._expect(self.request("run_once", cast(dict[str, Any], request)), "turn_result"),
        )

    def subscribe_events(
        self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        *,
        after_sequence: int = 0,
        wait_ms: int = 0,
        max_events: int = 0,
    ) -> EventBatch:
        """Protocol method kept for goldens.

        Current daemons refuse ``subscribe_events`` with
        ``session_surface_removed``.
        """
        result = cast(
            EventBatch,
            self._expect(
                self.request(
                    "subscribe_events",
                    {
                        "session_id": session_id,
                        "generation_id": generation_id,
                        "after_sequence": after_sequence,
                        "wait_ms": wait_ms,
                        "max_events": max_events,
                    },
                ),
                "events",
            ),
        )
        _validate_batch(session_id, generation_id, after_sequence, result)
        return result

    def events(
        self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        *,
        after_sequence: int = 0,
        wait_ms: int = 30_000,
        max_events: int = 128,
        reconnect_delay: float = 0.25,
        max_reconnect_attempts: int | None = None,
        stop_event: threading.Event | None = None,
        on_reconnect: Callable[[PmuxTransportError, int, int], None] | None = None,
    ) -> EventSubscription:
        """Protocol method kept for goldens.

        Current daemons refuse ``subscribe_events`` with
        ``session_surface_removed``.
        """
        return EventSubscription(
            self,
            session_id,
            generation_id,
            after_sequence=after_sequence,
            wait_ms=wait_ms,
            max_events=max_events,
            reconnect_delay=reconnect_delay,
            max_reconnect_attempts=max_reconnect_attempts,
            stop_event=stop_event,
            on_reconnect=on_reconnect,
        )

    def _ensure_frame_size(self, size: int) -> None:
        if size > self.max_frame_bytes:
            raise PmuxFrameTooLargeError(size, self.max_frame_bytes)

    @staticmethod
    def _read_exact(stream: socket.socket, size: int) -> bytes:
        chunks = bytearray(size)
        view = memoryview(chunks)
        offset = 0
        while offset < size:
            received = stream.recv_into(view[offset:])
            if received == 0:
                raise PmuxTransportError("socket closed before a complete frame")
            offset += received
        return bytes(chunks)

    @staticmethod
    def _expect(result: ResponseResult, expected: str) -> Any:
        actual = result.get("type")
        if actual != expected:
            raise PmuxUnexpectedResultError(expected, actual)
        if "data" not in result:
            raise PmuxProtocolError("response result is missing data")
        return result["data"]

    @staticmethod
    def _decode_response(payload: bytes, request_id: str) -> ResponseResult:
        try:
            decoded = json.loads(payload, parse_constant=_reject_json_constant)
        except (UnicodeDecodeError, ValueError) as error:
            raise PmuxProtocolError("pmuxd returned invalid JSON") from error
        _validate_json_numeric_domain(decoded, "response")
        if not isinstance(decoded, dict):
            raise PmuxProtocolError("pmuxd response must be an object")
        version = decoded.get("version")
        if not isinstance(version, int) or isinstance(version, bool) or version != PROTOCOL_VERSION:
            raise PmuxVersionError(version)
        response_request_id = _require_uuid_value(decoded.get("request_id"), "response.request_id")
        if not _same_uuid(response_request_id, request_id):
            raise PmuxRequestIdMismatchError(request_id, response_request_id)
        # Do not exact-key-check v1 responses. Additive object fields are a
        # compatible minor evolution; required fields, result/error exclusivity,
        # typed-method discriminants, and the major remain strict.
        has_result = "result" in decoded
        has_error = "error" in decoded
        if has_result == has_error:
            raise PmuxProtocolError("response must contain exactly one of result or error")
        if has_error:
            error_body = _validate_error(decoded["error"], "response.error")
            raise PmuxServerError(cast(ErrorBody, error_body))
        result = decoded["result"]
        _validate_result(result)
        return cast(ResponseResult, result)


class EventSubscription(Iterator[EventStreamItem]):
    """Strict cursor-preserving long-poll iterator.

    Transport failures reconnect without advancing ``after_sequence``. A replay
    loss is returned as :class:`ReplayGapItem`, never hidden or auto-reconciled.
    """

    def __init__(
        self,
        client: PmuxClient,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        *,
        after_sequence: int,
        wait_ms: int,
        max_events: int,
        reconnect_delay: float,
        max_reconnect_attempts: int | None,
        stop_event: threading.Event | None,
        on_reconnect: Callable[[PmuxTransportError, int, int], None] | None,
    ) -> None:
        _require_sequence(after_sequence, "after_sequence")
        self.client = client
        self.session_id = session_id
        self.generation_id = generation_id
        self.after_sequence = after_sequence
        self.wait_ms = wait_ms
        self.max_events = max_events
        self.reconnect_delay = reconnect_delay
        self.max_reconnect_attempts = max_reconnect_attempts
        self.stop_event = stop_event or threading.Event()
        self.on_reconnect = on_reconnect
        self._pending: deque[tuple[EventStreamItem, int]] = deque()
        self._closed = False

    def __iter__(self) -> EventSubscription:
        return self

    def __next__(self) -> EventStreamItem:
        while True:
            if self._closed or self.stop_event.is_set():
                raise StopIteration
            if self._pending:
                item, cursor = self._pending.popleft()
                self.after_sequence = cursor
                return item

            reconnect_attempts = 0
            while True:
                try:
                    batch = self.client.subscribe_events(
                        self.session_id,
                        self.generation_id,
                        after_sequence=self.after_sequence,
                        wait_ms=self.wait_ms,
                        max_events=self.max_events,
                    )
                    break
                except PmuxTransportError as error:
                    if (
                        self.max_reconnect_attempts is not None
                        and reconnect_attempts >= self.max_reconnect_attempts
                    ):
                        raise
                    reconnect_attempts += 1
                    if self.on_reconnect is not None:
                        self.on_reconnect(error, reconnect_attempts, self.after_sequence)
                    delay = min(self.reconnect_delay * (2 ** min(reconnect_attempts - 1, 8)), 30.0)
                    if self.stop_event.wait(delay):
                        raise StopIteration from None

            cursor = self.after_sequence
            gap = batch.get("replay_gap")
            if gap is not None:
                cursor = gap["snapshot"]["last_sequence"]
                self._pending.append((ReplayGapItem(gap), cursor))
            for event in batch.get("events", []):
                cursor = event["sequence"]
                self._pending.append((EventItem(event), cursor))
            if not self._pending and self.stop_event.wait(0.025):
                raise StopIteration

    def close(self) -> None:
        self._closed = True
        self.stop_event.set()


def _reject_json_constant(value: str) -> None:
    raise ValueError(f"non-standard JSON constant {value} is not permitted")


def _validate_json_numeric_domain(value: object, field: str) -> None:
    if value is None or isinstance(value, bool | str):
        return
    if isinstance(value, int):
        if not -MAX_SAFE_JSON_INTEGER <= value <= MAX_SAFE_JSON_INTEGER:
            raise PmuxProtocolError(f"{field} integer is outside the signed safe-integer range")
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise PmuxProtocolError(f"{field} must not contain a non-finite number")
        if value.is_integer() and abs(value) > MAX_SAFE_JSON_INTEGER:
            raise PmuxProtocolError(f"{field} integer is outside the signed safe-integer range")
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            _validate_json_numeric_domain(item, f"{field}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            _validate_json_numeric_domain(item, f"{field}.{key}")


def _require_uuid_value(value: object, field: str) -> str:
    if not isinstance(value, str) or CANONICAL_UUID_PATTERN.fullmatch(value) is None:
        raise PmuxProtocolError(f"{field} must be a canonical UUID")
    return value


def _require_uuid(record: dict[str, Any], key: str, field: str) -> str:
    return _require_uuid_value(_require_field(record, key, field), f"{field}.{key}")


def _same_uuid(left: str, right: str) -> bool:
    return left.casefold() == right.casefold()


def _validate_start_request_identity(value: object, field: str) -> None:
    request = _require_record(value, field)
    identity = _require_record(_require_field(request, "identity", field), f"{field}.identity")
    if "session_id" in identity:
        _require_uuid(identity, "session_id", f"{field}.identity")
    elif identity.get("mode") == "resume":
        raise PmuxProtocolError(f"{field}.identity.session_id is required")


def _validate_turn_request_identity(value: object, field: str) -> None:
    turn = _require_record(value, field)
    _require_uuid(turn, "turn_id", field)


def _validate_turn_request_numbers(value: object, field: str) -> None:
    turn = _require_record(value, field)
    _optional_sequence(turn, "deadline_unix_ms", field)
    if "lease" in turn:
        lease = _require_record(turn["lease"], f"{field}.lease")
        _optional_sequence(lease, "heartbeat_timeout_ms", f"{field}.lease")


def _validate_start_request_numbers(value: object, field: str) -> None:
    request = _require_record(value, field)
    if "terminal" in request:
        terminal = _require_record(request["terminal"], f"{field}.terminal")
        _require_int(terminal, "rows", f"{field}.terminal")
        _require_int(terminal, "cols", f"{field}.terminal")
    if "lifecycle" in request:
        lifecycle = _require_record(request["lifecycle"], f"{field}.lifecycle")
        _optional_sequence(lifecycle, "hook_timeout_ms", f"{field}.lifecycle")
    if "retention" in request:
        retention = _require_record(request["retention"], f"{field}.retention")
        _optional_sequence(retention, "idle_ttl_ms", f"{field}.retention")


def _validate_request_identities(method: str, params: dict[str, Any] | None) -> None:
    if method == "ping":
        return
    if method == "start_session":
        _validate_start_request_identity(params, "request.params")
        _validate_start_request_numbers(params, "request.params")
        return
    if method == "run_once":
        request = _require_record(params, "request.params")
        _validate_start_request_identity(
            _require_field(request, "session", "request.params"),
            "request.params.session",
        )
        _validate_start_request_numbers(
            _require_field(request, "session", "request.params"),
            "request.params.session",
        )
        _validate_turn_request_identity(
            _require_field(request, "turn", "request.params"),
            "request.params.turn",
        )
        _validate_turn_request_numbers(
            _require_field(request, "turn", "request.params"),
            "request.params.turn",
        )
        return
    if method not in {
        "run_turn",
        "cancel_turn",
        "inspect_session",
        "attach_session",
        "close_session",
        "subscribe_events",
    }:
        return

    request = _require_record(params, "request.params")
    _require_uuid(request, "session_id", "request.params")
    _require_uuid(request, "generation_id", "request.params")
    if method == "run_turn":
        _validate_turn_request_identity(
            _require_field(request, "turn", "request.params"),
            "request.params.turn",
        )
        _validate_turn_request_numbers(
            _require_field(request, "turn", "request.params"),
            "request.params.turn",
        )
    elif method == "cancel_turn":
        _require_uuid(request, "turn_id", "request.params")
    elif method == "attach_session" and "size" in request:
        size = _require_record(request["size"], "request.params.size")
        _require_int(size, "rows", "request.params.size")
        _require_int(size, "cols", "request.params.size")
    elif method == "subscribe_events":
        _optional_sequence(request, "after_sequence", "request.params")
        _optional_sequence(request, "wait_ms", "request.params")
        _optional_sequence(request, "max_events", "request.params")


def _require_matching_identity(actual: object, expected: object, field: str) -> None:
    actual_uuid = _require_uuid_value(actual, field)
    expected_uuid = _require_uuid_value(expected, f"request {field}")
    if not _same_uuid(actual_uuid, expected_uuid):
        raise PmuxProtocolError(f"{field} {actual_uuid} does not match request {expected_uuid}")


def _expected_start_session_id(request: object) -> object | None:
    request_record = _require_record(request, "request.params")
    identity = _require_record(
        _require_field(request_record, "identity", "request.params"),
        "request.params.identity",
    )
    return identity.get("session_id")


def _validate_result_for_request(
    method: str,
    params: dict[str, Any] | None,
    result: ResponseResult,
) -> None:
    result_type = result.get("type")
    data = _require_record(result.get("data"), "response.result.data")
    if method == "ping" and result_type == "pong":
        if data.get("protocol_version") != PROTOCOL_VERSION:
            raise PmuxVersionError(data.get("protocol_version"))
        return
    if params is None:
        return
    if method == "start_session" and result_type == "session_started":
        expected_session = _expected_start_session_id(params)
        if expected_session is not None:
            _require_matching_identity(
                data.get("session_id"), expected_session, "result session_id"
            )
    elif method == "run_turn" and result_type == "turn_accepted":
        turn = _require_record(params.get("turn"), "request.params.turn")
        _require_matching_identity(
            data.get("session_id"), params.get("session_id"), "result session_id"
        )
        _require_matching_identity(
            data.get("generation_id"),
            params.get("generation_id"),
            "result generation_id",
        )
        _require_matching_identity(data.get("turn_id"), turn.get("turn_id"), "result turn_id")
    elif method == "cancel_turn" and result_type == "turn_cancelled":
        _require_matching_identity(
            data.get("session_id"), params.get("session_id"), "result session_id"
        )
        _require_matching_identity(
            data.get("generation_id"),
            params.get("generation_id"),
            "result generation_id",
        )
        _require_matching_identity(data.get("turn_id"), params.get("turn_id"), "result turn_id")
    elif (method, result_type) in {
        ("inspect_session", "session_snapshot"),
        ("attach_session", "attach_capability"),
        ("close_session", "session_closed"),
        ("clear_session", "session_cleared"),
    }:
        _require_matching_identity(
            data.get("session_id"), params.get("session_id"), "result session_id"
        )
        _require_matching_identity(
            data.get("generation_id"),
            params.get("generation_id"),
            "result generation_id",
        )
    elif method == "run_once" and result_type == "turn_result":
        expected_session = _expected_start_session_id(params.get("session"))
        turn = _require_record(params.get("turn"), "request.params.turn")
        if expected_session is not None:
            _require_matching_identity(
                data.get("session_id"), expected_session, "result session_id"
            )
        _require_matching_identity(data.get("turn_id"), turn.get("turn_id"), "result turn_id")


def _require_sequence(value: object, field: str) -> int:
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or not 0 <= value <= MAX_SAFE_JSON_INTEGER
    ):
        raise PmuxProtocolError(f"{field} must be an integer between 0 and {MAX_SAFE_JSON_INTEGER}")
    return value


def _optional_sequence(record: dict[str, Any], key: str, field: str) -> None:
    if key in record:
        _require_sequence(record[key], f"{field}.{key}")


def _next_sequence(cursor: int, field: str) -> int:
    return _require_sequence(cursor + 1, field)


def _require_record(value: object, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise PmuxProtocolError(f"{field} must be an object")
    return value


def _require_field(record: dict[str, Any], key: str, field: str) -> Any:
    if key not in record:
        raise PmuxProtocolError(f"{field}.{key} is required")
    return record[key]


def _require_string(record: dict[str, Any], key: str, field: str) -> str:
    value = _require_field(record, key, field)
    if not isinstance(value, str):
        raise PmuxProtocolError(f"{field}.{key} must be a string")
    return value


def _require_bool(record: dict[str, Any], key: str, field: str) -> bool:
    value = _require_field(record, key, field)
    if not isinstance(value, bool):
        raise PmuxProtocolError(f"{field}.{key} must be a boolean")
    return value


def _require_int(record: dict[str, Any], key: str, field: str) -> int:
    return _require_sequence(_require_field(record, key, field), f"{field}.{key}")


def _require_list(record: dict[str, Any], key: str, field: str) -> list[Any]:
    value = _require_field(record, key, field)
    if not isinstance(value, list):
        raise PmuxProtocolError(f"{field}.{key} must be an array")
    return value


def _require_enum(
    record: dict[str, Any], key: str, field: str, values: frozenset[str] | set[str]
) -> str:
    value = _require_string(record, key, field)
    if value not in values:
        raise PmuxProtocolError(f"{field}.{key} has an unknown discriminant")
    return value


def _values(name: str) -> frozenset[str]:
    """Runtime validator domain for one v1 value enum, from the typed copy."""
    return frozenset(V1_VALUE_ENUMS[name])


def _variants(name: str) -> frozenset[str]:
    """Runtime validator domain for one v1 internally-tagged union, from the typed copy."""
    return frozenset(V1_TAGGED_UNIONS[name]["variants"])


_SESSION_STATES = _values("SessionState")


def _validate_compatibility(value: object, field: str) -> None:
    report = _require_record(value, field)
    _require_string(report, "claude_version", field)
    _require_string(report, "os", field)
    _require_string(report, "arch", field)
    _require_enum(report, "terminal_profile", field, _values("TerminalProfile"))
    _require_enum(report, "input_transport", field, {"sdk"})
    _require_bool(report, "tested", field)
    drain_ms = _require_int(report, "transcript_drain_ms", field)
    if not 1 <= drain_ms <= 60_000:
        raise PmuxProtocolError(f"{field}.transcript_drain_ms must be between 1 and 60000")


#: A diagnosis is only useful if its coarse outcome and its fine finding agree,
#: so these tables let the validator check that relationship rather than merely
#: checking that both fields are members of their enums. A report whose summary
#: promises more than its finding tested is a false report with a confession
#: attached.
_RUNTIME_FINDING_OUTCOMES: Final[dict[str, str]] = {
    "private_runtime_responsive": "pass",
    "control_plane_unreachable": "fail",
    "control_plane_unresponsive": "fail",
    "control_plane_refused": "fail",
    "launch_broker_stopped": "fail",
}

_SESSION_FINDING_OUTCOMES: Final[dict[str, str]] = {
    "terminal_present": "pass",
    "terminal_missing": "fail",
    "session_declared_unusable": "unproven",
    "session_actor_unresponsive": "unproven",
    "session_closed_during_probe": "unproven",
    "not_probed": "unproven",
}


def _require_derived_outcome(record: dict[str, Any], field: str, outcomes: dict[str, str]) -> None:
    _require_enum(record, "outcome", field, _values("ProbeOutcome"))
    finding = _require_enum(record, "finding", field, frozenset(outcomes))
    if record["outcome"] != outcomes[finding]:
        raise PmuxProtocolError(f"{field}.outcome contradicts {field}.finding")


#: The same derived-outcome relationship, one level down, for the health tree.
_LAYER_FINDING_OUTCOMES: Final[dict[str, str]] = {
    "exercised": "pass",
    "faulted": "fail",
    # A layer with no subject is vacuously fine, and a layer whose subject
    # could not be reached is not. The daemon derives ``outcome`` from
    # ``finding``; this table is what lets a client REFUSE a report where the
    # two disagree, so it has to carry the same mapping and not a summary of it.
    "nothing_to_exercise": "pass",
    "not_established": "unproven",
}


def _validate_diagnosis(value: object, field: str) -> None:
    data = _require_record(value, field)
    for index, entry in enumerate(data.get("layers", [])):
        scope = f"{field}.layers[{index}]"
        layer = _require_record(entry, scope)
        _require_enum(layer, "layer", scope, _values("HealthLayerName"))
        _require_derived_outcome(layer, scope, _LAYER_FINDING_OUTCOMES)
        # Required for every finding, `exercised` included. A layer that passed
        # without saying what it exercised is the boolean this tree replaced.
        detail = _require_string(layer, "detail", scope)
        if not detail:
            raise PmuxProtocolError(f"{scope}.detail must not be empty")
    named = [_require_record(entry, field).get("layer") for entry in data.get("layers", [])]
    if len(named) != len(set(named)):
        raise PmuxProtocolError(f"{field}.layers reports one layer twice")
    runtime = _require_record(_require_field(data, "runtime", field), f"{field}.runtime")
    _require_derived_outcome(runtime, f"{field}.runtime", _RUNTIME_FINDING_OUTCOMES)
    _require_int(runtime, "elapsed_ms", f"{field}.runtime")
    _optional_sequence(runtime, "live_private_terminals", f"{field}.runtime")
    for index, entry in enumerate(_require_list(data, "sessions", field)):
        scope = f"{field}.sessions[{index}]"
        session = _require_record(entry, scope)
        _require_uuid(session, "session_id", scope)
        _require_uuid(session, "generation_id", scope)
        _require_derived_outcome(session, scope, _SESSION_FINDING_OUTCOMES)
        if "state" in session:
            _require_enum(session, "state", scope, _SESSION_STATES)
        if "private_terminal_present" in session:
            _require_bool(session, "private_terminal_present", scope)


def missing_health_layers(diagnosis: DaemonDiagnosis) -> list[HealthLayerName]:
    """Layers the report does not carry an entry for, in declaration order.

    A layer that is ABSENT is ``not_established``, never healthy. A caller
    folding ``layers`` alone would report a daemon that established nothing as
    healthy, because an empty fold is a pass.
    """
    present = {layer["layer"] for layer in diagnosis.get("layers", [])}
    return [name for name in get_args(HealthLayerName) if name not in present]


def _validate_snapshot(value: object, field: str) -> None:
    data = _require_record(value, field)
    _require_uuid(data, "session_id", field)
    _require_uuid(data, "generation_id", field)
    _require_uuid(data, "transcript_session_id", field)
    _require_enum(data, "cell", field, _values("SessionCell"))
    _require_enum(data, "state", field, _SESSION_STATES)
    _require_string(data, "cwd", field)
    _validate_compatibility(_require_field(data, "compatibility", field), f"{field}.compatibility")
    _require_int(data, "created_at_ms", field)
    _require_int(data, "updated_at_ms", field)
    _optional_sequence(data, "idle_deadline_ms", field)
    _require_bool(data, "resumable", field)
    _require_int(data, "last_sequence", field)
    if "active_turn_id" in data:
        _require_uuid(data, "active_turn_id", field)
    if "last_turn" in data:
        last_turn = _require_record(data["last_turn"], f"{field}.last_turn")
        _require_uuid(last_turn, "turn_id", f"{field}.last_turn")
        _require_enum(last_turn, "outcome", f"{field}.last_turn", _values("TurnOutcome"))
        _require_int(last_turn, "completed_at_ms", f"{field}.last_turn")
        _require_int(last_turn, "final_sequence", f"{field}.last_turn")
    if "needs_input" in data:
        _validate_needs_input(data["needs_input"], f"{field}.needs_input")


def _validate_error(value: object, field: str) -> dict[str, Any]:
    data = _require_record(value, field)
    code = _require_string(data, "code", field)
    if code not in KNOWN_ERROR_CODES:
        raise PmuxProtocolError(f"{field}.code has an unknown discriminant")
    _require_string(data, "message", field)
    _require_bool(data, "retryable", field)
    return data


def _validate_token_usage(value: object, field: str) -> None:
    usage = _require_record(value, field)
    for key in (
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ):
        _require_int(usage, key, field)


def _validate_stop_reason(value: object, field: str) -> None:
    reason = _require_record(value, field)
    _require_enum(reason, "kind", field, _values("StopReasonKind"))
    if "raw" in reason:
        _require_string(reason, "raw", field)


def _validate_message_block(value: object, field: str) -> None:
    block = _require_record(value, field)
    kind = _require_enum(block, "kind", field, _variants("MessageBlock"))
    if kind == "text":
        _require_string(block, "text", field)
    elif kind == "tool_use":
        _require_string(block, "id", field)
        _require_string(block, "name", field)
        _require_field(block, "input", field)
    elif kind == "tool_result":
        _require_string(block, "tool_use_id", field)
        _require_field(block, "content", field)
        _require_bool(block, "is_error", field)
    elif kind == "unknown":
        _require_string(block, "block_type", field)
        _require_field(block, "data", field)
    else:
        # Reachable only from a `MessageBlock` variant added to `protocol.py`
        # and not here. The domain above is that union's own variant list, so
        # widening it without widening this used to fall through the `else` and
        # demand `block_type` of a block that never carries one.
        raise PmuxProtocolError(f"{field}.kind is a pinned variant this client does not validate")


def _validate_warning(value: object, field: str) -> None:
    warning = _require_record(value, field)
    _require_string(warning, "code", field)
    _require_string(warning, "message", field)


def _validate_needs_input(value: object, field: str) -> None:
    data = _require_record(value, field)
    _require_enum(data, "kind", field, _values("NeedsInputKind"))
    _require_string(data, "message", field)


def _validate_stateless_result(value: object, field: str) -> None:
    """The Path B answer, validated for what it must and must NOT carry.

    The absence check is not decoration. ``session_id`` is the one field whose
    presence would mean a pool instance had been named on the wire, and a caller
    that can name a resource is one step from aliasing one. It is asserted here,
    in the client, because the client is where a daemon that regressed would be
    caught by somebody who did not deploy it.
    """
    data = _require_record(value, field)
    _require_string(data, "model", field)
    _require_string(data, "text", field)
    _require_string(data, "claude_version", field)
    if "reported_model" in data:
        _require_string(data, "reported_model", field)
    if "effort" in data:
        _require_enum(data, "effort", field, _values("EffortLevel"))
    # FOUND BY GIVING ``run_stateless`` A GOLDEN PAIR. This validator was the
    # only one of the three that reads a ``stop_reason`` and did not check it:
    # the corpus's required-field inventory deletes ``stop_reason/kind`` from
    # every result that carries one and requires the client to reject the frame,
    # and this client accepted it, because ``run_stateless`` was the one method
    # with no golden pair in any language.
    if "stop_reason" in data:
        _validate_stop_reason(data["stop_reason"], f"{field}.stop_reason")
    usage = _require_record(_require_field(data, "usage", field), f"{field}.usage")
    for key in ("main", "sidechain", "combined"):
        _validate_token_usage(_require_field(usage, key, f"{field}.usage"), f"{field}.usage.{key}")
    if "cost_usd" in usage:
        _require_string(usage, "cost_usd", f"{field}.usage")
    for named in ("session_id", "generation_id", "cwd", "config_root", "system_prompt"):
        if named in data:
            raise PmuxProtocolError(
                f"{field} carries {named}, which would name a pool resource on the wire"
            )


def _validate_turn_result(value: object, field: str) -> None:
    data = _require_record(value, field)
    _require_uuid(data, "session_id", field)
    _require_uuid(data, "generation_id", field)
    _require_uuid(data, "turn_id", field)
    _require_enum(data, "outcome", field, _values("TurnOutcome"))
    _require_string(data, "text", field)
    _require_string(data, "claude_version", field)
    _validate_compatibility(_require_field(data, "compatibility", field), f"{field}.compatibility")
    _require_int(data, "final_sequence", field)
    usage = _require_record(_require_field(data, "usage", field), f"{field}.usage")
    for key in ("main", "sidechain", "combined"):
        _validate_token_usage(_require_field(usage, key, f"{field}.usage"), f"{field}.usage.{key}")
    if "cost_usd" in usage:
        _require_string(usage, "cost_usd", f"{field}.usage")
    timings = _require_record(_require_field(data, "timings", field), f"{field}.timings")
    _require_int(timings, "submitted_at_ms", f"{field}.timings")
    _optional_sequence(timings, "prompt_acknowledged_at_ms", f"{field}.timings")
    _optional_sequence(timings, "terminal_candidate_at_ms", f"{field}.timings")
    _require_int(timings, "completed_at_ms", f"{field}.timings")
    _optional_sequence(timings, "drain_ms", f"{field}.timings")
    _optional_sequence(timings, "last_transcript_activity_at_ms", f"{field}.timings")
    _optional_sequence(timings, "stop_hook_at_ms", f"{field}.timings")
    completion = _require_record(_require_field(data, "completion", field), f"{field}.completion")
    _require_enum(completion, "authority", f"{field}.completion", _values("CompletionAuthority"))
    for key in (
        "prompt_acknowledged",
        "terminal_message_observed",
        "terminal_prompt_observed",
        "terminal_quiet_observed",
        "transcript_drained",
        "lifecycle_hook_observed",
    ):
        _require_bool(completion, key, f"{field}.completion")
    if "model" in data:
        _require_string(data, "model", field)
    if "stop_reason" in data:
        _validate_stop_reason(data["stop_reason"], f"{field}.stop_reason")
    if "final_blocks" in data:
        for index, block in enumerate(_require_list(data, "final_blocks", field)):
            _validate_message_block(block, f"{field}.final_blocks[{index}]")
    if "tools" in data:
        for index, tool_value in enumerate(_require_list(data, "tools", field)):
            tool = _require_record(tool_value, f"{field}.tools[{index}]")
            _require_string(tool, "tool_use_id", f"{field}.tools[{index}]")
            _require_string(tool, "name", f"{field}.tools[{index}]")
            _require_field(tool, "input", f"{field}.tools[{index}]")
            _require_enum(tool, "status", f"{field}.tools[{index}]", _values("ToolStatus"))
            _optional_sequence(tool, "started_at_ms", f"{field}.tools[{index}]")
            _optional_sequence(tool, "completed_at_ms", f"{field}.tools[{index}]")
    if "warnings" in data:
        for index, warning in enumerate(_require_list(data, "warnings", field)):
            _validate_warning(warning, f"{field}.warnings[{index}]")


def _validate_replay_gap(value: object, field: str) -> None:
    gap = _require_record(value, field)
    _require_int(gap, "requested_after", field)
    _require_int(gap, "oldest_available", field)
    _require_int(gap, "next_sequence", field)
    _validate_snapshot(_require_field(gap, "snapshot", field), f"{field}.snapshot")


def _validate_event(value: object, field: str) -> None:
    envelope = _require_record(value, field)
    if _require_int(envelope, "schema_version", field) != PROTOCOL_VERSION:
        raise PmuxVersionError(envelope.get("schema_version"))
    _require_uuid(envelope, "session_id", field)
    _require_uuid(envelope, "generation_id", field)
    _require_int(envelope, "sequence", field)
    _require_int(envelope, "timestamp_ms", field)
    if "turn_id" in envelope:
        _require_uuid(envelope, "turn_id", field)
    event = _require_record(_require_field(envelope, "event", field), f"{field}.event")
    event_type = _require_string(event, "type", f"{field}.event")
    data = _require_record(_require_field(event, "data", f"{field}.event"), f"{field}.event.data")
    data_field = f"{field}.event.data"
    if event_type == "session_state_changed":
        _require_enum(data, "previous", data_field, _SESSION_STATES)
        _require_enum(data, "current", data_field, _SESSION_STATES)
    elif event_type == "prompt_acknowledged":
        _require_string(data, "prompt_uuid", data_field)
        _require_int(data, "transcript_offset", data_field)
    elif event_type == "logical_message":
        _require_string(data, "message_id", data_field)
        _require_enum(data, "scope", data_field, _values("MessageScope"))
        for index, block in enumerate(_require_list(data, "blocks", data_field)):
            _validate_message_block(block, f"{data_field}.blocks[{index}]")
        _require_bool(data, "terminal", data_field)
        if "request_id" in data:
            _require_string(data, "request_id", data_field)
        if "model" in data:
            _require_string(data, "model", data_field)
        if "stop_reason" in data:
            _validate_stop_reason(data["stop_reason"], f"{data_field}.stop_reason")
        if "usage" in data:
            _validate_token_usage(data["usage"], f"{data_field}.usage")
    elif event_type == "tool_started":
        _require_string(data, "tool_use_id", data_field)
        _require_string(data, "name", data_field)
        _require_field(data, "input", data_field)
    elif event_type == "tool_completed":
        _require_string(data, "tool_use_id", data_field)
        _require_field(data, "output", data_field)
        _require_bool(data, "is_error", data_field)
    elif event_type == "rate_limit":
        _require_enum(data, "status", data_field, _values("RateLimitStatus"))
        _optional_sequence(data, "resets_at_ms", data_field)
    elif event_type == "needs_input":
        _validate_needs_input(data, data_field)
    elif event_type == "terminal_candidate":
        _require_string(data, "message_id", data_field)
        if "stop_reason" in data:
            _validate_stop_reason(data["stop_reason"], f"{data_field}.stop_reason")
    elif event_type == "turn_completed":
        _validate_turn_result(data, data_field)
    elif event_type == "turn_cancelled":
        _require_enum(data, "outcome", data_field, _values("CancelOutcome"))
        _require_bool(data, "recovered_to_ready", data_field)
    elif event_type == "turn_failed":
        _validate_error(data, data_field)
    elif event_type == "warning":
        _validate_warning(data, data_field)
    elif event_type == "replay_gap":
        _validate_replay_gap(data, data_field)
    elif event_type == "heartbeat":
        _require_enum(data, "session_state", data_field, _SESSION_STATES)
    else:
        raise PmuxProtocolError(f"{field}.event.type has an unknown discriminant")


def _validate_event_batch_shape(value: object, field: str) -> None:
    batch = _require_record(value, field)
    next_sequence = _require_int(batch, "next_sequence", field)
    events: list[Any] = []
    if "events" in batch:
        events = _require_list(batch, "events", field)
        for index, event in enumerate(events):
            _validate_event(event, f"{field}.events[{index}]")
    if "replay_gap" in batch:
        _validate_replay_gap(batch["replay_gap"], f"{field}.replay_gap")
        if events:
            raise PmuxProtocolError("a replay-gap batch cannot contain ordinary events")
        gap = _require_record(batch["replay_gap"], f"{field}.replay_gap")
        snapshot = _require_record(gap["snapshot"], f"{field}.replay_gap.snapshot")
        snapshot_last = _require_sequence(
            snapshot["last_sequence"], f"{field}.replay_gap.snapshot.last_sequence"
        )
        expected_next = _next_sequence(snapshot_last, f"{field}.replay_gap.snapshot.next_sequence")
        first_requested = _next_sequence(
            gap["requested_after"], f"{field}.replay_gap.requested_after.next_sequence"
        )
        if gap["next_sequence"] != expected_next or next_sequence != expected_next:
            raise PmuxProtocolError("replay-gap, snapshot, and batch cursors must agree exactly")
        if first_requested >= gap["oldest_available"] or gap["oldest_available"] > expected_next:
            raise PmuxProtocolError(
                "replay-gap retained range does not prove that requested events were lost"
            )


def _validate_agent_descriptor(data: dict[str, object], field: str) -> None:
    """One stored agent version.

    ``config_digest`` is required because it is IDENTITY: a descriptor without
    one names nothing, and it is the value a caller compares to answer "is this
    the configuration I wrote".

    ``spec`` is checked for being an object and NOTHING ELSE. It is the stored
    document echoed back, opaque on a response for the reason the daemon's own
    type states: a request must refuse an unknown field and a response must
    tolerate one, and no client in any language keeps two decoders for one type.
    """
    _require_uuid(data, "agent_id", field)
    _require_agent_version(data, "version", field)
    _require_string(data, "config_digest", field)
    _require_int(data, "created_at_ms", field)
    _require_int(data, "updated_at_ms", field)
    _require_record(_require_field(data, "spec", field), f"{field}.spec")


def _validate_agent_list(data: dict[str, object], field: str) -> None:
    """A listing, and the records it could not read.

    Both arrays are optional on the wire and omitted when empty, so an ordinary
    listing's bytes are what every release before ``unreadable`` existed sent.
    It is validated rather than passed through for the reason it exists: a
    caller who cannot see that a record was unreadable sees a stored agent
    simply stop appearing.
    """
    if "agents" in data:
        agents = _require_field(data, "agents", field)
        if not isinstance(agents, list):
            raise PmuxProtocolError(f"{field}.agents must be an array")
        for index, entry in enumerate(agents):
            scope = f"{field}.agents[{index}]"
            summary = _require_record(entry, scope)
            _require_uuid(summary, "agent_id", scope)
            _require_agent_version(summary, "version", scope)
            _require_string(summary, "config_digest", scope)
            _require_string(summary, "name", scope)
            _require_enum(summary, "cell", scope, _values("SessionCell"))
            _require_int(summary, "updated_at_ms", scope)
    if "unreadable" not in data:
        return
    unreadable = _require_field(data, "unreadable", field)
    if not isinstance(unreadable, list):
        raise PmuxProtocolError(f"{field}.unreadable must be an array")
    for index, entry in enumerate(unreadable):
        scope = f"{field}.unreadable[{index}]"
        failure = _require_record(entry, scope)
        _require_uuid(failure, "agent_id", scope)
        _require_string(failure, "reason", scope)


def _require_agent_version(data: dict[str, object], key: str, field: str) -> int:
    """An agent version starts at 1.

    Checked here for the same reason the daemon's newtype checks it: a zero is
    not a version, and a caller that pinned one would be naming a stored object
    that cannot exist.
    """
    value = _require_int(data, key, field)
    if value < 1:
        raise PmuxProtocolError(f"{field}.{key} must be at least 1; there is no version 0")
    return value


def _validate_result(value: object) -> None:
    result = _require_record(value, "response.result")
    result_type = _require_string(result, "type", "response.result")
    data = _require_record(
        _require_field(result, "data", "response.result"), "response.result.data"
    )
    field = "response.result.data"
    if result_type == "pong":
        _require_string(data, "server_version", field)
        _require_int(data, "protocol_version", field)
    elif result_type == "session_started":
        _require_uuid(data, "session_id", field)
        _require_uuid(data, "generation_id", field)
        _require_enum(data, "state", field, _SESSION_STATES)
        _validate_compatibility(
            _require_field(data, "compatibility", field), f"{field}.compatibility"
        )
        _require_int(data, "created_at_ms", field)
        _require_int(data, "last_sequence", field)
    elif result_type == "turn_accepted":
        _require_uuid(data, "session_id", field)
        _require_uuid(data, "generation_id", field)
        _require_uuid(data, "turn_id", field)
        _require_bool(data, "replayed", field)
        _require_enum(data, "state", field, _SESSION_STATES)
        _require_int(data, "next_sequence", field)
    elif result_type == "turn_cancelled":
        _require_uuid(data, "session_id", field)
        _require_uuid(data, "generation_id", field)
        _require_uuid(data, "turn_id", field)
        _require_enum(data, "outcome", field, _values("CancelOutcome"))
        _require_enum(data, "session_state", field, _SESSION_STATES)
    elif result_type == "session_snapshot":
        _validate_snapshot(data, field)
    elif result_type == "attach_capability":
        _require_uuid(data, "session_id", field)
        _require_uuid(data, "generation_id", field)
        _require_string(data, "token", field)
        _require_string(data, "endpoint", field)
        _require_int(data, "expires_at_ms", field)
        _require_bool(data, "read_only", field)
    elif result_type == "session_closed":
        _require_uuid(data, "session_id", field)
        _require_uuid(data, "generation_id", field)
        _require_bool(data, "already_closed", field)
        _require_bool(data, "process_reaped", field)
    elif result_type == "events":
        _validate_event_batch_shape(data, field)
    elif result_type == "turn_result":
        _validate_turn_result(data, field)
    elif result_type == "session_cleared":
        _require_uuid(data, "session_id", field)
        _require_uuid(data, "generation_id", field)
        _require_uuid(data, "transcript_session_id", field)
        _require_bool(data, "rotated", field)
        _require_enum(data, "state", field, _SESSION_STATES)
    elif result_type == "diagnosis":
        _validate_diagnosis(data, field)
    elif result_type == "stateless_result":
        _validate_stateless_result(data, field)
    elif result_type in {"agent_created", "agent", "agent_updated"}:
        _validate_agent_descriptor(data, field)
    elif result_type == "agent_list":
        _validate_agent_list(data, field)
    else:
        raise PmuxProtocolError("response.result.type has an unknown discriminant")


def _validate_batch(
    session_id: SessionId,
    generation_id: SessionGenerationId,
    requested_after: int,
    batch: EventBatch,
) -> None:
    if not isinstance(batch, dict):
        raise PmuxProtocolError("event batch must be an object")
    cursor = requested_after
    events = batch.get("events", [])
    if not isinstance(events, list):
        raise PmuxProtocolError("events must be an array")
    gap = batch.get("replay_gap")
    if gap is not None:
        if events:
            raise PmuxProtocolError("a replay-gap batch cannot contain ordinary events")
        if gap.get("requested_after") != requested_after:
            raise PmuxProtocolError("replay gap cursor does not match the request")
        snapshot = gap.get("snapshot")
        if not isinstance(snapshot, dict) or not _same_uuid(
            _require_uuid_value(snapshot.get("session_id"), "replay_gap.snapshot.session_id"),
            session_id,
        ):
            raise PmuxProtocolError("replay gap snapshot belongs to another session")
        if not _same_uuid(
            _require_uuid_value(snapshot.get("generation_id"), "replay_gap.snapshot.generation_id"),
            generation_id,
        ):
            raise PmuxProtocolError("replay gap snapshot belongs to another process generation")
        cursor = _require_sequence(snapshot.get("last_sequence"), "replay_gap snapshot sequence")
        expected_next = _next_sequence(cursor, "replay_gap.snapshot.next_sequence")
        if gap.get("next_sequence") != expected_next or batch.get("next_sequence") != expected_next:
            raise PmuxProtocolError("replay-gap, snapshot, and batch cursors must agree exactly")
    for event in events:
        _validate_event(event, "event")
        if event.get("schema_version") != PROTOCOL_VERSION:
            raise PmuxVersionError(event.get("schema_version"))
        if not _same_uuid(
            _require_uuid_value(event.get("session_id"), "event.session_id"), session_id
        ):
            raise PmuxProtocolError("event belongs to another session")
        if not _same_uuid(
            _require_uuid_value(event.get("generation_id"), "event.generation_id"), generation_id
        ):
            raise PmuxProtocolError("event belongs to another process generation")
        sequence = _require_sequence(event.get("sequence"), "event.sequence")
        expected_sequence = _next_sequence(cursor, "event.next_sequence")
        if sequence != expected_sequence:
            raise PmuxSequenceError(expected_sequence, sequence)
        cursor = sequence
    next_sequence = _require_sequence(batch.get("next_sequence"), "batch.next_sequence")
    expected_next = _next_sequence(cursor, "batch.next_sequence")
    if next_sequence != expected_next:
        raise PmuxSequenceError(expected_next, next_sequence)
