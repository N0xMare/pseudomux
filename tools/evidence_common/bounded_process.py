#!/usr/bin/env python3
"""Dependency-free, fail-closed local process supervision for release evidence.

The caller binds an executable before launch, then carries that immutable
witness into :func:`run`.  Linux executes the opened descriptor.  macOS maps
the executable into a suspended ``posix_spawn`` child and verifies the mapped
vnode before allowing user code to run.  Both implementations anchor the cwd,
bound combined output, positively attribute descendants, and reap every
observed owned process by PID plus precise birth identity.

The inherited read-only vnode marker covers ordinary non-hostile child trees,
including descendants that change session or parentage.  It is not claimed to
resist a child that intentionally closes the reserved marker descriptor before
escaping observation.  Marker loss on a still-running leader fails closed;
descriptor-table teardown already followed by a reaped leader is recorded
explicitly instead of being misclassified as a live marker loss.
"""

from __future__ import annotations

import ctypes
import errno
import fcntl
import hashlib
import json
import os
import pathlib
import re
import resource
import selectors
import signal
import stat
import sys
import tempfile
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

PROCESS_LEDGER_DOMAIN = "pmux.evidence.process-ledger.v1"
EXECUTION_RECEIPT_DOMAIN = "pmux.evidence.bounded-process-receipt.v1"
FAILURE_RECEIPT_DOMAIN = "pmux.evidence.bounded-process-failure-receipt.v1"
ENVIRONMENT_IDENTITY_DOMAIN = "pmux.evidence.process-environment.v1"
MAX_PROCESS_ARGUMENT_BYTES = 16 * 1024 * 1024
MAX_STANDARD_INPUT_BYTES = 16 * 1024 * 1024
MAX_PROCESS_COUNT = 1_000_000
OWNERSHIP_MARKER_FD = 198
OWNERSHIP_MARKER_BYTES = 32
OWNERSHIP_MARKER_EXIT_GRACE_SECONDS = 0.25
TERMINATE_GRACE_SECONDS = 5.0
KILL_GRACE_SECONDS = 5.0
LEADER_REAP_SECONDS = 5.0
FAILURE_REASONS = frozenset(
    {
        "binding_changed",
        "descendant_survived",
        "drain_timeout",
        "launch_identity",
        "leader_exit_timeout",
        "observation_incomplete",
        "output_limit",
        "spool_identity",
        "supervisor_error",
        "supervisor_interrupted",
        "timeout",
    }
)
PROCESS_LEDGER_KEYS = frozenset(
    {
        "pid",
        "ppid",
        "pgid",
        "sid",
        "started",
        "command",
        "ownership_marker_sha256",
        "reaped",
    }
)
STANDARD_INPUT_DESCRIPTOR_KEYS = frozenset(
    {
        "device",
        "inode",
        "uid",
        "gid",
        "mode",
        "nlink",
        "size",
        "mtime_ns",
        "ctime_ns",
        "offset",
    }
)
STANDARD_INPUT_KEYS = frozenset(
    {
        "schema_version",
        "source",
        "present",
        "maximum_bytes",
        "size",
        "sha256",
        "source_descriptor",
    }
)
OWNERSHIP_MARKER_KEYS = frozenset(
    {
        "schema_version",
        "fd",
        "device",
        "inode",
        "uid",
        "gid",
        "mode",
        "nlink",
        "size",
        "sha256",
        "unlinked_before_launch",
        "writer_closed_before_launch",
        "inherited_read_only",
        "parent_fd_collision",
        "leader_verified_before_release",
        "leader_marker_loss_observed_at_exit",
        "supervisor_descriptor_closed",
    }
)


class BoundedProcessError(RuntimeError):
    """A process could not be run and evidenced within the exact contract."""


@dataclass(frozen=True)
class BoundExecutable:
    path: str
    device: int
    inode: int
    uid: int
    gid: int
    mode: int
    nlink: int
    size: int
    mtime_ns: int
    ctime_ns: int
    sha256: str
    executable_format: str


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    ppid: int
    pgid: int
    sid: int
    started: str
    command: str


@dataclass(frozen=True)
class RunResult:
    exit_code: int
    stdout: bytes
    stderr: bytes
    process_ledger: tuple[Mapping[str, Any], ...]
    receipt: Mapping[str, Any]


@dataclass(frozen=True)
class FailureResult:
    reason: str
    exit_code: int | None
    stdout: bytes
    stderr: bytes
    process_ledger: tuple[Mapping[str, Any], ...]
    cleanup_complete: bool
    output_complete: bool
    receipt: Mapping[str, Any]


class BoundedProcessFailure(BoundedProcessError):
    """A launched process failed with an exact bounded failure receipt."""

    def __init__(self, message: str, result: FailureResult) -> None:
        super().__init__(message)
        self.result = result


class _PostLaunchFailure(RuntimeError):
    def __init__(self, reason: str, message: str) -> None:
        super().__init__(message)
        if reason not in FAILURE_REASONS:
            raise AssertionError(f"unknown post-launch failure reason: {reason}")
        self.reason = reason


class _DarwinProcBsdInfo(ctypes.Structure):
    _fields_ = [
        ("pbi_flags", ctypes.c_uint32),
        ("pbi_status", ctypes.c_uint32),
        ("pbi_xstatus", ctypes.c_uint32),
        ("pbi_pid", ctypes.c_uint32),
        ("pbi_ppid", ctypes.c_uint32),
        ("pbi_uid", ctypes.c_uint32),
        ("pbi_gid", ctypes.c_uint32),
        ("pbi_ruid", ctypes.c_uint32),
        ("pbi_rgid", ctypes.c_uint32),
        ("pbi_svuid", ctypes.c_uint32),
        ("pbi_svgid", ctypes.c_uint32),
        ("rfu_1", ctypes.c_uint32),
        ("pbi_comm", ctypes.c_char * 16),
        ("pbi_name", ctypes.c_char * 32),
        ("pbi_nfiles", ctypes.c_uint32),
        ("pbi_pgid", ctypes.c_uint32),
        ("pbi_pjobc", ctypes.c_uint32),
        ("e_tdev", ctypes.c_uint32),
        ("e_tpgid", ctypes.c_uint32),
        ("pbi_nice", ctypes.c_int32),
        ("pbi_start_tvsec", ctypes.c_uint64),
        ("pbi_start_tvusec", ctypes.c_uint64),
    ]


class _DarwinProcFdInfo(ctypes.Structure):
    _fields_ = [
        ("proc_fd", ctypes.c_int32),
        ("proc_fdtype", ctypes.c_uint32),
    ]


class _DarwinProcFileInfo(ctypes.Structure):
    _fields_ = [
        ("fi_openflags", ctypes.c_uint32),
        ("fi_status", ctypes.c_uint32),
        ("fi_offset", ctypes.c_int64),
        ("fi_type", ctypes.c_int32),
        ("fi_guardflags", ctypes.c_uint32),
    ]


class _DarwinVinfoStat(ctypes.Structure):
    _fields_ = [
        ("vst_dev", ctypes.c_uint32),
        ("vst_mode", ctypes.c_uint16),
        ("vst_nlink", ctypes.c_uint16),
        ("vst_ino", ctypes.c_uint64),
        ("vst_uid", ctypes.c_uint32),
        ("vst_gid", ctypes.c_uint32),
        ("vst_atime", ctypes.c_int64),
        ("vst_atimensec", ctypes.c_int64),
        ("vst_mtime", ctypes.c_int64),
        ("vst_mtimensec", ctypes.c_int64),
        ("vst_ctime", ctypes.c_int64),
        ("vst_ctimensec", ctypes.c_int64),
        ("vst_birthtime", ctypes.c_int64),
        ("vst_birthtimensec", ctypes.c_int64),
        ("vst_size", ctypes.c_int64),
        ("vst_blocks", ctypes.c_int64),
        ("vst_blksize", ctypes.c_int32),
        ("vst_flags", ctypes.c_uint32),
        ("vst_gen", ctypes.c_uint32),
        ("vst_rdev", ctypes.c_uint32),
        ("vst_qspare", ctypes.c_int64 * 2),
    ]


class _DarwinVnodeInfo(ctypes.Structure):
    _fields_ = [
        ("vi_stat", _DarwinVinfoStat),
        ("vi_type", ctypes.c_int32),
        ("vi_pad", ctypes.c_int32),
        ("vi_fsid", ctypes.c_int32 * 2),
    ]


class _DarwinVnodeFdInfo(ctypes.Structure):
    _fields_ = [
        ("pfi", _DarwinProcFileInfo),
        ("pvi", _DarwinVnodeInfo),
    ]


def _exact_int(
    value: object, *, minimum: int | None = None, maximum: int | None = None
) -> bool:
    if type(value) is not int:
        return False
    if minimum is not None and value < minimum:
        return False
    return maximum is None or value <= maximum


def _sha256(value: object) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def validate_process_ledger(
    raw: Sequence[Mapping[str, Any]],
    *,
    require_nonempty: bool = True,
    require_all_reaped: bool = True,
) -> tuple[dict[str, Any], ...]:
    if not isinstance(raw, (list, tuple)):
        raise BoundedProcessError("process ledger must be a list or tuple")
    records: list[dict[str, Any]] = []
    pids: set[int] = set()
    for raw_record in raw:
        if not isinstance(raw_record, Mapping) or frozenset(raw_record) != (
            PROCESS_LEDGER_KEYS
        ):
            raise BoundedProcessError("process-ledger record fields are not exact")
        record = dict(raw_record)
        pid = record.get("pid")
        if (
            not _exact_int(pid, minimum=1)
            or not _exact_int(record.get("ppid"), minimum=0)
            or not _exact_int(record.get("pgid"), minimum=0)
            or not _exact_int(record.get("sid"), minimum=0)
            or not isinstance(record.get("started"), str)
            or not record["started"]
            or not isinstance(record.get("command"), str)
            or not record["command"]
            or not _sha256(record.get("ownership_marker_sha256"))
            or type(record.get("reaped")) is not bool
            or (require_all_reaped and record["reaped"] is not True)
            or pid in pids
        ):
            raise BoundedProcessError("process-ledger record is invalid")
        pids.add(pid)
        records.append(record)
    if [record["pid"] for record in records] != sorted(pids):
        raise BoundedProcessError("process ledger is not PID ordered")
    if require_nonempty and not records:
        raise BoundedProcessError("process ledger is empty")
    return tuple(records)


def _canonical_json_sha256(value: object, *, domain: str) -> str:
    if (
        not isinstance(domain, str)
        or re.fullmatch(r"[a-z0-9._-]{1,128}", domain) is None
    ):
        raise BoundedProcessError("receipt hash domain is invalid")
    try:
        encoded = json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BoundedProcessError("receipt is not canonical JSON") from error
    digest = hashlib.sha256()
    digest.update(b"pmux-canonical-json-sha256-v1\0")
    digest.update(domain.encode("ascii"))
    digest.update(b"\0")
    digest.update(encoded)
    return digest.hexdigest()


def environment_identity(environment: Mapping[str, str]) -> dict[str, Any]:
    if not isinstance(environment, Mapping):
        raise BoundedProcessError("process environment is not a mapping")
    selected: dict[str, str] = {}
    for name, value in environment.items():
        if (
            not isinstance(name, str)
            or not name
            or "=" in name
            or "\0" in name
            or not isinstance(value, str)
            or "\0" in value
        ):
            raise BoundedProcessError("process environment is malformed")
        try:
            name.encode("utf-8")
            value.encode("utf-8")
        except UnicodeEncodeError as error:
            raise BoundedProcessError("process environment is not UTF-8") from error
        selected[name] = value
    names = sorted(selected)
    body = {name: selected[name] for name in names}
    return {
        "schema_version": 1,
        "variable_count": len(names),
        "names": names,
        "environment_sha256": _canonical_json_sha256(
            body, domain=ENVIRONMENT_IDENTITY_DOMAIN
        ),
    }


def _validate_environment_identity(value: object) -> dict[str, Any]:
    expected_keys = frozenset(
        {"schema_version", "variable_count", "names", "environment_sha256"}
    )
    if not isinstance(value, Mapping) or frozenset(value) != expected_keys:
        raise BoundedProcessError("receipt environment identity fields are not exact")
    identity = dict(value)
    names = identity.get("names")
    if (
        not _exact_int(identity.get("schema_version"), minimum=1, maximum=1)
        or not _exact_int(identity.get("variable_count"), minimum=0)
        or not isinstance(names, list)
        or not all(
            isinstance(name, str) and name and "=" not in name and "\0" not in name
            for name in names
        )
        or names != sorted(set(names))
        or identity["variable_count"] != len(names)
        or not _sha256(identity.get("environment_sha256"))
    ):
        raise BoundedProcessError("receipt environment identity is malformed")
    return identity


def _cwd_witness(path: pathlib.Path, metadata: os.stat_result) -> dict[str, Any]:
    return {
        "path": str(path),
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "uid": metadata.st_uid,
        "gid": metadata.st_gid,
        "mode": metadata.st_mode,
    }


def _validate_cwd_witness(value: object) -> dict[str, Any]:
    expected_keys = frozenset({"path", "device", "inode", "uid", "gid", "mode"})
    if not isinstance(value, Mapping) or frozenset(value) != expected_keys:
        raise BoundedProcessError("receipt cwd witness fields are not exact")
    witness = dict(value)
    path = witness.get("path")
    if (
        not isinstance(path, str)
        or not path
        or "\0" in path
        or not pathlib.Path(path).is_absolute()
        or path != os.path.normpath(path)
        or not _exact_int(witness.get("device"), minimum=0)
        or not _exact_int(witness.get("inode"), minimum=1)
        or not _exact_int(witness.get("uid"), minimum=0)
        or not _exact_int(witness.get("gid"), minimum=0)
        or not _exact_int(witness.get("mode"), minimum=1)
        or not stat.S_ISDIR(witness["mode"])
    ):
        raise BoundedProcessError("receipt cwd witness is malformed")
    return witness


def _standard_input_descriptor_witness(metadata: os.stat_result) -> dict[str, Any]:
    return {
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "uid": metadata.st_uid,
        "gid": metadata.st_gid,
        "mode": metadata.st_mode,
        "nlink": metadata.st_nlink,
        "size": metadata.st_size,
        "mtime_ns": metadata.st_mtime_ns,
        "ctime_ns": metadata.st_ctime_ns,
        "offset": 0,
    }


def _validate_standard_input_descriptor(value: object) -> dict[str, Any]:
    if not isinstance(value, Mapping) or frozenset(value) != (
        STANDARD_INPUT_DESCRIPTOR_KEYS
    ):
        raise BoundedProcessError(
            "receipt standard-input descriptor fields are not exact"
        )
    descriptor = dict(value)
    if (
        not _exact_int(descriptor.get("device"), minimum=0)
        or not _exact_int(descriptor.get("inode"), minimum=1)
        or not _exact_int(descriptor.get("uid"), minimum=0)
        or not _exact_int(descriptor.get("gid"), minimum=0)
        or not _exact_int(descriptor.get("mode"), minimum=1)
        or not stat.S_ISREG(descriptor["mode"])
        or stat.S_IMODE(descriptor["mode"]) & 0o077 != 0
        or stat.S_IMODE(descriptor["mode"]) & stat.S_IRUSR == 0
        or not _exact_int(descriptor.get("nlink"), minimum=0, maximum=0)
        or not _exact_int(
            descriptor.get("size"), minimum=0, maximum=MAX_STANDARD_INPUT_BYTES
        )
        or not _exact_int(descriptor.get("mtime_ns"), minimum=0)
        or not _exact_int(descriptor.get("ctime_ns"), minimum=0)
        or not _exact_int(descriptor.get("offset"), minimum=0, maximum=0)
    ):
        raise BoundedProcessError("receipt standard-input descriptor is malformed")
    return descriptor


def _standard_input_identity(
    payload: bytes | None,
    *,
    source: str,
    source_descriptor: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    size = 0 if payload is None else len(payload)
    return {
        "schema_version": 1,
        "source": source,
        "present": source != "none",
        "maximum_bytes": MAX_STANDARD_INPUT_BYTES,
        "size": size,
        "sha256": hashlib.sha256(b"" if payload is None else payload).hexdigest(),
        "source_descriptor": (
            None if source_descriptor is None else dict(source_descriptor)
        ),
    }


def _validate_standard_input_identity(value: object) -> dict[str, Any]:
    if not isinstance(value, Mapping) or frozenset(value) != STANDARD_INPUT_KEYS:
        raise BoundedProcessError("receipt standard-input fields are not exact")
    identity = dict(value)
    source = identity.get("source")
    present = identity.get("present")
    size = identity.get("size")
    descriptor_raw = identity.get("source_descriptor")
    descriptor = (
        None
        if descriptor_raw is None
        else _validate_standard_input_descriptor(descriptor_raw)
    )
    if (
        not _exact_int(identity.get("schema_version"), minimum=1, maximum=1)
        or source not in ("none", "bytes", "descriptor")
        or type(present) is not bool
        or not _exact_int(
            identity.get("maximum_bytes"),
            minimum=MAX_STANDARD_INPUT_BYTES,
            maximum=MAX_STANDARD_INPUT_BYTES,
        )
        or not _exact_int(size, minimum=0, maximum=MAX_STANDARD_INPUT_BYTES)
        or not _sha256(identity.get("sha256"))
        or (source == "none" and present is not False)
        or (source != "none" and present is not True)
        or (source == "none" and size != 0)
        or (source == "none" and identity["sha256"] != hashlib.sha256(b"").hexdigest())
        or (source in ("none", "bytes") and descriptor is not None)
        or (source == "descriptor" and descriptor is None)
        or (descriptor is not None and descriptor["size"] != size)
    ):
        raise BoundedProcessError("receipt standard-input identity is malformed")
    return identity


def _read_private_standard_input_descriptor(
    descriptor: int,
) -> tuple[bytes, dict[str, Any]]:
    if not _exact_int(descriptor, minimum=0):
        raise BoundedProcessError("standard-input descriptor is invalid")
    try:
        before = os.fstat(descriptor)
        flags = fcntl.fcntl(descriptor, fcntl.F_GETFL)
        descriptor_flags = fcntl.fcntl(descriptor, fcntl.F_GETFD)
        offset = os.lseek(descriptor, 0, os.SEEK_CUR)
    except OSError as error:
        raise BoundedProcessError(
            "standard-input descriptor cannot be inspected"
        ) from error
    witness = _standard_input_descriptor_witness(before)
    _validate_standard_input_descriptor(witness)
    if before.st_uid != os.geteuid():
        raise BoundedProcessError("standard-input descriptor is not caller-owned")
    if flags & os.O_ACCMODE != os.O_RDONLY:
        raise BoundedProcessError("standard-input descriptor must be read-only")
    if descriptor_flags & fcntl.FD_CLOEXEC == 0:
        raise BoundedProcessError("standard-input descriptor must be close-on-exec")
    if offset != 0:
        raise BoundedProcessError(
            "standard-input descriptor must be positioned at zero"
        )
    payload = bytearray()
    while len(payload) < before.st_size:
        try:
            chunk = os.pread(
                descriptor,
                min(64 * 1024, before.st_size - len(payload)),
                len(payload),
            )
        except OSError as error:
            raise BoundedProcessError(
                "standard-input descriptor cannot be read"
            ) from error
        if not chunk:
            raise BoundedProcessError("standard-input descriptor ended early")
        payload.extend(chunk)
    try:
        after = os.fstat(descriptor)
        final_offset = os.lseek(descriptor, 0, os.SEEK_CUR)
    except OSError as error:
        raise BoundedProcessError(
            "standard-input descriptor cannot be revalidated"
        ) from error
    if _stat_fields(before) != _stat_fields(after) or final_offset != 0:
        raise BoundedProcessError("standard-input descriptor changed during capture")
    return bytes(payload), witness


def _prepare_standard_input(
    stdin_bytes: bytes | None,
    stdin_fd: int | None,
) -> tuple[bytes | None, dict[str, Any]]:
    if stdin_bytes is not None and stdin_fd is not None:
        raise BoundedProcessError(
            "standard input must use bytes or one private descriptor, not both"
        )
    if stdin_bytes is not None:
        if (
            type(stdin_bytes) is not bytes
            or len(stdin_bytes) > MAX_STANDARD_INPUT_BYTES
        ):
            raise BoundedProcessError("standard-input bytes exceeded their exact bound")
        return stdin_bytes, _standard_input_identity(stdin_bytes, source="bytes")
    if stdin_fd is not None:
        payload, descriptor = _read_private_standard_input_descriptor(stdin_fd)
        return payload, _standard_input_identity(
            payload,
            source="descriptor",
            source_descriptor=descriptor,
        )
    return None, _standard_input_identity(None, source="none")


def _stage_standard_input(
    payload: bytes, *, prefix: str = "pmux-evidence-stdin-"
) -> int:
    write_descriptor = -1
    read_descriptor = -1
    path = ""
    try:
        write_descriptor, path = tempfile.mkstemp(prefix=prefix)
        os.fchmod(write_descriptor, 0o600)
        view = memoryview(payload)
        while view:
            written = os.write(write_descriptor, view)
            if written < 1:
                raise BoundedProcessError("standard-input staging made no progress")
            view = view[written:]
        os.fsync(write_descriptor)
        before = os.fstat(write_descriptor)
        read_descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        if _stat_fields(before) != _stat_fields(os.fstat(read_descriptor)):
            raise BoundedProcessError("standard-input staging identity changed")
        os.unlink(path)
        path = ""
        os.close(write_descriptor)
        write_descriptor = -1
        staged = os.fstat(read_descriptor)
        if (
            not stat.S_ISREG(staged.st_mode)
            or staged.st_uid != os.geteuid()
            or staged.st_nlink != 0
            or stat.S_IMODE(staged.st_mode) != 0o600
            or staged.st_size != len(payload)
            or os.lseek(read_descriptor, 0, os.SEEK_CUR) != 0
        ):
            raise BoundedProcessError("standard-input staging is not private and exact")
        if _descriptor_sha256(read_descriptor) != hashlib.sha256(payload).hexdigest():
            raise BoundedProcessError("standard-input staging content changed")
        os.lseek(read_descriptor, 0, os.SEEK_SET)
        os.set_inheritable(read_descriptor, False)
        return read_descriptor
    except BaseException:
        if read_descriptor >= 0:
            try:
                os.close(read_descriptor)
            except OSError:
                pass
        raise
    finally:
        if write_descriptor >= 0:
            try:
                os.close(write_descriptor)
            except OSError:
                pass
        if path:
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass


def _create_ownership_marker() -> tuple[int, int | None, dict[str, Any]]:
    soft_limit, _hard_limit = resource.getrlimit(resource.RLIMIT_NOFILE)
    if soft_limit != resource.RLIM_INFINITY and soft_limit <= OWNERSHIP_MARKER_FD:
        raise BoundedProcessError(
            "open-file limit cannot hold the reserved ownership-marker descriptor"
        )
    payload = os.urandom(OWNERSHIP_MARKER_BYTES)
    descriptor = _stage_standard_input(
        payload,
        prefix="pmux-evidence-owner-",
    )
    if descriptor == OWNERSHIP_MARKER_FD:
        try:
            replacement = fcntl.fcntl(
                descriptor,
                getattr(fcntl, "F_DUPFD_CLOEXEC", fcntl.F_DUPFD),
                3,
            )
        except OSError:
            os.close(descriptor)
            raise
        os.close(descriptor)
        descriptor = replacement
        os.set_inheritable(descriptor, False)
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or metadata.st_nlink != 0
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size != OWNERSHIP_MARKER_BYTES
        or os.lseek(descriptor, 0, os.SEEK_CUR) != 0
        or os.get_inheritable(descriptor)
    ):
        os.close(descriptor)
        raise BoundedProcessError("ownership marker is not private and exact")
    witness = {
        "schema_version": 1,
        "fd": OWNERSHIP_MARKER_FD,
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "uid": metadata.st_uid,
        "gid": metadata.st_gid,
        "mode": metadata.st_mode,
        "nlink": metadata.st_nlink,
        "size": metadata.st_size,
        "sha256": hashlib.sha256(payload).hexdigest(),
        "unlinked_before_launch": True,
        "writer_closed_before_launch": True,
        "inherited_read_only": True,
    }
    if _descriptor_sha256(descriptor) != witness["sha256"]:
        os.close(descriptor)
        raise BoundedProcessError("ownership marker content changed")
    reservation: int | None = None
    try:
        fcntl.fcntl(OWNERSHIP_MARKER_FD, fcntl.F_GETFD)
        collision = True
    except OSError as error:
        if error.errno != errno.EBADF:
            os.close(descriptor)
            raise BoundedProcessError(
                "ownership-marker target descriptor cannot be inspected"
            ) from error
        os.dup2(descriptor, OWNERSHIP_MARKER_FD, inheritable=False)
        reservation = OWNERSHIP_MARKER_FD
        collision = False
    witness["parent_fd_collision"] = collision
    return descriptor, reservation, witness


def _ownership_marker_receipt(
    witness: Mapping[str, Any],
    *,
    leader_verified_before_release: bool,
    leader_marker_loss_observed_at_exit: bool,
    supervisor_descriptor_closed: bool,
) -> dict[str, Any]:
    return {
        **dict(witness),
        "leader_verified_before_release": leader_verified_before_release,
        "leader_marker_loss_observed_at_exit": leader_marker_loss_observed_at_exit,
        "supervisor_descriptor_closed": supervisor_descriptor_closed,
    }


def _validate_ownership_marker(
    value: object,
    *,
    require_leader_verified: bool | None,
    require_supervisor_closed: bool | None,
) -> dict[str, Any]:
    if not isinstance(value, Mapping) or frozenset(value) != OWNERSHIP_MARKER_KEYS:
        raise BoundedProcessError("receipt ownership-marker fields are not exact")
    marker = dict(value)
    if (
        not _exact_int(marker.get("schema_version"), minimum=1, maximum=1)
        or not _exact_int(
            marker.get("fd"),
            minimum=OWNERSHIP_MARKER_FD,
            maximum=OWNERSHIP_MARKER_FD,
        )
        or not _exact_int(marker.get("device"), minimum=0)
        or not _exact_int(marker.get("inode"), minimum=1)
        or not _exact_int(marker.get("uid"), minimum=0)
        or not _exact_int(marker.get("gid"), minimum=0)
        or not _exact_int(marker.get("mode"), minimum=1)
        or not stat.S_ISREG(marker["mode"])
        or stat.S_IMODE(marker["mode"]) != 0o600
        or not _exact_int(marker.get("nlink"), minimum=0, maximum=0)
        or not _exact_int(
            marker.get("size"),
            minimum=OWNERSHIP_MARKER_BYTES,
            maximum=OWNERSHIP_MARKER_BYTES,
        )
        or not _sha256(marker.get("sha256"))
        or marker.get("unlinked_before_launch") is not True
        or marker.get("writer_closed_before_launch") is not True
        or marker.get("inherited_read_only") is not True
        or type(marker.get("parent_fd_collision")) is not bool
        or type(marker.get("leader_verified_before_release")) is not bool
        or type(marker.get("leader_marker_loss_observed_at_exit")) is not bool
        or (
            marker["leader_marker_loss_observed_at_exit"]
            and not marker["leader_verified_before_release"]
        )
        or type(marker.get("supervisor_descriptor_closed")) is not bool
        or (
            require_leader_verified is not None
            and marker["leader_verified_before_release"] is not require_leader_verified
        )
        or (
            require_supervisor_closed is not None
            and marker["supervisor_descriptor_closed"] is not require_supervisor_closed
        )
    ):
        raise BoundedProcessError("receipt ownership marker is malformed")
    return marker


def _witness_object(witness: BoundExecutable) -> dict[str, Any]:
    return {
        "path": witness.path,
        "device": witness.device,
        "inode": witness.inode,
        "uid": witness.uid,
        "gid": witness.gid,
        "mode": witness.mode,
        "nlink": witness.nlink,
        "size": witness.size,
        "mtime_ns": witness.mtime_ns,
        "ctime_ns": witness.ctime_ns,
        "sha256": witness.sha256,
        "executable_format": witness.executable_format,
    }


def validate_execution_receipt(value: Mapping[str, Any]) -> dict[str, Any]:
    expected_keys = frozenset(
        {
            "schema_version",
            "kind",
            "executable",
            "argv",
            "cwd",
            "cwd_witness",
            "environment",
            "standard_input",
            "ownership_marker",
            "timeout_seconds",
            "drain_timeout_seconds",
            "maximum_output_bytes",
            "exit_code",
            "stdout_size",
            "stdout_sha256",
            "stderr_size",
            "stderr_sha256",
            "process_ledger",
            "process_ledger_sha256",
            "receipt_sha256",
        }
    )
    if not isinstance(value, Mapping) or frozenset(value) != expected_keys:
        raise BoundedProcessError("execution receipt fields are not exact")
    receipt = dict(value)
    executable = receipt.get("executable")
    if not isinstance(executable, Mapping):
        raise BoundedProcessError("execution receipt executable is malformed")
    expected_executable_keys = frozenset(
        {
            "path",
            "device",
            "inode",
            "uid",
            "gid",
            "mode",
            "nlink",
            "size",
            "mtime_ns",
            "ctime_ns",
            "sha256",
            "executable_format",
        }
    )
    if frozenset(executable) != expected_executable_keys:
        raise BoundedProcessError("execution receipt executable fields are not exact")
    try:
        witness = BoundExecutable(**dict(executable))
    except TypeError as error:
        raise BoundedProcessError(
            "execution receipt executable is malformed"
        ) from error
    _validate_witness(witness)
    argv = receipt.get("argv")
    cwd_witness = _validate_cwd_witness(receipt.get("cwd_witness"))
    _validate_environment_identity(receipt.get("environment"))
    _validate_standard_input_identity(receipt.get("standard_input"))
    _validate_ownership_marker(
        receipt.get("ownership_marker"),
        require_leader_verified=True,
        require_supervisor_closed=True,
    )
    ledger_raw = receipt.get("process_ledger")
    if not isinstance(ledger_raw, list):
        raise BoundedProcessError("execution receipt process ledger is malformed")
    ledger = validate_process_ledger(ledger_raw)
    if (
        not _exact_int(receipt.get("schema_version"), minimum=1, maximum=1)
        or receipt.get("kind") != "pmux_bounded_process"
        or not isinstance(argv, list)
        or not argv
        or not all(isinstance(item, str) and item and "\0" not in item for item in argv)
        or argv[0] != witness.path
        or not isinstance(receipt.get("cwd"), str)
        or "\0" in receipt["cwd"]
        or not pathlib.Path(receipt["cwd"]).is_absolute()
        or receipt["cwd"] != os.path.normpath(receipt["cwd"])
        or cwd_witness["path"] != receipt["cwd"]
        or not _exact_int(receipt.get("timeout_seconds"), minimum=1, maximum=86_400)
        or not _exact_int(
            receipt.get("drain_timeout_seconds"),
            minimum=1,
            maximum=receipt["timeout_seconds"],
        )
        or not _exact_int(
            receipt.get("maximum_output_bytes"),
            minimum=1,
            maximum=1024 * 1024 * 1024,
        )
        or not _exact_int(receipt.get("exit_code"), minimum=-255, maximum=255)
        or not _exact_int(
            receipt.get("stdout_size"),
            minimum=0,
            maximum=receipt["maximum_output_bytes"],
        )
        or not _sha256(receipt.get("stdout_sha256"))
        or not _exact_int(
            receipt.get("stderr_size"),
            minimum=0,
            maximum=receipt["maximum_output_bytes"],
        )
        or receipt["stdout_size"] + receipt["stderr_size"]
        > receipt["maximum_output_bytes"]
        or not _sha256(receipt.get("stderr_sha256"))
        or receipt.get("process_ledger_sha256")
        != _canonical_json_sha256(list(ledger), domain=PROCESS_LEDGER_DOMAIN)
    ):
        raise BoundedProcessError("execution receipt binding is invalid")
    digest = receipt.get("receipt_sha256")
    if not _sha256(digest):
        raise BoundedProcessError("execution receipt digest is malformed")
    body = dict(receipt)
    del body["receipt_sha256"]
    if digest != _canonical_json_sha256(body, domain=EXECUTION_RECEIPT_DOMAIN):
        raise BoundedProcessError("execution receipt digest does not match")
    return receipt


def load_execution_receipt(payload: bytes) -> dict[str, Any]:
    if not isinstance(payload, bytes) or len(payload) > 4 * 1024 * 1024:
        raise BoundedProcessError("execution receipt JSON exceeded its bound")

    def object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise BoundedProcessError(f"execution receipt JSON repeats key: {key}")
            result[key] = value
        return result

    def reject_constant(value: str) -> None:
        raise BoundedProcessError(
            f"execution receipt JSON contains non-finite number: {value}"
        )

    try:
        loaded = json.loads(
            payload.decode("utf-8"),
            object_pairs_hook=object_pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise BoundedProcessError("execution receipt JSON is malformed") from error
    if not isinstance(loaded, dict):
        raise BoundedProcessError("execution receipt JSON is not an object")
    return validate_execution_receipt(loaded)


def dump_execution_receipt(value: Mapping[str, Any]) -> bytes:
    receipt = validate_execution_receipt(value)
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


def validate_failure_receipt(value: Mapping[str, Any]) -> dict[str, Any]:
    expected_keys = frozenset(
        {
            "schema_version",
            "kind",
            "executable",
            "argv",
            "cwd",
            "cwd_witness",
            "environment",
            "standard_input",
            "ownership_marker",
            "timeout_seconds",
            "drain_timeout_seconds",
            "maximum_output_bytes",
            "leader_pid",
            "failure_reason",
            "cleanup_complete",
            "output_complete",
            "output_limit_stream",
            "exit_code",
            "stdout_size",
            "stdout_sha256",
            "stderr_size",
            "stderr_sha256",
            "process_observation_complete",
            "process_ledger",
            "process_ledger_sha256",
            "receipt_sha256",
        }
    )
    if not isinstance(value, Mapping) or frozenset(value) != expected_keys:
        raise BoundedProcessError("failure receipt fields are not exact")
    receipt = dict(value)
    executable = receipt.get("executable")
    if not isinstance(executable, Mapping):
        raise BoundedProcessError("failure receipt executable is malformed")
    expected_executable_keys = frozenset(
        {
            "path",
            "device",
            "inode",
            "uid",
            "gid",
            "mode",
            "nlink",
            "size",
            "mtime_ns",
            "ctime_ns",
            "sha256",
            "executable_format",
        }
    )
    if frozenset(executable) != expected_executable_keys:
        raise BoundedProcessError("failure receipt executable fields are not exact")
    try:
        witness = BoundExecutable(**dict(executable))
    except TypeError as error:
        raise BoundedProcessError("failure receipt executable is malformed") from error
    _validate_witness(witness)
    argv = receipt.get("argv")
    cwd_witness = _validate_cwd_witness(receipt.get("cwd_witness"))
    _validate_environment_identity(receipt.get("environment"))
    _validate_standard_input_identity(receipt.get("standard_input"))
    ownership_marker = _validate_ownership_marker(
        receipt.get("ownership_marker"),
        require_leader_verified=None,
        require_supervisor_closed=None,
    )
    ledger_raw = receipt.get("process_ledger")
    if not isinstance(ledger_raw, list):
        raise BoundedProcessError("failure receipt process ledger is malformed")
    ledger = validate_process_ledger(
        ledger_raw,
        require_nonempty=False,
        require_all_reaped=False,
    )
    exit_code = receipt.get("exit_code")
    output_limit_stream = receipt.get("output_limit_stream")
    observation_complete = receipt.get("process_observation_complete")
    cleanup_complete = receipt.get("cleanup_complete")
    leader_pid = receipt.get("leader_pid")
    leader_observed = any(record["pid"] == leader_pid for record in ledger)
    expected_cleanup = (
        observation_complete is True
        and exit_code is not None
        and leader_observed
        and ownership_marker["supervisor_descriptor_closed"] is True
        and all(record["reaped"] is True for record in ledger)
    )
    if (
        not _exact_int(receipt.get("schema_version"), minimum=1, maximum=1)
        or receipt.get("kind") != "pmux_bounded_process_failure"
        or not isinstance(argv, list)
        or not argv
        or not all(isinstance(item, str) and item and "\0" not in item for item in argv)
        or argv[0] != witness.path
        or not isinstance(receipt.get("cwd"), str)
        or "\0" in receipt["cwd"]
        or not pathlib.Path(receipt["cwd"]).is_absolute()
        or receipt["cwd"] != os.path.normpath(receipt["cwd"])
        or cwd_witness["path"] != receipt["cwd"]
        or not _exact_int(receipt.get("timeout_seconds"), minimum=1, maximum=86_400)
        or not _exact_int(
            receipt.get("drain_timeout_seconds"),
            minimum=1,
            maximum=receipt["timeout_seconds"],
        )
        or not _exact_int(
            receipt.get("maximum_output_bytes"),
            minimum=1,
            maximum=1024 * 1024 * 1024,
        )
        or not _exact_int(leader_pid, minimum=1)
        or not isinstance(receipt.get("failure_reason"), str)
        or receipt["failure_reason"] not in FAILURE_REASONS
        or type(cleanup_complete) is not bool
        or type(receipt.get("output_complete")) is not bool
        or output_limit_stream not in (None, "stdout", "stderr")
        or (receipt["failure_reason"] == "output_limit" and output_limit_stream is None)
        or (output_limit_stream is not None and receipt["output_complete"] is True)
        or (
            exit_code is not None
            and not _exact_int(exit_code, minimum=-255, maximum=255)
        )
        or (
            ownership_marker["leader_marker_loss_observed_at_exit"]
            and exit_code is None
        )
        or not _exact_int(
            receipt.get("stdout_size"),
            minimum=0,
            maximum=receipt["maximum_output_bytes"],
        )
        or not _sha256(receipt.get("stdout_sha256"))
        or not _exact_int(
            receipt.get("stderr_size"),
            minimum=0,
            maximum=receipt["maximum_output_bytes"],
        )
        or receipt["stdout_size"] + receipt["stderr_size"]
        > receipt["maximum_output_bytes"]
        or not _sha256(receipt.get("stderr_sha256"))
        or type(observation_complete) is not bool
        or (observation_complete is True and not leader_observed)
        or cleanup_complete is not expected_cleanup
        or receipt.get("process_ledger_sha256")
        != _canonical_json_sha256(list(ledger), domain=PROCESS_LEDGER_DOMAIN)
    ):
        raise BoundedProcessError("failure receipt binding is invalid")
    digest = receipt.get("receipt_sha256")
    if not _sha256(digest):
        raise BoundedProcessError("failure receipt digest is malformed")
    body = dict(receipt)
    del body["receipt_sha256"]
    if digest != _canonical_json_sha256(body, domain=FAILURE_RECEIPT_DOMAIN):
        raise BoundedProcessError("failure receipt digest does not match")
    return receipt


def load_failure_receipt(payload: bytes) -> dict[str, Any]:
    if not isinstance(payload, bytes) or len(payload) > 4 * 1024 * 1024:
        raise BoundedProcessError("failure receipt JSON exceeded its bound")

    def object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, item in pairs:
            if key in result:
                raise BoundedProcessError(f"failure receipt JSON repeats key: {key}")
            result[key] = item
        return result

    def reject_constant(item: str) -> None:
        raise BoundedProcessError(
            f"failure receipt JSON contains non-finite number: {item}"
        )

    try:
        loaded = json.loads(
            payload.decode("utf-8"),
            object_pairs_hook=object_pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise BoundedProcessError("failure receipt JSON is malformed") from error
    if not isinstance(loaded, dict):
        raise BoundedProcessError("failure receipt JSON is not an object")
    return validate_failure_receipt(loaded)


def dump_failure_receipt(value: Mapping[str, Any]) -> bytes:
    receipt = validate_failure_receipt(value)
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


def verify_receipt_context(
    value: Mapping[str, Any],
    *,
    cwd: pathlib.Path,
    environment: Mapping[str, str],
    stdin_bytes: bytes | None = None,
    stdin_fd: int | None = None,
) -> dict[str, Any]:
    """Verify a receipt against caller-held launch context without secrets.

    Receipt schema validation proves internal consistency.  This verifier also
    compares its redacted environment/input identities and cwd inode witness to
    the exact context independently retained by the caller.
    """

    if not isinstance(value, Mapping):
        raise BoundedProcessError("bounded process receipt is not a mapping")
    kind = value.get("kind")
    if kind == "pmux_bounded_process":
        receipt = validate_execution_receipt(value)
    elif kind == "pmux_bounded_process_failure":
        receipt = validate_failure_receipt(value)
    else:
        raise BoundedProcessError("bounded process receipt kind is unknown")
    canonical_cwd, cwd_descriptor, cwd_metadata = _open_cwd(cwd)
    try:
        expected_cwd = _cwd_witness(canonical_cwd, cwd_metadata)
    finally:
        os.close(cwd_descriptor)
    _payload, expected_input = _prepare_standard_input(stdin_bytes, stdin_fd)
    if receipt["cwd_witness"] != expected_cwd:
        raise BoundedProcessError("receipt cwd context does not match")
    if receipt["environment"] != environment_identity(environment):
        raise BoundedProcessError("receipt environment context does not match")
    if receipt["standard_input"] != expected_input:
        raise BoundedProcessError("receipt standard-input context does not match")
    return receipt


def _stat_fields(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_uid,
        metadata.st_gid,
        metadata.st_mode,
        metadata.st_nlink,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def _executable_format(prefix: bytes) -> str:
    if prefix.startswith(b"\x7fELF"):
        return "elf"
    if prefix[:4] in {
        b"\xfe\xed\xfa\xce",
        b"\xce\xfa\xed\xfe",
        b"\xfe\xed\xfa\xcf",
        b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe",
        b"\xbe\xba\xfe\xca",
        b"\xca\xfe\xba\xbf",
        b"\xbf\xba\xfe\xca",
    }:
        return "mach-o"
    if prefix.startswith(b"#!"):
        raise BoundedProcessError(
            "direct script execution is forbidden; bind and invoke its interpreter"
        )
    raise BoundedProcessError("bound executable is not a supported native binary")


def bind_executable(path: pathlib.Path) -> BoundExecutable:
    if not path.is_absolute():
        raise BoundedProcessError("executable path must be absolute")
    try:
        canonical = path.resolve(strict=True)
        descriptor = os.open(
            canonical,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
    except (OSError, RuntimeError) as error:
        raise BoundedProcessError(
            "executable cannot be opened without following"
        ) from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_nlink < 1
            or not os.access(canonical, os.X_OK)
        ):
            raise BoundedProcessError("executable is not an executable regular file")
        digest = hashlib.sha256()
        prefix = b""
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            if len(prefix) < 16:
                prefix += chunk[: 16 - len(prefix)]
            digest.update(chunk)
        after = os.fstat(descriptor)
        current = canonical.lstat()
        if _stat_fields(before) != _stat_fields(after) or _stat_fields(after) != (
            _stat_fields(current)
        ):
            raise BoundedProcessError("executable changed while it was bound")
        return BoundExecutable(
            path=str(canonical),
            device=after.st_dev,
            inode=after.st_ino,
            uid=after.st_uid,
            gid=after.st_gid,
            mode=after.st_mode,
            nlink=after.st_nlink,
            size=after.st_size,
            mtime_ns=after.st_mtime_ns,
            ctime_ns=after.st_ctime_ns,
            sha256=digest.hexdigest(),
            executable_format=_executable_format(prefix),
        )
    finally:
        os.close(descriptor)


def _witness_stat(witness: BoundExecutable) -> tuple[int, ...]:
    return (
        witness.device,
        witness.inode,
        witness.uid,
        witness.gid,
        witness.mode,
        witness.nlink,
        witness.size,
        witness.mtime_ns,
        witness.ctime_ns,
    )


def _validate_witness(witness: BoundExecutable) -> None:
    if (
        not isinstance(witness, BoundExecutable)
        or not isinstance(witness.path, str)
        or not witness.path
        or "\0" in witness.path
        or not pathlib.Path(witness.path).is_absolute()
        or witness.path != os.path.normpath(witness.path)
        or not all(
            _exact_int(value, minimum=minimum)
            for value, minimum in (
                (witness.device, 0),
                (witness.inode, 1),
                (witness.uid, 0),
                (witness.gid, 0),
                (witness.mode, 1),
                (witness.nlink, 1),
                (witness.size, 1),
                (witness.mtime_ns, 0),
                (witness.ctime_ns, 0),
            )
        )
        or not _sha256(witness.sha256)
        or not isinstance(witness.executable_format, str)
        or witness.executable_format not in {"elf", "mach-o"}
    ):
        raise BoundedProcessError("executable witness is malformed")


def _descriptor_sha256(descriptor: int) -> str:
    digest = hashlib.sha256()
    offset = 0
    while True:
        chunk = os.pread(descriptor, 1024 * 1024, offset)
        if not chunk:
            return digest.hexdigest()
        digest.update(chunk)
        offset += len(chunk)


def _decode_command(raw: bytes) -> str:
    try:
        value = raw.decode("utf-8")
    except UnicodeDecodeError:
        value = f"command-bytes:{raw.hex()}"
    return value or "unknown-command"


def _darwin_information(pid: int) -> _DarwinProcBsdInfo | None:
    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    proc_pidinfo = library.proc_pidinfo
    proc_pidinfo.argtypes = [
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_uint64,
        ctypes.c_void_p,
        ctypes.c_int,
    ]
    proc_pidinfo.restype = ctypes.c_int
    information = _DarwinProcBsdInfo()
    returned = proc_pidinfo(
        pid, 3, 0, ctypes.byref(information), ctypes.sizeof(information)
    )
    if returned == 0:
        return None
    if returned != ctypes.sizeof(information) or information.pbi_pid != pid:
        raise BoundedProcessError("macOS returned partial process information")
    return information


def precise_process_started(pid: int) -> str | None:
    if sys.platform == "darwin":
        information = _darwin_information(pid)
        if information is None:
            return None
        if information.pbi_start_tvsec < 1 or information.pbi_start_tvusec >= 1_000_000:
            raise BoundedProcessError("macOS returned an invalid process birth token")
        return (
            f"darwin:{information.pbi_start_tvsec}:{information.pbi_start_tvusec:06d}"
        )
    if sys.platform.startswith("linux"):
        try:
            payload = (pathlib.Path("/proc") / str(pid) / "stat").read_bytes()
            boot_id = (
                pathlib.Path("/proc/sys/kernel/random/boot_id")
                .read_text(encoding="ascii")
                .strip()
            )
        except FileNotFoundError:
            return None
        except OSError as error:
            raise BoundedProcessError(
                "Linux process birth token is unreadable"
            ) from error
        closing = payload.rfind(b")")
        fields = payload[closing + 2 :].split() if closing >= 0 else []
        if len(fields) < 20 or not boot_id:
            raise BoundedProcessError("Linux returned an invalid process birth token")
        try:
            start_ticks = int(fields[19])
        except ValueError as error:
            raise BoundedProcessError("Linux returned an invalid start tick") from error
        return f"linux:{boot_id}:{start_ticks}"
    raise BoundedProcessError(f"unsupported process platform: {sys.platform}")


def _snapshot() -> dict[int, ProcessIdentity]:
    rows: list[ProcessIdentity] = []
    if sys.platform == "darwin":
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        proc_listpids = library.proc_listpids
        proc_listpids.argtypes = [
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_int,
        ]
        proc_listpids.restype = ctypes.c_int
        needed = proc_listpids(1, 0, None, 0)
        if needed < 0 or needed > MAX_PROCESS_COUNT * ctypes.sizeof(ctypes.c_int):
            raise BoundedProcessError("macOS PID enumeration exceeded its bound")
        capacity = max(needed + 4096 * ctypes.sizeof(ctypes.c_int), 4096)
        buffer = (ctypes.c_int * (capacity // ctypes.sizeof(ctypes.c_int)))()
        returned = proc_listpids(1, 0, buffer, ctypes.sizeof(buffer))
        if returned < 0 or returned >= ctypes.sizeof(buffer):
            raise BoundedProcessError("macOS PID enumeration was not stable")
        for pid in sorted(set(buffer[: returned // ctypes.sizeof(ctypes.c_int)])):
            if pid < 1:
                continue
            information = _darwin_information(pid)
            if information is None or information.pbi_uid != os.geteuid():
                continue
            try:
                sid = os.getsid(pid)
            except ProcessLookupError:
                continue
            confirmed = _darwin_information(pid)
            if (
                confirmed is None
                or confirmed.pbi_start_tvsec != information.pbi_start_tvsec
                or confirmed.pbi_start_tvusec != information.pbi_start_tvusec
                or confirmed.pbi_ppid != information.pbi_ppid
                or confirmed.pbi_pgid != information.pbi_pgid
            ):
                continue
            rows.append(
                ProcessIdentity(
                    pid=pid,
                    ppid=int(information.pbi_ppid),
                    pgid=int(information.pbi_pgid),
                    sid=sid,
                    started=(
                        f"darwin:{information.pbi_start_tvsec}:"
                        f"{information.pbi_start_tvusec:06d}"
                    ),
                    command=_decode_command(
                        bytes(information.pbi_name) or bytes(information.pbi_comm)
                    ),
                )
            )
    elif sys.platform.startswith("linux"):
        try:
            boot_id = (
                pathlib.Path("/proc/sys/kernel/random/boot_id")
                .read_text(encoding="ascii")
                .strip()
            )
            proc_entries = list(pathlib.Path("/proc").iterdir())
        except OSError as error:
            raise BoundedProcessError("Linux PID enumeration failed") from error
        if not boot_id or len(proc_entries) > MAX_PROCESS_COUNT:
            raise BoundedProcessError("Linux PID enumeration exceeded its bound")
        for entry in proc_entries:
            if not entry.name.isdigit():
                continue
            try:
                payload = (entry / "stat").read_bytes()
                owner = entry.stat().st_uid
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
            if owner != os.geteuid():
                continue
            opening = payload.find(b"(")
            closing = payload.rfind(b")")
            fields = payload[closing + 2 :].split() if closing > opening >= 0 else []
            if len(fields) < 20:
                continue
            try:
                rows.append(
                    ProcessIdentity(
                        pid=int(entry.name),
                        ppid=int(fields[1]),
                        pgid=int(fields[2]),
                        sid=int(fields[3]),
                        started=f"linux:{boot_id}:{int(fields[19])}",
                        command=_decode_command(payload[opening + 1 : closing]),
                    )
                )
            except ValueError:
                continue
    else:
        raise BoundedProcessError(f"unsupported process platform: {sys.platform}")
    snapshot: dict[int, ProcessIdentity] = {}
    for identity in rows:
        if min(identity.pid, identity.ppid, identity.pgid, identity.sid) < 0:
            continue
        if identity.pid in snapshot:
            raise BoundedProcessError("PID enumeration returned a duplicate")
        snapshot[identity.pid] = identity
    if os.getpid() not in snapshot:
        raise BoundedProcessError("live process root is absent from the snapshot")
    return snapshot


def _same_process(
    expected: ProcessIdentity | None, current: ProcessIdentity | None
) -> bool:
    return (
        expected is not None
        and current is not None
        and expected.pid == current.pid
        and expected.started == current.started
    )


def _assert_owned_pid_identities(
    owned: Mapping[int, ProcessIdentity], current: Mapping[int, ProcessIdentity]
) -> None:
    """Fail closed if an already-owned PID now names a different process birth."""
    for pid, expected in owned.items():
        observed = current.get(pid)
        if observed is not None and not _same_process(expected, observed):
            raise BoundedProcessError(f"owned PID {pid} identity changed")


def _process_has_ownership_marker(pid: int, marker: Mapping[str, Any]) -> bool:
    _validate_ownership_marker(
        _ownership_marker_receipt(
            marker,
            leader_verified_before_release=False,
            leader_marker_loss_observed_at_exit=False,
            supervisor_descriptor_closed=False,
        ),
        require_leader_verified=False,
        require_supervisor_closed=False,
    )
    if sys.platform.startswith("linux"):
        try:
            metadata = (
                pathlib.Path("/proc") / str(pid) / "fd" / str(OWNERSHIP_MARKER_FD)
            ).stat()
        except (FileNotFoundError, ProcessLookupError):
            return False
        except (PermissionError, OSError) as error:
            raise BoundedProcessError(
                "ownership-marker descriptor observation failed"
            ) from error
        return (
            metadata.st_dev == marker["device"]
            and metadata.st_ino == marker["inode"]
            and metadata.st_uid == marker["uid"]
            and metadata.st_gid == marker["gid"]
            and metadata.st_mode == marker["mode"]
            and metadata.st_nlink == marker["nlink"]
            and metadata.st_size == marker["size"]
        )
    if sys.platform == "darwin":
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        proc_pidfdinfo = library.proc_pidfdinfo
        proc_pidfdinfo.argtypes = [
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.c_int,
        ]
        proc_pidfdinfo.restype = ctypes.c_int
        information = _DarwinVnodeFdInfo()
        returned = proc_pidfdinfo(
            pid,
            OWNERSHIP_MARKER_FD,
            1,
            ctypes.byref(information),
            ctypes.sizeof(information),
        )
        if returned == 0:
            return False
        if returned != ctypes.sizeof(information):
            raise BoundedProcessError(
                "Darwin ownership-marker descriptor observation was partial"
            )
        observed = information.pvi.vi_stat
        return (
            observed.vst_dev == marker["device"] & 0xFFFFFFFF
            and observed.vst_ino == marker["inode"]
            and observed.vst_uid == marker["uid"]
            and observed.vst_gid == marker["gid"]
            and observed.vst_mode == marker["mode"] & 0xFFFF
            and observed.vst_nlink == marker["nlink"]
            and observed.vst_size == marker["size"]
        )
    raise BoundedProcessError(f"unsupported process platform: {sys.platform}")


def _darwin_spawn_suspended(
    executable: pathlib.Path,
    argv: Sequence[str],
    environment: Mapping[str, str],
    *,
    cwd_fd: int,
    stdin_fd: int | None,
    stdout_fd: int,
    stderr_fd: int,
    ownership_marker_fd: int,
) -> int:
    library = ctypes.CDLL(None, use_errno=True)
    attribute = ctypes.c_void_p()
    actions = ctypes.c_void_p()

    def function(name: str, argument_types: Sequence[object]) -> Any:
        item = getattr(library, name)
        item.argtypes = list(argument_types)
        item.restype = ctypes.c_int
        return item

    attribute_init = function(
        "posix_spawnattr_init", (ctypes.POINTER(ctypes.c_void_p),)
    )
    attribute_flags = function(
        "posix_spawnattr_setflags",
        (ctypes.POINTER(ctypes.c_void_p), ctypes.c_short),
    )
    attribute_destroy = function(
        "posix_spawnattr_destroy", (ctypes.POINTER(ctypes.c_void_p),)
    )
    actions_init = function(
        "posix_spawn_file_actions_init", (ctypes.POINTER(ctypes.c_void_p),)
    )
    actions_dup2 = function(
        "posix_spawn_file_actions_adddup2",
        (ctypes.POINTER(ctypes.c_void_p), ctypes.c_int, ctypes.c_int),
    )
    actions_open = function(
        "posix_spawn_file_actions_addopen",
        (
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_uint16,
        ),
    )
    actions_fchdir = function(
        "posix_spawn_file_actions_addfchdir_np",
        (ctypes.POINTER(ctypes.c_void_p), ctypes.c_int),
    )
    actions_destroy = function(
        "posix_spawn_file_actions_destroy", (ctypes.POINTER(ctypes.c_void_p),)
    )
    spawn = function(
        "posix_spawn",
        (
            ctypes.POINTER(ctypes.c_int),
            ctypes.c_char_p,
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_char_p),
            ctypes.POINTER(ctypes.c_char_p),
        ),
    )

    attribute_ready = False
    actions_ready = False
    spawned_pid = 0
    try:
        status_value = attribute_init(ctypes.byref(attribute))
        if status_value != 0:
            raise BoundedProcessError(
                f"Darwin spawn attribute init failed: {os.strerror(status_value)}"
            )
        attribute_ready = True
        status_value = attribute_flags(
            ctypes.byref(attribute), 0x0080 | 0x0400 | 0x4000
        )
        if status_value != 0:
            raise BoundedProcessError(
                f"Darwin spawn attribute flags failed: {os.strerror(status_value)}"
            )
        status_value = actions_init(ctypes.byref(actions))
        if status_value != 0:
            raise BoundedProcessError(
                f"Darwin spawn action init failed: {os.strerror(status_value)}"
            )
        actions_ready = True
        stdin_action = (
            actions_open(
                ctypes.byref(actions), 0, os.fsencode(os.devnull), os.O_RDONLY, 0
            )
            if stdin_fd is None
            else actions_dup2(ctypes.byref(actions), stdin_fd, 0)
        )
        action_calls = (
            stdin_action,
            actions_dup2(ctypes.byref(actions), stdout_fd, 1),
            actions_dup2(ctypes.byref(actions), stderr_fd, 2),
            actions_fchdir(ctypes.byref(actions), cwd_fd),
            actions_dup2(
                ctypes.byref(actions), ownership_marker_fd, OWNERSHIP_MARKER_FD
            ),
        )
        if any(item != 0 for item in action_calls):
            raise BoundedProcessError("Darwin spawn file action failed")
        encoded_argv = [os.fsencode(value) for value in argv]
        encoded_environment = [
            os.fsencode(f"{name}={value}")
            for name, value in sorted(environment.items())
        ]
        if any(b"\0" in value for value in (*encoded_argv, *encoded_environment)):
            raise BoundedProcessError("Darwin spawn input contains NUL")
        argv_array = (ctypes.c_char_p * (len(encoded_argv) + 1))(*encoded_argv, None)
        environment_array = (ctypes.c_char_p * (len(encoded_environment) + 1))(
            *encoded_environment, None
        )
        pid_value = ctypes.c_int()
        status_value = spawn(
            ctypes.byref(pid_value),
            os.fsencode(executable),
            ctypes.byref(actions),
            ctypes.byref(attribute),
            argv_array,
            environment_array,
        )
        if status_value != 0 or pid_value.value < 1:
            detail = os.strerror(status_value) if status_value else "invalid PID"
            raise BoundedProcessError(f"Darwin suspended spawn failed: {detail}")
        spawned_pid = pid_value.value
        return spawned_pid
    finally:
        destroy_errors: list[int] = []
        if actions_ready:
            destroy_errors.append(actions_destroy(ctypes.byref(actions)))
        if attribute_ready:
            destroy_errors.append(attribute_destroy(ctypes.byref(attribute)))
        if any(item != 0 for item in destroy_errors):
            if spawned_pid:
                try:
                    os.kill(spawned_pid, signal.SIGKILL)
                    os.waitpid(spawned_pid, 0)
                except (ChildProcessError, ProcessLookupError):
                    pass
            raise BoundedProcessError("Darwin suspended spawn cleanup failed")


def _darwin_mapped_executable_matches(pid: int, expected: os.stat_result) -> bool:
    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    proc_pidinfo = library.proc_pidinfo
    proc_pidinfo.argtypes = [
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_uint64,
        ctypes.c_void_p,
        ctypes.c_int,
    ]
    proc_pidinfo.restype = ctypes.c_int
    address = 0
    for _index in range(4096):
        information = ctypes.create_string_buffer(1272)
        returned = proc_pidinfo(pid, 8, address, information, len(information))
        if returned == 0:
            return False
        if returned != len(information):
            raise BoundedProcessError("Darwin returned a partial mapped vnode")
        raw = information.raw
        protection = int.from_bytes(raw[0:4], "little")
        region_address = int.from_bytes(raw[80:88], "little")
        region_size = int.from_bytes(raw[88:96], "little")
        device = int.from_bytes(raw[96:100], "little")
        mode = int.from_bytes(raw[100:102], "little")
        inode = int.from_bytes(raw[104:112], "little")
        if (
            protection & 0x4
            and device == expected.st_dev & 0xFFFFFFFF
            and inode == expected.st_ino
            and stat.S_IFMT(mode) == stat.S_IFREG
        ):
            return True
        next_address = region_address + region_size
        if region_size == 0 or next_address <= address:
            raise BoundedProcessError("Darwin mapped-vnode probe did not advance")
        address = next_address
    raise BoundedProcessError("Darwin mapped-vnode probe exceeded its bound")


def _open_bound_executable(witness: BoundExecutable) -> tuple[int, os.stat_result]:
    _validate_witness(witness)
    path = pathlib.Path(witness.path)
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise BoundedProcessError("executable witness path is not canonical")
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
    except OSError as error:
        raise BoundedProcessError("bound executable cannot be reopened") from error
    current = os.fstat(descriptor)
    if _stat_fields(current) != _witness_stat(witness):
        os.close(descriptor)
        raise BoundedProcessError("bound executable identity changed")
    prefix = os.pread(descriptor, 16, 0)
    if _executable_format(prefix) != witness.executable_format:
        os.close(descriptor)
        raise BoundedProcessError("bound executable format changed")
    if _descriptor_sha256(descriptor) != witness.sha256:
        os.close(descriptor)
        raise BoundedProcessError("bound executable content changed")
    return descriptor, current


def _open_cwd(path: pathlib.Path | None) -> tuple[pathlib.Path, int, os.stat_result]:
    selected = pathlib.Path.cwd() if path is None else path
    if not selected.is_absolute():
        raise BoundedProcessError("cwd must be absolute")
    try:
        canonical = selected.resolve(strict=True)
    except OSError as error:
        raise BoundedProcessError("cwd is missing") from error
    if canonical != selected:
        raise BoundedProcessError("cwd must already be canonical")
    try:
        descriptor = os.open(
            canonical,
            os.O_RDONLY
            | getattr(os, "O_DIRECTORY", 0)
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0),
        )
    except OSError as error:
        raise BoundedProcessError("cwd cannot be opened without following") from error
    metadata = os.fstat(descriptor)
    if not stat.S_ISDIR(metadata.st_mode):
        os.close(descriptor)
        raise BoundedProcessError("cwd is not a directory")
    return canonical, descriptor, metadata


def _cwd_stable(expected: os.stat_result, current: os.stat_result) -> bool:
    fields = ("st_dev", "st_ino", "st_uid", "st_gid", "st_mode")
    return all(getattr(expected, field) == getattr(current, field) for field in fields)


def _write_spool(descriptor: int | None, payload: bytes) -> None:
    if descriptor is None or not payload:
        return
    view = memoryview(payload)
    while view:
        written = os.write(descriptor, view)
        if written < 1:
            raise BoundedProcessError("durable output spool made no progress")
        view = view[written:]
    os.fsync(descriptor)


def _validate_spool(descriptor: int | None, description: str) -> os.stat_result | None:
    if descriptor is None:
        return None
    if not _exact_int(descriptor, minimum=0):
        raise BoundedProcessError(f"{description} spool descriptor is invalid")
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size != 0
        or os.lseek(descriptor, 0, os.SEEK_CUR) != 0
    ):
        raise BoundedProcessError(
            f"{description} spool must be one private owned regular file"
        )
    return metadata


def run(
    executable: BoundExecutable,
    argv: Sequence[str],
    *,
    cwd: pathlib.Path | None,
    environment: Mapping[str, str],
    timeout_seconds: int,
    drain_timeout_seconds: int,
    maximum_output_bytes: int,
    description: str = "bounded process",
    stdout_spool_fd: int | None = None,
    stderr_spool_fd: int | None = None,
    stdin_bytes: bytes | None = None,
    stdin_fd: int | None = None,
) -> RunResult:
    """Run one native executable with bounded time, output, and descendants.

    The optional spool and standard-input descriptors remain caller-owned.
    Standard input is captured into a bounded private read-only staging file
    and its writer is closed before any child code can run.  Bytes are written to
    them before being acknowledged in the in-memory result; both descriptors
    are fsynced on success.  This supports long-running-but-finitely-bounded
    commands without weakening the combined output ceiling.  Detached daemon
    ownership is intentionally unsupported: every observed survivor is reaped
    and the call fails.
    """

    if (
        not isinstance(executable, BoundExecutable)
        or not argv
        or not all(
            isinstance(argument, str) and argument and "\0" not in argument
            for argument in argv
        )
        or argv[0] != executable.path
        or not isinstance(environment, Mapping)
        or not all(
            isinstance(name, str)
            and name
            and "=" not in name
            and "\0" not in name
            and isinstance(value, str)
            and "\0" not in value
            for name, value in environment.items()
        )
        or not _exact_int(timeout_seconds, minimum=1, maximum=86_400)
        or not _exact_int(drain_timeout_seconds, minimum=1, maximum=timeout_seconds)
        or not _exact_int(maximum_output_bytes, minimum=1, maximum=1024 * 1024 * 1024)
        or not isinstance(description, str)
        or not description
    ):
        raise BoundedProcessError("bounded process arguments are invalid")
    try:
        for argument in argv:
            argument.encode("utf-8")
    except UnicodeEncodeError as error:
        raise BoundedProcessError("bounded process argv is not UTF-8") from error
    standard_input_payload, standard_input_receipt = _prepare_standard_input(
        stdin_bytes, stdin_fd
    )
    environment_receipt = environment_identity(environment)
    stdout_spool_before = _validate_spool(stdout_spool_fd, "stdout")
    stderr_spool_before = _validate_spool(stderr_spool_fd, "stderr")
    if (
        stdout_spool_fd is not None
        and stderr_spool_fd is not None
        and stdout_spool_before is not None
        and stderr_spool_before is not None
        and (stdout_spool_before.st_dev, stdout_spool_before.st_ino)
        == (stderr_spool_before.st_dev, stderr_spool_before.st_ino)
    ):
        raise BoundedProcessError("stdout and stderr spools must be distinct")
    source_descriptor = standard_input_receipt["source_descriptor"]
    if source_descriptor is not None:
        for spool in (stdout_spool_before, stderr_spool_before):
            if spool is not None and (
                source_descriptor["device"],
                source_descriptor["inode"],
            ) == (spool.st_dev, spool.st_ino):
                raise BoundedProcessError(
                    "standard input and output spools must be distinct"
                )

    started_at = time.monotonic()
    deadline = started_at + timeout_seconds
    child_environment = dict(environment)
    (
        ownership_marker_fd,
        ownership_marker_reservation_fd,
        ownership_marker_witness,
    ) = _create_ownership_marker()
    leader_marker_verified = False
    leader_marker_loss_observed_at_exit = False
    marker_supervisor_closed = False

    def close_marker_reservation() -> None:
        nonlocal ownership_marker_reservation_fd
        if ownership_marker_reservation_fd is None:
            return
        descriptor = ownership_marker_reservation_fd
        ownership_marker_reservation_fd = None
        os.close(descriptor)

    def close_ownership_marker() -> None:
        nonlocal ownership_marker_fd, marker_supervisor_closed
        if ownership_marker_fd is None:
            return
        descriptor = ownership_marker_fd
        ownership_marker_fd = None
        os.close(descriptor)
        marker_supervisor_closed = True

    try:
        executable_fd, executable_stat = _open_bound_executable(executable)
    except BaseException:
        close_marker_reservation()
        close_ownership_marker()
        raise
    try:
        canonical_cwd, cwd_fd, cwd_stat = _open_cwd(cwd)
    except BaseException:
        os.close(executable_fd)
        close_marker_reservation()
        close_ownership_marker()
        raise
    try:
        baseline = _snapshot()
    except BaseException:
        os.close(cwd_fd)
        os.close(executable_fd)
        close_marker_reservation()
        close_ownership_marker()
        raise
    cwd_receipt = _cwd_witness(canonical_cwd, cwd_stat)
    staged_stdin_fd: int | None = None
    if standard_input_payload is not None:
        try:
            staged_stdin_fd = _stage_standard_input(standard_input_payload)
        except BaseException:
            os.close(cwd_fd)
            os.close(executable_fd)
            close_marker_reservation()
            close_ownership_marker()
            raise

    def verify_bindings() -> None:
        try:
            current_executable = pathlib.Path(executable.path).lstat()
            current_cwd = canonical_cwd.lstat()
        except OSError as error:
            raise _PostLaunchFailure(
                "binding_changed", "launch binding disappeared"
            ) from error
        if _stat_fields(current_executable) != _witness_stat(executable):
            raise _PostLaunchFailure(
                "binding_changed", "executable binding changed during execution"
            )
        if not _cwd_stable(cwd_stat, current_cwd):
            raise _PostLaunchFailure(
                "binding_changed", "cwd binding changed during execution"
            )

    created_pipes: list[tuple[int, int]] = []
    try:
        for _index in range(4):
            created_pipes.append(os.pipe())
    except BaseException:
        for pair in created_pipes:
            for descriptor in pair:
                os.close(descriptor)
        if staged_stdin_fd is not None:
            os.close(staged_stdin_fd)
        close_marker_reservation()
        close_ownership_marker()
        os.close(cwd_fd)
        os.close(executable_fd)
        raise
    (
        (stdout_read, stdout_write),
        (stderr_read, stderr_write),
        (
            ready_read,
            ready_write,
        ),
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
            process_pid = _darwin_spawn_suspended(
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
            raise BoundedProcessError(f"unsupported process platform: {sys.platform}")
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
        try:
            close_marker_reservation()
            close_ownership_marker()
        except OSError:
            pass
        raise

    if process_pid == 0:  # pragma: no cover - exercised as a separate process
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
                OWNERSHIP_MARKER_FD,
                inheritable=True,
            )
            for descriptor in (
                input_descriptor,
                stdout_write,
                stderr_write,
                ownership_marker_fd,
                ownership_marker_reservation_fd,
            ):
                if descriptor is None or descriptor == OWNERSHIP_MARKER_FD:
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
                os.write(2, f"bounded exec failed: {error!r}\n".encode("utf-8"))
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
    close_marker_reservation()

    streams = {"stdout": bytearray(), "stderr": bytearray()}
    try:
        selector = selectors.DefaultSelector()
        selector.register(stdout_read, selectors.EVENT_READ, "stdout")
        selector.register(stderr_read, selectors.EVENT_READ, "stderr")
    except BaseException:
        if "selector" in locals():
            selector.close()
        try:
            os.kill(process_pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(process_pid, 0)
        except ChildProcessError:
            pass
        for descriptor in (stdout_read, stderr_read, ready_read, release_write):
            try:
                os.close(descriptor)
            except OSError:
                pass
        close_ownership_marker()
        raise
    owned: dict[int, ProcessIdentity] = {}
    leader_returncode: int | None = None
    leader_resumed = not darwin_suspended
    post_launch_failure: _PostLaunchFailure | None = None
    output_limit_stream: str | None = None
    output_capture_complete = True
    drain_deadline: float | None = None

    def poll_leader() -> int | None:
        nonlocal leader_returncode, drain_deadline
        if leader_returncode is None:
            try:
                waited_pid, status_value = os.waitpid(process_pid, os.WNOHANG)
            except ChildProcessError:
                waited_pid = 0
            if waited_pid == process_pid:
                leader_returncode = os.waitstatus_to_exitcode(status_value)
                drain_deadline = min(deadline, time.monotonic() + drain_timeout_seconds)
        return leader_returncode

    def wait_leader(seconds: float) -> int:
        local_deadline = time.monotonic() + seconds
        while time.monotonic() < local_deadline:
            returncode = poll_leader()
            if returncode is not None:
                return returncode
            time.sleep(0.01)
        raise TimeoutError

    def scan_owned() -> dict[int, ProcessIdentity]:
        nonlocal leader_marker_loss_observed_at_exit
        current = _snapshot()
        try:
            _assert_owned_pid_identities(owned, current)
        except BoundedProcessError as error:
            raise _PostLaunchFailure(
                "observation_incomplete",
                f"{description} owned PID identity changed",
            ) from error
        leader = current.get(process_pid)
        if leader is not None:
            owned.setdefault(process_pid, leader)
            try:
                leader_has_marker = _process_has_ownership_marker(
                    process_pid, ownership_marker_witness
                )
            except BoundedProcessError as error:
                raise _PostLaunchFailure(
                    "observation_incomplete",
                    f"{description} ownership-marker observation failed",
                ) from error
            if not leader_has_marker:
                # On Darwin the process may remain momentarily enumerable after
                # its descriptor table has been torn down.  Reap once more at
                # the exact observation boundary, then permit only one small
                # exit-only grace.  A leader still running after that grace
                # fails.  This does not broaden the stated non-hostile-child
                # boundary for deliberate marker-close-then-escape behavior.
                marker_exit_deadline = min(
                    deadline,
                    time.monotonic() + OWNERSHIP_MARKER_EXIT_GRACE_SECONDS,
                )
                while (
                    leader_returncode is None
                    and time.monotonic() < marker_exit_deadline
                ):
                    poll_leader()
                    if leader_returncode is None:
                        time.sleep(0.002)
                if leader_returncode is None:
                    raise _PostLaunchFailure(
                        "observation_incomplete",
                        f"{description} leader lost its ownership marker",
                    )
                leader_marker_loss_observed_at_exit = True
        changed = True
        while changed:
            changed = False
            parents = {process_pid, *owned}
            for identity in current.values():
                if identity.pid in owned:
                    continue
                if identity.ppid in parents or identity.pgid == process_pid:
                    owned[identity.pid] = identity
                    changed = True
        for identity in current.values():
            if (
                identity.pid in owned
                or identity.pid == os.getpid()
                or _same_process(baseline.get(identity.pid), identity)
            ):
                continue
            if _process_has_ownership_marker(identity.pid, ownership_marker_witness):
                owned[identity.pid] = identity
        return current

    def verify_leader_marker_before_release() -> None:
        nonlocal leader_marker_verified
        try:
            present = _process_has_ownership_marker(
                process_pid, ownership_marker_witness
            )
        except BoundedProcessError as error:
            raise _PostLaunchFailure(
                "observation_incomplete",
                f"{description} ownership-marker observation failed before release",
            ) from error
        if not present:
            raise _PostLaunchFailure(
                "launch_identity",
                f"{description} lost its ownership marker before release",
            )
        leader_marker_verified = True

    def survivors(current: Mapping[int, ProcessIdentity]) -> list[ProcessIdentity]:
        return [
            identity
            for pid, identity in owned.items()
            if _same_process(identity, current.get(pid))
        ]

    def signal_known(signal_number: signal.Signals) -> None:
        for identity in sorted(owned.values(), key=lambda item: item.pid, reverse=True):
            try:
                if precise_process_started(identity.pid) == identity.started:
                    os.kill(identity.pid, signal_number)
            except ProcessLookupError:
                pass

    def wait_owned_exit(seconds: float) -> bool:
        local_deadline = time.monotonic() + seconds
        while time.monotonic() < local_deadline:
            poll_leader()
            alive = False
            for identity in owned.values():
                try:
                    if precise_process_started(identity.pid) == identity.started:
                        alive = True
                        break
                except ProcessLookupError:
                    continue
            if not alive:
                return True
            time.sleep(0.05)
        return False

    def terminate_all() -> None:
        nonlocal leader_resumed
        try:
            scan_owned()
        except (BoundedProcessError, _PostLaunchFailure):
            pass
        if process_pid not in owned:
            started = precise_process_started(process_pid)
            if started is not None:
                try:
                    os.kill(process_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if darwin_suspended and not leader_resumed:
            try:
                os.kill(process_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            leader_resumed = True
        else:
            signal_known(signal.SIGTERM)
        if not wait_owned_exit(TERMINATE_GRACE_SECONDS):
            signal_known(signal.SIGKILL)
            if not wait_owned_exit(KILL_GRACE_SECONDS):
                raise BoundedProcessError("owned processes could not be reaped")
        try:
            wait_leader(1)
        except TimeoutError:
            try:
                os.kill(process_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                wait_leader(LEADER_REAP_SECONDS)
            except TimeoutError as error:
                raise BoundedProcessError("leader could not be reaped") from error

    try:
        if darwin_suspended:
            os.close(ready_read)
            os.close(release_write)
            current = _snapshot()
            leader = current.get(process_pid)
            if leader is not None:
                owned[process_pid] = ProcessIdentity(
                    leader.pid,
                    leader.ppid,
                    leader.pgid,
                    leader.sid,
                    leader.started,
                    executable.path,
                )
            if (
                leader is None
                or leader.ppid != os.getpid()
                or leader.pgid != process_pid
                or leader.sid != process_pid
                or not _darwin_mapped_executable_matches(process_pid, executable_stat)
            ):
                post_launch_failure = _PostLaunchFailure(
                    "launch_identity",
                    f"{description} lost its suspended executable identity",
                )
            else:
                verify_bindings()
                verify_leader_marker_before_release()
                os.kill(process_pid, signal.SIGCONT)
                leader_resumed = True
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
                post_launch_failure = _PostLaunchFailure(
                    "launch_identity",
                    f"{description} failed its bounded pre-exec handshake",
                )
            else:
                current = _snapshot()
                leader = current.get(process_pid)
                if leader is not None:
                    owned[process_pid] = ProcessIdentity(
                        leader.pid,
                        leader.ppid,
                        leader.pgid,
                        leader.sid,
                        leader.started,
                        executable.path,
                    )
                if (
                    leader is None
                    or leader.ppid != os.getpid()
                    or leader.pgid != process_pid
                    or leader.sid != process_pid
                ):
                    post_launch_failure = _PostLaunchFailure(
                        "launch_identity",
                        f"{description} lost its pre-exec leader identity",
                    )
                else:
                    verify_bindings()
                    verify_leader_marker_before_release()
                    os.write(release_write, b"G")
            os.close(release_write)

        while selector.get_map():
            if post_launch_failure is not None:
                break
            now = time.monotonic()
            active_deadline = min(
                deadline, drain_deadline if drain_deadline is not None else deadline
            )
            remaining = active_deadline - now
            if remaining <= 0:
                # `drain_deadline` is clamped to `deadline` when it is set
                # (see poll_leader), so `drain_deadline <= deadline` is true by
                # construction and cannot distinguish the two bounds.  The
                # lifetime deadline is therefore tested first, mirroring the
                # managed twin (managed_process.py `_observe_loop`): the drain
                # bound is the binding one only when it expires strictly before
                # the lifetime envelope is spent.  A command that consumed its
                # whole envelope must report "timeout" so downstream receipts
                # publish `timed_out: true`.
                lifetime_expired = now >= deadline
                post_launch_failure = _PostLaunchFailure(
                    "timeout" if lifetime_expired else "drain_timeout",
                    f"{description} "
                    + ("timed out" if lifetime_expired else "I/O drain timed out"),
                )
                break
            events = selector.select(min(remaining, 0.1))
            for key, _mask in events:
                captured_size = sum(len(value) for value in streams.values())
                available = maximum_output_bytes - captured_size
                chunk = os.read(key.fd, min(64 * 1024, available + 1))
                if chunk:
                    accepted = chunk[:available]
                    try:
                        _write_spool(
                            stdout_spool_fd
                            if key.data == "stdout"
                            else stderr_spool_fd,
                            accepted,
                        )
                    except BoundedProcessError as error:
                        output_capture_complete = False
                        post_launch_failure = _PostLaunchFailure(
                            "spool_identity", str(error)
                        )
                        break
                    streams[key.data].extend(accepted)
                    if len(chunk) > len(accepted):
                        output_capture_complete = False
                        output_limit_stream = key.data
                        post_launch_failure = _PostLaunchFailure(
                            "output_limit",
                            f"{description} exceeded its bounded output",
                        )
                        break
                else:
                    selector.unregister(key.fd)
            if post_launch_failure is not None:
                break
            poll_leader()
            scan_owned()

        if post_launch_failure is not None:
            raise post_launch_failure
        try:
            returncode = wait_leader(5)
        except TimeoutError as error:
            raise _PostLaunchFailure(
                "leader_exit_timeout", "leader did not exit after closing output"
            ) from error
        current = scan_owned()
        if survivors(current):
            raise _PostLaunchFailure(
                "descendant_survived", f"{description} left an owned descendant"
            )
        verify_bindings()
        for descriptor, before, label in (
            (stdout_spool_fd, stdout_spool_before, "stdout"),
            (stderr_spool_fd, stderr_spool_before, "stderr"),
        ):
            if descriptor is None or before is None:
                continue
            os.fsync(descriptor)
            after = os.fstat(descriptor)
            if (before.st_dev, before.st_ino, before.st_uid, before.st_nlink) != (
                after.st_dev,
                after.st_ino,
                after.st_uid,
                after.st_nlink,
            ):
                raise _PostLaunchFailure(
                    "spool_identity", f"{label} spool identity changed"
                )
        try:
            close_ownership_marker()
        except OSError as error:
            raise _PostLaunchFailure(
                "supervisor_error", "ownership-marker descriptor could not be closed"
            ) from error
        ledger = validate_process_ledger(
            tuple(
                {
                    "pid": identity.pid,
                    "ppid": identity.ppid,
                    "pgid": identity.pgid,
                    "sid": identity.sid,
                    "started": identity.started,
                    "command": identity.command,
                    "ownership_marker_sha256": ownership_marker_witness["sha256"],
                    "reaped": True,
                }
                for identity in sorted(owned.values(), key=lambda item: item.pid)
            )
        )
        receipt_body = {
            "schema_version": 1,
            "kind": "pmux_bounded_process",
            "executable": _witness_object(executable),
            "argv": list(argv),
            "cwd": str(canonical_cwd),
            "cwd_witness": cwd_receipt,
            "environment": environment_receipt,
            "standard_input": standard_input_receipt,
            "ownership_marker": _ownership_marker_receipt(
                ownership_marker_witness,
                leader_verified_before_release=leader_marker_verified,
                leader_marker_loss_observed_at_exit=leader_marker_loss_observed_at_exit,
                supervisor_descriptor_closed=marker_supervisor_closed,
            ),
            "timeout_seconds": timeout_seconds,
            "drain_timeout_seconds": drain_timeout_seconds,
            "maximum_output_bytes": maximum_output_bytes,
            "exit_code": returncode,
            "stdout_size": len(streams["stdout"]),
            "stdout_sha256": hashlib.sha256(streams["stdout"]).hexdigest(),
            "stderr_size": len(streams["stderr"]),
            "stderr_sha256": hashlib.sha256(streams["stderr"]).hexdigest(),
            "process_ledger": list(ledger),
            "process_ledger_sha256": _canonical_json_sha256(
                list(ledger), domain=PROCESS_LEDGER_DOMAIN
            ),
        }
        receipt = {
            **receipt_body,
            "receipt_sha256": _canonical_json_sha256(
                receipt_body, domain=EXECUTION_RECEIPT_DOMAIN
            ),
        }
        validated_receipt = validate_execution_receipt(receipt)
        return RunResult(
            exit_code=returncode,
            stdout=bytes(streams["stdout"]),
            stderr=bytes(streams["stderr"]),
            process_ledger=ledger,
            receipt=validated_receipt,
        )
    except BaseException as operation_error:
        if isinstance(operation_error, _PostLaunchFailure):
            failure_reason = operation_error.reason
            failure_message = str(operation_error)
        elif isinstance(operation_error, (KeyboardInterrupt, SystemExit)):
            failure_reason = "supervisor_interrupted"
            failure_message = f"{description} supervisor was interrupted"
        else:
            failure_reason = "supervisor_error"
            failure_message = f"{description} supervisor failed"

        cleanup_error: BaseException | None = None
        try:
            terminate_all()
        except BaseException as error:
            cleanup_error = error

        failure_drain_deadline = time.monotonic() + min(
            float(drain_timeout_seconds), 5.0
        )
        while selector.get_map() and time.monotonic() < failure_drain_deadline:
            events = selector.select(
                min(0.05, max(0.0, failure_drain_deadline - time.monotonic()))
            )
            if not events:
                continue
            for key, _mask in events:
                captured_size = sum(len(value) for value in streams.values())
                available = maximum_output_bytes - captured_size
                try:
                    chunk = os.read(key.fd, min(64 * 1024, available + 1))
                except OSError:
                    output_capture_complete = False
                    chunk = b""
                if not chunk:
                    try:
                        selector.unregister(key.fd)
                    except KeyError:
                        pass
                    continue
                accepted = chunk[:available]
                try:
                    _write_spool(
                        stdout_spool_fd if key.data == "stdout" else stderr_spool_fd,
                        accepted,
                    )
                except BoundedProcessError as error:
                    output_capture_complete = False
                    cleanup_error = cleanup_error or error
                    failure_reason = "spool_identity"
                    continue
                streams[key.data].extend(accepted)
                if len(chunk) > len(accepted):
                    output_capture_complete = False
                    output_limit_stream = key.data
                    if failure_reason == "supervisor_error":
                        failure_reason = "output_limit"
                    break
            if output_limit_stream is not None:
                break

        for descriptor, before, label in (
            (stdout_spool_fd, stdout_spool_before, "stdout"),
            (stderr_spool_fd, stderr_spool_before, "stderr"),
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
                    raise BoundedProcessError(f"{label} spool identity changed")
            except (OSError, BoundedProcessError) as error:
                cleanup_error = cleanup_error or error
                failure_reason = "spool_identity"

        final_snapshot: Mapping[int, ProcessIdentity] = {}
        process_observation_complete = False
        try:
            final_snapshot = scan_owned()
            process_observation_complete = process_pid in owned
            if survivors(final_snapshot):
                try:
                    terminate_all()
                except BaseException as error:
                    cleanup_error = cleanup_error or error
                final_snapshot = scan_owned()
                process_observation_complete = process_pid in owned
        except BaseException as error:
            cleanup_error = cleanup_error or error
            process_observation_complete = False
            final_snapshot = {}

        try:
            poll_leader()
        except BaseException as error:
            cleanup_error = cleanup_error or error

        try:
            close_ownership_marker()
        except OSError as error:
            cleanup_error = cleanup_error or error

        failure_ledger = validate_process_ledger(
            tuple(
                {
                    "pid": identity.pid,
                    "ppid": identity.ppid,
                    "pgid": identity.pgid,
                    "sid": identity.sid,
                    "started": identity.started,
                    "command": identity.command,
                    "ownership_marker_sha256": ownership_marker_witness["sha256"],
                    "reaped": process_observation_complete
                    and not _same_process(identity, final_snapshot.get(identity.pid)),
                }
                for identity in sorted(owned.values(), key=lambda item: item.pid)
            ),
            require_nonempty=False,
            require_all_reaped=False,
        )
        leader_observed = any(record["pid"] == process_pid for record in failure_ledger)
        cleanup_complete = (
            process_observation_complete
            and leader_returncode is not None
            and leader_observed
            and marker_supervisor_closed
            and all(record["reaped"] is True for record in failure_ledger)
        )
        output_complete = output_capture_complete and not selector.get_map()
        failure_body = {
            "schema_version": 1,
            "kind": "pmux_bounded_process_failure",
            "executable": _witness_object(executable),
            "argv": list(argv),
            "cwd": str(canonical_cwd),
            "cwd_witness": cwd_receipt,
            "environment": environment_receipt,
            "standard_input": standard_input_receipt,
            "ownership_marker": _ownership_marker_receipt(
                ownership_marker_witness,
                leader_verified_before_release=leader_marker_verified,
                leader_marker_loss_observed_at_exit=leader_marker_loss_observed_at_exit,
                supervisor_descriptor_closed=marker_supervisor_closed,
            ),
            "timeout_seconds": timeout_seconds,
            "drain_timeout_seconds": drain_timeout_seconds,
            "maximum_output_bytes": maximum_output_bytes,
            "leader_pid": process_pid,
            "failure_reason": failure_reason,
            "cleanup_complete": cleanup_complete,
            "output_complete": output_complete,
            "output_limit_stream": output_limit_stream,
            "exit_code": leader_returncode,
            "stdout_size": len(streams["stdout"]),
            "stdout_sha256": hashlib.sha256(streams["stdout"]).hexdigest(),
            "stderr_size": len(streams["stderr"]),
            "stderr_sha256": hashlib.sha256(streams["stderr"]).hexdigest(),
            "process_observation_complete": process_observation_complete,
            "process_ledger": list(failure_ledger),
            "process_ledger_sha256": _canonical_json_sha256(
                list(failure_ledger), domain=PROCESS_LEDGER_DOMAIN
            ),
        }
        failure_receipt = {
            **failure_body,
            "receipt_sha256": _canonical_json_sha256(
                failure_body, domain=FAILURE_RECEIPT_DOMAIN
            ),
        }
        validated_failure_receipt = validate_failure_receipt(failure_receipt)
        failure_result = FailureResult(
            reason=failure_reason,
            exit_code=leader_returncode,
            stdout=bytes(streams["stdout"]),
            stderr=bytes(streams["stderr"]),
            process_ledger=failure_ledger,
            cleanup_complete=cleanup_complete,
            output_complete=output_complete,
            receipt=validated_failure_receipt,
        )
        if cleanup_error is not None and not cleanup_complete:
            failure_message += "; owned-process cleanup is incomplete"
        raise BoundedProcessFailure(
            failure_message, failure_result
        ) from operation_error
    finally:
        selector.close()
        try:
            close_marker_reservation()
            close_ownership_marker()
        except OSError:
            pass
        for descriptor in (stdout_read, stderr_read, ready_read, release_write):
            try:
                os.close(descriptor)
            except OSError:
                pass
