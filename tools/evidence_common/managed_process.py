#!/usr/bin/env python3
"""Bounded long-lived local-process supervision with terminal receipts.

``start_managed`` performs descriptor-bound launch and leader verification in
the caller thread.  Only after that boundary does one private observer thread
drain output, enforce resource limits, and coherently track descendants.  A
handle has one successful terminal path: ``finalize`` records and delivers an
expected stop request.  Every other terminal path produces a structured
failure receipt and reaps all observed owned processes.

The inherited read-only vnode ownership marker has the same ordinary,
non-hostile-child limitation documented by :mod:`bounded_process`.
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import selectors
import signal
import sys
import threading
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

import bounded_process as bounded

MANAGED_EXECUTION_RECEIPT_DOMAIN = "pmux.evidence.managed-process-receipt.v1"
MANAGED_FAILURE_RECEIPT_DOMAIN = "pmux.evidence.managed-process-failure-receipt.v1"
MANAGED_FAILURE_REASONS = frozenset(
    {
        *bounded.FAILURE_REASONS,
        "abort_requested",
        "graceful_stop_timeout",
        "unexpected_exit",
    }
)
STOP_REQUEST_KEYS = frozenset(
    {
        "schema_version",
        "kind",
        "requested",
        "signal",
        "target_pid",
        "target_started",
    }
)
MANAGED_TERMINATE_GRACE_SECONDS = 5.0
MANAGED_KILL_GRACE_SECONDS = 5.0
MANAGED_OBSERVATION_INTERVAL_SECONDS = 0.05


@dataclass(frozen=True)
class ManagedProcessIdentity:
    leader_pid: int
    leader_started: str
    leader_pgid: int
    leader_sid: int
    executable_sha256: str
    ownership_marker_sha256: str
    cwd_device: int
    cwd_inode: int


@dataclass(frozen=True)
class ManagedProcessHealth:
    identity: ManagedProcessIdentity
    running: bool
    stop_requested: bool
    stdout_size: int
    stderr_size: int
    observed_process_count: int


def _stop_request(
    kind: str,
    *,
    signal_number: int | None = None,
    target_pid: int | None = None,
    target_started: str | None = None,
) -> dict[str, Any]:
    requested = kind != "none"
    return {
        "schema_version": 1,
        "kind": kind,
        "requested": requested,
        "signal": signal_number if requested else None,
        "target_pid": target_pid if requested else None,
        "target_started": target_started if requested else None,
    }


def _validate_stop_request(value: object) -> dict[str, Any]:
    if not isinstance(value, Mapping) or frozenset(value) != STOP_REQUEST_KEYS:
        raise bounded.BoundedProcessError("managed stop-request fields are not exact")
    request = dict(value)
    kind = request.get("kind")
    requested = request.get("requested")
    signal_number = request.get("signal")
    target_pid = request.get("target_pid")
    target_started = request.get("target_started")
    if (
        not bounded._exact_int(request.get("schema_version"), minimum=1, maximum=1)
        or kind not in ("none", "expected", "abort")
        or type(requested) is not bool
        or requested is not (kind != "none")
        or (
            requested
            and (
                not bounded._exact_int(signal_number, minimum=1, maximum=127)
                or not bounded._exact_int(target_pid, minimum=1)
                or not isinstance(target_started, str)
                or not target_started
            )
        )
        or (
            not requested
            and any(
                item is not None for item in (signal_number, target_pid, target_started)
            )
        )
    ):
        raise bounded.BoundedProcessError("managed stop request is malformed")
    return request


def _finite_surrogate(value: Mapping[str, Any], *, failure: bool) -> dict[str, Any]:
    surrogate = dict(value)
    surrogate.pop("stop_request", None)
    surrogate.pop("graceful_stop_timeout_seconds", None)
    surrogate["kind"] = (
        "pmux_bounded_process_failure" if failure else "pmux_bounded_process"
    )
    if failure and surrogate.get("failure_reason") not in bounded.FAILURE_REASONS:
        surrogate["failure_reason"] = "supervisor_error"
    surrogate.pop("receipt_sha256", None)
    domain = (
        bounded.FAILURE_RECEIPT_DOMAIN if failure else bounded.EXECUTION_RECEIPT_DOMAIN
    )
    surrogate["receipt_sha256"] = bounded._canonical_json_sha256(
        surrogate, domain=domain
    )
    return surrogate


def validate_managed_execution_receipt(
    value: Mapping[str, Any],
) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise bounded.BoundedProcessError("managed execution receipt is not a mapping")
    receipt = dict(value)
    if receipt.get("kind") != "pmux_managed_process":
        raise bounded.BoundedProcessError("managed execution receipt kind is invalid")
    stop_request = _validate_stop_request(receipt.get("stop_request"))
    if not bounded._exact_int(
        receipt.get("graceful_stop_timeout_seconds"),
        minimum=1,
        maximum=receipt.get("timeout_seconds")
        if bounded._exact_int(receipt.get("timeout_seconds"), minimum=1)
        else None,
    ):
        raise bounded.BoundedProcessError("managed graceful-stop timeout is invalid")
    if stop_request["kind"] != "expected":
        raise bounded.BoundedProcessError(
            "managed success receipt lacks an expected stop request"
        )
    bounded.validate_execution_receipt(_finite_surrogate(receipt, failure=False))
    digest = receipt.get("receipt_sha256")
    if not bounded._sha256(digest):
        raise bounded.BoundedProcessError("managed execution digest is malformed")
    body = dict(receipt)
    del body["receipt_sha256"]
    if digest != bounded._canonical_json_sha256(
        body, domain=MANAGED_EXECUTION_RECEIPT_DOMAIN
    ):
        raise bounded.BoundedProcessError("managed execution digest does not match")
    return receipt


def validate_managed_failure_receipt(
    value: Mapping[str, Any],
) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise bounded.BoundedProcessError("managed failure receipt is not a mapping")
    receipt = dict(value)
    if receipt.get("kind") != "pmux_managed_process_failure":
        raise bounded.BoundedProcessError("managed failure receipt kind is invalid")
    reason = receipt.get("failure_reason")
    if not isinstance(reason, str) or reason not in MANAGED_FAILURE_REASONS:
        raise bounded.BoundedProcessError("managed failure reason is invalid")
    stop_request = _validate_stop_request(receipt.get("stop_request"))
    if not bounded._exact_int(
        receipt.get("graceful_stop_timeout_seconds"),
        minimum=1,
        maximum=receipt.get("timeout_seconds")
        if bounded._exact_int(receipt.get("timeout_seconds"), minimum=1)
        else None,
    ):
        raise bounded.BoundedProcessError("managed graceful-stop timeout is invalid")
    if reason == "abort_requested" and stop_request["kind"] != "abort":
        raise bounded.BoundedProcessError("managed abort receipt lacks abort request")
    if reason == "graceful_stop_timeout" and stop_request["kind"] != "expected":
        raise bounded.BoundedProcessError(
            "managed stop timeout lacks expected stop request"
        )
    bounded.validate_failure_receipt(_finite_surrogate(receipt, failure=True))
    digest = receipt.get("receipt_sha256")
    if not bounded._sha256(digest):
        raise bounded.BoundedProcessError("managed failure digest is malformed")
    body = dict(receipt)
    del body["receipt_sha256"]
    if digest != bounded._canonical_json_sha256(
        body, domain=MANAGED_FAILURE_RECEIPT_DOMAIN
    ):
        raise bounded.BoundedProcessError("managed failure digest does not match")
    return receipt


def _load_receipt(payload: bytes, *, failure: bool) -> dict[str, Any]:
    if not isinstance(payload, bytes) or len(payload) > 4 * 1024 * 1024:
        raise bounded.BoundedProcessError("managed receipt JSON exceeded its bound")

    def object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, item in pairs:
            if key in result:
                raise bounded.BoundedProcessError(
                    f"managed receipt JSON repeats key: {key}"
                )
            result[key] = item
        return result

    def reject_constant(item: str) -> None:
        raise bounded.BoundedProcessError(
            f"managed receipt JSON contains non-finite number: {item}"
        )

    try:
        loaded = json.loads(
            payload.decode("utf-8"),
            object_pairs_hook=object_pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise bounded.BoundedProcessError(
            "managed receipt JSON is malformed"
        ) from error
    if not isinstance(loaded, dict):
        raise bounded.BoundedProcessError("managed receipt JSON is not an object")
    return (
        validate_managed_failure_receipt(loaded)
        if failure
        else validate_managed_execution_receipt(loaded)
    )


def load_managed_execution_receipt(payload: bytes) -> dict[str, Any]:
    return _load_receipt(payload, failure=False)


def load_managed_failure_receipt(payload: bytes) -> dict[str, Any]:
    return _load_receipt(payload, failure=True)


def _dump_receipt(value: Mapping[str, Any], *, failure: bool) -> bytes:
    receipt = (
        validate_managed_failure_receipt(value)
        if failure
        else validate_managed_execution_receipt(value)
    )
    return (
        json.dumps(
            receipt,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        + b"\n"
    )


def dump_managed_execution_receipt(value: Mapping[str, Any]) -> bytes:
    return _dump_receipt(value, failure=False)


def dump_managed_failure_receipt(value: Mapping[str, Any]) -> bytes:
    return _dump_receipt(value, failure=True)


class _ManagedFault(RuntimeError):
    def __init__(self, reason: str, message: str) -> None:
        super().__init__(message)
        if reason not in MANAGED_FAILURE_REASONS:
            raise AssertionError(f"unknown managed failure reason: {reason}")
        self.reason = reason


class _ManagedProcessBase:
    """Opaque handle for one verified long-lived local process."""

    def __init__(
        self,
        *,
        executable: bounded.BoundExecutable,
        argv: Sequence[str],
        canonical_cwd: pathlib.Path,
        cwd_stat: os.stat_result,
        environment_receipt: Mapping[str, Any],
        standard_input_receipt: Mapping[str, Any],
        timeout_seconds: int,
        graceful_stop_timeout_seconds: int,
        drain_timeout_seconds: int,
        maximum_output_bytes: int,
        description: str,
        stdout_spool_fd: int | None,
        stderr_spool_fd: int | None,
        stdout_spool_before: os.stat_result | None,
        stderr_spool_before: os.stat_result | None,
        process_pid: int,
        leader: bounded.ProcessIdentity,
        baseline: Mapping[int, bounded.ProcessIdentity],
        stdout_read: int,
        stderr_read: int,
        ownership_marker_fd: int,
        ownership_marker_witness: Mapping[str, Any],
        started_at: float,
    ) -> None:
        self._executable = executable
        self._argv = tuple(argv)
        self._canonical_cwd = canonical_cwd
        self._cwd_stat = cwd_stat
        self._cwd_receipt = bounded._cwd_witness(canonical_cwd, cwd_stat)
        self._environment_receipt = dict(environment_receipt)
        self._standard_input_receipt = dict(standard_input_receipt)
        self._timeout_seconds = timeout_seconds
        self._graceful_stop_timeout_seconds = graceful_stop_timeout_seconds
        self._drain_timeout_seconds = drain_timeout_seconds
        self._maximum_output_bytes = maximum_output_bytes
        self._description = description
        self._stdout_spool_fd = stdout_spool_fd
        self._stderr_spool_fd = stderr_spool_fd
        self._stdout_spool_before = stdout_spool_before
        self._stderr_spool_before = stderr_spool_before
        self._process_pid = process_pid
        self._leader = leader
        self._baseline = dict(baseline)
        self._stdout_read: int | None = stdout_read
        self._stderr_read: int | None = stderr_read
        self._ownership_marker_fd: int | None = ownership_marker_fd
        self._ownership_marker_witness = dict(ownership_marker_witness)
        self._started_at = started_at
        self._deadline = started_at + timeout_seconds
        self._condition = threading.Condition(threading.RLock())
        self._streams = {"stdout": bytearray(), "stderr": bytearray()}
        self._owned: dict[int, bounded.ProcessIdentity] = {process_pid: leader}
        self._leader_returncode: int | None = None
        self._drain_deadline: float | None = None
        self._stop_request = _stop_request("none")
        self._stop_deadline: float | None = None
        self._requested_fault: _ManagedFault | None = None
        self._output_capture_complete = True
        self._output_limit_stream: str | None = None
        self._process_observation_complete = True
        self._marker_supervisor_closed = False
        self._leader_marker_loss_observed_at_exit = False
        self._leader_marker_missing_since: float | None = None
        self._terminal: bounded.RunResult | bounded.FailureResult | None = None
        self._terminal_message = ""
        self._terminal_publication_error: BaseException | None = None
        self._identity = ManagedProcessIdentity(
            leader_pid=leader.pid,
            leader_started=leader.started,
            leader_pgid=leader.pgid,
            leader_sid=leader.sid,
            executable_sha256=executable.sha256,
            ownership_marker_sha256=self._ownership_marker_witness["sha256"],
            cwd_device=cwd_stat.st_dev,
            cwd_inode=cwd_stat.st_ino,
        )
        self._thread = threading.Thread(
            target=self._observe_loop,
            name=f"pmux-managed-{process_pid}",
            daemon=False,
        )
        self._thread.start()

    @property
    def identity(self) -> ManagedProcessIdentity:
        return self._identity

    @property
    def terminal_result(
        self,
    ) -> bounded.RunResult | bounded.FailureResult | None:
        with self._condition:
            return self._terminal

    def health(self) -> ManagedProcessHealth:
        with self._condition:
            if self._terminal_publication_error is not None:
                raise bounded.BoundedProcessError(
                    "managed observer terminated without a terminal receipt"
                ) from self._terminal_publication_error
            if isinstance(self._terminal, bounded.FailureResult):
                raise bounded.BoundedProcessFailure(
                    self._terminal_message, self._terminal
                )
            return ManagedProcessHealth(
                identity=self._identity,
                running=self._terminal is None and self._leader_returncode is None,
                stop_requested=self._stop_request["kind"] == "expected",
                stdout_size=len(self._streams["stdout"]),
                stderr_size=len(self._streams["stderr"]),
                observed_process_count=len(self._owned),
            )


def _raise_managed_launch_failure(
    *,
    reason: str,
    message: str,
    executable: bounded.BoundExecutable,
    argv: Sequence[str],
    canonical_cwd: pathlib.Path,
    cwd_stat: os.stat_result,
    environment_receipt: Mapping[str, Any],
    standard_input_receipt: Mapping[str, Any],
    ownership_marker_fd: int,
    ownership_marker_witness: Mapping[str, Any],
    timeout_seconds: int,
    graceful_stop_timeout_seconds: int,
    drain_timeout_seconds: int,
    maximum_output_bytes: int,
    process_pid: int,
    leader: bounded.ProcessIdentity | None,
    stdout_read: int,
    stderr_read: int,
    stdout_spool_fd: int | None,
    stderr_spool_fd: int | None,
) -> None:
    try:
        os.kill(process_pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    returncode: int | None = None
    try:
        waited_pid, status_value = os.waitpid(process_pid, 0)
        if waited_pid == process_pid:
            returncode = os.waitstatus_to_exitcode(status_value)
    except ChildProcessError:
        pass
    streams = {"stdout": bytearray(), "stderr": bytearray()}
    selector = selectors.DefaultSelector()
    output_complete = True
    output_limit_stream: str | None = None
    try:
        selector.register(stdout_read, selectors.EVENT_READ, "stdout")
        selector.register(stderr_read, selectors.EVENT_READ, "stderr")
        deadline = time.monotonic() + min(float(drain_timeout_seconds), 2.0)
        while selector.get_map() and time.monotonic() < deadline:
            for key, _mask in selector.select(
                min(0.05, max(0.0, deadline - time.monotonic()))
            ):
                available = maximum_output_bytes - sum(
                    len(value) for value in streams.values()
                )
                chunk = os.read(key.fd, min(64 * 1024, available + 1))
                if not chunk:
                    selector.unregister(key.fd)
                    continue
                accepted = chunk[:available]
                bounded._write_spool(
                    stdout_spool_fd if key.data == "stdout" else stderr_spool_fd,
                    accepted,
                )
                streams[key.data].extend(accepted)
                if len(chunk) > len(accepted):
                    output_complete = False
                    output_limit_stream = key.data
                    break
            if output_limit_stream is not None:
                break
        if selector.get_map():
            output_complete = False
    finally:
        selector.close()
        for descriptor in (stdout_read, stderr_read):
            try:
                os.close(descriptor)
            except OSError:
                pass
    marker_closed = False
    try:
        os.close(ownership_marker_fd)
        marker_closed = True
    except OSError:
        pass
    ledger = bounded.validate_process_ledger(
        []
        if leader is None
        else [
            {
                "pid": leader.pid,
                "ppid": leader.ppid,
                "pgid": leader.pgid,
                "sid": leader.sid,
                "started": leader.started,
                "command": leader.command,
                "ownership_marker_sha256": ownership_marker_witness["sha256"],
                "reaped": returncode is not None,
            }
        ],
        require_nonempty=False,
        require_all_reaped=False,
    )
    process_observation_complete = leader is not None
    cleanup_complete = (
        process_observation_complete
        and returncode is not None
        and marker_closed
        and all(row["reaped"] for row in ledger)
    )
    body = {
        "schema_version": 1,
        "kind": "pmux_managed_process_failure",
        "executable": bounded._witness_object(executable),
        "argv": list(argv),
        "cwd": str(canonical_cwd),
        "cwd_witness": bounded._cwd_witness(canonical_cwd, cwd_stat),
        "environment": dict(environment_receipt),
        "standard_input": dict(standard_input_receipt),
        "ownership_marker": bounded._ownership_marker_receipt(
            ownership_marker_witness,
            leader_verified_before_release=False,
            leader_marker_loss_observed_at_exit=False,
            supervisor_descriptor_closed=marker_closed,
        ),
        "timeout_seconds": timeout_seconds,
        "graceful_stop_timeout_seconds": graceful_stop_timeout_seconds,
        "drain_timeout_seconds": drain_timeout_seconds,
        "maximum_output_bytes": maximum_output_bytes,
        "stop_request": _stop_request("none"),
        "leader_pid": process_pid,
        "failure_reason": reason,
        "cleanup_complete": cleanup_complete,
        "output_complete": output_complete,
        "output_limit_stream": output_limit_stream,
        "exit_code": returncode,
        "stdout_size": len(streams["stdout"]),
        "stdout_sha256": hashlib.sha256(streams["stdout"]).hexdigest(),
        "stderr_size": len(streams["stderr"]),
        "stderr_sha256": hashlib.sha256(streams["stderr"]).hexdigest(),
        "process_observation_complete": process_observation_complete,
        "process_ledger": list(ledger),
        "process_ledger_sha256": bounded._canonical_json_sha256(
            list(ledger), domain=bounded.PROCESS_LEDGER_DOMAIN
        ),
    }
    receipt = {
        **body,
        "receipt_sha256": bounded._canonical_json_sha256(
            body, domain=MANAGED_FAILURE_RECEIPT_DOMAIN
        ),
    }
    validated = validate_managed_failure_receipt(receipt)
    result = bounded.FailureResult(
        reason=reason,
        exit_code=returncode,
        stdout=bytes(streams["stdout"]),
        stderr=bytes(streams["stderr"]),
        process_ledger=ledger,
        cleanup_complete=cleanup_complete,
        output_complete=output_complete,
        receipt=validated,
    )
    raise bounded.BoundedProcessFailure(message, result)


def start_managed(
    executable: bounded.BoundExecutable,
    argv: Sequence[str],
    *,
    cwd: pathlib.Path | None,
    environment: Mapping[str, str],
    timeout_seconds: int,
    graceful_stop_timeout_seconds: int,
    drain_timeout_seconds: int,
    maximum_output_bytes: int,
    description: str = "managed process",
    stdout_spool_fd: int | None = None,
    stderr_spool_fd: int | None = None,
    stdin_bytes: bytes | None = None,
    stdin_fd: int | None = None,
) -> ManagedProcess:
    """Start and verify one managed process before creating its observer thread."""

    if (
        not isinstance(executable, bounded.BoundExecutable)
        or not argv
        or not all(
            isinstance(argument, str) and argument and "\0" not in argument
            for argument in argv
        )
        or argv[0] != executable.path
        or not isinstance(environment, Mapping)
        or not bounded._exact_int(timeout_seconds, minimum=1, maximum=86_400)
        or not bounded._exact_int(
            graceful_stop_timeout_seconds,
            minimum=1,
            maximum=timeout_seconds,
        )
        or not bounded._exact_int(
            drain_timeout_seconds, minimum=1, maximum=timeout_seconds
        )
        or not bounded._exact_int(
            maximum_output_bytes, minimum=1, maximum=1024 * 1024 * 1024
        )
        or not isinstance(description, str)
        or not description
    ):
        raise bounded.BoundedProcessError("managed process arguments are invalid")
    try:
        for argument in argv:
            argument.encode("utf-8")
    except UnicodeEncodeError as error:
        raise bounded.BoundedProcessError("managed argv is not UTF-8") from error
    standard_input_payload, standard_input_receipt = bounded._prepare_standard_input(
        stdin_bytes, stdin_fd
    )
    environment_receipt = bounded.environment_identity(environment)
    stdout_spool_before = bounded._validate_spool(stdout_spool_fd, "stdout")
    stderr_spool_before = bounded._validate_spool(stderr_spool_fd, "stderr")
    if (
        stdout_spool_before is not None
        and stderr_spool_before is not None
        and (stdout_spool_before.st_dev, stdout_spool_before.st_ino)
        == (stderr_spool_before.st_dev, stderr_spool_before.st_ino)
    ):
        raise bounded.BoundedProcessError("managed output spools must be distinct")

    started_at = time.monotonic()
    deadline = started_at + timeout_seconds
    child_environment = dict(environment)
    (
        ownership_marker_fd,
        ownership_marker_reservation_fd,
        ownership_marker_witness,
    ) = bounded._create_ownership_marker()

    def close_prelaunch() -> None:
        nonlocal ownership_marker_fd, ownership_marker_reservation_fd
        for descriptor in (
            ownership_marker_reservation_fd,
            ownership_marker_fd,
        ):
            if descriptor is not None:
                try:
                    os.close(descriptor)
                except OSError:
                    pass
        ownership_marker_reservation_fd = None
        ownership_marker_fd = None

    try:
        executable_fd, executable_stat = bounded._open_bound_executable(executable)
        canonical_cwd, cwd_fd, cwd_stat = bounded._open_cwd(cwd)
        baseline = bounded._snapshot()
        staged_stdin_fd = (
            None
            if standard_input_payload is None
            else bounded._stage_standard_input(standard_input_payload)
        )
    except BaseException:
        for name in ("staged_stdin_fd", "cwd_fd", "executable_fd"):
            descriptor = locals().get(name)
            if isinstance(descriptor, int):
                try:
                    os.close(descriptor)
                except OSError:
                    pass
        close_prelaunch()
        raise

    created_pipes: list[tuple[int, int]] = []
    try:
        for _index in range(4):
            created_pipes.append(os.pipe())
    except BaseException:
        for pair in created_pipes:
            for descriptor in pair:
                os.close(descriptor)
        for descriptor in (staged_stdin_fd, cwd_fd, executable_fd):
            if descriptor is not None:
                os.close(descriptor)
        close_prelaunch()
        raise
    (
        (stdout_read, stdout_write),
        (stderr_read, stderr_write),
        (ready_read, ready_write),
        (release_read, release_write),
    ) = created_pipes
    all_pipe_fds = (
        stdout_read,
        stdout_write,
        stderr_read,
        stderr_write,
        ready_read,
        ready_write,
        release_read,
        release_write,
    )
    for descriptor in all_pipe_fds:
        os.set_inheritable(descriptor, False)

    darwin_suspended = sys.platform == "darwin"
    process_pid = 0
    try:
        if darwin_suspended:
            process_pid = bounded._darwin_spawn_suspended(
                pathlib.Path(executable.path),
                argv,
                child_environment,
                cwd_fd=cwd_fd,
                stdin_fd=staged_stdin_fd,
                stdout_fd=stdout_write,
                stderr_fd=stderr_write,
                ownership_marker_fd=ownership_marker_fd,
            )
        elif sys.platform.startswith("linux"):
            process_pid = os.fork()
        else:
            raise bounded.BoundedProcessError(
                f"unsupported managed process platform: {sys.platform}"
            )
    except BaseException:
        for descriptor in (
            *all_pipe_fds,
            cwd_fd,
            executable_fd,
            *(() if staged_stdin_fd is None else (staged_stdin_fd,)),
        ):
            try:
                os.close(descriptor)
            except OSError:
                pass
        close_prelaunch()
        raise

    if process_pid == 0:  # pragma: no cover - exercised in the child process
        try:
            os.close(stdout_read)
            os.close(stderr_read)
            os.close(ready_read)
            os.close(release_write)
            os.setsid()
            input_descriptor = staged_stdin_fd
            if input_descriptor is None:
                input_descriptor = os.open(os.devnull, os.O_RDONLY)
            os.dup2(input_descriptor, 0)
            os.dup2(stdout_write, 1)
            os.dup2(stderr_write, 2)
            os.fchdir(cwd_fd)
            os.dup2(
                ownership_marker_fd,
                bounded.OWNERSHIP_MARKER_FD,
                inheritable=True,
            )
            for descriptor in (
                input_descriptor,
                stdout_write,
                stderr_write,
                ownership_marker_fd,
                ownership_marker_reservation_fd,
            ):
                if descriptor is None or descriptor == bounded.OWNERSHIP_MARKER_FD:
                    continue
                if descriptor > 2:
                    os.close(descriptor)
            os.write(ready_write, b"R")
            os.close(ready_write)
            if os.read(release_read, 1) != b"G":
                os._exit(125)
            os.close(release_read)
            os.close(cwd_fd)
            os.execve(executable_fd, list(argv), child_environment)
        except BaseException as error:
            try:
                os.write(2, f"managed exec failed: {error!r}\n".encode("utf-8"))
            finally:
                os._exit(126)

    os.close(stdout_write)
    os.close(stderr_write)
    os.close(ready_write)
    os.close(release_read)
    os.close(cwd_fd)
    os.close(executable_fd)
    if staged_stdin_fd is not None:
        os.close(staged_stdin_fd)
    if ownership_marker_reservation_fd is not None:
        os.close(ownership_marker_reservation_fd)
        ownership_marker_reservation_fd = None

    leader: bounded.ProcessIdentity | None = None
    launch_reason = "launch_identity"
    launch_message = f"{description} failed launch identity verification"
    try:
        if darwin_suspended:
            os.close(ready_read)
            os.close(release_write)
            current = bounded._snapshot()
            observed = current.get(process_pid)
            if observed is not None:
                leader = bounded.ProcessIdentity(
                    observed.pid,
                    observed.ppid,
                    observed.pgid,
                    observed.sid,
                    observed.started,
                    executable.path,
                )
            if (
                observed is None
                or observed.ppid != os.getpid()
                or observed.pgid != process_pid
                or observed.sid != process_pid
                or not bounded._darwin_mapped_executable_matches(
                    process_pid, executable_stat
                )
                or not bounded._process_has_ownership_marker(
                    process_pid, ownership_marker_witness
                )
            ):
                raise bounded.BoundedProcessError(
                    "managed suspended leader identity is invalid"
                )
            if bounded._stat_fields(
                pathlib.Path(executable.path).lstat()
            ) != bounded._witness_stat(executable) or not bounded._cwd_stable(
                cwd_stat, canonical_cwd.lstat()
            ):
                raise bounded.BoundedProcessError(
                    "managed launch binding changed before release"
                )
            os.kill(process_pid, signal.SIGCONT)
        else:
            handshake = selectors.DefaultSelector()
            try:
                handshake.register(ready_read, selectors.EVENT_READ)
                events = handshake.select(max(0.0, deadline - time.monotonic()))
                ready = os.read(ready_read, 1) if events else b""
            finally:
                handshake.close()
                os.close(ready_read)
            if ready != b"R":
                raise bounded.BoundedProcessError("managed pre-exec handshake failed")
            current = bounded._snapshot()
            observed = current.get(process_pid)
            if observed is not None:
                leader = bounded.ProcessIdentity(
                    observed.pid,
                    observed.ppid,
                    observed.pgid,
                    observed.sid,
                    observed.started,
                    executable.path,
                )
            if (
                observed is None
                or observed.ppid != os.getpid()
                or observed.pgid != process_pid
                or observed.sid != process_pid
                or not bounded._process_has_ownership_marker(
                    process_pid, ownership_marker_witness
                )
            ):
                raise bounded.BoundedProcessError(
                    "managed pre-exec leader identity is invalid"
                )
            if bounded._stat_fields(
                pathlib.Path(executable.path).lstat()
            ) != bounded._witness_stat(executable) or not bounded._cwd_stable(
                cwd_stat, canonical_cwd.lstat()
            ):
                raise bounded.BoundedProcessError(
                    "managed launch binding changed before release"
                )
            os.write(release_write, b"G")
            os.close(release_write)
    except BaseException as error:
        if not isinstance(error, bounded.BoundedProcessError):
            launch_reason = "observation_incomplete"
        launch_message = f"{description} launch failed: {error}"
        _raise_managed_launch_failure(
            reason=launch_reason,
            message=launch_message,
            executable=executable,
            argv=argv,
            canonical_cwd=canonical_cwd,
            cwd_stat=cwd_stat,
            environment_receipt=environment_receipt,
            standard_input_receipt=standard_input_receipt,
            ownership_marker_fd=ownership_marker_fd,
            ownership_marker_witness=ownership_marker_witness,
            timeout_seconds=timeout_seconds,
            graceful_stop_timeout_seconds=graceful_stop_timeout_seconds,
            drain_timeout_seconds=drain_timeout_seconds,
            maximum_output_bytes=maximum_output_bytes,
            process_pid=process_pid,
            leader=leader,
            stdout_read=stdout_read,
            stderr_read=stderr_read,
            stdout_spool_fd=stdout_spool_fd,
            stderr_spool_fd=stderr_spool_fd,
        )
    assert leader is not None
    return ManagedProcess(
        executable=executable,
        argv=argv,
        canonical_cwd=canonical_cwd,
        cwd_stat=cwd_stat,
        environment_receipt=environment_receipt,
        standard_input_receipt=standard_input_receipt,
        timeout_seconds=timeout_seconds,
        graceful_stop_timeout_seconds=graceful_stop_timeout_seconds,
        drain_timeout_seconds=drain_timeout_seconds,
        maximum_output_bytes=maximum_output_bytes,
        description=description,
        stdout_spool_fd=stdout_spool_fd,
        stderr_spool_fd=stderr_spool_fd,
        stdout_spool_before=stdout_spool_before,
        stderr_spool_before=stderr_spool_before,
        process_pid=process_pid,
        leader=leader,
        baseline=baseline,
        stdout_read=stdout_read,
        stderr_read=stderr_read,
        ownership_marker_fd=ownership_marker_fd,
        ownership_marker_witness=ownership_marker_witness,
        started_at=started_at,
    )


class ManagedProcess(_ManagedProcessBase):
    """Opaque handle for one verified long-lived local process."""

    def finalize(
        self, *, signal_number: int | signal.Signals = signal.SIGTERM
    ) -> bounded.RunResult:
        signal_value = (
            int(signal_number)
            if isinstance(signal_number, signal.Signals)
            else signal_number
        )
        if not bounded._exact_int(signal_value, minimum=1, maximum=127):
            raise bounded.BoundedProcessError("managed stop signal is invalid")
        with self._condition:
            if isinstance(self._terminal, bounded.RunResult):
                return self._terminal
            if isinstance(self._terminal, bounded.FailureResult):
                raise bounded.BoundedProcessFailure(
                    self._terminal_message, self._terminal
                )
            if self._stop_request["kind"] == "none":
                self._stop_request = _stop_request(
                    "expected",
                    signal_number=signal_value,
                    target_pid=self._leader.pid,
                    target_started=self._leader.started,
                )
                self._stop_deadline = (
                    time.monotonic() + self._graceful_stop_timeout_seconds
                )
                should_signal = True
            elif self._stop_request["kind"] == "expected":
                if self._stop_request["signal"] != signal_value:
                    raise bounded.BoundedProcessError(
                        "managed stop was already requested with another signal"
                    )
                should_signal = False
            else:
                should_signal = False
            self._condition.notify_all()
        if should_signal:
            try:
                started = bounded.precise_process_started(self._leader.pid)
                if started == self._leader.started:
                    os.kill(self._leader.pid, signal_value)
                elif started is not None:
                    self._request_fault(
                        "observation_incomplete",
                        f"{self._description} leader PID was reused before stop",
                    )
            except ProcessLookupError:
                pass
            except bounded.BoundedProcessError as error:
                self._request_fault(
                    "observation_incomplete",
                    f"{self._description} leader identity could not be checked: {error}",
                )
        terminal = self._wait_terminal()
        if isinstance(terminal, bounded.RunResult):
            return terminal
        raise bounded.BoundedProcessFailure(self._terminal_message, terminal)

    def abort(self) -> None:
        terminal = self.close()
        if isinstance(terminal, bounded.FailureResult):
            raise bounded.BoundedProcessFailure(self._terminal_message, terminal)

    def close(self) -> bounded.RunResult | bounded.FailureResult:
        with self._condition:
            if self._terminal is not None:
                terminal = self._terminal
            else:
                if self._stop_request["kind"] == "none":
                    self._stop_request = _stop_request(
                        "abort",
                        signal_number=int(signal.SIGTERM),
                        target_pid=self._leader.pid,
                        target_started=self._leader.started,
                    )
                    self._requested_fault = self._requested_fault or _ManagedFault(
                        "abort_requested",
                        f"{self._description} was aborted before successful finalize",
                    )
                # An expected stop is already the handle's one successful
                # terminal path.  A concurrent/idempotent close must join that
                # path instead of publishing an abort fault paired with the
                # immutable expected-stop receipt.
                self._condition.notify_all()
                terminal = None
        if terminal is None:
            terminal = self._wait_terminal()
        return terminal

    def __enter__(self) -> ManagedProcess:
        return self

    def __exit__(
        self,
        _exception_type: object,
        _exception: object,
        _traceback: object,
    ) -> None:
        self.close()

    def _wait_terminal(self) -> bounded.RunResult | bounded.FailureResult:
        with self._condition:
            while self._terminal is None and self._terminal_publication_error is None:
                self._condition.wait(timeout=0.25)
            terminal = self._terminal
            publication_error = self._terminal_publication_error
        if threading.current_thread() is not self._thread:
            self._thread.join(timeout=5)
            if self._thread.is_alive():
                raise bounded.BoundedProcessError(
                    "managed observer thread did not terminate"
                )
        if publication_error is not None:
            raise bounded.BoundedProcessError(
                "managed observer terminated without a terminal receipt"
            ) from publication_error
        assert terminal is not None
        return terminal

    def _request_fault(self, reason: str, message: str) -> None:
        with self._condition:
            if self._terminal is None and self._requested_fault is None:
                self._requested_fault = _ManagedFault(reason, message)
                self._condition.notify_all()

    def _poll_leader(self) -> int | None:
        if self._leader_returncode is None:
            try:
                waited_pid, status_value = os.waitpid(self._process_pid, os.WNOHANG)
            except ChildProcessError:
                waited_pid = 0
            if waited_pid == self._process_pid:
                self._leader_returncode = os.waitstatus_to_exitcode(status_value)
                self._drain_deadline = min(
                    self._deadline,
                    time.monotonic() + self._drain_timeout_seconds,
                )
        return self._leader_returncode

    def _verify_bindings(self) -> None:
        try:
            current_executable = pathlib.Path(self._executable.path).lstat()
            current_cwd = self._canonical_cwd.lstat()
        except OSError as error:
            raise _ManagedFault(
                "binding_changed", "managed launch binding disappeared"
            ) from error
        if bounded._stat_fields(current_executable) != bounded._witness_stat(
            self._executable
        ):
            raise _ManagedFault("binding_changed", "managed executable binding changed")
        if not bounded._cwd_stable(self._cwd_stat, current_cwd):
            raise _ManagedFault("binding_changed", "managed cwd binding changed")

    def _scan_owned(self) -> dict[int, bounded.ProcessIdentity]:
        try:
            current = bounded._snapshot()
        except bounded.BoundedProcessError as error:
            raise _ManagedFault(
                "observation_incomplete", "managed process observation failed"
            ) from error
        try:
            bounded._assert_owned_pid_identities(self._owned, current)
        except bounded.BoundedProcessError as error:
            raise _ManagedFault(
                "observation_incomplete",
                "managed owned PID identity changed",
            ) from error
        leader = current.get(self._process_pid)
        if leader is not None:
            if not bounded._same_process(self._leader, leader):
                raise _ManagedFault(
                    "observation_incomplete",
                    "managed leader PID identity changed",
                )
            self._owned.setdefault(self._process_pid, leader)
            try:
                leader_has_marker = bounded._process_has_ownership_marker(
                    self._process_pid, self._ownership_marker_witness
                )
            except bounded.BoundedProcessError as error:
                raise _ManagedFault(
                    "observation_incomplete",
                    "managed ownership-marker observation failed",
                ) from error
            if not leader_has_marker:
                # A terminating process can disappear from the descriptor table
                # just before waitpid(2) reports it.  Permit only that narrow
                # race after an explicit expected stop; a live process that
                # keeps running without the marker still fails closed.
                self._poll_leader()
                with self._condition:
                    expected_stop = self._stop_request["kind"] == "expected"
                if self._leader_returncode is None and expected_stop:
                    now = time.monotonic()
                    if self._leader_marker_missing_since is None:
                        self._leader_marker_missing_since = now
                    if (
                        now - self._leader_marker_missing_since
                        <= bounded.OWNERSHIP_MARKER_EXIT_GRACE_SECONDS
                    ):
                        leader_has_marker = True
                if not leader_has_marker and self._leader_returncode is None:
                    raise _ManagedFault(
                        "observation_incomplete",
                        "managed leader lost its ownership marker",
                    )
                if not leader_has_marker:
                    self._leader_marker_loss_observed_at_exit = True
            else:
                self._leader_marker_missing_since = None
        changed = True
        while changed:
            changed = False
            parents = {self._process_pid, *self._owned}
            for identity in current.values():
                if identity.pid in self._owned:
                    continue
                if identity.ppid in parents or identity.pgid == self._process_pid:
                    self._owned[identity.pid] = identity
                    changed = True
        for identity in current.values():
            if (
                identity.pid in self._owned
                or identity.pid == os.getpid()
                or bounded._same_process(self._baseline.get(identity.pid), identity)
            ):
                continue
            if bounded._process_has_ownership_marker(
                identity.pid, self._ownership_marker_witness
            ):
                self._owned[identity.pid] = identity
        return current

    def _survivors(
        self, current: Mapping[int, bounded.ProcessIdentity]
    ) -> list[bounded.ProcessIdentity]:
        return [
            identity
            for pid, identity in self._owned.items()
            if bounded._same_process(identity, current.get(pid))
        ]

    def _capture_events(self, selector: selectors.BaseSelector, timeout: float) -> None:
        for key, _mask in selector.select(timeout):
            captured_size = sum(len(value) for value in self._streams.values())
            available = self._maximum_output_bytes - captured_size
            chunk = os.read(key.fd, min(64 * 1024, available + 1))
            if not chunk:
                selector.unregister(key.fd)
                continue
            accepted = chunk[:available]
            try:
                bounded._write_spool(
                    self._stdout_spool_fd
                    if key.data == "stdout"
                    else self._stderr_spool_fd,
                    accepted,
                )
            except bounded.BoundedProcessError as error:
                self._output_capture_complete = False
                raise _ManagedFault("spool_identity", str(error)) from error
            self._streams[key.data].extend(accepted)
            if len(chunk) > len(accepted):
                self._output_capture_complete = False
                self._output_limit_stream = key.data
                raise _ManagedFault(
                    "output_limit",
                    f"{self._description} exceeded its bounded output",
                )

    def _observe_loop(self) -> None:
        selector = selectors.DefaultSelector()
        try:
            assert self._stdout_read is not None and self._stderr_read is not None
            selector.register(self._stdout_read, selectors.EVENT_READ, "stdout")
            selector.register(self._stderr_read, selectors.EVENT_READ, "stderr")
            while True:
                with self._condition:
                    requested_fault = self._requested_fault
                    stop_kind = self._stop_request["kind"]
                    stop_deadline = self._stop_deadline
                if requested_fault is not None:
                    raise requested_fault
                now = time.monotonic()
                if now >= self._deadline:
                    raise _ManagedFault(
                        "timeout", f"{self._description} exceeded its lifetime"
                    )
                if (
                    stop_kind == "expected"
                    and stop_deadline is not None
                    and now >= stop_deadline
                    and self._leader_returncode is None
                ):
                    raise _ManagedFault(
                        "graceful_stop_timeout",
                        f"{self._description} did not honor its expected stop",
                    )
                active_deadline = min(
                    self._deadline,
                    self._drain_deadline
                    if self._drain_deadline is not None
                    else self._deadline,
                    stop_deadline
                    if stop_kind == "expected" and stop_deadline is not None
                    else self._deadline,
                )
                self._capture_events(
                    selector,
                    min(
                        MANAGED_OBSERVATION_INTERVAL_SECONDS,
                        max(0.0, active_deadline - now),
                    ),
                )
                self._poll_leader()
                current = self._scan_owned()
                self._verify_bindings()
                if (
                    self._leader_returncode is not None
                    and self._drain_deadline is not None
                    and time.monotonic() >= self._drain_deadline
                    and selector.get_map()
                ):
                    raise _ManagedFault(
                        "drain_timeout", f"{self._description} output drain timed out"
                    )
                if self._leader_returncode is not None and not selector.get_map():
                    if self._survivors(current):
                        raise _ManagedFault(
                            "descendant_survived",
                            f"{self._description} left an owned descendant",
                        )
                    with self._condition:
                        stop_kind = self._stop_request["kind"]
                    if stop_kind != "expected":
                        raise _ManagedFault(
                            "unexpected_exit",
                            f"{self._description} exited before an expected stop",
                        )
                    expected_codes = {
                        0,
                        -int(self._stop_request["signal"]),
                    }
                    if self._leader_returncode not in expected_codes:
                        raise _ManagedFault(
                            "unexpected_exit",
                            f"{self._description} exited with an unexpected status",
                        )
                    self._complete_success()
                    return
                with self._condition:
                    self._condition.notify_all()
        except _ManagedFault as fault:
            self._publish_failure(fault.reason, str(fault), selector)
        except BaseException:
            self._publish_failure(
                "supervisor_error", f"{self._description} observer failed", selector
            )
        finally:
            selector.close()
            for attribute in ("_stdout_read", "_stderr_read"):
                descriptor = getattr(self, attribute)
                if descriptor is not None:
                    try:
                        os.close(descriptor)
                    except OSError:
                        pass
                    setattr(self, attribute, None)

    def _publish_failure(
        self,
        reason: str,
        message: str,
        selector: selectors.BaseSelector,
    ) -> None:
        try:
            self._complete_failure(reason, message, selector)
        except BaseException as error:
            # Receipt construction is deliberately strict and may itself expose
            # an internal invariant defect.  Never convert that defect into an
            # unbounded public wait: publish the observer failure and wake every
            # caller after the cleanup work already attempted by
            # ``_complete_failure``.
            with self._condition:
                self._terminal_publication_error = error
                self._terminal_message = (
                    f"{self._description} could not publish a terminal receipt"
                )
                self._condition.notify_all()

    def _signal_known(self, signal_number: signal.Signals) -> None:
        for identity in sorted(
            self._owned.values(), key=lambda item: item.pid, reverse=True
        ):
            try:
                if bounded.precise_process_started(identity.pid) == identity.started:
                    os.kill(identity.pid, signal_number)
            except ProcessLookupError:
                pass

    def _wait_owned_exit(self, seconds: float) -> bool:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self._poll_leader()
            current = self._scan_owned()
            if not self._survivors(current):
                # Confirm quiescence with a fresh complete snapshot.  A known
                # process can fork a marker-bearing child while handling TERM
                # and exit between the first snapshot and liveness check.
                confirmation = self._scan_owned()
                if not self._survivors(confirmation):
                    return True
            time.sleep(0.02)
        return False

    def _terminate_all(self) -> BaseException | None:
        cleanup_error: BaseException | None = None
        try:
            self._scan_owned()
        except BaseException as error:
            cleanup_error = error
            self._process_observation_complete = False
        if self._process_pid not in self._owned:
            started = bounded.precise_process_started(self._process_pid)
            if started is not None:
                try:
                    os.kill(self._process_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        self._signal_known(signal.SIGTERM)
        try:
            terminated = self._wait_owned_exit(MANAGED_TERMINATE_GRACE_SECONDS)
        except BaseException as error:
            cleanup_error = cleanup_error or error
            self._process_observation_complete = False
            terminated = False
        if not terminated:
            self._signal_known(signal.SIGKILL)
            try:
                killed = self._wait_owned_exit(MANAGED_KILL_GRACE_SECONDS)
            except BaseException as error:
                cleanup_error = cleanup_error or error
                self._process_observation_complete = False
                killed = False
            if not killed:
                cleanup_error = cleanup_error or bounded.BoundedProcessError(
                    "managed owned processes could not be reaped"
                )
        leader_deadline = time.monotonic() + MANAGED_KILL_GRACE_SECONDS
        while self._leader_returncode is None and time.monotonic() < leader_deadline:
            self._poll_leader()
            if self._leader_returncode is None:
                time.sleep(0.01)
        if self._leader_returncode is None:
            try:
                if (
                    bounded.precise_process_started(self._process_pid)
                    == self._leader.started
                ):
                    os.kill(self._process_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self._poll_leader()
        if self._leader_returncode is None:
            cleanup_error = cleanup_error or bounded.BoundedProcessError(
                "managed leader could not be reaped"
            )
        return cleanup_error

    def _drain_after_failure(self, selector: selectors.BaseSelector) -> None:
        deadline = time.monotonic() + min(float(self._drain_timeout_seconds), 5.0)
        while selector.get_map() and time.monotonic() < deadline:
            try:
                self._capture_events(
                    selector,
                    min(0.05, max(0.0, deadline - time.monotonic())),
                )
            except _ManagedFault as fault:
                self._output_capture_complete = False
                if fault.reason == "output_limit":
                    break
        if selector.get_map():
            self._output_capture_complete = False

    def _verify_spools(self) -> BaseException | None:
        for descriptor, before, label in (
            (self._stdout_spool_fd, self._stdout_spool_before, "stdout"),
            (self._stderr_spool_fd, self._stderr_spool_before, "stderr"),
        ):
            if descriptor is None or before is None:
                continue
            try:
                os.fsync(descriptor)
                after = os.fstat(descriptor)
                if (before.st_dev, before.st_ino, before.st_uid, before.st_nlink) != (
                    after.st_dev,
                    after.st_ino,
                    after.st_uid,
                    after.st_nlink,
                ):
                    raise bounded.BoundedProcessError(
                        f"managed {label} spool identity changed"
                    )
            except (OSError, bounded.BoundedProcessError) as error:
                return error
        return None

    def _close_marker(self) -> BaseException | None:
        if self._ownership_marker_fd is None:
            return None
        descriptor = self._ownership_marker_fd
        self._ownership_marker_fd = None
        try:
            os.close(descriptor)
        except OSError as error:
            return error
        self._marker_supervisor_closed = True
        return None

    def _final_snapshot(self) -> Mapping[int, bounded.ProcessIdentity]:
        try:
            return bounded._snapshot()
        except bounded.BoundedProcessError:
            self._process_observation_complete = False
            return {}

    def _ledger(
        self,
        final_snapshot: Mapping[int, bounded.ProcessIdentity],
        *,
        require_all_reaped: bool,
    ) -> tuple[dict[str, Any], ...]:
        return bounded.validate_process_ledger(
            tuple(
                {
                    "pid": identity.pid,
                    "ppid": identity.ppid,
                    "pgid": identity.pgid,
                    "sid": identity.sid,
                    "started": identity.started,
                    "command": identity.command,
                    "ownership_marker_sha256": self._ownership_marker_witness["sha256"],
                    "reaped": self._process_observation_complete
                    and not bounded._same_process(
                        identity, final_snapshot.get(identity.pid)
                    ),
                }
                for identity in sorted(self._owned.values(), key=lambda item: item.pid)
            ),
            require_nonempty=True,
            require_all_reaped=require_all_reaped,
        )

    def _common_receipt_body(
        self,
        *,
        kind: str,
        exit_code: int | None,
        ledger: Sequence[Mapping[str, Any]],
    ) -> dict[str, Any]:
        return {
            "schema_version": 1,
            "kind": kind,
            "executable": bounded._witness_object(self._executable),
            "argv": list(self._argv),
            "cwd": str(self._canonical_cwd),
            "cwd_witness": self._cwd_receipt,
            "environment": self._environment_receipt,
            "standard_input": self._standard_input_receipt,
            "ownership_marker": bounded._ownership_marker_receipt(
                self._ownership_marker_witness,
                leader_verified_before_release=True,
                leader_marker_loss_observed_at_exit=(
                    self._leader_marker_loss_observed_at_exit
                ),
                supervisor_descriptor_closed=self._marker_supervisor_closed,
            ),
            "timeout_seconds": self._timeout_seconds,
            "graceful_stop_timeout_seconds": self._graceful_stop_timeout_seconds,
            "drain_timeout_seconds": self._drain_timeout_seconds,
            "maximum_output_bytes": self._maximum_output_bytes,
            "stop_request": dict(self._stop_request),
            "exit_code": exit_code,
            "stdout_size": len(self._streams["stdout"]),
            "stdout_sha256": hashlib.sha256(self._streams["stdout"]).hexdigest(),
            "stderr_size": len(self._streams["stderr"]),
            "stderr_sha256": hashlib.sha256(self._streams["stderr"]).hexdigest(),
            "process_ledger": list(ledger),
            "process_ledger_sha256": bounded._canonical_json_sha256(
                list(ledger), domain=bounded.PROCESS_LEDGER_DOMAIN
            ),
        }

    def _complete_success(self) -> None:
        try:
            final_owned_snapshot = self._scan_owned()
        except _ManagedFault as fault:
            self._complete_failure(fault.reason, str(fault), None)
            return
        if self._survivors(final_owned_snapshot):
            self._complete_failure(
                "descendant_survived",
                f"{self._description} left an owned descendant",
                None,
            )
            return
        spool_error = self._verify_spools()
        marker_error = self._close_marker()
        if spool_error is not None:
            self._complete_failure("spool_identity", str(spool_error), None)
            return
        if marker_error is not None:
            self._complete_failure(
                "supervisor_error", "managed ownership marker could not be closed", None
            )
            return
        final_snapshot = self._final_snapshot()
        ledger = self._ledger(final_snapshot, require_all_reaped=True)
        body = self._common_receipt_body(
            kind="pmux_managed_process",
            exit_code=self._leader_returncode,
            ledger=ledger,
        )
        receipt = {
            **body,
            "receipt_sha256": bounded._canonical_json_sha256(
                body, domain=MANAGED_EXECUTION_RECEIPT_DOMAIN
            ),
        }
        validated = validate_managed_execution_receipt(receipt)
        result = bounded.RunResult(
            exit_code=int(self._leader_returncode),
            stdout=bytes(self._streams["stdout"]),
            stderr=bytes(self._streams["stderr"]),
            process_ledger=ledger,
            receipt=validated,
        )
        with self._condition:
            self._terminal = result
            self._terminal_message = ""
            self._condition.notify_all()

    def _complete_failure(
        self,
        reason: str,
        message: str,
        selector: selectors.BaseSelector | None,
    ) -> None:
        cleanup_error = self._terminate_all()
        if selector is not None:
            self._drain_after_failure(selector)
        spool_error = self._verify_spools()
        if spool_error is not None:
            cleanup_error = cleanup_error or spool_error
            reason = "spool_identity"
        marker_error = self._close_marker()
        if marker_error is not None:
            cleanup_error = cleanup_error or marker_error
        final_snapshot = self._final_snapshot()
        ledger = self._ledger(final_snapshot, require_all_reaped=False)
        leader_observed = any(row["pid"] == self._process_pid for row in ledger)
        cleanup_complete = (
            self._process_observation_complete
            and self._leader_returncode is not None
            and leader_observed
            and self._marker_supervisor_closed
            and all(row["reaped"] is True for row in ledger)
        )
        output_complete = self._output_capture_complete and (
            selector is None or not selector.get_map()
        )
        body = self._common_receipt_body(
            kind="pmux_managed_process_failure",
            exit_code=self._leader_returncode,
            ledger=ledger,
        )
        body.update(
            {
                "leader_pid": self._process_pid,
                "failure_reason": reason,
                "cleanup_complete": cleanup_complete,
                "output_complete": output_complete,
                "output_limit_stream": self._output_limit_stream,
                "process_observation_complete": self._process_observation_complete,
            }
        )
        receipt = {
            **body,
            "receipt_sha256": bounded._canonical_json_sha256(
                body, domain=MANAGED_FAILURE_RECEIPT_DOMAIN
            ),
        }
        validated = validate_managed_failure_receipt(receipt)
        result = bounded.FailureResult(
            reason=reason,
            exit_code=self._leader_returncode,
            stdout=bytes(self._streams["stdout"]),
            stderr=bytes(self._streams["stderr"]),
            process_ledger=ledger,
            cleanup_complete=cleanup_complete,
            output_complete=output_complete,
            receipt=validated,
        )
        if cleanup_error is not None and not cleanup_complete:
            message += "; managed cleanup is incomplete"
        with self._condition:
            self._terminal = result
            self._terminal_message = message
            self._condition.notify_all()
