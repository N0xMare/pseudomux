#!/usr/bin/env python3
"""Prove exact cross-UID denial on the real release pmuxd Unix socket."""

from __future__ import annotations

import errno
import os
import pathlib
import pwd
import secrets
import signal
import socket
import stat
import sys
import time
from collections.abc import Mapping, Sequence
from typing import Any

import evidence
from evidence import (
    EvidenceError,
    atomic_write_json,
    canonical_json_sha256,
    load_json,
    strict_json_loads,
    verify_release_binary_manifest,
)

bounded_process = evidence.bounded_process
managed_process = evidence.managed_process


RUNUSER = pathlib.Path("/usr/sbin/runuser")
ENV = pathlib.Path("/usr/bin/env")
PYTHON = pathlib.Path("/usr/bin/python3")
COMMAND_ENVIRONMENT = {
    "LANG": "C",
    "LC_ALL": "C",
    "PATH": "/usr/sbin:/usr/bin:/bin",
    "PYTHONDONTWRITEBYTECODE": "1",
    "TZ": "UTC",
}
_DIRECTORY = getattr(os, "O_DIRECTORY", 0)
_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)
_CLOEXEC = getattr(os, "O_CLOEXEC", 0)


def process_identity(pid: int) -> dict[str, int] | None:
    try:
        text = pathlib.Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
    except (FileNotFoundError, ProcessLookupError):
        return None
    fields = text.rsplit(")", 1)[1].split()
    if len(fields) < 20:
        raise EvidenceError("Linux process identity is truncated")
    return {
        "pid": pid,
        "process_group": int(fields[2]),
        "session": int(fields[3]),
        "start_ticks": int(fields[19]),
    }


def same_birth(expected: Mapping[str, int]) -> bool:
    current = process_identity(expected["pid"])
    return current is not None and current["start_ticks"] == expected["start_ticks"]


def session_members(session_id: int) -> list[dict[str, int]]:
    members: list[dict[str, int]] = []
    for entry in pathlib.Path("/proc").iterdir():
        if entry.name.isdigit():
            identity = process_identity(int(entry.name))
            if identity is not None and identity["session"] == session_id:
                members.append(identity)
    return sorted(members, key=lambda item: item["pid"])


def wait_for_session_empty(session_id: int, timeout: float) -> list[dict[str, int]]:
    deadline = time.monotonic() + timeout
    members: list[dict[str, int]] = []
    while time.monotonic() < deadline:
        members = session_members(session_id)
        if not members:
            return []
        time.sleep(0.025)
    return members


def _user_command(
    user_name: str, executable: pathlib.Path, arguments: Sequence[str]
) -> list[str]:
    account = pwd.getpwnam(user_name)
    return [
        str(RUNUSER),
        "-u",
        user_name,
        "--",
        str(ENV),
        "-i",
        f"HOME={account.pw_dir}",
        f"LOGNAME={user_name}",
        "PATH=/usr/local/bin:/usr/bin:/bin",
        f"USER={user_name}",
        str(executable),
        *arguments,
    ]


def run_as_user(
    user_name: str,
    executable: pathlib.Path,
    arguments: Sequence[str],
    *,
    description: str,
    timeout_seconds: int = 15,
    maximum_output_bytes: int = 1024 * 1024,
) -> bounded_process.RunResult:
    witness = bounded_process.bind_executable(RUNUSER)
    return bounded_process.run(
        witness,
        _user_command(user_name, executable, arguments),
        cwd=pathlib.Path("/workspace"),
        environment=COMMAND_ENVIRONMENT,
        timeout_seconds=timeout_seconds,
        drain_timeout_seconds=5,
        maximum_output_bytes=maximum_output_bytes,
        description=description,
    )


def wait_for_socket(path: pathlib.Path, daemon: Any) -> os.stat_result:
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        health = daemon.health()
        if not health.running:
            raise EvidenceError("pmuxd exited before binding its socket")
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            time.sleep(0.025)
            continue
        if not stat.S_ISSOCK(metadata.st_mode):
            raise EvidenceError("pmuxd public endpoint is not one Unix socket")
        return metadata
    raise EvidenceError("pmuxd did not bind its socket within 20 seconds")


def _socket_identity(metadata: os.stat_result) -> dict[str, Any]:
    return {
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "uid": metadata.st_uid,
        "gid": metadata.st_gid,
        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
    }


def _same_socket(first: os.stat_result, second: os.stat_result) -> bool:
    return stat.S_ISSOCK(second.st_mode) and all(
        getattr(first, field) == getattr(second, field)
        for field in ("st_dev", "st_ino", "st_uid", "st_gid", "st_mode")
    )


def _open_private_probe_tree(
    pmux_uid: int, pmux_gid: int
) -> tuple[int, int, int, str, pathlib.Path]:
    parent_fd = os.open("/var/tmp", os.O_RDONLY | _DIRECTORY | _CLOEXEC)
    name = f"pmux-uds-{secrets.token_hex(16)}"
    root_fd = -1
    runtime_fd = -1
    root_created = False
    runtime_created = False
    try:
        os.mkdir(name, 0o700, dir_fd=parent_fd)
        root_created = True
        root_fd = os.open(
            name,
            os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC,
            dir_fd=parent_fd,
        )
        os.mkdir("runtimes", 0o700, dir_fd=root_fd)
        runtime_created = True
        runtime_fd = os.open(
            "runtimes",
            os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC,
            dir_fd=root_fd,
        )
        os.fchown(runtime_fd, pmux_uid, pmux_gid)
        os.fchown(root_fd, pmux_uid, pmux_gid)
    except BaseException:
        if runtime_fd >= 0:
            try:
                os.fchown(runtime_fd, os.geteuid(), os.getegid())
                os.fchmod(runtime_fd, 0o700)
            except OSError:
                pass
            os.close(runtime_fd)
        if root_fd >= 0:
            try:
                os.fchown(root_fd, os.geteuid(), os.getegid())
                os.fchmod(root_fd, 0o700)
                if runtime_created:
                    os.rmdir("runtimes", dir_fd=root_fd)
            except OSError:
                pass
            os.close(root_fd)
        try:
            if root_created:
                os.rmdir(name, dir_fd=parent_fd)
        except OSError:
            pass
        os.close(parent_fd)
        raise
    return parent_fd, root_fd, runtime_fd, name, pathlib.Path("/var/tmp") / name


def _cleanup_private_probe_tree(
    parent_fd: int,
    root_fd: int,
    runtime_fd: int,
    name: str,
    *,
    expected_root: tuple[int, int],
    expected_runtime: tuple[int, int],
) -> None:
    linked_root = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    current_root = os.fstat(root_fd)
    current_runtime = os.fstat(runtime_fd)
    if (linked_root.st_dev, linked_root.st_ino) != expected_root or (
        current_root.st_dev,
        current_root.st_ino,
    ) != expected_root:
        raise EvidenceError("private probe root identity changed before cleanup")
    if (current_runtime.st_dev, current_runtime.st_ino) != expected_runtime:
        raise EvidenceError("private probe runtime identity changed before cleanup")
    if os.listdir(runtime_fd):
        raise EvidenceError("private probe runtime is not empty")
    os.fchown(root_fd, os.geteuid(), os.getegid())
    os.fchmod(root_fd, 0o700)
    os.fchown(runtime_fd, os.geteuid(), os.getegid())
    os.fchmod(runtime_fd, 0o700)
    os.rmdir("runtimes", dir_fd=root_fd)
    if os.listdir(root_fd):
        raise EvidenceError("private probe root is not empty")
    os.rmdir(name, dir_fd=parent_fd)
    os.fsync(parent_fd)


def _connect_denied(socket_path: pathlib.Path) -> int:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        try:
            client.connect(str(socket_path))
        except PermissionError as error:
            if error.errno != errno.EACCES:
                print(
                    '{"denied":false,"errno_name":"OTHER_PERMISSION"}',
                    flush=True,
                )
                return 4
            print(
                '{"denied":true,"errno_name":"EACCES","errno_number":13}',
                flush=True,
            )
            return 0
        except OSError as error:
            print(
                '{"denied":false,"errno_name":"%s","errno_number":%d}'
                % (errno.errorcode.get(error.errno or 0, "UNKNOWN"), error.errno or 0),
                flush=True,
            )
            return 5
        print('{"denied":false,"errno_name":"CONNECTED"}', flush=True)
        return 3
    finally:
        client.close()


def _write_denied(path: pathlib.Path) -> int:
    try:
        before = path.lstat()
    except OSError as error:
        print(
            '{"denied":false,"errno_name":"%s","errno_number":%d}'
            % (errno.errorcode.get(error.errno or 0, "UNKNOWN"), error.errno or 0),
            flush=True,
        )
        return 6
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        print('{"denied":false,"errno_name":"UNSAFE_TARGET"}', flush=True)
        return 7
    try:
        descriptor = os.open(
            path,
            os.O_WRONLY | os.O_APPEND | _NOFOLLOW | _CLOEXEC,
        )
    except PermissionError as error:
        if error.errno != errno.EACCES:
            print('{"denied":false,"errno_name":"OTHER_PERMISSION"}', flush=True)
            return 8
        after = path.lstat()
        fields = (
            "st_dev",
            "st_ino",
            "st_uid",
            "st_gid",
            "st_mode",
            "st_size",
            "st_mtime_ns",
            "st_ctime_ns",
            "st_nlink",
        )
        if any(getattr(before, field) != getattr(after, field) for field in fields):
            print('{"denied":false,"errno_name":"TARGET_CHANGED"}', flush=True)
            return 9
        print(
            '{"denied":true,"errno_name":"EACCES","errno_number":13}',
            flush=True,
        )
        return 0
    except OSError as error:
        print(
            '{"denied":false,"errno_name":"%s","errno_number":%d}'
            % (errno.errorcode.get(error.errno or 0, "UNKNOWN"), error.errno or 0),
            flush=True,
        )
        return 10
    else:
        os.close(descriptor)
        print('{"denied":false,"errno_name":"WRITABLE"}', flush=True)
        return 11


def _probe(
    destination: pathlib.Path, binary_manifest_path: pathlib.Path
) -> tuple[int, dict[str, Any]]:
    if os.geteuid() != 0:
        raise EvidenceError("cross-UID permissions probe must be supervised by root")
    manifest = verify_release_binary_manifest(load_json(binary_manifest_path))
    binaries = manifest["binaries"]
    pmuxd = pathlib.Path(binaries["pmuxd"]["path"])
    pmux = pathlib.Path(binaries["pmux"]["path"])
    pmux_user = pwd.getpwnam("pmux")

    parent_fd, root_fd, runtime_fd, root_name, root = _open_private_probe_tree(
        pmux_user.pw_uid, pmux_user.pw_gid
    )
    root_metadata = os.fstat(root_fd)
    runtime_metadata = os.fstat(runtime_fd)
    expected_root = (root_metadata.st_dev, root_metadata.st_ino)
    expected_runtime = (runtime_metadata.st_dev, runtime_metadata.st_ino)
    socket_path = root / "pmux.sock"
    runtimes = root / "runtimes"
    daemon: Any | None = None
    daemon_terminal_receipt: Mapping[str, Any] | None = None
    daemon_identity: dict[str, int] | None = None
    socket_metadata: os.stat_result | None = None
    owner_receipt: Mapping[str, Any] | None = None
    intruder_receipt: Mapping[str, Any] | None = None
    candidate_write_receipt: Mapping[str, Any] | None = None
    owner_exit_code: int | None = None
    intruder_exit_code: int | None = None
    candidate_write_exit_code: int | None = None
    candidate_write_denial: Mapping[str, Any] | None = None
    candidate_manifest_revalidated = False
    protocol_version: int | None = None
    server_version: str | None = None
    residual_members: list[dict[str, int]] = []
    runtime_entries: list[str] = []
    socket_removed = False
    socket_revalidated = False
    runtime_parent_revalidated = False
    private_tree_removed = False
    denial_exact = False
    daemon_exit_code: int | None = None
    failure: BaseException | None = None
    try:
        daemon = managed_process.start_managed(
            bounded_process.bind_executable(RUNUSER),
            _user_command(
                "pmux",
                pmuxd,
                [
                    "serve",
                    "--socket",
                    str(socket_path),
                    "--runtime-parent",
                    str(runtimes),
                ],
            ),
            cwd=pathlib.Path("/workspace"),
            environment=COMMAND_ENVIRONMENT,
            timeout_seconds=90,
            graceful_stop_timeout_seconds=20,
            drain_timeout_seconds=10,
            maximum_output_bytes=16 * 1024 * 1024,
            description="root-supervised pmuxd for cross-UID permission proof",
        )
        deadline = time.monotonic() + 2
        while daemon_identity is None and time.monotonic() < deadline:
            daemon_identity = process_identity(daemon.identity.leader_pid)
            if daemon_identity is None:
                time.sleep(0.01)
        if (
            daemon_identity is None
            or daemon_identity["session"] != daemon.identity.leader_pid
            or daemon_identity["process_group"] != daemon.identity.leader_pid
        ):
            raise EvidenceError("pmuxd supervisor did not establish one POSIX session")
        socket_metadata = wait_for_socket(socket_path, daemon)
        if socket_metadata.st_uid != pmux_user.pw_uid:
            raise EvidenceError("pmuxd socket owner is not exact")
        if stat.S_IMODE(socket_metadata.st_mode) != 0o600:
            raise EvidenceError("pmuxd socket mode is not 0600")

        owner = run_as_user(
            "pmux",
            pmux,
            ["--socket", str(socket_path), "--output", "json", "ping"],
            description="owner pmux ping",
        )
        owner_receipt = owner.receipt
        owner_exit_code = owner.exit_code
        if owner.exit_code != 0:
            raise EvidenceError("the socket owner could not ping pmuxd")
        pong = strict_json_loads(owner.stdout, description="owner ping JSON")
        if not isinstance(pong, Mapping) or set(pong) != {
            "protocol_version",
            "server_version",
        }:
            raise EvidenceError("owner ping schema is not exact")
        if (
            type(pong.get("protocol_version")) is not int
            or pong["protocol_version"] != 1
            or not isinstance(pong.get("server_version"), str)
            or not pong["server_version"]
            or len(pong["server_version"].encode("utf-8")) > 1024
            or any(character in pong["server_version"] for character in "\0\r\n")
        ):
            raise EvidenceError("owner ping returned an unexpected protocol")
        protocol_version = pong["protocol_version"]
        server_version = pong["server_version"]

        intruder = run_as_user(
            "intruder",
            PYTHON,
            [
                str(pathlib.Path(__file__).resolve()),
                "--connect-denied",
                str(socket_path),
            ],
            description="intruder direct Unix-socket denial",
        )
        intruder_receipt = intruder.receipt
        intruder_exit_code = intruder.exit_code
        denial = strict_json_loads(intruder.stdout, description="intruder denial JSON")
        if (
            intruder.exit_code != 0
            or not isinstance(denial, Mapping)
            or denial
            != {"denied": True, "errno_name": "EACCES", "errno_number": errno.EACCES}
        ):
            raise EvidenceError("different-UID denial was not exact EACCES")
        denial_exact = True

        candidate_write = run_as_user(
            "pmux",
            PYTHON,
            [
                str(pathlib.Path(__file__).resolve()),
                "--write-denied",
                str(pmuxd),
            ],
            description="unprivileged release-candidate mutation denial",
        )
        candidate_write_receipt = candidate_write.receipt
        candidate_write_exit_code = candidate_write.exit_code
        candidate_write_denial = strict_json_loads(
            candidate_write.stdout, description="candidate write-denial JSON"
        )
        if (
            candidate_write.exit_code != 0
            or not isinstance(candidate_write_denial, Mapping)
            or candidate_write_denial
            != {"denied": True, "errno_name": "EACCES", "errno_number": errno.EACCES}
        ):
            raise EvidenceError(
                "the unprivileged test user could mutate the release candidate"
            )
        if verify_release_binary_manifest(load_json(binary_manifest_path)) != manifest:
            raise EvidenceError("release candidate changed during mutation denial")
        candidate_manifest_revalidated = True

        current_socket = socket_path.lstat()
        if not _same_socket(socket_metadata, current_socket):
            raise EvidenceError("pmuxd socket identity changed during the probe")
        socket_revalidated = True
        current_root = os.fstat(root_fd)
        current_runtime = os.fstat(runtime_fd)
        if (
            (current_root.st_dev, current_root.st_ino) != expected_root
            or current_root.st_uid != pmux_user.pw_uid
            or current_root.st_gid != pmux_user.pw_gid
            or stat.S_IMODE(current_root.st_mode) != 0o700
        ):
            raise EvidenceError("pmuxd socket parent identity changed during the probe")
        if (
            (current_runtime.st_dev, current_runtime.st_ino) != expected_runtime
            or current_runtime.st_uid != pmux_user.pw_uid
            or current_runtime.st_gid != pmux_user.pw_gid
            or stat.S_IMODE(current_runtime.st_mode) != 0o700
        ):
            raise EvidenceError(
                "pmuxd runtime parent identity changed during the probe"
            )
        runtime_parent_revalidated = True
    except BaseException as error:
        failure = error
    finally:
        if daemon is not None:
            try:
                daemon_terminal = daemon.finalize(signal_number=signal.SIGTERM)
            except bounded_process.BoundedProcessFailure as managed_error:
                daemon_terminal = managed_error.result
                if failure is None:
                    failure = managed_error
            daemon_exit_code = daemon_terminal.exit_code
            daemon_terminal_receipt = daemon_terminal.receipt
        if daemon_identity is not None:
            residual_members = wait_for_session_empty(daemon_identity["session"], 10)
        try:
            runtime_entries = sorted(os.listdir(runtime_fd))
            try:
                current_socket = os.stat(
                    "pmux.sock", dir_fd=root_fd, follow_symlinks=False
                )
            except FileNotFoundError:
                socket_removed = True
            else:
                if socket_metadata is not None and _same_socket(
                    socket_metadata, current_socket
                ):
                    os.fchown(root_fd, os.geteuid(), os.getegid())
                    os.unlink("pmux.sock", dir_fd=root_fd)
                if failure is None:
                    failure = EvidenceError("pmuxd socket remained after shutdown")
            if residual_members or runtime_entries:
                if failure is None:
                    failure = EvidenceError(
                        "cross-UID probe left owned runtime or process residue"
                    )
            _cleanup_private_probe_tree(
                parent_fd,
                root_fd,
                runtime_fd,
                root_name,
                expected_root=expected_root,
                expected_runtime=expected_runtime,
            )
            private_tree_removed = True
        except BaseException as cleanup_error:
            if failure is None:
                failure = cleanup_error
        finally:
            for descriptor in (runtime_fd, root_fd, parent_fd):
                try:
                    os.close(descriptor)
                except OSError as error:
                    if error.errno != errno.EBADF and failure is None:
                        failure = error

    manifest_sha256 = canonical_json_sha256(
        manifest, domain="pmux.evidence.release-binary-manifest.v1"
    )
    body = {
        "schema_version": 3,
        "status": "pass" if failure is None else "fail",
        "release_binary_manifest_sha256": manifest_sha256,
        "pmuxd_sha256": binaries["pmuxd"]["sha256"],
        "pmux_sha256": binaries["pmux"]["sha256"],
        "pmuxd_process": daemon_identity,
        "pmuxd_managed_receipt": daemon_terminal_receipt,
        "managed_process_implementation": evidence.MANAGED_PROCESS_IMPLEMENTATION,
        "daemon_exit_code": daemon_exit_code,
        "socket_identity": (
            _socket_identity(socket_metadata) if socket_metadata is not None else None
        ),
        "socket_parent_device": root_metadata.st_dev,
        "socket_parent_inode": root_metadata.st_ino,
        "socket_parent_uid": pmux_user.pw_uid,
        "socket_parent_gid": pmux_user.pw_gid,
        "socket_parent_mode": "0700",
        "socket_revalidated": socket_revalidated,
        "runtime_parent_device": runtime_metadata.st_dev,
        "runtime_parent_inode": runtime_metadata.st_ino,
        "runtime_parent_uid": pmux_user.pw_uid,
        "runtime_parent_gid": pmux_user.pw_gid,
        "runtime_parent_mode": "0700",
        "runtime_parent_revalidated": runtime_parent_revalidated,
        "owner_exit_code": owner_exit_code,
        "owner_process_receipt": owner_receipt,
        "intruder_exit_code": intruder_exit_code,
        "intruder_process_receipt": intruder_receipt,
        "intruder_denial": {
            "denied": denial_exact,
            "errno_name": "EACCES" if denial_exact else None,
            "errno_number": errno.EACCES if denial_exact else None,
        },
        "candidate_write_exit_code": candidate_write_exit_code,
        "candidate_write_process_receipt": candidate_write_receipt,
        "candidate_write_denial": candidate_write_denial,
        "candidate_manifest_revalidated": candidate_manifest_revalidated,
        "protocol_version": protocol_version,
        "server_version": server_version,
        "different_uid_denied": denial_exact,
        "process_session_empty": not residual_members,
        "residual_processes": residual_members,
        "socket_removed": socket_removed,
        "runtime_entries_after_shutdown": runtime_entries,
        "private_probe_tree_removed": private_tree_removed,
        "failure_type": type(failure).__name__ if failure is not None else None,
        "failure_message": str(failure)[:1024] if failure is not None else None,
    }
    payload = {
        **body,
        "report_sha256": canonical_json_sha256(
            body, domain="pmux.evidence.uds-permissions-report.v3"
        ),
    }
    atomic_write_json(destination, payload)
    print(payload["report_sha256"], flush=True)
    return (0 if failure is None else 1), payload


def main() -> int:
    if len(sys.argv) == 3 and sys.argv[1] == "--connect-denied":
        return _connect_denied(pathlib.Path(sys.argv[2]))
    if len(sys.argv) == 3 and sys.argv[1] == "--write-denied":
        return _write_denied(pathlib.Path(sys.argv[2]))
    if len(sys.argv) != 3:
        print("usage: permissions_probe.py OUTPUT BINARY_MANIFEST", file=sys.stderr)
        return 2
    status, _ = _probe(pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]))
    return status


if __name__ == "__main__":
    raise SystemExit(main())
