#!/usr/bin/env python3
"""Evidence-only primitives for the deterministic Linux portability runner.

Nothing in this module parses Claude transcripts or predicts pmux state.  It
only protects artifact publication and binds source, binaries, platforms, and
runner-owned Docker objects to exact identities.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import json
import os
import pathlib
import platform as host_platform
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import types
from collections.abc import Mapping, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any, Iterator

import source_digest as source_identity
from source_digest import (
    SOURCE_ALGORITHM,
    SourceIdentityError,
    is_included,
    validate_expected_digest,
    workspace_source_manifest,
)

bounded_process = source_identity.bounded_process


def _load_exact_managed_process() -> tuple[types.ModuleType, dict[str, Any]]:
    path = (
        pathlib.Path(__file__).resolve().parents[1]
        / "evidence_common"
        / "managed_process.py"
    )
    payload = source_identity._read_stable_file(path, path.lstat())[0]
    module_name = f"_pmux_managed_process_authority_{os.urandom(16).hex()}"
    module = types.ModuleType(module_name)
    module.__file__ = str(path)
    module.__package__ = ""
    previous = sys.modules.get("bounded_process")
    sys.modules["bounded_process"] = bounded_process
    sys.modules[module_name] = module
    try:
        exec(compile(payload, str(path), "exec", dont_inherit=True), module.__dict__)
    except Exception as error:
        raise SourceIdentityError(
            f"shared managed-process authority could not load: {error}"
        ) from error
    finally:
        if previous is None:
            sys.modules.pop("bounded_process", None)
        else:
            sys.modules["bounded_process"] = previous
        sys.modules.pop(module_name, None)
    for name in (
        "start_managed",
        "validate_managed_execution_receipt",
        "dump_managed_execution_receipt",
    ):
        if not hasattr(module, name):
            raise SourceIdentityError(
                "shared managed-process authority lacks its required interface"
            )
    return module, {
        "path": "tools/evidence_common/managed_process.py",
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }


managed_process, MANAGED_PROCESS_IMPLEMENTATION = _load_exact_managed_process()


def revalidate_managed_process_authority() -> dict[str, Any]:
    path = (
        pathlib.Path(__file__).resolve().parents[1]
        / "evidence_common"
        / "managed_process.py"
    )
    payload = source_identity._read_stable_file(path, path.lstat())[0]
    identity = {
        "path": "tools/evidence_common/managed_process.py",
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }
    if identity != MANAGED_PROCESS_IMPLEMENTATION:
        raise SourceIdentityError("shared managed-process authority changed")
    return identity


REQUIRED_RELEASE_BINARIES = (
    "pmux",
    "pmuxd",
    "pmux-rmuxd",
    "pmux-launcher",
    "pmux-hook",
    "pmux-mcp",
    "claude-p",
    "pmux-test-claude",
)
PLATFORMS = ("linux/arm64", "linux/amd64")
DEBIAN_SNAPSHOT = "20250725T000000Z"
DEBIAN_SNAPSHOT_INRELEASE_SHA256 = {
    "debian_bookworm": "919b6d130d8afa68a8680a24db6a09a9ccdc9226188b42079cd3a3d6fad028de",
    "debian_bookworm_updates": "ee3934d9fb7836e3bf303fad2c0b02d366020367fa4e0f4092dae51f82dd0425",
    "debian_security_bookworm": "2cbfcb4744de07ab4aebbe19466d6de02065ce47846d2a8274a26f6e06b3e4ea",
}
PYTHON_REQUIREMENTS_SHA256 = (
    "e2438b6ee5a56701f219479b3bbd6b5c523ff779fa3de1c8d6fbadc4936d780a"
)
PRIVATE_DIRECTORY_MODE = 0o700
PRIVATE_FILE_MODE = 0o600
MAX_LEDGER_RECORD_BYTES = 64 * 1024
MAX_JSON_EVIDENCE_BYTES = 64 * 1024 * 1024
MAX_RELEASE_BINARY_BYTES = 512 * 1024 * 1024
MAX_SAFE_INTEGER = 9_007_199_254_740_991
MAX_U64 = (1 << 64) - 1
_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)
_DIRECTORY = getattr(os, "O_DIRECTORY", 0)
_CLOEXEC = getattr(os, "O_CLOEXEC", 0)
_DIRECTORY_STABLE_FIELDS = (
    "st_dev",
    "st_ino",
    "st_uid",
    "st_gid",
    "st_mode",
)
_DIRFD_PRIMITIVES = (
    ("open", os.open),
    ("stat", os.stat),
    ("mkdir", os.mkdir),
    ("unlink", os.unlink),
    ("link", os.link),
)


class EvidenceError(RuntimeError):
    """Evidence could not be acquired or verified without ambiguity."""


def _reject_json_constant(value: str) -> None:
    raise EvidenceError(f"JSON contains a non-finite number: {value}")


def _strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise EvidenceError(f"JSON contains a duplicate object key: {key}")
        result[key] = value
    return result


def strict_json_loads(payload: str | bytes, *, description: str = "JSON") -> Any:
    """Parse finite JSON while rejecting duplicate keys at every depth."""

    try:
        text = payload.decode("utf-8") if isinstance(payload, bytes) else payload
        return json.loads(
            text,
            object_pairs_hook=_strict_object,
            parse_constant=_reject_json_constant,
        )
    except UnicodeDecodeError as error:
        raise EvidenceError(f"{description} is not UTF-8") from error
    except json.JSONDecodeError as error:
        raise EvidenceError(f"{description} is malformed") from error


def canonical_json_sha256(payload: Any, *, domain: str) -> str:
    """Hash canonical finite JSON in one explicit schema domain."""

    if not isinstance(domain, str) or not re.fullmatch(r"[a-z0-9._-]{1,128}", domain):
        raise EvidenceError("canonical JSON hash domain is invalid")
    try:
        rendered = json.dumps(
            payload, separators=(",", ":"), sort_keys=True, allow_nan=False
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise EvidenceError("canonical JSON payload is not finite JSON") from error
    digest = hashlib.sha256()
    digest.update(b"pmux-canonical-json-sha256-v1\0")
    digest.update(domain.encode("ascii"))
    digest.update(b"\0")
    digest.update(rendered)
    return digest.hexdigest()


def _exact_object(
    value: Any, fields: frozenset[str] | set[str], *, description: str
) -> Mapping[str, Any]:
    if not isinstance(value, Mapping) or set(value) != set(fields):
        raise EvidenceError(f"{description} schema is not exact")
    return value


def _exact_int(
    value: Any,
    *,
    description: str,
    minimum: int = 0,
    maximum: int = MAX_SAFE_INTEGER,
) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise EvidenceError(f"{description} is not an exact bounded integer")
    return value


def _exact_bool(value: Any, *, description: str) -> bool:
    if type(value) is not bool:
        raise EvidenceError(f"{description} is not an exact Boolean")
    return value


def _exact_digest(value: Any, *, description: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise EvidenceError(f"{description} is not one lowercase SHA-256 digest")
    return value


def _exact_mode(value: Any, *, description: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"0[0-7]{3}", value) is None:
        raise EvidenceError(f"{description} is not one exact four-digit mode")
    return value


def _exact_text(
    value: Any,
    *,
    description: str,
    maximum_bytes: int = 1024 * 1024,
    allow_empty: bool = False,
    allow_newline: bool = False,
) -> str:
    if not isinstance(value, str) or (not allow_empty and not value):
        raise EvidenceError(f"{description} is not an exact string")
    try:
        encoded = value.encode("utf-8")
    except UnicodeEncodeError as error:
        raise EvidenceError(f"{description} is not valid UTF-8") from error
    if len(encoded) > maximum_bytes or "\x00" in value:
        raise EvidenceError(f"{description} exceeds its string contract")
    if not allow_newline and any(character in value for character in "\r\n"):
        raise EvidenceError(f"{description} contains a line break")
    return value


def _portable_relative_path(value: Any, *, description: str) -> str:
    text = _exact_text(value, description=description, maximum_bytes=4096)
    path = pathlib.PurePosixPath(text)
    if (
        path.is_absolute()
        or text != path.as_posix()
        or any(part in {"", ".", ".."} for part in path.parts)
        or "\\" in text
    ):
        raise EvidenceError(f"{description} is not one canonical relative path")
    return text


def _canonical_absolute_path(value: Any, *, description: str) -> pathlib.Path:
    text = _exact_text(value, description=description, maximum_bytes=16 * 1024)
    path = pathlib.Path(text)
    if (
        not path.is_absolute()
        or str(path) != text
        or any(part in {"", ".", ".."} for part in path.parts[1:])
    ):
        raise EvidenceError(f"{description} is not one canonical absolute path")
    return path


@dataclass
class _AnchoredDirectory:
    """An absolute directory path held open component-by-component."""

    path: pathlib.Path
    descriptors: list[int]
    component_names: list[str]
    identities: list[os.stat_result]

    @property
    def fd(self) -> int:
        return self.descriptors[-1]

    def close(self) -> None:
        for descriptor in reversed(self.descriptors):
            os.close(descriptor)
        self.descriptors.clear()


def _absolute_components(path: pathlib.Path, description: str) -> list[str]:
    if not path.is_absolute():
        raise EvidenceError(f"{description} must be absolute")
    components = list(path.parts[1:])
    if any(component in {"", ".", ".."} for component in components):
        raise EvidenceError(f"{description} is not a canonical absolute path")
    return components


def _require_dirfd_primitives() -> None:
    unsupported = [
        name
        for name, function in _DIRFD_PRIMITIVES
        if function not in os.supports_dir_fd
    ]
    if _NOFOLLOW == 0 or _DIRECTORY == 0 or unsupported:
        raise EvidenceError(
            "this host lacks required no-follow directory-descriptor primitives: "
            + ",".join(unsupported)
        )


def _validate_open_directory(
    metadata: os.stat_result,
    *,
    description: str,
    owner_uid: int | None = None,
    private: bool = False,
) -> None:
    if not stat.S_ISDIR(metadata.st_mode):
        raise EvidenceError(f"{description} is not a real directory")
    if owner_uid is not None and metadata.st_uid != owner_uid:
        raise EvidenceError(f"{description} is not owned by the expected user")
    if stat.S_IMODE(metadata.st_mode) & 0o7000:
        raise EvidenceError(f"{description} has unsupported special mode bits")
    if private and stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE:
        raise EvidenceError(f"{description} is not mode 0700")


def _open_anchored_directory(
    path: pathlib.Path,
    *,
    description: str,
    final_owner_uid: int | None = None,
    final_private: bool = False,
) -> _AnchoredDirectory:
    """Open every absolute path component without following aliases."""

    _require_dirfd_primitives()
    components = _absolute_components(path, description)
    flags = os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC
    descriptors: list[int] = []
    names: list[str] = []
    identities: list[os.stat_result] = []
    try:
        root_fd = os.open("/", flags)
        descriptors.append(root_fd)
        identities.append(os.fstat(root_fd))
        for component in components:
            descriptor = os.open(component, flags, dir_fd=descriptors[-1])
            metadata = os.fstat(descriptor)
            _validate_open_directory(metadata, description=description)
            linked = os.stat(component, dir_fd=descriptors[-1], follow_symlinks=False)
            if (linked.st_dev, linked.st_ino) != (metadata.st_dev, metadata.st_ino):
                os.close(descriptor)
                raise EvidenceError(f"{description} changed while opening")
            descriptors.append(descriptor)
            names.append(component)
            identities.append(metadata)
        final = identities[-1]
        _validate_open_directory(
            final,
            description=description,
            owner_uid=final_owner_uid,
            private=final_private,
        )
        opened = _AnchoredDirectory(path, descriptors, names, identities)
        _revalidate_directory_chain(opened, description=description)
        return opened
    except BaseException:
        for descriptor in reversed(descriptors):
            os.close(descriptor)
        raise


def _revalidate_directory_chain(
    opened: _AnchoredDirectory, *, description: str
) -> None:
    if len(opened.descriptors) != len(opened.identities):
        raise EvidenceError(f"{description} descriptor chain is incomplete")
    for index, descriptor in enumerate(opened.descriptors):
        current = os.fstat(descriptor)
        original = opened.identities[index]
        if any(
            getattr(current, field) != getattr(original, field)
            for field in _DIRECTORY_STABLE_FIELDS
        ):
            raise EvidenceError(f"{description} identity changed while in use")
        if index == 0:
            continue
        linked = os.stat(
            opened.component_names[index - 1],
            dir_fd=opened.descriptors[index - 1],
            follow_symlinks=False,
        )
        if not stat.S_ISDIR(linked.st_mode) or (
            linked.st_dev,
            linked.st_ino,
        ) != (current.st_dev, current.st_ino):
            raise EvidenceError(f"{description} path was replaced while in use")


@contextmanager
def _anchored_directory(
    path: pathlib.Path,
    *,
    description: str,
    final_owner_uid: int | None = None,
    final_private: bool = False,
) -> Iterator[_AnchoredDirectory]:
    opened = _open_anchored_directory(
        path,
        description=description,
        final_owner_uid=final_owner_uid,
        final_private=final_private,
    )
    try:
        yield opened
    finally:
        opened.close()


def _lstat(path: pathlib.Path, *, description: str) -> os.stat_result:
    try:
        return path.lstat()
    except FileNotFoundError as error:
        raise EvidenceError(f"{description} is missing: {path}") from error


def _require_real_directory(
    path: pathlib.Path,
    *,
    description: str,
    private: bool = False,
    owner_uid: int | None = None,
) -> os.stat_result:
    metadata = _lstat(path, description=description)
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise EvidenceError(f"{description} must be a real directory: {path}")
    expected_owner = os.geteuid() if owner_uid is None else owner_uid
    if metadata.st_uid != expected_owner:
        raise EvidenceError(f"{description} is not owned by the invoking user: {path}")
    if private and stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE:
        raise EvidenceError(f"{description} is not mode 0700: {path}")
    return metadata


def prepare_empty_private_directory(path: pathlib.Path) -> pathlib.Path:
    """Create or validate one absolute, empty, owner-only evidence directory."""

    _require_dirfd_primitives()
    components = _absolute_components(path, "evidence directory")
    if not components:
        raise EvidenceError("evidence directory must not be the filesystem root")
    flags = os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC
    descriptors: list[int] = []
    names: list[str] = []
    identities: list[os.stat_result] = []
    created_indexes: set[int] = set()
    mutated_parent_indexes: set[int] = set()
    try:
        root_fd = os.open("/", flags)
        descriptors.append(root_fd)
        identities.append(os.fstat(root_fd))
        for component in components:
            parent_fd = descriptors[-1]
            try:
                descriptor = os.open(component, flags, dir_fd=parent_fd)
            except FileNotFoundError:
                try:
                    os.mkdir(component, PRIVATE_DIRECTORY_MODE, dir_fd=parent_fd)
                except FileExistsError as error:
                    raise EvidenceError(
                        "evidence path changed during anchored creation"
                    ) from error
                descriptor = os.open(component, flags, dir_fd=parent_fd)
                os.fchmod(descriptor, PRIVATE_DIRECTORY_MODE)
                mutated_parent_indexes.add(len(descriptors) - 1)
                created_indexes.add(len(descriptors))
            metadata = os.fstat(descriptor)
            _validate_open_directory(metadata, description="evidence directory")
            linked = os.stat(component, dir_fd=parent_fd, follow_symlinks=False)
            if (linked.st_dev, linked.st_ino) != (metadata.st_dev, metadata.st_ino):
                os.close(descriptor)
                raise EvidenceError("evidence path changed during anchored creation")
            descriptors.append(descriptor)
            names.append(component)
            identities.append(metadata)
        opened = _AnchoredDirectory(path, descriptors, names, identities)
        final = os.fstat(opened.fd)
        _validate_open_directory(
            final,
            description="evidence directory",
            owner_uid=os.geteuid(),
        )
        before_names = sorted(os.listdir(opened.fd))
        if before_names:
            raise EvidenceError(
                f"refusing to mix evidence in a non-empty directory: {path}"
            )
        os.fchmod(opened.fd, PRIVATE_DIRECTORY_MODE)
        opened.identities[-1] = os.fstat(opened.fd)
        for index in created_indexes | mutated_parent_indexes:
            opened.identities[index] = os.fstat(opened.descriptors[index])
        after_names = sorted(os.listdir(opened.fd))
        if after_names != before_names:
            raise EvidenceError(
                "evidence directory membership changed during preparation"
            )
        _validate_open_directory(
            opened.identities[-1],
            description="evidence directory",
            owner_uid=os.geteuid(),
            private=True,
        )
        _revalidate_directory_chain(opened, description="evidence directory")
        os.fsync(opened.fd)
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)
    return path


def _private_output_parent(path: pathlib.Path) -> pathlib.Path:
    parent = path.parent
    _absolute_components(path, "artifact path")
    with _anchored_directory(
        parent,
        description="artifact parent",
        final_owner_uid=os.geteuid(),
        final_private=True,
    ) as opened:
        _revalidate_directory_chain(opened, description="artifact parent")
    return parent


def atomic_write_bytes(path: pathlib.Path, payload: bytes) -> None:
    """Publish one new private artifact atomically and durably."""

    _absolute_components(path, "artifact path")
    parent = path.parent
    with _anchored_directory(
        parent,
        description="artifact parent",
        final_owner_uid=os.geteuid(),
        final_private=True,
    ) as opened:
        parent_fd = opened.fd
        membership_before = sorted(os.listdir(parent_fd))
        try:
            os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise EvidenceError(
                f"refusing to replace an existing evidence artifact: {path}"
            )
        temporary = f".{path.name}.tmp.{os.getpid()}.{secrets.token_hex(8)}"
        flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | _CLOEXEC | _NOFOLLOW
        descriptor = os.open(temporary, flags, PRIVATE_FILE_MODE, dir_fd=parent_fd)
        temporary_present = True
        try:
            os.fchmod(descriptor, PRIVATE_FILE_MODE)
            view = memoryview(payload)
            while view:
                written = os.write(descriptor, view)
                if written <= 0:
                    raise EvidenceError(
                        f"short write while publishing evidence: {path}"
                    )
                view = view[written:]
            os.fsync(descriptor)
            temporary_metadata = os.fstat(descriptor)
            if (
                not stat.S_ISREG(temporary_metadata.st_mode)
                or temporary_metadata.st_uid != os.geteuid()
                or temporary_metadata.st_nlink != 1
                or stat.S_IMODE(temporary_metadata.st_mode) != PRIVATE_FILE_MODE
                or temporary_metadata.st_size != len(payload)
            ):
                raise EvidenceError(
                    "temporary evidence artifact has an invalid identity"
                )
            os.lseek(descriptor, 0, os.SEEK_SET)
            digest = hashlib.sha256()
            while True:
                chunk = os.read(descriptor, 1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
            if digest.digest() != hashlib.sha256(payload).digest():
                raise EvidenceError(
                    "temporary evidence bytes changed before publication"
                )
            try:
                os.link(
                    temporary,
                    path.name,
                    src_dir_fd=parent_fd,
                    dst_dir_fd=parent_fd,
                    follow_symlinks=False,
                )
            except FileExistsError as error:
                raise EvidenceError(
                    f"evidence destination appeared during publication: {path}"
                ) from error
            published_fd = os.open(
                path.name, os.O_RDONLY | _CLOEXEC | _NOFOLLOW, dir_fd=parent_fd
            )
            try:
                published = os.fstat(published_fd)
                if (published.st_dev, published.st_ino) != (
                    temporary_metadata.st_dev,
                    temporary_metadata.st_ino,
                ):
                    raise EvidenceError(
                        "published evidence is not the prepared temporary inode"
                    )
                os.unlink(temporary, dir_fd=parent_fd)
                temporary_present = False
                os.fsync(parent_fd)
                final = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
                opened_final = os.fstat(published_fd)
                if (
                    not stat.S_ISREG(final.st_mode)
                    or (final.st_dev, final.st_ino)
                    != (opened_final.st_dev, opened_final.st_ino)
                    or final.st_uid != os.geteuid()
                    or final.st_nlink != 1
                    or stat.S_IMODE(final.st_mode) != PRIVATE_FILE_MODE
                    or final.st_size != len(payload)
                ):
                    raise EvidenceError(
                        f"published evidence artifact has an invalid identity: {path}"
                    )
                os.lseek(published_fd, 0, os.SEEK_SET)
                final_digest = hashlib.sha256()
                while True:
                    chunk = os.read(published_fd, 1024 * 1024)
                    if not chunk:
                        break
                    final_digest.update(chunk)
                if final_digest.digest() != hashlib.sha256(payload).digest():
                    raise EvidenceError("published evidence content changed")
            finally:
                os.close(published_fd)
            membership_after = sorted(os.listdir(parent_fd))
            expected_membership = sorted([*membership_before, path.name])
            if membership_after != expected_membership:
                raise EvidenceError(
                    "artifact-parent membership changed unexpectedly during publication"
                )
            opened.identities[-1] = os.fstat(parent_fd)
            _revalidate_directory_chain(opened, description="artifact parent")
        finally:
            os.close(descriptor)
            if temporary_present:
                try:
                    os.unlink(temporary, dir_fd=parent_fd)
                    os.fsync(parent_fd)
                except FileNotFoundError:
                    pass


def atomic_write_json(path: pathlib.Path, payload: Any) -> None:
    rendered = json.dumps(
        payload, separators=(",", ":"), sort_keys=True, allow_nan=False
    )
    atomic_write_bytes(path, (rendered + "\n").encode("utf-8"))


def transfer_private_artifact_to_uid(
    source: pathlib.Path,
    destination: pathlib.Path,
    *,
    destination_uid: int,
    destination_gid: int,
    maximum_bytes: int,
) -> dict[str, Any]:
    """Transfer one root-private artifact through inherited memory to another UID."""

    if os.geteuid() != 0:
        raise EvidenceError("cross-UID evidence transfer requires root supervision")
    uid = _exact_int(
        destination_uid, description="artifact destination UID", maximum=MAX_U64
    )
    gid = _exact_int(
        destination_gid, description="artifact destination GID", maximum=MAX_U64
    )
    bound = _exact_int(
        maximum_bytes,
        description="artifact transfer byte bound",
        minimum=1,
        maximum=MAX_JSON_EVIDENCE_BYTES,
    )
    source_before = _lstat(source, description="private transfer source")
    if (
        not stat.S_ISREG(source_before.st_mode)
        or source_before.st_uid != 0
        or source_before.st_nlink != 1
        or stat.S_IMODE(source_before.st_mode) != PRIVATE_FILE_MODE
    ):
        raise EvidenceError("private transfer source identity is invalid")
    payload = _stable_regular_bytes(
        source,
        description="private transfer source",
        maximum_bytes=bound,
        before=source_before,
    )
    digest = hashlib.sha256(payload).hexdigest()
    child = os.fork()
    if child == 0:  # pragma: no cover - exercised by the root-only Docker gate.
        try:
            os.setgroups([])
            os.setgid(gid)
            os.setuid(uid)
            os.umask(0o077)
            atomic_write_bytes(destination, payload)
        except BaseException:
            os._exit(1)
        os._exit(0)
    waited, status = os.waitpid(child, 0)
    if waited != child or not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0:
        raise EvidenceError("cross-UID artifact publisher failed")
    with _anchored_directory(
        source.parent,
        description="private transfer source parent",
        final_owner_uid=0,
        final_private=True,
    ) as opened:
        current = os.stat(source.name, dir_fd=opened.fd, follow_symlinks=False)
        if any(
            getattr(current, field) != getattr(source_before, field)
            for field in (
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
        ):
            raise EvidenceError("private transfer source changed before removal")
        os.unlink(source.name, dir_fd=opened.fd)
        os.fsync(opened.fd)
        _revalidate_directory_chain(
            opened, description="private transfer source parent"
        )
    return {
        "schema_version": 1,
        "destination": str(destination),
        "destination_uid": uid,
        "destination_gid": gid,
        "size": len(payload),
        "sha256": digest,
        "source_removed": True,
    }


@contextmanager
def private_output_spool(path: pathlib.Path) -> Iterator[int]:
    """Create one private no-follow output file and retain its open descriptor."""

    _absolute_components(path, "private output spool")
    with _anchored_directory(
        path.parent,
        description="private output spool parent",
        final_owner_uid=os.geteuid(),
        final_private=True,
    ) as opened:
        flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | _CLOEXEC | _NOFOLLOW
        try:
            descriptor = os.open(path.name, flags, PRIVATE_FILE_MODE, dir_fd=opened.fd)
        except FileExistsError as error:
            raise EvidenceError("private output spool already exists") from error
        try:
            os.fchmod(descriptor, PRIVATE_FILE_MODE)
            metadata = os.fstat(descriptor)
            linked = os.stat(path.name, dir_fd=opened.fd, follow_symlinks=False)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or metadata.st_uid != os.geteuid()
                or metadata.st_nlink != 1
                or stat.S_IMODE(metadata.st_mode) != PRIVATE_FILE_MODE
                or metadata.st_size != 0
                or (linked.st_dev, linked.st_ino) != (metadata.st_dev, metadata.st_ino)
            ):
                raise EvidenceError("private output spool identity is invalid")
            yield descriptor
            os.fsync(descriptor)
            after = os.fstat(descriptor)
            final_link = os.stat(path.name, dir_fd=opened.fd, follow_symlinks=False)
            if (
                not stat.S_ISREG(after.st_mode)
                or after.st_uid != metadata.st_uid
                or after.st_nlink != 1
                or stat.S_IMODE(after.st_mode) != PRIVATE_FILE_MODE
                or (after.st_dev, after.st_ino) != (metadata.st_dev, metadata.st_ino)
                or (final_link.st_dev, final_link.st_ino)
                != (after.st_dev, after.st_ino)
            ):
                raise EvidenceError("private output spool changed during use")
            os.fsync(opened.fd)
            _revalidate_directory_chain(
                opened, description="private output spool parent"
            )
        finally:
            os.close(descriptor)


def _stable_regular_bytes(
    path: pathlib.Path,
    *,
    description: str,
    maximum_bytes: int,
    before: os.stat_result | None = None,
) -> bytes:
    """Read one exact regular file without following or accepting replacement."""

    _absolute_components(path, description)
    with _anchored_directory(
        path.parent, description=f"{description} parent"
    ) as opened_parent:
        try:
            linked_before = os.stat(
                path.name, dir_fd=opened_parent.fd, follow_symlinks=False
            )
        except FileNotFoundError as error:
            raise EvidenceError(f"{description} is missing: {path}") from error
        metadata = before if before is not None else linked_before
        if any(
            getattr(linked_before, field) != getattr(metadata, field)
            for field in (
                "st_dev",
                "st_ino",
                "st_mode",
                "st_size",
                "st_mtime_ns",
                "st_ctime_ns",
                "st_nlink",
            )
        ):
            raise EvidenceError(f"{description} changed before read: {path}")
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise EvidenceError(f"{description} is not a real regular file: {path}")
        if metadata.st_nlink != 1:
            raise EvidenceError(
                f"{description} has an ambiguous hard-link alias: {path}"
            )
        if metadata.st_size > maximum_bytes:
            raise EvidenceError(
                f"{description} exceeds its {maximum_bytes}-byte bound: {path}"
            )
        flags = os.O_RDONLY | _CLOEXEC | _NOFOLLOW
        try:
            descriptor = os.open(path.name, flags, dir_fd=opened_parent.fd)
        except OSError as error:
            raise EvidenceError(
                f"could not open {description} exactly: {path}: {error}"
            ) from error
        identity_fields = (
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
        try:
            opened = os.fstat(descriptor)
            if any(
                getattr(opened, field) != getattr(metadata, field)
                for field in identity_fields
            ):
                raise EvidenceError(f"{description} changed before read: {path}")
            chunks: list[bytes] = []
            remaining = maximum_bytes + 1
            while remaining > 0:
                chunk = os.read(descriptor, min(1024 * 1024, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            if remaining == 0 and os.read(descriptor, 1):
                raise EvidenceError(
                    f"{description} exceeds its {maximum_bytes}-byte bound: {path}"
                )
            after = os.fstat(descriptor)
            if any(
                getattr(after, field) != getattr(opened, field)
                for field in identity_fields
            ):
                raise EvidenceError(f"{description} changed while read: {path}")
        finally:
            os.close(descriptor)
        final = os.stat(path.name, dir_fd=opened_parent.fd, follow_symlinks=False)
        if any(
            getattr(final, field) != getattr(after, field) for field in identity_fields
        ):
            raise EvidenceError(f"{description} path changed after read: {path}")
        _revalidate_directory_chain(opened_parent, description=f"{description} parent")
        payload = b"".join(chunks)
        if len(payload) != after.st_size:
            raise EvidenceError(f"{description} size changed while read: {path}")
        return payload


def _stable_regular_size_sha256(
    path: pathlib.Path, *, description: str, maximum_bytes: int
) -> tuple[int, str]:
    """Hash one bounded regular file without retaining its bytes in memory."""

    _absolute_components(path, description)
    with _anchored_directory(
        path.parent, description=f"{description} parent"
    ) as opened_parent:
        try:
            linked = os.stat(path.name, dir_fd=opened_parent.fd, follow_symlinks=False)
        except FileNotFoundError as error:
            raise EvidenceError(f"{description} is missing: {path}") from error
        if (
            not stat.S_ISREG(linked.st_mode)
            or linked.st_nlink != 1
            or linked.st_size > maximum_bytes
        ):
            raise EvidenceError(f"{description} identity or size is invalid: {path}")
        descriptor = os.open(
            path.name, os.O_RDONLY | _CLOEXEC | _NOFOLLOW, dir_fd=opened_parent.fd
        )
        identity_fields = (
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
        try:
            opened = os.fstat(descriptor)
            if any(
                getattr(opened, field) != getattr(linked, field)
                for field in identity_fields
            ):
                raise EvidenceError(f"{description} changed before hashing: {path}")
            digest = hashlib.sha256()
            observed = 0
            while True:
                chunk = os.read(descriptor, 1024 * 1024)
                if not chunk:
                    break
                observed += len(chunk)
                if observed > maximum_bytes:
                    raise EvidenceError(
                        f"{description} exceeds its {maximum_bytes}-byte bound: {path}"
                    )
                digest.update(chunk)
            after = os.fstat(descriptor)
            if any(
                getattr(after, field) != getattr(opened, field)
                for field in identity_fields
            ):
                raise EvidenceError(f"{description} changed while hashing: {path}")
        finally:
            os.close(descriptor)
        final = os.stat(path.name, dir_fd=opened_parent.fd, follow_symlinks=False)
        if any(
            getattr(final, field) != getattr(after, field) for field in identity_fields
        ):
            raise EvidenceError(f"{description} path changed after hashing: {path}")
        if observed != after.st_size:
            raise EvidenceError(f"{description} size changed while hashing: {path}")
        _revalidate_directory_chain(opened_parent, description=f"{description} parent")
        return observed, digest.hexdigest()


def _validate_ledger_records(payload: bytes) -> tuple[int, str | None]:
    if not payload:
        return 0, None
    if not payload.endswith(b"\n"):
        raise EvidenceError("resource ledger has a truncated final record")
    previous: str | None = None
    ordinal = 0
    for raw_line in payload.splitlines():
        if len(raw_line) > MAX_LEDGER_RECORD_BYTES:
            raise EvidenceError("resource-ledger record exceeds the 64 KiB bound")
        record = strict_json_loads(raw_line, description="resource-ledger record")
        if not isinstance(record, dict) or set(record) != {
            "schema_version",
            "kind",
            "ordinal",
            "previous_record_sha256",
            "payload",
            "record_sha256",
        }:
            raise EvidenceError("resource-ledger record schema is not exact")
        current_ordinal = record.get("ordinal")
        if (
            record.get("schema_version") != 1
            or record.get("kind") != "pmux_private_jsonl_record"
            or type(current_ordinal) is not int
            or current_ordinal != ordinal + 1
            or record.get("previous_record_sha256") != previous
            or not isinstance(record.get("payload"), dict)
        ):
            raise EvidenceError("resource-ledger record chain is invalid")
        body = dict(record)
        digest = body.pop("record_sha256", None)
        expected = canonical_json_sha256(
            body, domain="pmux.evidence.private-jsonl-record.v1"
        )
        if digest != expected:
            raise EvidenceError("resource-ledger record digest is invalid")
        ordinal = current_ordinal
        previous = expected
    return ordinal, previous


def append_private_jsonl(
    path: pathlib.Path,
    payload: Mapping[str, Any],
    *,
    expected_ordinal: int,
    expected_prior_sha256: str | None,
) -> str:
    """Append one externally anchored, hash-chained private JSON record."""

    if type(expected_ordinal) is not int or expected_ordinal < 1:
        raise EvidenceError("resource-ledger expected ordinal is invalid")
    if expected_prior_sha256 is not None and not re.fullmatch(
        r"[0-9a-f]{64}", expected_prior_sha256
    ):
        raise EvidenceError("resource-ledger expected prior digest is invalid")
    _absolute_components(path, "resource ledger")
    with _anchored_directory(
        path.parent,
        description="resource-ledger parent",
        final_owner_uid=os.geteuid(),
        final_private=True,
    ) as opened:
        parent_fd = opened.fd
        membership_before = sorted(os.listdir(parent_fd))
        flags = os.O_RDWR | os.O_APPEND | _CLOEXEC | _NOFOLLOW
        created = False
        try:
            descriptor = os.open(path.name, flags, dir_fd=parent_fd)
        except FileNotFoundError:
            descriptor = os.open(
                path.name,
                flags | os.O_CREAT | os.O_EXCL,
                PRIVATE_FILE_MODE,
                dir_fd=parent_fd,
            )
            created = True
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX)
            if created:
                os.fchmod(descriptor, PRIVATE_FILE_MODE)
            metadata = os.fstat(descriptor)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or metadata.st_uid != os.geteuid()
                or metadata.st_nlink != 1
                or stat.S_IMODE(metadata.st_mode) != PRIVATE_FILE_MODE
            ):
                raise EvidenceError("resource ledger is not one owned mode-0600 file")
            linked = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            if (linked.st_dev, linked.st_ino) != (
                metadata.st_dev,
                metadata.st_ino,
            ):
                raise EvidenceError("resource-ledger path changed before append")
            os.lseek(descriptor, 0, os.SEEK_SET)
            existing_chunks: list[bytes] = []
            while True:
                chunk = os.read(descriptor, 1024 * 1024)
                if not chunk:
                    break
                existing_chunks.append(chunk)
            existing = b"".join(existing_chunks)
            ordinal, prior = _validate_ledger_records(existing)
            if ordinal + 1 != expected_ordinal or prior != expected_prior_sha256:
                raise EvidenceError(
                    "resource ledger differs from the externally carried tail"
                )
            body = {
                "schema_version": 1,
                "kind": "pmux_private_jsonl_record",
                "ordinal": expected_ordinal,
                "previous_record_sha256": expected_prior_sha256,
                "payload": dict(payload),
            }
            digest = canonical_json_sha256(
                body, domain="pmux.evidence.private-jsonl-record.v1"
            )
            record = {**body, "record_sha256": digest}
            rendered = (
                json.dumps(
                    record, separators=(",", ":"), sort_keys=True, allow_nan=False
                )
                + "\n"
            ).encode("utf-8")
            if len(rendered) > MAX_LEDGER_RECORD_BYTES:
                raise EvidenceError(
                    "resource-ledger record exceeds the 64 KiB evidence bound"
                )
            written = os.write(descriptor, rendered)
            if written != len(rendered):
                raise EvidenceError("resource-ledger append was incomplete")
            os.fsync(descriptor)
            after = os.fstat(descriptor)
            final_link = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            if (
                (after.st_dev, after.st_ino) != (metadata.st_dev, metadata.st_ino)
                or (final_link.st_dev, final_link.st_ino)
                != (after.st_dev, after.st_ino)
                or after.st_nlink != 1
                or stat.S_IMODE(after.st_mode) != PRIVATE_FILE_MODE
                or after.st_size != len(existing) + len(rendered)
            ):
                raise EvidenceError("resource ledger changed during append")
            os.lseek(descriptor, 0, os.SEEK_SET)
            final_chunks: list[bytes] = []
            while True:
                chunk = os.read(descriptor, 1024 * 1024)
                if not chunk:
                    break
                final_chunks.append(chunk)
            final_ordinal, final_digest = _validate_ledger_records(
                b"".join(final_chunks)
            )
            if final_ordinal != expected_ordinal or final_digest != digest:
                raise EvidenceError("resource ledger tail verification failed")
            os.fsync(parent_fd)
            membership_after = sorted(os.listdir(parent_fd))
            expected_membership = (
                sorted([*membership_before, path.name])
                if created
                else membership_before
            )
            if membership_after != expected_membership:
                raise EvidenceError(
                    "resource-ledger parent membership changed during append"
                )
            opened.identities[-1] = os.fstat(parent_fd)
            _revalidate_directory_chain(opened, description="resource-ledger parent")
            return digest
        except BaseException:
            if created:
                try:
                    current = os.stat(
                        path.name, dir_fd=parent_fd, follow_symlinks=False
                    )
                    opened_file = os.fstat(descriptor)
                    if (current.st_dev, current.st_ino) == (
                        opened_file.st_dev,
                        opened_file.st_ino,
                    ) and opened_file.st_size == 0:
                        os.unlink(path.name, dir_fd=parent_fd)
                        os.fsync(parent_fd)
                except FileNotFoundError:
                    pass
            raise
        finally:
            os.close(descriptor)


def _bounded_command_label(value: Any, *, description: str) -> str:
    label = _exact_text(value, description=description, maximum_bytes=128)
    if re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,127}", label) is None:
        raise EvidenceError(f"{description} is not one canonical command label")
    return label


def _validated_bounded_process_receipt(payload: bytes) -> dict[str, Any]:
    loaded = strict_json_loads(payload, description="bounded process receipt")
    if not isinstance(loaded, Mapping):
        raise EvidenceError("bounded process receipt is not an object")
    try:
        if loaded.get("kind") == "pmux_bounded_process":
            return bounded_process.validate_execution_receipt(loaded)
        if loaded.get("kind") == "pmux_bounded_process_failure":
            return bounded_process.validate_failure_receipt(loaded)
    except bounded_process.BoundedProcessError as error:
        raise EvidenceError("bounded process receipt is invalid") from error
    raise EvidenceError("bounded process receipt kind is invalid")


def append_bounded_command_receipt(
    ledger: pathlib.Path,
    receipt_path: pathlib.Path,
    *,
    label: str,
    scope: str,
    expected_ordinal: int,
    expected_prior_sha256: str | None,
) -> str:
    """Append one full validated process receipt to the external command chain."""

    normalized_label = _bounded_command_label(label, description="command label")
    if scope != "host" and scope not in PLATFORMS:
        raise EvidenceError("bounded command scope is invalid")
    receipt = _validated_bounded_process_receipt(
        _stable_regular_bytes(
            receipt_path,
            description="bounded command execution receipt",
            maximum_bytes=4 * 1024 * 1024,
        )
    )
    suffix = ".receipt.json"
    if not receipt_path.name.endswith(suffix):
        raise EvidenceError("bounded command receipt filename is invalid")
    prefix = receipt_path.name[: -len(suffix)]
    stdout_path = receipt_path.with_name(f"{prefix}.stdout")
    stderr_path = receipt_path.with_name(f"{prefix}.stderr")
    evidence_root = ledger.parent
    try:
        receipt_relative = receipt_path.relative_to(evidence_root).as_posix()
        stdout_relative = stdout_path.relative_to(evidence_root).as_posix()
        stderr_relative = stderr_path.relative_to(evidence_root).as_posix()
    except ValueError as error:
        raise EvidenceError(
            "bounded command artifacts escape the evidence root"
        ) from error
    receipt_relative = _portable_relative_path(
        receipt_relative, description="bounded command receipt path"
    )
    stdout_relative = _portable_relative_path(
        stdout_relative, description="bounded command stdout path"
    )
    stderr_relative = _portable_relative_path(
        stderr_relative, description="bounded command stderr path"
    )
    stdout_size, stdout_sha256 = _stable_regular_size_sha256(
        stdout_path,
        description="bounded command stdout spool",
        maximum_bytes=receipt["maximum_output_bytes"],
    )
    stderr_size, stderr_sha256 = _stable_regular_size_sha256(
        stderr_path,
        description="bounded command stderr spool",
        maximum_bytes=receipt["maximum_output_bytes"],
    )
    if (
        stdout_size != receipt["stdout_size"]
        or stdout_sha256 != receipt["stdout_sha256"]
        or stderr_size != receipt["stderr_size"]
        or stderr_sha256 != receipt["stderr_sha256"]
    ):
        raise EvidenceError("bounded command spools differ from their receipt")
    return append_private_jsonl(
        ledger,
        {
            "schema_version": 1,
            "kind": "pmux_bounded_command_receipt",
            "label": normalized_label,
            "scope": scope,
            "receipt_path": receipt_relative,
            "stdout_path": stdout_relative,
            "stderr_path": stderr_relative,
            "receipt_sha256": receipt["receipt_sha256"],
            "process_receipt": receipt,
        },
        expected_ordinal=expected_ordinal,
        expected_prior_sha256=expected_prior_sha256,
    )


def bounded_command_ledger_report(
    ledger: pathlib.Path,
    *,
    expected_count: int,
    expected_tail_sha256: str,
) -> dict[str, Any]:
    """Validate every full receipt and bind the externally carried ledger tail."""

    count = _exact_int(
        expected_count,
        description="bounded command ledger expected count",
        maximum=1_000_000,
    )
    tail = _exact_digest(
        expected_tail_sha256,
        description="bounded command ledger expected tail",
    )
    payload = _stable_regular_bytes(
        ledger,
        description="bounded command receipt ledger",
        maximum_bytes=MAX_JSON_EVIDENCE_BYTES,
    )
    observed_count, observed_tail = _validate_ledger_records(payload)
    if observed_count != count or observed_tail != tail:
        raise EvidenceError("bounded command ledger differs from its external tail")
    labels: list[str] = []
    scopes: list[str] = []
    receipt_digests: list[str] = []
    infrastructure_failure_count = 0
    for index, raw_line in enumerate(payload.splitlines(), start=1):
        chained = strict_json_loads(
            raw_line, description=f"bounded command ledger row {index}"
        )
        row = chained["payload"]
        _exact_object(
            row,
            {
                "schema_version",
                "kind",
                "label",
                "scope",
                "receipt_path",
                "stdout_path",
                "stderr_path",
                "receipt_sha256",
                "process_receipt",
            },
            description=f"bounded command payload {index}",
        )
        if (
            _exact_int(
                row.get("schema_version"),
                description=f"bounded command schema {index}",
            )
            != 1
            or row.get("kind") != "pmux_bounded_command_receipt"
        ):
            raise EvidenceError("bounded command payload identity is invalid")
        label = _bounded_command_label(
            row.get("label"), description=f"bounded command label {index}"
        )
        scope = row.get("scope")
        if scope != "host" and scope not in PLATFORMS:
            raise EvidenceError("bounded command payload scope is invalid")
        paths = {
            name: _portable_relative_path(
                row.get(name), description=f"bounded command {name} {index}"
            )
            for name in ("receipt_path", "stdout_path", "stderr_path")
        }
        if len(set(paths.values())) != 3:
            raise EvidenceError("bounded command artifact paths are aliased")
        try:
            raw_receipt = row.get("process_receipt")
            if not isinstance(raw_receipt, Mapping):
                raise bounded_process.BoundedProcessError("receipt is not an object")
            if raw_receipt.get("kind") == "pmux_bounded_process":
                receipt = bounded_process.validate_execution_receipt(raw_receipt)
            elif raw_receipt.get("kind") == "pmux_bounded_process_failure":
                receipt = bounded_process.validate_failure_receipt(raw_receipt)
            else:
                raise bounded_process.BoundedProcessError("receipt kind is invalid")
        except bounded_process.BoundedProcessError as error:
            raise EvidenceError("bounded command payload receipt is invalid") from error
        receipt_digest = _exact_digest(
            row.get("receipt_sha256"),
            description=f"bounded command receipt digest {index}",
        )
        if receipt_digest != receipt["receipt_sha256"]:
            raise EvidenceError("bounded command receipt digest was substituted")
        retained_receipt = _validated_bounded_process_receipt(
            _stable_regular_bytes(
                ledger.parent / paths["receipt_path"],
                description=f"retained bounded command receipt {index}",
                maximum_bytes=4 * 1024 * 1024,
            )
        )
        if retained_receipt != receipt:
            raise EvidenceError("retained bounded command receipt was substituted")
        for stream in ("stdout", "stderr"):
            size, digest = _stable_regular_size_sha256(
                ledger.parent / paths[f"{stream}_path"],
                description=f"retained bounded command {stream} {index}",
                maximum_bytes=receipt["maximum_output_bytes"],
            )
            if (
                size != receipt[f"{stream}_size"]
                or digest != receipt[f"{stream}_sha256"]
            ):
                raise EvidenceError(
                    f"retained bounded command {stream} differs from its receipt"
                )
        if receipt["kind"] == "pmux_bounded_process_failure":
            infrastructure_failure_count += 1
        labels.append(label)
        scopes.append(scope)
        receipt_digests.append(receipt_digest)
    if len(labels) != len(set(labels)):
        raise EvidenceError("bounded command labels are duplicated")
    body = {
        "schema_version": 1,
        "kind": "pmux_bounded_command_ledger_report",
        "command_count": count,
        "tail_record_sha256": tail,
        "ledger_bytes": len(payload),
        "ledger_sha256": hashlib.sha256(payload).hexdigest(),
        "labels": labels,
        "scopes": scopes,
        "receipt_digests_sha256": canonical_json_sha256(
            receipt_digests,
            domain="pmux.evidence.bounded-command-receipt-digests.v1",
        ),
        "all_receipts_valid": True,
        "infrastructure_failure_count": infrastructure_failure_count,
        "all_commands_bounded": infrastructure_failure_count == 0,
    }
    return {
        **body,
        "report_sha256": canonical_json_sha256(
            body, domain="pmux.evidence.bounded-command-ledger-report.v1"
        ),
    }


def secure_private_tree(root: pathlib.Path) -> None:
    """Reject ambiguous nodes and enforce owner-only modes on copied evidence."""

    def secure_directory(descriptor: int, display: pathlib.Path) -> None:
        opened_metadata = os.fstat(descriptor)
        _validate_open_directory(
            opened_metadata,
            description=f"evidence directory {display}",
            owner_uid=os.geteuid(),
        )
        names_before = sorted(os.listdir(descriptor))
        for name in names_before:
            if name in {".", ".."} or "/" in name:
                raise EvidenceError("evidence tree contains an invalid member name")
            child_display = display / name
            before = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            if before.st_uid != os.geteuid():
                raise EvidenceError(
                    f"evidence node has the wrong owner: {child_display}"
                )
            if stat.S_ISLNK(before.st_mode):
                raise EvidenceError(
                    f"evidence tree contains a symlink: {child_display}"
                )
            if stat.S_ISDIR(before.st_mode):
                child_fd = os.open(
                    name,
                    os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC,
                    dir_fd=descriptor,
                )
                try:
                    opened_child = os.fstat(child_fd)
                    if (opened_child.st_dev, opened_child.st_ino) != (
                        before.st_dev,
                        before.st_ino,
                    ):
                        raise EvidenceError(
                            f"evidence directory changed before securing: {child_display}"
                        )
                    secure_directory(child_fd, child_display)
                    os.fchmod(child_fd, PRIVATE_DIRECTORY_MODE)
                    after = os.fstat(child_fd)
                    linked = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                    if (
                        (after.st_dev, after.st_ino)
                        != (opened_child.st_dev, opened_child.st_ino)
                        or (linked.st_dev, linked.st_ino)
                        != (after.st_dev, after.st_ino)
                        or not stat.S_ISDIR(linked.st_mode)
                        or stat.S_IMODE(after.st_mode) != PRIVATE_DIRECTORY_MODE
                    ):
                        raise EvidenceError(
                            f"evidence directory changed while securing: {child_display}"
                        )
                finally:
                    os.close(child_fd)
            elif stat.S_ISREG(before.st_mode):
                if before.st_nlink != 1:
                    raise EvidenceError(
                        f"evidence tree contains a hard-link alias: {child_display}"
                    )
                child_fd = os.open(
                    name, os.O_RDONLY | _NOFOLLOW | _CLOEXEC, dir_fd=descriptor
                )
                try:
                    opened_child = os.fstat(child_fd)
                    if (opened_child.st_dev, opened_child.st_ino) != (
                        before.st_dev,
                        before.st_ino,
                    ) or opened_child.st_nlink != 1:
                        raise EvidenceError(
                            f"evidence file changed before securing: {child_display}"
                        )
                    os.fchmod(child_fd, PRIVATE_FILE_MODE)
                    after = os.fstat(child_fd)
                    linked = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                    if (
                        (after.st_dev, after.st_ino)
                        != (opened_child.st_dev, opened_child.st_ino)
                        or (linked.st_dev, linked.st_ino)
                        != (after.st_dev, after.st_ino)
                        or not stat.S_ISREG(linked.st_mode)
                        or after.st_nlink != 1
                        or stat.S_IMODE(after.st_mode) != PRIVATE_FILE_MODE
                    ):
                        raise EvidenceError(
                            f"evidence file changed while securing: {child_display}"
                        )
                finally:
                    os.close(child_fd)
            else:
                raise EvidenceError(
                    f"evidence tree contains a special file: {child_display}"
                )
        if sorted(os.listdir(descriptor)) != names_before:
            raise EvidenceError(
                f"evidence directory membership changed while securing: {display}"
            )

    with _anchored_directory(
        root,
        description="evidence root",
        final_owner_uid=os.geteuid(),
    ) as opened:
        secure_directory(opened.fd, root)
        os.fchmod(opened.fd, PRIVATE_DIRECTORY_MODE)
        opened.identities[-1] = os.fstat(opened.fd)
        _validate_open_directory(
            opened.identities[-1],
            description="evidence root",
            owner_uid=os.geteuid(),
            private=True,
        )
        _revalidate_directory_chain(opened, description="evidence root")
        os.fsync(opened.fd)


_TREE_IDENTITY_FIELDS = (
    "st_dev",
    "st_ino",
    "st_mode",
    "st_size",
    "st_mtime_ns",
    "st_ctime_ns",
    "st_nlink",
)


def _tree_metadata_snapshot(
    root: pathlib.Path, *, excluded_paths: frozenset[str]
) -> dict[str, os.stat_result]:
    snapshot: dict[str, os.stat_result] = {}
    stack = [root]
    while stack:
        current = stack.pop()
        for child in sorted(
            current.iterdir(), key=lambda item: item.name, reverse=True
        ):
            relative = child.relative_to(root).as_posix()
            if relative in excluded_paths:
                continue
            metadata = child.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                raise EvidenceError(f"artifact tree contains a symlink: {child}")
            if stat.S_ISDIR(metadata.st_mode):
                stack.append(child)
            elif not stat.S_ISREG(metadata.st_mode):
                raise EvidenceError(f"artifact tree contains a special file: {child}")
            elif metadata.st_nlink != 1:
                raise EvidenceError(
                    f"artifact tree contains a hard-link alias: {child}"
                )
            snapshot[relative] = metadata
    return snapshot


def _anchored_tree_capture(
    descriptor: int,
    *,
    relative_prefix: str,
    excluded_paths: frozenset[str],
    read_files: bool,
) -> tuple[dict[str, os.stat_result], dict[str, bytes]]:
    snapshot: dict[str, os.stat_result] = {}
    contents: dict[str, bytes] = {}
    names_before = sorted(os.listdir(descriptor))
    for name in names_before:
        relative = f"{relative_prefix}/{name}" if relative_prefix else name
        if relative in excluded_paths:
            continue
        before = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        if stat.S_ISLNK(before.st_mode):
            raise EvidenceError(f"artifact tree contains a symlink: {relative}")
        if stat.S_ISDIR(before.st_mode):
            child_fd = os.open(
                name,
                os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC,
                dir_fd=descriptor,
            )
            try:
                opened = os.fstat(child_fd)
                if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                    raise EvidenceError(
                        f"artifact directory changed before traversal: {relative}"
                    )
                snapshot[relative] = opened
                child_snapshot, child_contents = _anchored_tree_capture(
                    child_fd,
                    relative_prefix=relative,
                    excluded_paths=excluded_paths,
                    read_files=read_files,
                )
                snapshot.update(child_snapshot)
                contents.update(child_contents)
                after = os.fstat(child_fd)
                linked = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if any(
                    getattr(after, field) != getattr(opened, field)
                    for field in _TREE_IDENTITY_FIELDS
                ) or (linked.st_dev, linked.st_ino) != (
                    after.st_dev,
                    after.st_ino,
                ):
                    raise EvidenceError(
                        f"artifact directory changed during traversal: {relative}"
                    )
            finally:
                os.close(child_fd)
            continue
        if not stat.S_ISREG(before.st_mode):
            raise EvidenceError(f"artifact tree contains a special file: {relative}")
        if before.st_nlink != 1:
            raise EvidenceError(f"artifact tree contains a hard-link alias: {relative}")
        file_fd = os.open(name, os.O_RDONLY | _NOFOLLOW | _CLOEXEC, dir_fd=descriptor)
        try:
            opened = os.fstat(file_fd)
            if any(
                getattr(opened, field) != getattr(before, field)
                for field in _TREE_IDENTITY_FIELDS
            ):
                raise EvidenceError(
                    f"artifact file changed before traversal: {relative}"
                )
            if read_files:
                chunks: list[bytes] = []
                while True:
                    chunk = os.read(file_fd, 1024 * 1024)
                    if not chunk:
                        break
                    chunks.append(chunk)
                contents[relative] = b"".join(chunks)
            after = os.fstat(file_fd)
            linked = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            if any(
                getattr(after, field) != getattr(opened, field)
                for field in _TREE_IDENTITY_FIELDS
            ) or (linked.st_dev, linked.st_ino) != (
                after.st_dev,
                after.st_ino,
            ):
                raise EvidenceError(
                    f"artifact file changed during traversal: {relative}"
                )
            snapshot[relative] = after
        finally:
            os.close(file_fd)
    if sorted(os.listdir(descriptor)) != names_before:
        raise EvidenceError("artifact tree membership changed during traversal")
    return snapshot, contents


def regular_tree_manifest(
    root: pathlib.Path, *, excluded_paths: frozenset[str] = frozenset()
) -> dict[str, Any]:
    """Hash one exact whole tree and reject path, mode, or identity races."""

    with _anchored_directory(root, description="artifact tree") as opened:
        root_metadata = os.fstat(opened.fd)
        before, contents = _anchored_tree_capture(
            opened.fd,
            relative_prefix="",
            excluded_paths=excluded_paths,
            read_files=True,
        )
        directories: list[dict[str, Any]] = []
        entries: list[dict[str, Any]] = []
        for relative, metadata in sorted(before.items()):
            if stat.S_ISDIR(metadata.st_mode):
                directories.append(
                    {
                        "path": relative,
                        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
                    }
                )
                continue
            payload = contents[relative]
            if len(payload) != metadata.st_size:
                raise EvidenceError(
                    f"artifact file size changed while hashing: {relative}"
                )
            entries.append(
                {
                    "path": relative,
                    "size": metadata.st_size,
                    "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
                    "sha256": hashlib.sha256(payload).hexdigest(),
                }
            )
        after, _unused = _anchored_tree_capture(
            opened.fd,
            relative_prefix="",
            excluded_paths=excluded_paths,
            read_files=False,
        )
        if before.keys() != after.keys():
            raise EvidenceError("artifact tree membership changed while hashing")
        for relative, original in before.items():
            current = after[relative]
            if any(
                getattr(current, field) != getattr(original, field)
                for field in _TREE_IDENTITY_FIELDS
            ):
                raise EvidenceError(
                    f"artifact tree identity changed while hashing: {root / relative}"
                )
        root_after = os.fstat(opened.fd)
        if any(
            getattr(root_after, field) != getattr(root_metadata, field)
            for field in _TREE_IDENTITY_FIELDS
        ):
            raise EvidenceError("artifact tree root changed while hashing")
        _revalidate_directory_chain(opened, description="artifact tree")
    aggregate = hashlib.sha256()
    aggregate.update(b"pmux-artifact-tree-v2\0")
    for kind, entry in [
        *(("directory", entry) for entry in directories),
        *(("file", entry) for entry in entries),
    ]:
        rendered = json.dumps(
            {"kind": kind, **entry}, separators=(",", ":"), sort_keys=True
        ).encode("utf-8")
        aggregate.update(len(rendered).to_bytes(4, "big"))
        aggregate.update(rendered)
    return {
        "schema_version": 2,
        "algorithm": "pmux-artifact-tree-v2-sha256",
        "root": ".",
        "excluded_paths": sorted(excluded_paths),
        "directory_count": len(directories),
        "file_count": len(entries),
        "tree_sha256": aggregate.hexdigest(),
        "directories": directories,
        "files": entries,
    }


def verify_regular_tree_manifest(
    root: pathlib.Path,
    manifest: Mapping[str, Any],
    *,
    expected_excluded_paths: frozenset[str] | None = None,
) -> dict[str, Any]:
    if manifest.get("schema_version") != 2:
        raise EvidenceError("artifact tree manifest schema is unsupported")
    raw_excluded = manifest.get("excluded_paths")
    if not isinstance(raw_excluded, list) or not all(
        isinstance(item, str) and item and not pathlib.PurePosixPath(item).is_absolute()
        for item in raw_excluded
    ):
        raise EvidenceError("artifact tree manifest exclusions are malformed")
    excluded = frozenset(raw_excluded)
    if any(".." in pathlib.PurePosixPath(item).parts for item in excluded):
        raise EvidenceError("artifact tree manifest exclusion escapes its root")
    if expected_excluded_paths is not None and excluded != expected_excluded_paths:
        raise EvidenceError("artifact tree manifest excludes unexpected evidence paths")
    current = regular_tree_manifest(root, excluded_paths=excluded)
    if current != manifest:
        raise EvidenceError("artifact tree identity changed after capture")
    return current


def _stable_regular_identity_at(
    directory_fd: int, name: str, path: pathlib.Path
) -> dict[str, Any]:
    try:
        metadata = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError as error:
        raise EvidenceError(f"release binary is missing: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise EvidenceError(f"release binary is not a real regular file: {path}")
    if metadata.st_nlink != 1:
        raise EvidenceError(f"release binary has an ambiguous hard-link alias: {path}")
    mode = stat.S_IMODE(metadata.st_mode)
    if mode & 0o7000:
        raise EvidenceError(f"release binary has special mode bits: {path}")
    if mode & 0o500 != 0o500:
        raise EvidenceError(f"release binary is not owner-readable/executable: {path}")
    if mode & 0o022:
        raise EvidenceError(f"release binary is group/world writable: {path}")
    flags = os.O_RDONLY | _CLOEXEC | _NOFOLLOW
    descriptor = os.open(name, flags, dir_fd=directory_fd)
    try:
        opened = os.fstat(descriptor)
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
        if any(getattr(opened, field) != getattr(metadata, field) for field in fields):
            raise EvidenceError(f"release binary changed before hashing: {path}")
        digest = hashlib.sha256()
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
        after = os.fstat(descriptor)
        if any(getattr(after, field) != getattr(opened, field) for field in fields):
            raise EvidenceError(f"release binary changed while hashing: {path}")
    finally:
        os.close(descriptor)
    final = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    if any(getattr(final, field) != getattr(after, field) for field in fields):
        raise EvidenceError(f"release binary path changed after hashing: {path}")
    return {
        "path": str(path),
        "size": after.st_size,
        "mode": f"{stat.S_IMODE(after.st_mode):04o}",
        "sha256": digest.hexdigest(),
        "device": after.st_dev,
        "inode": after.st_ino,
        "uid": after.st_uid,
        "gid": after.st_gid,
        "nlink": after.st_nlink,
        "mtime_ns": after.st_mtime_ns,
        "ctime_ns": after.st_ctime_ns,
    }


def _stable_regular_identity(path: pathlib.Path) -> dict[str, Any]:
    _absolute_components(path, "release binary path")
    with _anchored_directory(
        path.parent, description="release binary parent"
    ) as opened:
        identity = _stable_regular_identity_at(opened.fd, path.name, path)
        _revalidate_directory_chain(opened, description="release binary parent")
        return identity


def release_binary_manifest(
    directory: pathlib.Path, *, expected_owner_uid: int | None = None
) -> dict[str, Any]:
    """Capture every exact release executable used by the Rust E2E boundary."""

    if not directory.is_absolute():
        raise EvidenceError("release binary directory must be absolute")
    owner_uid = os.geteuid() if expected_owner_uid is None else expected_owner_uid
    with _anchored_directory(
        directory,
        description="release binary directory",
        final_owner_uid=owner_uid,
    ) as opened:
        root_metadata = os.fstat(opened.fd)
        root_mode = stat.S_IMODE(root_metadata.st_mode)
        if root_mode & 0o500 != 0o500:
            raise EvidenceError(
                "release binary directory is not owner-readable/executable"
            )
        if root_mode & 0o022:
            raise EvidenceError("release binary directory is group/world writable")
        binaries: dict[str, dict[str, Any]] = {}
        file_identities: set[tuple[int, int]] = set()
        for name in REQUIRED_RELEASE_BINARIES:
            candidate = directory / name
            identity = _stable_regular_identity_at(opened.fd, name, candidate)
            if (
                identity["uid"] != root_metadata.st_uid
                or identity["gid"] != root_metadata.st_gid
            ):
                raise EvidenceError(
                    f"release binary owner differs from its exact directory: {candidate}"
                )
            file_identity = (identity["device"], identity["inode"])
            if file_identity in file_identities:
                raise EvidenceError("release binary names must identify distinct files")
            file_identities.add(file_identity)
            binaries[name] = identity
        root_after = os.fstat(opened.fd)
        directory_fields = (
            "st_dev",
            "st_ino",
            "st_uid",
            "st_gid",
            "st_mode",
            "st_nlink",
            "st_mtime_ns",
            "st_ctime_ns",
        )
        if any(
            getattr(root_after, field) != getattr(root_metadata, field)
            for field in directory_fields
        ):
            raise EvidenceError("release binary directory changed during capture")
        _revalidate_directory_chain(opened, description="release binary directory")
        return {
            "schema_version": 1,
            "directory": str(directory),
            "directory_device": root_metadata.st_dev,
            "directory_inode": root_metadata.st_ino,
            "directory_uid": root_metadata.st_uid,
            "directory_gid": root_metadata.st_gid,
            "directory_mode": f"{stat.S_IMODE(root_metadata.st_mode):04o}",
            "directory_nlink": root_metadata.st_nlink,
            "directory_mtime_ns": root_metadata.st_mtime_ns,
            "directory_ctime_ns": root_metadata.st_ctime_ns,
            "required_names": list(REQUIRED_RELEASE_BINARIES),
            "binaries": binaries,
        }


def validate_release_binary_manifest_schema(
    manifest: Mapping[str, Any],
) -> dict[str, Any]:
    """Validate the complete release-binary identity without reading its paths."""

    expected_fields = {
        "schema_version",
        "directory",
        "directory_device",
        "directory_inode",
        "directory_uid",
        "directory_gid",
        "directory_mode",
        "directory_nlink",
        "directory_mtime_ns",
        "directory_ctime_ns",
        "required_names",
        "binaries",
    }
    _exact_object(manifest, expected_fields, description="release binary manifest")
    if (
        _exact_int(
            manifest.get("schema_version"),
            description="release manifest schema version",
        )
        != 1
    ):
        raise EvidenceError("release binary manifest schema is unsupported")
    if manifest.get("required_names") != list(REQUIRED_RELEASE_BINARIES):
        raise EvidenceError(
            "release binary manifest does not name the complete exact set"
        )
    directory = _canonical_absolute_path(
        manifest.get("directory"), description="release binary directory"
    )
    directory_values = {
        field: _exact_int(
            manifest.get(field),
            description=f"release manifest {field}",
            maximum=MAX_U64,
        )
        for field in (
            "directory_device",
            "directory_inode",
            "directory_uid",
            "directory_gid",
            "directory_nlink",
            "directory_mtime_ns",
            "directory_ctime_ns",
        )
    }
    if directory_values["directory_nlink"] < 1:
        raise EvidenceError("release binary directory link count is invalid")
    directory_mode = _exact_mode(
        manifest.get("directory_mode"), description="release binary directory mode"
    )
    parsed_directory_mode = int(directory_mode, 8)
    if parsed_directory_mode & 0o500 != 0o500 or parsed_directory_mode & 0o7022:
        raise EvidenceError("release binary directory mode is unsafe")
    binaries = manifest.get("binaries")
    if not isinstance(binaries, Mapping) or set(binaries) != set(
        REQUIRED_RELEASE_BINARIES
    ):
        raise EvidenceError(
            "release binary manifest entries are incomplete or unexpected"
        )
    expected_binary_fields = {
        "path",
        "size",
        "mode",
        "sha256",
        "device",
        "inode",
        "uid",
        "gid",
        "nlink",
        "mtime_ns",
        "ctime_ns",
    }
    normalized_binaries: dict[str, dict[str, Any]] = {}
    file_identities: set[tuple[int, int]] = set()
    for name in REQUIRED_RELEASE_BINARIES:
        identity = binaries[name]
        _exact_object(
            identity,
            expected_binary_fields,
            description=f"release binary identity {name}",
        )
        expected_path = directory / name
        if (
            _canonical_absolute_path(
                identity.get("path"), description=f"release binary path {name}"
            )
            != expected_path
        ):
            raise EvidenceError(f"release binary path binding is invalid: {name}")
        values = {
            field: _exact_int(
                identity.get(field),
                description=f"release binary {name}.{field}",
                maximum=(MAX_RELEASE_BINARY_BYTES if field == "size" else MAX_U64),
            )
            for field in (
                "size",
                "device",
                "inode",
                "uid",
                "gid",
                "nlink",
                "mtime_ns",
                "ctime_ns",
            )
        }
        if values["nlink"] != 1:
            raise EvidenceError(f"release binary link count is invalid: {name}")
        if (
            values["uid"] != directory_values["directory_uid"]
            or values["gid"] != directory_values["directory_gid"]
        ):
            raise EvidenceError(f"release binary owner binding is invalid: {name}")
        file_identity = (values["device"], values["inode"])
        if file_identity in file_identities:
            raise EvidenceError("release binary names must identify distinct files")
        file_identities.add(file_identity)
        mode = _exact_mode(
            identity.get("mode"), description=f"release binary mode {name}"
        )
        parsed_mode = int(mode, 8)
        if parsed_mode & 0o500 != 0o500 or parsed_mode & 0o7022:
            raise EvidenceError(f"release binary mode is unsafe: {name}")
        normalized_binaries[name] = {
            "path": str(expected_path),
            "size": values["size"],
            "mode": mode,
            "sha256": _exact_digest(
                identity.get("sha256"), description=f"release binary digest {name}"
            ),
            "device": values["device"],
            "inode": values["inode"],
            "uid": values["uid"],
            "gid": values["gid"],
            "nlink": values["nlink"],
            "mtime_ns": values["mtime_ns"],
            "ctime_ns": values["ctime_ns"],
        }
    return {
        "schema_version": 1,
        "directory": str(directory),
        **directory_values,
        "directory_mode": directory_mode,
        "required_names": list(REQUIRED_RELEASE_BINARIES),
        "binaries": normalized_binaries,
    }


def verify_release_binary_manifest(manifest: Mapping[str, Any]) -> dict[str, Any]:
    validated = validate_release_binary_manifest_schema(manifest)
    directory = pathlib.Path(validated["directory"])
    current = release_binary_manifest(
        directory, expected_owner_uid=validated["directory_uid"]
    )
    if current != validated:
        raise EvidenceError("release binary identity changed after capture")
    return current


def stage_reproduced_release_binaries(
    source_directory: pathlib.Path, destination_directory: pathlib.Path
) -> dict[str, Any]:
    """Copy the eight reproduced executables through anchored descriptors.

    Both roots stay descriptor-bound for the whole operation.  Sources must be
    unambiguous single-link regular files, destinations are created with
    ``O_EXCL|O_NOFOLLOW``, and a partial stage is removed through the already
    opened destination descriptor before an error is returned.
    """

    if not source_directory.is_absolute() or not destination_directory.is_absolute():
        raise EvidenceError("release staging roots must be absolute")
    created: dict[str, tuple[int, int]] = {}
    with _anchored_directory(
        source_directory, description="reproduced release source directory"
    ) as source_opened:
        with _anchored_directory(
            destination_directory,
            description="reproduced release destination directory",
            final_owner_uid=os.geteuid(),
        ) as destination_opened:
            destination_root = os.fstat(destination_opened.fd)
            if stat.S_IMODE(destination_root.st_mode) & 0o077:
                raise EvidenceError("release staging destination is not owner-private")
            if os.listdir(destination_opened.fd):
                raise EvidenceError("release staging destination is not empty")
            try:
                for name in REQUIRED_RELEASE_BINARIES:
                    source_path = source_directory / name
                    before = os.stat(
                        name, dir_fd=source_opened.fd, follow_symlinks=False
                    )
                    if (
                        not stat.S_ISREG(before.st_mode)
                        or stat.S_ISLNK(before.st_mode)
                        or before.st_nlink != 1
                        or before.st_size > MAX_RELEASE_BINARY_BYTES
                        or stat.S_IMODE(before.st_mode) & 0o7000
                        or stat.S_IMODE(before.st_mode) & 0o500 != 0o500
                    ):
                        raise EvidenceError(
                            f"reproduced release source is ambiguous or unsafe: {source_path}"
                        )
                    source_fd = os.open(
                        name,
                        os.O_RDONLY | _CLOEXEC | _NOFOLLOW,
                        dir_fd=source_opened.fd,
                    )
                    destination_fd = -1
                    try:
                        opened = os.fstat(source_fd)
                        if any(
                            getattr(opened, field) != getattr(before, field)
                            for field in _TREE_IDENTITY_FIELDS
                        ):
                            raise EvidenceError(
                                f"reproduced release source changed before staging: {name}"
                            )
                        destination_fd = os.open(
                            name,
                            os.O_WRONLY | os.O_CREAT | os.O_EXCL | _CLOEXEC | _NOFOLLOW,
                            0o500,
                            dir_fd=destination_opened.fd,
                        )
                        created_metadata = os.fstat(destination_fd)
                        created[name] = (
                            created_metadata.st_dev,
                            created_metadata.st_ino,
                        )
                        copied = 0
                        while True:
                            chunk = os.read(source_fd, 1024 * 1024)
                            if not chunk:
                                break
                            copied += len(chunk)
                            if copied > MAX_RELEASE_BINARY_BYTES:
                                raise EvidenceError(
                                    f"reproduced release source exceeded its bound: {name}"
                                )
                            offset = 0
                            while offset < len(chunk):
                                written = os.write(destination_fd, chunk[offset:])
                                if written <= 0:
                                    raise EvidenceError(
                                        f"reproduced release destination write stalled: {name}"
                                    )
                                offset += written
                        os.fchmod(destination_fd, 0o500)
                        os.fsync(destination_fd)
                        source_after = os.fstat(source_fd)
                        destination_after = os.fstat(destination_fd)
                        linked_source = os.stat(
                            name, dir_fd=source_opened.fd, follow_symlinks=False
                        )
                        linked_destination = os.stat(
                            name,
                            dir_fd=destination_opened.fd,
                            follow_symlinks=False,
                        )
                        if any(
                            getattr(source_after, field) != getattr(opened, field)
                            for field in _TREE_IDENTITY_FIELDS
                        ) or (linked_source.st_dev, linked_source.st_ino) != (
                            source_after.st_dev,
                            source_after.st_ino,
                        ):
                            raise EvidenceError(
                                f"reproduced release source changed during staging: {name}"
                            )
                        if (
                            not stat.S_ISREG(destination_after.st_mode)
                            or destination_after.st_nlink != 1
                            or stat.S_IMODE(destination_after.st_mode) != 0o500
                            or destination_after.st_size != opened.st_size
                            or (linked_destination.st_dev, linked_destination.st_ino)
                            != (destination_after.st_dev, destination_after.st_ino)
                        ):
                            raise EvidenceError(
                                f"reproduced release destination changed during staging: {name}"
                            )
                    finally:
                        if destination_fd >= 0:
                            os.close(destination_fd)
                        os.close(source_fd)
                if sorted(os.listdir(destination_opened.fd)) != sorted(
                    REQUIRED_RELEASE_BINARIES
                ):
                    raise EvidenceError(
                        "release staging destination membership changed"
                    )
                os.fsync(destination_opened.fd)
                _revalidate_directory_chain(
                    source_opened, description="reproduced release source directory"
                )
                _revalidate_directory_chain(
                    destination_opened,
                    description="reproduced release destination directory",
                )
            except BaseException:
                for name in reversed(created):
                    try:
                        linked = os.stat(
                            name,
                            dir_fd=destination_opened.fd,
                            follow_symlinks=False,
                        )
                    except FileNotFoundError:
                        continue
                    if (linked.st_dev, linked.st_ino) == created[name]:
                        os.unlink(name, dir_fd=destination_opened.fd)
                os.fsync(destination_opened.fd)
                raise
    return release_binary_manifest(destination_directory)


def portable_release_binary_projection(
    manifest: Mapping[str, Any],
) -> dict[str, Any]:
    """Project one path/stat identity to the bytes shared across evidence planes."""

    validated = validate_release_binary_manifest_schema(manifest)
    body = {
        "schema_version": 1,
        "kind": "pmux_portable_release_binaries",
        "required_names": list(REQUIRED_RELEASE_BINARIES),
        "comparison_fields": ["name", "mode", "size", "sha256"],
        "binaries": [
            {
                "name": name,
                "mode": validated["binaries"][name]["mode"],
                "size": validated["binaries"][name]["size"],
                "sha256": validated["binaries"][name]["sha256"],
            }
            for name in REQUIRED_RELEASE_BINARIES
        ],
    }
    return {
        **body,
        "portable_sha256": canonical_json_sha256(
            body, domain="pmux.evidence.portable-release-binaries.v1"
        ),
    }


def validate_release_reproduction_comparison(
    report: Mapping[str, Any],
    *,
    candidate_manifest: Mapping[str, Any],
    reproduced_manifest: Mapping[str, Any],
) -> dict[str, Any]:
    """Independently validate a complete candidate/reproduction comparison."""

    _exact_object(
        report,
        {
            "schema_version",
            "kind",
            "candidate_manifest_sha256",
            "reproduced_manifest_sha256",
            "required_names",
            "comparison_fields",
            "binaries",
            "verified",
            "comparison_sha256",
        },
        description="release reproduction comparison",
    )
    if (
        _exact_int(
            report.get("schema_version"),
            description="release reproduction schema version",
        )
        != 1
        or report.get("kind") != "pmux_release_reproduction_comparison"
    ):
        raise EvidenceError("release reproduction comparison schema is unsupported")
    if report.get("required_names") != list(REQUIRED_RELEASE_BINARIES) or report.get(
        "comparison_fields"
    ) != ["name", "mode", "size", "sha256", "bytes"]:
        raise EvidenceError("release reproduction comparison contract is invalid")
    candidate = validate_release_binary_manifest_schema(candidate_manifest)
    reproduced = validate_release_binary_manifest_schema(reproduced_manifest)
    candidate_digest = canonical_json_sha256(
        candidate, domain="pmux.evidence.release-binary-manifest.v1"
    )
    reproduced_digest = canonical_json_sha256(
        reproduced, domain="pmux.evidence.release-binary-manifest.v1"
    )
    if (
        _exact_digest(
            report.get("candidate_manifest_sha256"),
            description="candidate release manifest digest",
        )
        != candidate_digest
        or _exact_digest(
            report.get("reproduced_manifest_sha256"),
            description="reproduced release manifest digest",
        )
        != reproduced_digest
    ):
        raise EvidenceError("release reproduction manifest binding is invalid")
    if (
        portable_release_binary_projection(candidate)["portable_sha256"]
        != portable_release_binary_projection(reproduced)["portable_sha256"]
    ):
        raise EvidenceError("release reproduction portable identities differ")
    rows = report.get("binaries")
    if not isinstance(rows, list) or len(rows) != len(REQUIRED_RELEASE_BINARIES):
        raise EvidenceError("release reproduction rows are incomplete")
    normalized_rows: list[dict[str, Any]] = []
    for index, name in enumerate(REQUIRED_RELEASE_BINARIES):
        row = rows[index]
        _exact_object(
            row,
            {"name", "size", "mode", "sha256", "bytes_identical"},
            description=f"release reproduction row {index}",
        )
        candidate_identity = candidate["binaries"][name]
        expected_row = {
            "name": name,
            "size": candidate_identity["size"],
            "mode": candidate_identity["mode"],
            "sha256": candidate_identity["sha256"],
            "bytes_identical": True,
        }
        if dict(row) != expected_row or not _exact_bool(
            row.get("bytes_identical"),
            description=f"release reproduction bytes flag {index}",
        ):
            raise EvidenceError(f"release reproduction row is invalid: {name}")
        normalized_rows.append(expected_row)
    if not _exact_bool(
        report.get("verified"), description="release reproduction verdict"
    ):
        raise EvidenceError("release reproduction comparison did not pass")
    normalized_body = {
        "schema_version": 1,
        "kind": "pmux_release_reproduction_comparison",
        "candidate_manifest_sha256": candidate_digest,
        "reproduced_manifest_sha256": reproduced_digest,
        "required_names": list(REQUIRED_RELEASE_BINARIES),
        "comparison_fields": ["name", "mode", "size", "sha256", "bytes"],
        "binaries": normalized_rows,
        "verified": True,
    }
    expected_comparison_digest = canonical_json_sha256(
        normalized_body, domain="pmux.evidence.release-reproduction-comparison.v1"
    )
    if (
        _exact_digest(
            report.get("comparison_sha256"),
            description="release reproduction comparison digest",
        )
        != expected_comparison_digest
    ):
        raise EvidenceError("release reproduction comparison digest is invalid")
    return {**normalized_body, "comparison_sha256": expected_comparison_digest}


def compare_reproduced_release_binaries(
    candidate_manifest: Mapping[str, Any], reproduced_directory: pathlib.Path
) -> dict[str, Any]:
    """Compare a fresh release build to one stable frozen candidate.

    Same-path device/inode/stat identity remains mandatory within each plane,
    but cross-plane equivalence deliberately compares only the required names,
    exact modes, sizes, hashes, and bytes.
    """

    candidate = verify_release_binary_manifest(candidate_manifest)
    reproduced = release_binary_manifest(reproduced_directory)
    comparisons: list[dict[str, Any]] = []
    for name in REQUIRED_RELEASE_BINARIES:
        candidate_identity = candidate["binaries"][name]
        reproduced_identity = reproduced["binaries"][name]
        portable_candidate = {
            field: candidate_identity[field] for field in ("size", "mode", "sha256")
        }
        portable_reproduced = {
            field: reproduced_identity[field] for field in ("size", "mode", "sha256")
        }
        if portable_candidate != portable_reproduced:
            raise EvidenceError(
                f"reproduced release binary differs from candidate: {name}"
            )
        if candidate_identity["size"] > MAX_RELEASE_BINARY_BYTES:
            raise EvidenceError(
                f"candidate release binary exceeds the comparison bound: {name}"
            )
        candidate_bytes = _stable_regular_bytes(
            pathlib.Path(candidate_identity["path"]),
            description=f"frozen candidate release binary {name}",
            maximum_bytes=MAX_RELEASE_BINARY_BYTES,
        )
        reproduced_bytes = _stable_regular_bytes(
            pathlib.Path(reproduced_identity["path"]),
            description=f"reproduced release binary {name}",
            maximum_bytes=MAX_RELEASE_BINARY_BYTES,
        )
        if (
            len(candidate_bytes) != candidate_identity["size"]
            or hashlib.sha256(candidate_bytes).hexdigest()
            != candidate_identity["sha256"]
            or len(reproduced_bytes) != reproduced_identity["size"]
            or hashlib.sha256(reproduced_bytes).hexdigest()
            != reproduced_identity["sha256"]
            or candidate_bytes != reproduced_bytes
        ):
            raise EvidenceError(
                f"reproduced release binary bytes differ from candidate: {name}"
            )
        comparisons.append(
            {
                "name": name,
                "size": candidate_identity["size"],
                "mode": candidate_identity["mode"],
                "sha256": candidate_identity["sha256"],
                "bytes_identical": True,
            }
        )
    if verify_release_binary_manifest(candidate) != candidate:
        raise EvidenceError("frozen candidate changed during reproduction comparison")
    if verify_release_binary_manifest(reproduced) != reproduced:
        raise EvidenceError("reproduced binaries changed during comparison")
    body = {
        "schema_version": 1,
        "kind": "pmux_release_reproduction_comparison",
        "candidate_manifest_sha256": canonical_json_sha256(
            candidate, domain="pmux.evidence.release-binary-manifest.v1"
        ),
        "reproduced_manifest_sha256": canonical_json_sha256(
            reproduced, domain="pmux.evidence.release-binary-manifest.v1"
        ),
        "required_names": list(REQUIRED_RELEASE_BINARIES),
        "comparison_fields": ["name", "mode", "size", "sha256", "bytes"],
        "binaries": comparisons,
        "verified": True,
    }
    report = {
        **body,
        "comparison_sha256": canonical_json_sha256(
            body, domain="pmux.evidence.release-reproduction-comparison.v1"
        ),
    }
    return validate_release_reproduction_comparison(
        report,
        candidate_manifest=candidate,
        reproduced_manifest=reproduced,
    )


def load_json(path: pathlib.Path) -> Any:
    payload = _stable_regular_bytes(
        path,
        description="JSON evidence",
        maximum_bytes=MAX_JSON_EVIDENCE_BYTES,
    )
    return strict_json_loads(payload, description=f"JSON evidence {path}")


def read_image_iid(path: pathlib.Path) -> str:
    payload = _stable_regular_bytes(
        path, description="Buildx image-ID receipt", maximum_bytes=256
    )
    try:
        value = payload.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise EvidenceError("Buildx image-ID receipt is not ASCII") from error
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        raise EvidenceError("Buildx image-ID receipt is not one exact content ID")
    return value


def parse_builder_platforms(text: str) -> list[str]:
    """Parse Buildx's platform line without accepting wildcard drift."""

    platforms: set[str] = set()
    for line in text.splitlines():
        if not line.startswith("Platforms:"):
            continue
        for raw in line.split(":", 1)[1].split(","):
            token = raw.strip()
            if token.endswith("*"):
                token = token[:-1]
            if re.fullmatch(r"linux/(?:arm64|amd64)(?:/v[1-9][0-9]*)?", token):
                platforms.add(token)
    return sorted(platforms)


def platform_report(requested: str, inspect_text: str) -> dict[str, Any]:
    if requested not in PLATFORMS:
        raise EvidenceError(f"unsupported requested platform: {requested}")
    reported = parse_builder_platforms(inspect_text)
    return {
        "schema_version": 1,
        "requested_platform": requested,
        "reported_platforms": reported,
        "supported": requested in reported,
    }


SENSITIVE_ENVIRONMENT_PATTERNS = (
    "ANTHROPIC",
    "CLAUDE",
    "AWS_ACCESS_KEY",
    "AWS_SECRET",
    "AWS_SESSION_TOKEN",
    "BEDROCK",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "VERTEX",
    "AZURE_OPENAI",
)
SENSITIVE_HOME_PATHS = (
    ".anthropic",
    ".aws",
    ".claude",
    ".config/claude",
    ".config/gcloud",
    ".netrc",
    ".npmrc",
)


def credential_free_guard(
    *,
    home: pathlib.Path,
    path_value: str,
    environment: Mapping[str, str],
    effective_uid: int,
    effective_capabilities_hex: str,
    require_linux: bool = True,
) -> dict[str, Any]:
    if require_linux and host_platform.system() != "Linux":
        raise EvidenceError("credential-free product gates require a Linux kernel")
    if effective_uid == 0:
        raise EvidenceError("credential-free product gates must not run as root")
    try:
        capabilities = int(effective_capabilities_hex, 16)
    except ValueError as error:
        raise EvidenceError("effective capability mask is malformed") from error
    if capabilities != 0:
        raise EvidenceError(
            "credential-free product gates retained effective capabilities"
        )
    for key in environment:
        upper = key.upper()
        if any(pattern in upper for pattern in SENSITIVE_ENVIRONMENT_PATTERNS):
            raise EvidenceError(f"credential-like environment key is present: {key}")
    for relative in SENSITIVE_HOME_PATHS:
        candidate = home / relative
        if candidate.exists() or candidate.is_symlink():
            raise EvidenceError(f"credential/config path is present: {candidate}")
    cargo_home = environment.get("CARGO_HOME")
    if cargo_home:
        for name in ("credentials", "credentials.toml"):
            candidate = pathlib.Path(cargo_home) / name
            if candidate.exists() or candidate.is_symlink():
                raise EvidenceError(f"Cargo credential path is present: {candidate}")
    if shutil.which("claude", path=path_value) is not None:
        raise EvidenceError("a real Claude executable is reachable in PATH")
    return {
        "credential_free": True,
        "real_claude_available": False,
        "effective_uid": effective_uid,
        "effective_capabilities_hex": effective_capabilities_hex,
    }


def _process_status(field: str) -> str:
    prefix = f"{field}:"
    try:
        lines = (
            pathlib.Path("/proc/self/status").read_text(encoding="ascii").splitlines()
        )
    except FileNotFoundError as error:
        raise EvidenceError("Linux /proc status is unavailable") from error
    for line in lines:
        if line.startswith(prefix):
            return line.removeprefix(prefix).strip()
    raise EvidenceError(f"missing {field} in /proc/self/status")


def _command_output(*arguments: str) -> str:
    return subprocess.run(
        arguments,
        check=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=30,
    ).stdout.strip()


def validate_source_manifest_schema(
    manifest: Mapping[str, Any], *, expected_sha256: str | None = None
) -> dict[str, Any]:
    """Independently validate every portable canonical-source field."""

    _exact_object(
        manifest,
        {
            "schema_version",
            "algorithm",
            "workspace_source_sha256",
            "workspace_file_count",
            "workspace_directory_count",
            "directories",
            "files",
        },
        description="workspace source manifest",
    )
    if (
        _exact_int(manifest.get("schema_version"), description="source schema version")
        != 1
    ):
        raise EvidenceError("workspace source manifest schema version is unsupported")
    if manifest.get("algorithm") != SOURCE_ALGORITHM:
        raise EvidenceError("workspace source manifest algorithm is unsupported")
    digest = _exact_digest(
        manifest.get("workspace_source_sha256"), description="source aggregate digest"
    )
    if expected_sha256 is not None and digest != validate_expected_digest(
        expected_sha256
    ):
        raise EvidenceError("workspace source manifest differs from expected digest")
    raw_directories = manifest.get("directories")
    raw_files = manifest.get("files")
    if not isinstance(raw_directories, list) or not isinstance(raw_files, list):
        raise EvidenceError("workspace source membership lists are malformed")
    directory_count = _exact_int(
        manifest.get("workspace_directory_count"),
        description="source directory count",
        maximum=1_000_000,
    )
    file_count = _exact_int(
        manifest.get("workspace_file_count"),
        description="source file count",
        maximum=1_000_000,
    )
    if directory_count != len(raw_directories) or file_count != len(raw_files):
        raise EvidenceError("workspace source membership counts disagree")

    directories: list[dict[str, Any]] = []
    directory_paths: list[str] = []
    for index, row in enumerate(raw_directories):
        _exact_object(
            row, {"path", "mode"}, description=f"source directory row {index}"
        )
        path = _portable_relative_path(
            row.get("path"), description=f"source directory path {index}"
        )
        if pathlib.PurePosixPath(path).parts[0] not in {
            "apps",
            "clients",
            "crates",
            "fixtures",
            "fuzz",
            "scripts",
            "tests",
            "tools",
            "vendor",
        }:
            raise EvidenceError("source directory is outside the declared context")
        mode = row.get("mode")
        if not isinstance(mode, str) or re.fullmatch(r"0[0-7]{3}", mode) is None:
            raise EvidenceError("source directory mode is malformed")
        directory_paths.append(path)
        directories.append({"path": path, "mode": mode})
    if directory_paths != sorted(directory_paths) or len(directory_paths) != len(
        set(directory_paths)
    ):
        raise EvidenceError("source directories are duplicated or out of order")

    files: list[dict[str, Any]] = []
    file_paths: list[str] = []
    for index, row in enumerate(raw_files):
        _exact_object(
            row,
            {"path", "size", "mode", "sha256"},
            description=f"source file row {index}",
        )
        path = _portable_relative_path(
            row.get("path"), description=f"source file path {index}"
        )
        if not is_included(pathlib.Path(path)):
            raise EvidenceError("source file is outside the declared context")
        size = _exact_int(
            row.get("size"),
            description=f"source file size {index}",
            maximum=MAX_RELEASE_BINARY_BYTES,
        )
        mode = row.get("mode")
        if not isinstance(mode, str) or re.fullmatch(r"0[0-7]{3}", mode) is None:
            raise EvidenceError("source file mode is malformed")
        sha256 = _exact_digest(
            row.get("sha256"), description=f"source file digest {index}"
        )
        file_paths.append(path)
        files.append({"path": path, "size": size, "mode": mode, "sha256": sha256})
    if file_paths != sorted(file_paths) or len(file_paths) != len(set(file_paths)):
        raise EvidenceError("source files are duplicated or out of order")
    if set(file_paths).intersection(directory_paths):
        raise EvidenceError("source file and directory paths overlap")

    aggregate = hashlib.sha256()
    aggregate.update(b"pmux-source-v2\0")
    for row in directories:
        relative = row["path"].encode("utf-8")
        aggregate.update(b"D")
        aggregate.update(len(relative).to_bytes(4, "big"))
        aggregate.update(relative)
        aggregate.update(int(row["mode"], 8).to_bytes(4, "big"))
    for row in files:
        relative = row["path"].encode("utf-8")
        aggregate.update(b"F")
        aggregate.update(len(relative).to_bytes(4, "big"))
        aggregate.update(relative)
        aggregate.update(int(row["mode"], 8).to_bytes(4, "big"))
        aggregate.update(row["size"].to_bytes(8, "big"))
        aggregate.update(bytes.fromhex(row["sha256"]))
    if aggregate.hexdigest() != digest:
        raise EvidenceError("workspace source aggregate digest is inconsistent")
    return {
        "schema_version": 1,
        "algorithm": SOURCE_ALGORITHM,
        "workspace_source_sha256": digest,
        "workspace_file_count": file_count,
        "workspace_directory_count": directory_count,
        "directories": directories,
        "files": files,
    }


def validate_debian_snapshot_manifest(manifest: Mapping[str, Any]) -> dict[str, Any]:
    _exact_object(
        manifest,
        {"schema_version", "snapshot", "inrelease_sha256"},
        description="Debian snapshot manifest",
    )
    if (
        _exact_int(
            manifest.get("schema_version"), description="Debian snapshot schema version"
        )
        != 1
        or manifest.get("snapshot") != DEBIAN_SNAPSHOT
    ):
        raise EvidenceError("Debian snapshot identity is unsupported")
    hashes = manifest.get("inrelease_sha256")
    _exact_object(
        hashes,
        set(DEBIAN_SNAPSHOT_INRELEASE_SHA256),
        description="Debian snapshot InRelease hashes",
    )
    normalized_hashes = {
        name: _exact_digest(
            hashes.get(name), description=f"Debian snapshot digest {name}"
        )
        for name in DEBIAN_SNAPSHOT_INRELEASE_SHA256
    }
    if normalized_hashes != DEBIAN_SNAPSHOT_INRELEASE_SHA256:
        raise EvidenceError("Debian snapshot InRelease identity changed")
    return {
        "schema_version": 1,
        "snapshot": DEBIAN_SNAPSHOT,
        "inrelease_sha256": dict(DEBIAN_SNAPSHOT_INRELEASE_SHA256),
    }


def _installed_debian_snapshot_manifest(path: pathlib.Path) -> dict[str, Any]:
    payload = _stable_regular_bytes(
        path, description="installed Debian snapshot identity", maximum_bytes=4096
    )
    try:
        lines = payload.decode("ascii").splitlines()
    except UnicodeDecodeError as error:
        raise EvidenceError(
            "installed Debian snapshot identity is not ASCII"
        ) from error
    expected_keys = (
        "schema_version",
        "snapshot",
        "debian_bookworm_inrelease_sha256",
        "debian_bookworm_updates_inrelease_sha256",
        "debian_security_bookworm_inrelease_sha256",
    )
    if len(lines) != len(expected_keys):
        raise EvidenceError("installed Debian snapshot identity is incomplete")
    parsed: dict[str, str] = {}
    for expected_key, line in zip(expected_keys, lines, strict=True):
        key, separator, value = line.partition("=")
        if not separator or key != expected_key or not value or key in parsed:
            raise EvidenceError("installed Debian snapshot identity is malformed")
        parsed[key] = value
    if parsed["schema_version"] != "1":
        raise EvidenceError("installed Debian snapshot schema is unsupported")
    return validate_debian_snapshot_manifest(
        {
            "schema_version": int(parsed["schema_version"]),
            "snapshot": parsed["snapshot"],
            "inrelease_sha256": {
                "debian_bookworm": parsed["debian_bookworm_inrelease_sha256"],
                "debian_bookworm_updates": parsed[
                    "debian_bookworm_updates_inrelease_sha256"
                ],
                "debian_security_bookworm": parsed[
                    "debian_security_bookworm_inrelease_sha256"
                ],
            },
        }
    )


_RUNTIME_WORKSPACE_IDENTITY_FIELDS = (
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


def _runtime_workspace_link_target(relative: str, value: Any) -> str:
    """Validate one symlink target lexically without resolving through the host."""

    if relative == ".":
        raise EvidenceError("runtime workspace root is a symlink")
    target = _exact_text(
        value,
        description=f"runtime workspace symlink {relative}",
        maximum_bytes=16 * 1024,
        allow_empty=False,
    )
    target_path = pathlib.PurePosixPath(target)
    if target_path.is_absolute():
        raise EvidenceError("runtime workspace symlink is absolute")
    lexical: list[str] = list(pathlib.PurePosixPath(relative).parent.parts)
    for part in target_path.parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not lexical:
                raise EvidenceError("runtime workspace symlink escapes its root")
            lexical.pop()
        else:
            lexical.append(part)
    if not lexical:
        raise EvidenceError("runtime workspace symlink resolves to its root")
    return target


def _runtime_workspace_snapshot(
    root: pathlib.Path,
) -> dict[str, tuple[os.stat_result, str | None]]:
    snapshot: dict[str, tuple[os.stat_result, str | None]] = {}
    stack = [(root, ".")]
    while stack:
        current, relative = stack.pop()
        metadata = current.lstat()
        link_target: str | None = None
        if stat.S_ISLNK(metadata.st_mode):
            link_target = _runtime_workspace_link_target(relative, os.readlink(current))
        elif stat.S_ISDIR(metadata.st_mode):
            children = sorted(
                current.iterdir(), key=lambda path: path.name, reverse=True
            )
            for child in children:
                child_relative = (
                    child.name if relative == "." else f"{relative}/{child.name}"
                )
                stack.append((child, child_relative))
        elif not stat.S_ISREG(metadata.st_mode):
            raise EvidenceError(
                f"runtime workspace contains a special node: {relative}"
            )
        snapshot[relative] = (metadata, link_target)
    return snapshot


def runtime_workspace_permission_manifest(
    root: pathlib.Path, *, expected_owner_uid: int
) -> dict[str, Any]:
    """Prove every runtime workspace node is root-owned and test-UID read-only."""

    _exact_int(
        expected_owner_uid,
        description="runtime workspace expected owner",
        maximum=MAX_U64,
    )
    canonical_root = _canonical_absolute_path(
        str(root), description="runtime workspace root"
    )
    before = _runtime_workspace_snapshot(canonical_root)
    after = _runtime_workspace_snapshot(canonical_root)
    if before.keys() != after.keys():
        raise EvidenceError("runtime workspace membership changed during capture")
    records: list[dict[str, Any]] = []
    identities: set[tuple[int, int]] = set()
    directory_count = 0
    file_count = 0
    symlink_count = 0
    for relative in sorted(before):
        first, first_target = before[relative]
        second, second_target = after[relative]
        if first_target != second_target or any(
            getattr(first, field) != getattr(second, field)
            for field in _RUNTIME_WORKSPACE_IDENTITY_FIELDS
        ):
            raise EvidenceError(
                f"runtime workspace identity changed during capture: {relative}"
            )
        if first.st_uid != expected_owner_uid:
            raise EvidenceError(f"runtime workspace node has another owner: {relative}")
        mode = stat.S_IMODE(first.st_mode)
        if not stat.S_ISLNK(first.st_mode) and mode & 0o022:
            raise EvidenceError(
                f"runtime workspace node is group/world writable: {relative}"
            )
        kind: str
        if stat.S_ISDIR(first.st_mode):
            kind = "directory"
            directory_count += 1
        elif stat.S_ISREG(first.st_mode):
            kind = "file"
            file_count += 1
            if first.st_nlink != 1:
                raise EvidenceError(
                    f"runtime workspace file has a hard-link alias: {relative}"
                )
        else:
            kind = "symlink"
            symlink_count += 1
        identity = (first.st_dev, first.st_ino)
        if identity in identities:
            raise EvidenceError("runtime workspace node identity is aliased")
        identities.add(identity)
        records.append(
            {
                "path": relative,
                "kind": kind,
                "mode": f"{mode:04o}",
                "uid": first.st_uid,
                "gid": first.st_gid,
                "device": first.st_dev,
                "inode": first.st_ino,
                "nlink": first.st_nlink,
                "link_target": first_target,
            }
        )
    body = {
        "schema_version": 1,
        "root": str(canonical_root),
        "owner_uid": expected_owner_uid,
        "node_count": len(records),
        "directory_count": directory_count,
        "file_count": file_count,
        "symlink_count": symlink_count,
        "root_owned": True,
        "group_world_nonwritable": True,
        "records": records,
    }
    return {
        **body,
        "permissions_sha256": canonical_json_sha256(
            body, domain="pmux.evidence.runtime-workspace-permissions.v1"
        ),
    }


def validate_runtime_workspace_permission_manifest(
    manifest: Mapping[str, Any],
    *,
    expected_root: str,
    expected_owner_uid: int,
) -> dict[str, Any]:
    expected_root_path = _canonical_absolute_path(
        expected_root, description="expected runtime workspace permission root"
    )
    expected_owner = _exact_int(
        expected_owner_uid,
        description="expected runtime workspace permission owner",
        maximum=MAX_U64,
    )
    _exact_object(
        manifest,
        {
            "schema_version",
            "root",
            "owner_uid",
            "node_count",
            "directory_count",
            "file_count",
            "symlink_count",
            "root_owned",
            "group_world_nonwritable",
            "records",
            "permissions_sha256",
        },
        description="runtime workspace permission manifest",
    )
    if (
        _exact_int(
            manifest.get("schema_version"),
            description="runtime workspace permission schema version",
        )
        != 1
    ):
        raise EvidenceError("runtime workspace permission schema is unsupported")
    root = _canonical_absolute_path(
        manifest.get("root"), description="runtime workspace permission root"
    )
    if root != expected_root_path:
        raise EvidenceError("runtime workspace permission root is invalid")
    owner_uid = _exact_int(
        manifest.get("owner_uid"),
        description="runtime workspace permission owner",
        maximum=MAX_U64,
    )
    if owner_uid != expected_owner:
        raise EvidenceError("runtime workspace permission owner is invalid")
    if not all(
        (
            _exact_bool(
                manifest.get("root_owned"),
                description="runtime workspace root-owned verdict",
            ),
            _exact_bool(
                manifest.get("group_world_nonwritable"),
                description="runtime workspace nonwritable verdict",
            ),
        )
    ):
        raise EvidenceError("runtime workspace permission verdict did not pass")
    rows = manifest.get("records")
    if not isinstance(rows, list) or not rows:
        raise EvidenceError("runtime workspace permission records are empty")
    counts = {
        "directory": _exact_int(
            manifest.get("directory_count"),
            description="runtime workspace directory count",
            maximum=1_000_000,
        ),
        "file": _exact_int(
            manifest.get("file_count"),
            description="runtime workspace file count",
            maximum=1_000_000,
        ),
        "symlink": _exact_int(
            manifest.get("symlink_count"),
            description="runtime workspace symlink count",
            maximum=1_000_000,
        ),
    }
    if _exact_int(
        manifest.get("node_count"),
        description="runtime workspace node count",
        maximum=1_000_000,
    ) != len(rows) or sum(counts.values()) != len(rows):
        raise EvidenceError("runtime workspace permission counts disagree")
    normalized_rows: list[dict[str, Any]] = []
    paths: list[str] = []
    identities: set[tuple[int, int]] = set()
    observed_counts = {kind: 0 for kind in counts}
    for index, row in enumerate(rows):
        _exact_object(
            row,
            {
                "path",
                "kind",
                "mode",
                "uid",
                "gid",
                "device",
                "inode",
                "nlink",
                "link_target",
            },
            description=f"runtime workspace permission row {index}",
        )
        raw_path = row.get("path")
        path = (
            "."
            if raw_path == "."
            else _portable_relative_path(
                raw_path, description=f"runtime workspace permission path {index}"
            )
        )
        kind = row.get("kind")
        if kind not in observed_counts:
            raise EvidenceError("runtime workspace permission kind is invalid")
        mode = _exact_mode(
            row.get("mode"), description=f"runtime workspace mode {index}"
        )
        if kind != "symlink" and int(mode, 8) & 0o022:
            raise EvidenceError("runtime workspace permission row is writable")
        uid = _exact_int(
            row.get("uid"),
            description=f"runtime workspace uid {index}",
            maximum=MAX_U64,
        )
        if uid != owner_uid:
            raise EvidenceError("runtime workspace permission row owner differs")
        values = {
            field: _exact_int(
                row.get(field),
                description=f"runtime workspace {field} {index}",
                maximum=MAX_U64,
            )
            for field in ("gid", "device", "inode", "nlink")
        }
        if values["nlink"] < 1 or (kind == "file" and values["nlink"] != 1):
            raise EvidenceError("runtime workspace permission link count is invalid")
        link_target = row.get("link_target")
        if kind == "symlink":
            link_target = _runtime_workspace_link_target(path, link_target)
        elif link_target is not None:
            raise EvidenceError("runtime workspace non-symlink has a link target")
        identity = (values["device"], values["inode"])
        if identity in identities:
            raise EvidenceError("runtime workspace permission identity is duplicated")
        identities.add(identity)
        paths.append(path)
        observed_counts[kind] += 1
        normalized_rows.append(
            {
                "path": path,
                "kind": kind,
                "mode": mode,
                "uid": uid,
                "gid": values["gid"],
                "device": values["device"],
                "inode": values["inode"],
                "nlink": values["nlink"],
                "link_target": link_target,
            }
        )
    if paths != sorted(paths) or len(paths) != len(set(paths)) or paths[0] != ".":
        raise EvidenceError("runtime workspace permission paths are not exact")
    kind_by_path = {row["path"]: row["kind"] for row in normalized_rows}
    if kind_by_path["."] != "directory":
        raise EvidenceError("runtime workspace root record is not a directory")
    for path in paths[1:]:
        parent = pathlib.PurePosixPath(path).parent.as_posix()
        if kind_by_path.get(parent) != "directory":
            raise EvidenceError(
                "runtime workspace permission path has no exact directory parent"
            )
    if observed_counts != counts:
        raise EvidenceError("runtime workspace permission row kinds disagree")
    normalized_body = {
        "schema_version": 1,
        "root": str(root),
        "owner_uid": owner_uid,
        "node_count": len(rows),
        "directory_count": counts["directory"],
        "file_count": counts["file"],
        "symlink_count": counts["symlink"],
        "root_owned": True,
        "group_world_nonwritable": True,
        "records": normalized_rows,
    }
    digest = canonical_json_sha256(
        normalized_body, domain="pmux.evidence.runtime-workspace-permissions.v1"
    )
    if (
        _exact_digest(
            manifest.get("permissions_sha256"),
            description="runtime workspace permission digest",
        )
        != digest
    ):
        raise EvidenceError("runtime workspace permission digest is invalid")
    return {**normalized_body, "permissions_sha256": digest}


def validate_runtime_system_manifest(
    manifest: Mapping[str, Any],
    *,
    expected_source_sha256: str,
    expected_platform: str,
    expected_base_image: str,
) -> dict[str, Any]:
    expected_fields = {
        "schema_version",
        "kernel",
        "machine",
        "platform",
        "container_platform",
        "uid",
        "gid",
        "rustc",
        "cargo",
        "node",
        "python",
        "base_image",
        "installed_packages_sha256",
        "installed_packages_line_count",
        "installed_packages",
        "apt_reproducibility",
        "debian_snapshot",
        "python_requirements_sha256",
        "workspace_permissions",
        "source",
        "test_storage_filesystem",
        "real_claude_invoked",
        "credential_free",
        "real_claude_available",
        "effective_uid",
        "effective_capabilities_hex",
    }
    _exact_object(manifest, expected_fields, description="runtime system manifest")
    if (
        _exact_int(manifest.get("schema_version"), description="system schema version")
        != 1
    ):
        raise EvidenceError("runtime system manifest schema is unsupported")
    if (
        expected_platform not in PLATFORMS
        or manifest.get("container_platform") != expected_platform
    ):
        raise EvidenceError("runtime system platform binding is invalid")
    machine = _exact_text(
        manifest.get("machine"), description="runtime machine", maximum_bytes=128
    )
    expected_machine = {"linux/arm64": "aarch64", "linux/amd64": "x86_64"}[
        expected_platform
    ]
    if machine != expected_machine:
        raise EvidenceError("runtime machine disagrees with its platform")
    if manifest.get("base_image") != validate_base_image(expected_base_image):
        raise EvidenceError("runtime base-image binding is invalid")
    uid = _exact_int(manifest.get("uid"), description="runtime uid")
    gid = _exact_int(manifest.get("gid"), description="runtime gid")
    effective_uid = _exact_int(
        manifest.get("effective_uid"), description="runtime effective uid"
    )
    if uid == 0 or effective_uid != uid or gid == 0:
        raise EvidenceError("runtime identity is not one unprivileged user")
    if manifest.get("effective_capabilities_hex") != "0000000000000000":
        raise EvidenceError("runtime effective capabilities are not empty")
    for field, maximum, newline in (
        ("kernel", 4096, False),
        ("platform", 16 * 1024, False),
        ("rustc", 64 * 1024, True),
        ("cargo", 4096, False),
        ("node", 4096, False),
        ("python", 4096, False),
        ("test_storage_filesystem", 4096, False),
    ):
        _exact_text(
            manifest.get(field),
            description=f"runtime {field}",
            maximum_bytes=maximum,
            allow_newline=newline,
        )
    if manifest.get("apt_reproducibility") != (
        "snapshot_pinned_exact_inrelease_and_installed_closure"
    ):
        raise EvidenceError("runtime apt reproducibility label is invalid")
    debian_snapshot = validate_debian_snapshot_manifest(manifest.get("debian_snapshot"))
    if (
        _exact_digest(
            manifest.get("python_requirements_sha256"),
            description="runtime Python requirements digest",
        )
        != PYTHON_REQUIREMENTS_SHA256
    ):
        raise EvidenceError("runtime Python requirements identity changed")
    if not all(
        (
            _exact_bool(
                manifest.get("credential_free"), description="credential-free flag"
            ),
            not _exact_bool(
                manifest.get("real_claude_available"),
                description="real-Claude availability flag",
            ),
            not _exact_bool(
                manifest.get("real_claude_invoked"),
                description="real-Claude invocation flag",
            ),
        )
    ):
        raise EvidenceError("runtime credential/Claude guard is invalid")
    packages = manifest.get("installed_packages")
    if not isinstance(packages, list) or not packages:
        raise EvidenceError("runtime installed-package list is empty or malformed")
    package_count = _exact_int(
        manifest.get("installed_packages_line_count"),
        description="runtime package count",
        maximum=1_000_000,
    )
    if package_count != len(packages):
        raise EvidenceError("runtime installed-package count disagrees")
    normalized_packages: list[dict[str, str]] = []
    rows: list[str] = []
    for index, row in enumerate(packages):
        _exact_object(
            row, {"package", "version"}, description=f"runtime package row {index}"
        )
        package = _exact_text(
            row.get("package"),
            description=f"runtime package name {index}",
            maximum_bytes=4096,
        )
        version = _exact_text(
            row.get("version"),
            description=f"runtime package version {index}",
            maximum_bytes=4096,
        )
        if "\t" in package or "\t" in version:
            raise EvidenceError("runtime package row contains a tab")
        rows.append(f"{package}\t{version}")
        normalized_packages.append({"package": package, "version": version})
    if rows != sorted(rows) or len(rows) != len(set(rows)):
        raise EvidenceError("runtime package rows are duplicated or unsorted")
    packages_sha256 = _exact_digest(
        manifest.get("installed_packages_sha256"),
        description="installed-package digest",
    )
    reconstructed = ("\n".join(rows) + "\n").encode("utf-8")
    if hashlib.sha256(reconstructed).hexdigest() != packages_sha256:
        raise EvidenceError("installed-package digest is inconsistent")
    source = validate_source_manifest_schema(
        manifest.get("source"), expected_sha256=expected_source_sha256
    )
    workspace_permissions = validate_runtime_workspace_permission_manifest(
        manifest.get("workspace_permissions"),
        expected_root="/workspace",
        expected_owner_uid=0,
    )
    requirements_rows = [
        row
        for row in source["files"]
        if row["path"] == "tools/linux-docker/python-requirements.txt"
    ]
    if (
        len(requirements_rows) != 1
        or requirements_rows[0]["sha256"] != PYTHON_REQUIREMENTS_SHA256
    ):
        raise EvidenceError("source does not bind the Python requirements identity")
    return {
        **dict(manifest),
        "uid": uid,
        "gid": gid,
        "effective_uid": effective_uid,
        "installed_packages_line_count": package_count,
        "installed_packages": normalized_packages,
        "debian_snapshot": debian_snapshot,
        "workspace_permissions": workspace_permissions,
        "source": source,
    }


def runtime_system_manifest(
    workspace: pathlib.Path,
    expected_source_sha256: str,
    expected_platform: str,
) -> dict[str, Any]:
    expected = validate_expected_digest(expected_source_sha256)
    if expected_platform not in PLATFORMS:
        raise EvidenceError("container platform is outside the declared matrix")
    source = workspace_source_manifest(workspace)
    if source["workspace_source_sha256"] != expected:
        raise EvidenceError("container source does not match the frozen host digest")
    workspace_permissions = runtime_workspace_permission_manifest(
        workspace, expected_owner_uid=0
    )
    source_after_permissions = workspace_source_manifest(workspace)
    if source_after_permissions != source:
        raise EvidenceError("container source changed during permission capture")
    machine = host_platform.machine()
    observed_platform = {"aarch64": "linux/arm64", "x86_64": "linux/amd64"}.get(machine)
    if observed_platform != expected_platform:
        raise EvidenceError(
            f"container architecture mismatch: expected {expected_platform}, observed {machine}"
        )
    capabilities = _process_status("CapEff")
    guard = credential_free_guard(
        home=pathlib.Path(os.environ["HOME"]),
        path_value=os.environ.get("PATH", ""),
        environment=os.environ,
        effective_uid=os.geteuid(),
        effective_capabilities_hex=capabilities,
    )
    base_image = validate_base_image(os.environ.get("PMUX_BASE_IMAGE_REF", ""))
    package_manifest_path = pathlib.Path("/opt/pmux-system-packages.tsv")
    package_manifest = _stable_regular_bytes(
        package_manifest_path,
        description="installed-package identity",
        maximum_bytes=16 * 1024 * 1024,
    )
    try:
        package_lines = package_manifest.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise EvidenceError("installed-package identity is not UTF-8") from error
    if (
        not package_lines
        or package_lines != sorted(package_lines)
        or len(package_lines) != len(set(package_lines))
    ):
        raise EvidenceError(
            "installed-package identity is empty, duplicated, or unsorted"
        )
    installed_packages: list[dict[str, str]] = []
    for line in package_lines:
        fields = line.split("\t")
        if len(fields) != 2 or not fields[0] or not fields[1]:
            raise EvidenceError("installed-package identity contains a malformed row")
        installed_packages.append({"package": fields[0], "version": fields[1]})
    debian_snapshot = _installed_debian_snapshot_manifest(
        pathlib.Path("/opt/pmux-debian-snapshot.txt")
    )
    requirements_path = workspace / "tools/linux-docker/python-requirements.txt"
    requirements_sha256 = hashlib.sha256(
        _stable_regular_bytes(
            requirements_path,
            description="runtime Python requirements",
            maximum_bytes=1024 * 1024,
        )
    ).hexdigest()
    if requirements_sha256 != PYTHON_REQUIREMENTS_SHA256:
        raise EvidenceError("runtime Python requirements identity changed")
    return {
        "schema_version": 1,
        "kernel": host_platform.release(),
        "machine": machine,
        "platform": host_platform.platform(),
        "container_platform": observed_platform,
        "uid": os.geteuid(),
        "gid": os.getegid(),
        "rustc": _command_output("rustc", "--version", "--verbose"),
        "cargo": _command_output("cargo", "--version"),
        "node": _command_output("node", "--version"),
        "python": host_platform.python_version(),
        "base_image": base_image,
        "installed_packages_sha256": hashlib.sha256(package_manifest).hexdigest(),
        "installed_packages_line_count": len(package_lines),
        "installed_packages": installed_packages,
        "apt_reproducibility": (
            "snapshot_pinned_exact_inrelease_and_installed_closure"
        ),
        "debian_snapshot": debian_snapshot,
        "python_requirements_sha256": requirements_sha256,
        "workspace_permissions": workspace_permissions,
        "source": source,
        "test_storage_filesystem": _command_output(
            "stat", "-f", "-c", "%T", "/var/tmp/pmux-linux-suite"
        ),
        "real_claude_invoked": False,
        **guard,
    }


def compare_source_manifests(
    before: Mapping[str, Any], after: Mapping[str, Any], expected: str
) -> dict[str, Any]:
    normalized = validate_expected_digest(expected)
    before_validated = validate_source_manifest_schema(
        before, expected_sha256=normalized
    )
    after_validated = validate_source_manifest_schema(after, expected_sha256=normalized)
    before_digest = before_validated["workspace_source_sha256"]
    after_digest = after_validated["workspace_source_sha256"]
    verified = before_validated == after_validated
    return {
        "schema_version": 1,
        "verified": verified,
        "expected_source_sha256": normalized,
        "before_source_sha256": before_digest,
        "after_source_sha256": after_digest,
        "manifests_identical": before_validated == after_validated,
    }


def compare_workspace_revision_captures(
    before: Mapping[str, Any], after: Mapping[str, Any]
) -> dict[str, Any]:
    try:
        before_validated = source_identity.validate_workspace_revision_capture(before)
        after_validated = source_identity.validate_workspace_revision_capture(after)
    except source_identity.SourceIdentityError as error:
        raise EvidenceError("workspace revision causal capture is invalid") from error
    if before_validated["identity"] != after_validated["identity"]:
        raise EvidenceError("workspace revision identity changed during validation")
    identity = before_validated["identity"]
    return {
        "schema_version": 1,
        "kind": "pmux_workspace_revision_stability",
        "verified": True,
        "identity_sha256": canonical_json_sha256(
            identity, domain="pmux.evidence.workspace-revision-identity.v1"
        ),
        "before_capture_sha256": before_validated["capture_sha256"],
        "after_capture_sha256": after_validated["capture_sha256"],
        "captures_are_causal_not_equality_stable": True,
    }


def validate_workspace_revision_stability(value: Mapping[str, Any]) -> dict[str, Any]:
    fields = {
        "schema_version",
        "kind",
        "verified",
        "identity_sha256",
        "before_capture_sha256",
        "after_capture_sha256",
        "captures_are_causal_not_equality_stable",
    }
    _exact_object(value, fields, description="workspace revision stability")
    if (
        _exact_int(value.get("schema_version"), description="revision stability schema")
        != 1
        or value.get("kind") != "pmux_workspace_revision_stability"
        or not _exact_bool(
            value.get("verified"), description="revision stability verdict"
        )
        or not _exact_bool(
            value.get("captures_are_causal_not_equality_stable"),
            description="revision causal-capture flag",
        )
    ):
        raise EvidenceError("workspace revision stability contract is invalid")
    for field in (
        "identity_sha256",
        "before_capture_sha256",
        "after_capture_sha256",
    ):
        _exact_digest(value.get(field), description=f"revision stability {field}")
    return dict(value)


def verify_cell_binding(
    *,
    host_source: Mapping[str, Any],
    host_revision_before: Mapping[str, Any],
    host_revision_after: Mapping[str, Any],
    host_revision_stability: Mapping[str, Any],
    container_system: Mapping[str, Any],
    image_binaries: Mapping[str, Any],
    binaries_before: Mapping[str, Any],
    binaries_after: Mapping[str, Any],
    reproduced_binaries: Mapping[str, Any],
    reproduction_comparison: Mapping[str, Any],
    uds_binding: Mapping[str, Any],
    suite_result: Mapping[str, Any],
    gate_manifest: Mapping[str, Any],
    expected_source_sha256: str,
    expected_platform: str,
    expected_base_image: str,
) -> dict[str, Any]:
    expected = validate_expected_digest(expected_source_sha256)
    gate_manifest_validated = validate_platform_gate_manifest(
        gate_manifest, expected_platform=expected_platform
    )
    host_source_validated = validate_source_manifest_schema(
        host_source, expected_sha256=expected
    )
    revision_stability_validated = validate_workspace_revision_stability(
        host_revision_stability
    )
    revision_stability_observed = compare_workspace_revision_captures(
        host_revision_before, host_revision_after
    )
    if revision_stability_validated != revision_stability_observed:
        raise EvidenceError(
            "workspace revision stability disagrees with its causal captures"
        )
    system_validated = validate_runtime_system_manifest(
        container_system,
        expected_source_sha256=expected,
        expected_platform=expected_platform,
        expected_base_image=expected_base_image,
    )
    image_validated = validate_release_binary_manifest_schema(image_binaries)
    before_validated = validate_release_binary_manifest_schema(binaries_before)
    after_validated = validate_release_binary_manifest_schema(binaries_after)
    reproduced_validated = validate_release_binary_manifest_schema(reproduced_binaries)
    comparison_validated = validate_release_reproduction_comparison(
        reproduction_comparison,
        candidate_manifest=before_validated,
        reproduced_manifest=reproduced_validated,
    )
    suite_validated = validate_suite_result(suite_result, gate_manifest_validated)
    uds_expected_fields = {
        "schema_version",
        "verified",
        "release_binary_manifest_sha256",
        "uds_report_sha256",
        "owner_receipt_sha256",
        "intruder_receipt_sha256",
        "candidate_write_receipt_sha256",
        "outer_probe_receipt_sha256",
        "server_version",
    }
    _exact_object(uds_binding, uds_expected_fields, description="UDS binary binding")
    if _exact_int(
        uds_binding.get("schema_version"), description="UDS binding schema"
    ) != 1 or not _exact_bool(
        uds_binding.get("verified"), description="UDS binding verdict"
    ):
        raise EvidenceError("UDS binary binding did not verify")
    for field in uds_expected_fields - {"schema_version", "verified", "server_version"}:
        _exact_digest(uds_binding.get(field), description=f"UDS binding {field}")
    _exact_text(
        uds_binding.get("server_version"),
        description="UDS binding server version",
        maximum_bytes=1024,
    )
    candidate_manifest_sha256 = canonical_json_sha256(
        image_validated, domain="pmux.evidence.release-binary-manifest.v1"
    )
    source_matches = host_source_validated == system_validated["source"]
    binary_matches = image_validated == before_validated == after_validated
    reproduction_matches = comparison_validated["verified"] is True
    uds_matches = (
        uds_binding["release_binary_manifest_sha256"] == candidate_manifest_sha256
    )
    gate_manifest_matches = (
        suite_validated["failure_count"] == 0
        and suite_validated["status"] == "pass"
        and suite_validated["all_gate_evidence_verified"] is True
    )
    verified = all(
        (
            host_source_validated["workspace_source_sha256"] == expected,
            source_matches,
            revision_stability_validated["verified"] is True,
            binary_matches,
            reproduction_matches,
            uds_matches,
            system_validated["container_platform"] == expected_platform,
            system_validated["base_image"] == expected_base_image,
            system_validated["credential_free"] is True,
            system_validated["real_claude_available"] is False,
            system_validated["real_claude_invoked"] is False,
            gate_manifest_matches,
        )
    )
    return {
        "schema_version": 1,
        "verified": verified,
        "expected_source_sha256": expected,
        "expected_platform": expected_platform,
        "expected_base_image": expected_base_image,
        "host_container_source_identical": source_matches,
        "host_revision_identity_stable": revision_stability_validated["verified"],
        "host_revision_identity_sha256": revision_stability_validated[
            "identity_sha256"
        ],
        "host_revision_before_capture_sha256": revision_stability_validated[
            "before_capture_sha256"
        ],
        "host_revision_after_capture_sha256": revision_stability_validated[
            "after_capture_sha256"
        ],
        "release_binaries_unchanged": binary_matches,
        "release_binary_set_complete": image_validated["required_names"]
        == list(REQUIRED_RELEASE_BINARIES),
        "reproduced_release_binaries_identical": reproduction_matches,
        "uds_candidate_binding_verified": uds_matches,
        "candidate_manifest_sha256": candidate_manifest_sha256,
        "reproduction_comparison_sha256": comparison_validated["comparison_sha256"],
        "gate_evidence_tail_sha256": suite_validated["gate_evidence_tail_sha256"],
        "suite_status": suite_validated["status"],
        "gate_manifest_verified": gate_manifest_matches,
    }


@dataclass(frozen=True)
class DockerResourceIdentity:
    kind: str
    name: str
    object_id: str


def validate_docker_resource(
    identity: DockerResourceIdentity,
) -> DockerResourceIdentity:
    if identity.kind == "builder":
        if not re.fullmatch(
            r"pmux-linux-builder-(?:arm64|amd64)-[a-z0-9-]+", identity.name
        ):
            raise EvidenceError("builder name is outside the exact pmux run namespace")
        if not re.fullmatch(r"[0-9a-f]{64}", identity.object_id):
            raise EvidenceError("builder identity must be an inspect-record SHA-256")
    elif identity.kind == "container":
        if not re.fullmatch(r"pmux-linux-(?:arm64|amd64)-[a-z0-9-]+", identity.name):
            raise EvidenceError(
                "container name is outside the exact pmux run namespace"
            )
        if not re.fullmatch(r"[0-9a-f]{12,64}", identity.object_id):
            raise EvidenceError("container identity is not an exact Docker object ID")
    elif identity.kind == "image":
        if not re.fullmatch(
            r"pmux-linux-deterministic:(?:arm64|amd64)-[a-z0-9-]+", identity.name
        ):
            raise EvidenceError("image tag is outside the exact pmux run namespace")
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", identity.object_id):
            raise EvidenceError("image identity is not an exact content ID")
    else:
        raise EvidenceError(f"unsupported Docker resource kind: {identity.kind}")
    return identity


def cleanup_plan(identity: DockerResourceIdentity) -> tuple[str, ...]:
    exact = validate_docker_resource(identity)
    if exact.kind == "builder":
        return ("docker", "buildx", "rm", "--force", exact.name)
    if exact.kind == "container":
        return ("docker", "rm", "--force", exact.object_id)
    return ("docker", "image", "rm", exact.object_id)


def validate_planned_docker_resource(kind: str, name: str) -> None:
    placeholders = {
        "builder": "0" * 64,
        "container": "0" * 64,
        "image": "sha256:" + "0" * 64,
    }
    if kind not in placeholders:
        raise EvidenceError(f"unsupported Docker resource kind: {kind}")
    validate_docker_resource(DockerResourceIdentity(kind, name, placeholders[kind]))


def validate_base_image(value: str) -> str:
    if not re.fullmatch(
        r"docker\.io/library/rust:1\.88\.0-bookworm@sha256:[0-9a-f]{64}", value
    ):
        raise EvidenceError(
            "base image must be docker.io/library/rust:1.88.0-bookworm at one "
            "exact lowercase multiarch sha256 digest"
        )
    return value


def docker_transport_identity(socket_path: pathlib.Path) -> dict[str, Any]:
    """Bind one local Docker Unix transport without following path aliases."""

    if not socket_path.is_absolute() or str(socket_path) != os.path.normpath(
        str(socket_path)
    ):
        raise EvidenceError("Docker socket path is not canonical and absolute")
    with _anchored_directory(
        socket_path.parent, description="Docker socket parent"
    ) as opened:
        before = os.stat(socket_path.name, dir_fd=opened.fd, follow_symlinks=False)
        if not stat.S_ISSOCK(before.st_mode):
            raise EvidenceError("Docker transport is not one real Unix socket")
        if stat.S_IMODE(before.st_mode) & 0o7000:
            raise EvidenceError("Docker socket has unsupported special mode bits")
        _revalidate_directory_chain(opened, description="Docker socket parent")
        after = os.stat(socket_path.name, dir_fd=opened.fd, follow_symlinks=False)
        fields = ("st_dev", "st_ino", "st_uid", "st_gid", "st_mode")
        if any(getattr(before, field) != getattr(after, field) for field in fields):
            raise EvidenceError("Docker socket identity changed during capture")
        parent = os.fstat(opened.fd)
    return {
        "schema_version": 1,
        "kind": "pmux_local_docker_transport",
        "docker_host": f"unix://{socket_path}",
        "socket_path": str(socket_path),
        "socket_device": before.st_dev,
        "socket_inode": before.st_ino,
        "socket_uid": before.st_uid,
        "socket_gid": before.st_gid,
        "socket_mode": f"{stat.S_IMODE(before.st_mode):04o}",
        "parent_device": parent.st_dev,
        "parent_inode": parent.st_ino,
        "parent_uid": parent.st_uid,
        "parent_gid": parent.st_gid,
        "parent_mode": f"{stat.S_IMODE(parent.st_mode):04o}",
    }


def compare_docker_transport_identities(
    before: Mapping[str, Any], after: Mapping[str, Any]
) -> dict[str, Any]:
    expected_fields = {
        "schema_version",
        "kind",
        "docker_host",
        "socket_path",
        "socket_device",
        "socket_inode",
        "socket_uid",
        "socket_gid",
        "socket_mode",
        "parent_device",
        "parent_inode",
        "parent_uid",
        "parent_gid",
        "parent_mode",
    }
    for value in (before, after):
        _exact_object(value, expected_fields, description="Docker transport identity")
        if (
            _exact_int(
                value.get("schema_version"), description="Docker transport schema"
            )
            != 1
            or value.get("kind") != "pmux_local_docker_transport"
        ):
            raise EvidenceError("Docker transport identity schema is invalid")
        socket = pathlib.Path(
            _canonical_absolute_path(
                value.get("socket_path"), description="Docker socket path"
            )
        )
        if value.get("docker_host") != f"unix://{socket}":
            raise EvidenceError("Docker host and socket path disagree")
        for field in (
            "socket_device",
            "socket_inode",
            "socket_uid",
            "socket_gid",
            "parent_device",
            "parent_inode",
            "parent_uid",
            "parent_gid",
        ):
            _exact_int(value.get(field), description=f"Docker transport {field}")
        _exact_mode(value.get("socket_mode"), description="Docker socket mode")
        _exact_mode(value.get("parent_mode"), description="Docker socket parent mode")
    if before != after:
        raise EvidenceError("Docker transport identity changed during validation")
    return {
        "schema_version": 1,
        "kind": "pmux_local_docker_transport_stability",
        "verified": True,
        "transport_sha256": canonical_json_sha256(
            before, domain="pmux.evidence.local-docker-transport.v1"
        ),
    }


def docker_control_plane_report(
    *,
    workspace: pathlib.Path,
    docker_version_receipt: pathlib.Path,
    docker_version_stdout: pathlib.Path,
    docker_version_stderr: pathlib.Path,
    buildx_version_receipt: pathlib.Path,
    buildx_version_stdout: pathlib.Path,
    buildx_version_stderr: pathlib.Path,
    plugin_inventory_receipt: pathlib.Path,
    plugin_inventory_stdout: pathlib.Path,
    plugin_inventory_stderr: pathlib.Path,
    transport_identity: Mapping[str, Any],
) -> dict[str, Any]:
    """Bind Docker, Buildx's exact plugin executable, and the local UDS."""

    canonical_workspace = workspace.resolve(strict=True)
    receipts = [
        load_retained_process_receipt(
            docker_version_receipt, docker_version_stdout, docker_version_stderr
        ),
        load_retained_process_receipt(
            buildx_version_receipt, buildx_version_stdout, buildx_version_stderr
        ),
        load_retained_process_receipt(
            plugin_inventory_receipt,
            plugin_inventory_stdout,
            plugin_inventory_stderr,
        ),
    ]
    if any(
        receipt["kind"] != "pmux_bounded_process"
        or receipt["exit_code"] != 0
        or receipt["cwd"] != str(canonical_workspace)
        or receipt["stderr_size"] != 0
        for receipt in receipts
    ):
        raise EvidenceError("Docker control-plane command did not succeed exactly")
    executable = receipts[0]["executable"]
    environment = receipts[0]["environment"]
    if any(
        receipt["executable"] != executable or receipt["environment"] != environment
        for receipt in receipts[1:]
    ):
        raise EvidenceError("Docker executable or environment changed between probes")
    docker_path = executable["path"]
    expected_argv = [
        [docker_path, "version"],
        [docker_path, "buildx", "version"],
        [
            docker_path,
            "info",
            "--format",
            "{{json .ClientInfo.Plugins}}",
        ],
    ]
    if [receipt["argv"] for receipt in receipts] != expected_argv:
        raise EvidenceError("Docker control-plane command argv was substituted")
    version_payload = _stable_regular_bytes(
        docker_version_stdout,
        description="Docker version output",
        maximum_bytes=1024 * 1024,
    )
    buildx_payload = _stable_regular_bytes(
        buildx_version_stdout,
        description="Buildx version output",
        maximum_bytes=1024 * 1024,
    )
    plugin_payload = _stable_regular_bytes(
        plugin_inventory_stdout,
        description="Docker client plugin inventory",
        maximum_bytes=8 * 1024 * 1024,
    )
    if not version_payload.strip() or not buildx_payload.strip():
        raise EvidenceError("Docker or Buildx version output is empty")
    plugins = strict_json_loads(plugin_payload, description="Docker client plugins")
    if not isinstance(plugins, list):
        raise EvidenceError("Docker client plugin inventory is not a list")
    buildx_rows = []
    for row in plugins:
        if not isinstance(row, Mapping):
            raise EvidenceError("Docker client plugin row is not an object")
        name = row.get("Name", row.get("name"))
        if name == "buildx":
            buildx_rows.append(row)
    if len(buildx_rows) != 1:
        raise EvidenceError(
            "Docker client plugin inventory does not identify one Buildx"
        )
    buildx_row = buildx_rows[0]
    path_value = buildx_row.get("Path", buildx_row.get("path"))
    plugin_path = _canonical_absolute_path(
        path_value, description="Buildx plugin executable path"
    )
    plugin_identity = _stable_regular_identity(plugin_path)
    if int(plugin_identity["mode"], 8) & 0o111 == 0:
        raise EvidenceError("Buildx plugin is not executable")
    transport = compare_docker_transport_identities(
        transport_identity, transport_identity
    )
    normalized_plugin_metadata = strict_json_loads(
        json.dumps(
            buildx_row,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8"),
        description="Buildx plugin metadata",
    )
    body = {
        "schema_version": 1,
        "kind": "pmux_docker_buildx_control_plane",
        "docker_executable": executable,
        "docker_environment": environment,
        "docker_version_stdout_sha256": hashlib.sha256(version_payload).hexdigest(),
        "docker_version_stdout_bytes": len(version_payload),
        "buildx_version_stdout_sha256": hashlib.sha256(buildx_payload).hexdigest(),
        "buildx_version_stdout_bytes": len(buildx_payload),
        "plugin_inventory_sha256": hashlib.sha256(plugin_payload).hexdigest(),
        "plugin_inventory_bytes": len(plugin_payload),
        "buildx_plugin_metadata": normalized_plugin_metadata,
        "buildx_plugin_executable": plugin_identity,
        "transport_sha256": transport["transport_sha256"],
        "command_receipt_sha256": [receipt["receipt_sha256"] for receipt in receipts],
        "verified": True,
    }
    return {
        **body,
        "control_plane_sha256": canonical_json_sha256(
            body, domain="pmux.evidence.docker-buildx-control-plane.v1"
        ),
    }


def verify_base_image_index(
    reference: str, raw_manifest_path: pathlib.Path
) -> dict[str, Any]:
    exact_reference = validate_base_image(reference)
    expected_digest = exact_reference.rsplit("@sha256:", 1)[1]
    raw = _stable_regular_bytes(
        raw_manifest_path,
        description="base-image index manifest",
        maximum_bytes=16 * 1024 * 1024,
    )
    manifest_bytes = raw
    observed_digest = hashlib.sha256(manifest_bytes).hexdigest()
    stripped_cli_newline = False
    if observed_digest != expected_digest and manifest_bytes.endswith(b"\n"):
        without_newline = manifest_bytes[:-1]
        if hashlib.sha256(without_newline).hexdigest() == expected_digest:
            manifest_bytes = without_newline
            observed_digest = expected_digest
            stripped_cli_newline = True
    if observed_digest != expected_digest:
        raise EvidenceError("base-image index bytes do not match the requested digest")
    manifest = strict_json_loads(manifest_bytes, description="base-image index")
    if not isinstance(manifest, Mapping):
        raise EvidenceError("base-image index is not an object")
    required_index_fields = {"schemaVersion", "mediaType", "manifests"}
    optional_index_fields = {"annotations", "artifactType", "subject"}
    if not required_index_fields.issubset(manifest) or not set(manifest).issubset(
        required_index_fields | optional_index_fields
    ):
        raise EvidenceError("base-image index fields are unsupported")
    if _exact_int(
        manifest.get("schemaVersion"), description="base-image schema version"
    ) != 2 or manifest.get("mediaType") not in {
        "application/vnd.docker.distribution.manifest.list.v2+json",
        "application/vnd.oci.image.index.v1+json",
    }:
        raise EvidenceError("base-image digest does not identify a multiarch index")
    annotations = manifest.get("annotations")
    if annotations is not None:
        if not isinstance(annotations, Mapping):
            raise EvidenceError("base-image index annotations are malformed")
        for key, value in annotations.items():
            _exact_text(
                key, description="base-image annotation key", maximum_bytes=4096
            )
            _exact_text(
                value,
                description="base-image annotation value",
                maximum_bytes=64 * 1024,
                allow_empty=True,
                allow_newline=True,
            )
    if "artifactType" in manifest:
        _exact_text(
            manifest.get("artifactType"),
            description="base-image artifact type",
            maximum_bytes=4096,
        )
    if "subject" in manifest:
        raise EvidenceError("base-image index subject descriptors are unsupported")
    rows = manifest.get("manifests")
    if not isinstance(rows, list) or not rows:
        raise EvidenceError("base-image index has no manifest list")
    required_counts = {"linux/arm64": 0, "linux/amd64": 0}
    required_descriptors: list[dict[str, Any]] = []
    descriptor_digests: set[str] = set()
    descriptor_required = {"mediaType", "digest", "size", "platform"}
    descriptor_optional = {"annotations", "artifactType", "data", "urls"}
    for index, row in enumerate(rows):
        if (
            not isinstance(row, Mapping)
            or not descriptor_required.issubset(row)
            or not set(row).issubset(descriptor_required | descriptor_optional)
        ):
            raise EvidenceError(
                f"base-image index descriptor fields are malformed: {index}"
            )
        media_type = _exact_text(
            row.get("mediaType"),
            description=f"base-image descriptor media type {index}",
            maximum_bytes=4096,
        )
        digest = row.get("digest")
        if (
            not isinstance(digest, str)
            or re.fullmatch(r"sha256:[0-9a-f]{64}", digest) is None
        ):
            raise EvidenceError(f"base-image descriptor digest is invalid: {index}")
        if digest in descriptor_digests:
            raise EvidenceError("base-image descriptor digest is duplicated")
        descriptor_digests.add(digest)
        size = _exact_int(
            row.get("size"),
            description=f"base-image descriptor size {index}",
            maximum=MAX_RELEASE_BINARY_BYTES,
        )
        for optional_text in ("artifactType", "data"):
            if optional_text in row:
                _exact_text(
                    row.get(optional_text),
                    description=f"base-image descriptor {optional_text} {index}",
                    maximum_bytes=16 * 1024 * 1024,
                    allow_empty=True,
                    allow_newline=True,
                )
        urls = row.get("urls")
        if urls is not None:
            if not isinstance(urls, list):
                raise EvidenceError("base-image descriptor URLs are malformed")
            for url in urls:
                _exact_text(
                    url,
                    description="base-image descriptor URL",
                    maximum_bytes=16 * 1024,
                )
        row_annotations = row.get("annotations")
        if row_annotations is not None:
            if not isinstance(row_annotations, Mapping):
                raise EvidenceError("base-image descriptor annotations are malformed")
            for key, value in row_annotations.items():
                _exact_text(
                    key,
                    description="base-image descriptor annotation key",
                    maximum_bytes=4096,
                )
                _exact_text(
                    value,
                    description="base-image descriptor annotation value",
                    maximum_bytes=64 * 1024,
                    allow_empty=True,
                    allow_newline=True,
                )
        platform = row.get("platform")
        platform_required = {"os", "architecture"}
        platform_optional = {"variant", "os.version", "os.features", "features"}
        if (
            not isinstance(platform, Mapping)
            or not platform_required.issubset(platform)
            or not set(platform).issubset(platform_required | platform_optional)
        ):
            raise EvidenceError(f"base-image descriptor platform is malformed: {index}")
        operating_system = _exact_text(
            platform.get("os"),
            description=f"base-image descriptor OS {index}",
            maximum_bytes=128,
        )
        architecture = _exact_text(
            platform.get("architecture"),
            description=f"base-image descriptor architecture {index}",
            maximum_bytes=128,
        )
        variant = platform.get("variant")
        if variant is not None:
            variant = _exact_text(
                variant,
                description=f"base-image descriptor variant {index}",
                maximum_bytes=128,
                allow_empty=True,
            )
        for feature_field in ("os.features", "features"):
            features = platform.get(feature_field)
            if features is not None:
                if not isinstance(features, list):
                    raise EvidenceError("base-image platform features are malformed")
                for feature in features:
                    _exact_text(
                        feature,
                        description="base-image platform feature",
                        maximum_bytes=1024,
                    )
        if "os.version" in platform:
            _exact_text(
                platform.get("os.version"),
                description=f"base-image descriptor OS version {index}",
                maximum_bytes=4096,
            )
        name = f"{operating_system}/{architecture}"
        if name in required_counts and variant in {None, "", "v8"}:
            required_counts[name] += 1
            required_descriptors.append(
                {
                    "platform": name,
                    "variant": variant,
                    "media_type": media_type,
                    "digest": digest,
                    "size": size,
                }
            )
    if required_counts != {"linux/arm64": 1, "linux/amd64": 1}:
        raise EvidenceError(
            "base-image index must contain exactly one Linux arm64 and amd64 candidate"
        )
    required_descriptors.sort(key=lambda row: row["platform"])
    return {
        "schema_version": 1,
        "reference": exact_reference,
        "index_sha256": observed_digest,
        "capture_sha256": hashlib.sha256(raw).hexdigest(),
        "stripped_cli_newline": stripped_cli_newline,
        "media_type": manifest["mediaType"],
        "manifest_count": len(rows),
        "required_platforms": list(PLATFORMS),
        "required_platform_counts": required_counts,
        "required_platform_descriptors": required_descriptors,
        "verified": True,
    }


def validate_declared_gate_manifest(declared: Mapping[str, Any]) -> dict[str, Any]:
    """Validate the tracked platform-neutral gate declaration exactly."""

    _exact_object(
        declared,
        {"schema_version", "platforms", "gates"},
        description="declared Linux gate manifest",
    )
    if (
        _exact_int(
            declared.get("schema_version"), description="declared gate schema version"
        )
        != 1
    ):
        raise EvidenceError("gate manifest schema is unsupported")
    if declared.get("platforms") != list(PLATFORMS):
        raise EvidenceError("gate manifest does not name the exact Linux matrix")
    raw_gates = declared.get("gates")
    if not isinstance(raw_gates, list) or not raw_gates:
        raise EvidenceError("gate manifest must contain at least one gate")
    gates: list[dict[str, Any]] = []
    names: set[str] = set()
    phase_order = {
        name: index
        for index, name in enumerate(("P", "A", "B", "C", "D", "E", "F", "Z"))
    }
    last_phase = -1
    for ordinal, raw in enumerate(raw_gates, start=1):
        if not isinstance(raw, dict) or set(raw) != {"name", "phase"}:
            raise EvidenceError("gate manifest rows must contain only name and phase")
        name = raw.get("name")
        phase = raw.get("phase")
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9_]+", name):
            raise EvidenceError("gate manifest contains an invalid name")
        if name in names:
            raise EvidenceError(f"gate manifest contains a duplicate name: {name}")
        if not isinstance(phase, str) or phase not in phase_order:
            raise EvidenceError(f"gate manifest contains an invalid phase: {phase}")
        if phase_order[phase] < last_phase:
            raise EvidenceError("gate manifest phases are out of order")
        last_phase = phase_order[phase]
        names.add(name)
        gates.append({"phase": phase, "name": name})
    return {
        "schema_version": 1,
        "platforms": list(PLATFORMS),
        "gates": gates,
    }


def validate_platform_gate_manifest(
    manifest: Mapping[str, Any],
    *,
    expected_platform: str | None = None,
    declared: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Validate one platform expansion and optionally bind its declaration."""

    _exact_object(
        manifest,
        {
            "schema_version",
            "platform",
            "gate_count",
            "gates",
            "declared_manifest_sha256",
        },
        description="platform gate manifest",
    )
    if (
        _exact_int(
            manifest.get("schema_version"), description="platform gate schema version"
        )
        != 1
    ):
        raise EvidenceError("platform gate manifest schema is unsupported")
    platform = manifest.get("platform")
    if platform not in PLATFORMS or (
        expected_platform is not None and platform != expected_platform
    ):
        raise EvidenceError("platform gate manifest platform is invalid")
    raw_gates = manifest.get("gates")
    if not isinstance(raw_gates, list) or not raw_gates:
        raise EvidenceError("platform gate manifest is empty or malformed")
    gate_count = _exact_int(
        manifest.get("gate_count"),
        description="platform gate count",
        maximum=100_000,
    )
    if gate_count != len(raw_gates):
        raise EvidenceError("platform gate count disagrees with its rows")
    normalized_gates: list[dict[str, Any]] = []
    names: set[str] = set()
    phase_order = {
        name: index
        for index, name in enumerate(("P", "A", "B", "C", "D", "E", "F", "Z"))
    }
    last_phase = -1
    for index, row in enumerate(raw_gates, start=1):
        _exact_object(
            row,
            {"ordinal", "phase", "name"},
            description=f"platform gate row {index}",
        )
        if (
            _exact_int(row.get("ordinal"), description=f"platform gate ordinal {index}")
            != index
        ):
            raise EvidenceError("platform gate ordinals are not contiguous")
        phase = row.get("phase")
        name = row.get("name")
        if not isinstance(phase, str) or phase not in phase_order:
            raise EvidenceError(f"platform gate phase is invalid: {index}")
        if phase_order[phase] < last_phase:
            raise EvidenceError("platform gate phases are out of order")
        if not isinstance(name, str) or re.fullmatch(r"[a-z0-9_]+", name) is None:
            raise EvidenceError(f"platform gate name is invalid: {index}")
        if name in names:
            raise EvidenceError(f"platform gate name is duplicated: {name}")
        last_phase = phase_order[phase]
        names.add(name)
        normalized_gates.append({"ordinal": index, "phase": phase, "name": name})
    declared_digest = _exact_digest(
        manifest.get("declared_manifest_sha256"),
        description="declared gate manifest digest",
    )
    if declared is not None:
        normalized_declared = validate_declared_gate_manifest(declared)
        expected_digest = canonical_json_sha256(
            normalized_declared, domain="pmux.evidence.declared-gate-manifest.v1"
        )
        expected_rows = [
            {"ordinal": index, **row}
            for index, row in enumerate(normalized_declared["gates"], start=1)
        ]
        if declared_digest != expected_digest or normalized_gates != expected_rows:
            raise EvidenceError("platform gate manifest differs from its declaration")
    return {
        "schema_version": 1,
        "platform": platform,
        "gate_count": gate_count,
        "gates": normalized_gates,
        "declared_manifest_sha256": declared_digest,
    }


def platform_gate_manifest(
    declared: Mapping[str, Any], requested_platform: str
) -> dict[str, Any]:
    if requested_platform not in PLATFORMS:
        raise EvidenceError("gate manifest platform is outside the declared matrix")
    normalized_declared = validate_declared_gate_manifest(declared)
    gates = [
        {"ordinal": ordinal, **row}
        for ordinal, row in enumerate(normalized_declared["gates"], start=1)
    ]
    manifest = {
        "schema_version": 1,
        "platform": requested_platform,
        "gate_count": len(gates),
        "gates": gates,
        "declared_manifest_sha256": canonical_json_sha256(
            normalized_declared, domain="pmux.evidence.declared-gate-manifest.v1"
        ),
    }
    return validate_platform_gate_manifest(
        manifest, expected_platform=requested_platform, declared=normalized_declared
    )


def parse_gate_summary(
    path: pathlib.Path,
    expected_failures: int,
    expected_manifest: Mapping[str, Any],
) -> dict[str, Any]:
    gates: list[dict[str, Any]] = []
    observed_failures = 0
    payload = _stable_regular_bytes(
        path, description="gate summary", maximum_bytes=4 * 1024 * 1024
    )
    try:
        lines = payload.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise EvidenceError("gate summary is not UTF-8") from error
    for line in lines:
        fields = line.split("\t")
        if len(fields) != 4:
            raise EvidenceError("gate summary row does not have four fields")
        name, outcome, seconds, command_sha256 = fields
        if not re.fullmatch(r"[a-z0-9_]+", name):
            raise EvidenceError(f"invalid gate name: {name}")
        if outcome != "PASS":
            if not re.fullmatch(r"FAIL\((?:[0-9]+|SKIPPED_PREREQUISITE)\)", outcome):
                raise EvidenceError(f"invalid gate outcome: {outcome}")
            observed_failures += 1
        if not seconds.isdigit() or not re.fullmatch(r"[0-9a-f]{64}", command_sha256):
            raise EvidenceError("gate timing or command identity is malformed")
        gates.append(
            {
                "name": name,
                "outcome": outcome,
                "elapsed_seconds": int(seconds),
                "command_sha256": command_sha256,
            }
        )
    manifest_gates = expected_manifest.get("gates")
    if not isinstance(manifest_gates, list) or not manifest_gates:
        raise EvidenceError("expected platform gate manifest is empty or malformed")
    expected_names = [
        row.get("name") if isinstance(row, dict) else None for row in manifest_gates
    ]
    observed_names = [gate["name"] for gate in gates]
    if observed_names != expected_names:
        raise EvidenceError(
            "gate summary names are missing, duplicated, reordered, or extra"
        )
    if observed_failures != expected_failures:
        raise EvidenceError("gate failure count disagrees with the durable summary")
    return {
        "schema_version": 1,
        "status": "pass" if observed_failures == 0 else "fail",
        "failure_count": observed_failures,
        "gate_count": len(gates),
        "platform": expected_manifest.get("platform"),
        "expected_manifest_sha256": canonical_json_sha256(
            expected_manifest, domain="pmux.evidence.platform-gate-manifest.v1"
        ),
        "gates": gates,
    }


def gate_skip_record(
    gate: str, stdout_payload: bytes, stderr_payload: bytes
) -> dict[str, Any]:
    """Return the only admitted representation of a prerequisite skip."""

    normalized_gate = _bounded_command_label(gate, description="gate name")
    body = {
        "schema_version": 1,
        "kind": "pmux_gate_skip",
        "gate": normalized_gate,
        "outcome": "FAIL(SKIPPED_PREREQUISITE)",
        "reason": "prerequisite_failed",
        "stdout_size": len(stdout_payload),
        "stdout_sha256": hashlib.sha256(stdout_payload).hexdigest(),
        "stderr_size": len(stderr_payload),
        "stderr_sha256": hashlib.sha256(stderr_payload).hexdigest(),
    }
    return {
        **body,
        "skip_sha256": canonical_json_sha256(body, domain="pmux.evidence.gate-skip.v1"),
    }


def validate_gate_skip_record(value: Mapping[str, Any]) -> dict[str, Any]:
    _exact_object(
        value,
        {
            "schema_version",
            "kind",
            "gate",
            "outcome",
            "reason",
            "stdout_size",
            "stdout_sha256",
            "stderr_size",
            "stderr_sha256",
            "skip_sha256",
        },
        description="gate skip record",
    )
    gate = _bounded_command_label(value.get("gate"), description="gate skip name")
    stdout_size = _exact_int(
        value.get("stdout_size"), description="gate skip stdout size", maximum=8_388_608
    )
    stderr_size = _exact_int(
        value.get("stderr_size"), description="gate skip stderr size", maximum=8_388_608
    )
    body = {
        "schema_version": _exact_int(
            value.get("schema_version"), description="gate skip schema"
        ),
        "kind": value.get("kind"),
        "gate": gate,
        "outcome": value.get("outcome"),
        "reason": value.get("reason"),
        "stdout_size": stdout_size,
        "stdout_sha256": _exact_digest(
            value.get("stdout_sha256"), description="gate skip stdout digest"
        ),
        "stderr_size": stderr_size,
        "stderr_sha256": _exact_digest(
            value.get("stderr_sha256"), description="gate skip stderr digest"
        ),
    }
    if (
        body["schema_version"] != 1
        or body["kind"] != "pmux_gate_skip"
        or body["outcome"] != "FAIL(SKIPPED_PREREQUISITE)"
        or body["reason"] != "prerequisite_failed"
    ):
        raise EvidenceError("gate skip contract is invalid")
    expected = canonical_json_sha256(body, domain="pmux.evidence.gate-skip.v1")
    if (
        _exact_digest(value.get("skip_sha256"), description="gate skip digest")
        != expected
    ):
        raise EvidenceError("gate skip digest is invalid")
    return {**body, "skip_sha256": expected}


def publish_gate_skip(artifact_root: pathlib.Path, gate: str) -> pathlib.Path:
    normalized_gate = _bounded_command_label(gate, description="gate skip name")
    with _anchored_directory(
        artifact_root,
        description="gate artifact root",
        final_owner_uid=os.geteuid(),
    ) as opened:
        _revalidate_directory_chain(opened, description="gate artifact root")
    stdout_payload = b"prerequisite failed; gate was not executed\n"
    stderr_payload = b""
    stdout_path = artifact_root / f"{normalized_gate}.log"
    stderr_path = artifact_root / f"{normalized_gate}.stderr"
    skip_path = artifact_root / f"{normalized_gate}.skip.json"
    atomic_write_bytes(stdout_path, stdout_payload)
    atomic_write_bytes(stderr_path, stderr_payload)
    atomic_write_json(
        skip_path,
        gate_skip_record(normalized_gate, stdout_payload, stderr_payload),
    )
    return skip_path


def _gate_artifact_paths(
    ledger: pathlib.Path, gate: str, suffix: str
) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
    root = ledger.parent
    primary = root / f"{gate}.{suffix}"
    return primary, root / f"{gate}.log", root / f"{gate}.stderr"


def append_gate_execution(
    ledger: pathlib.Path,
    receipt_path: pathlib.Path,
    *,
    gate: str,
    outcome: str,
    elapsed_seconds: int,
    expected_ordinal: int,
    expected_prior_sha256: str | None,
) -> str:
    normalized_gate = _bounded_command_label(gate, description="gate name")
    if not re.fullmatch(r"(?:PASS|FAIL\([0-9]+\))", outcome):
        raise EvidenceError("executed gate outcome is invalid")
    elapsed = _exact_int(
        elapsed_seconds, description="gate elapsed seconds", maximum=86_400
    )
    expected_receipt, stdout_path, stderr_path = _gate_artifact_paths(
        ledger, normalized_gate, "receipt.json"
    )
    if receipt_path != expected_receipt:
        raise EvidenceError("gate receipt path is not canonical")
    receipt = _validated_bounded_process_receipt(
        _stable_regular_bytes(
            receipt_path,
            description="gate bounded-process receipt",
            maximum_bytes=4 * 1024 * 1024,
        )
    )
    expected_outcome = (
        "PASS" if receipt.get("exit_code") == 0 else f"FAIL({receipt.get('exit_code')})"
    )
    if receipt["kind"] == "pmux_bounded_process_failure":
        expected_outcome = "FAIL(124)"
    if outcome != expected_outcome:
        raise EvidenceError("gate outcome differs from its process receipt")
    for stream, path in (("stdout", stdout_path), ("stderr", stderr_path)):
        size, digest = _stable_regular_size_sha256(
            path,
            description=f"gate {stream} spool",
            maximum_bytes=receipt["maximum_output_bytes"],
        )
        if size != receipt[f"{stream}_size"] or digest != receipt[f"{stream}_sha256"]:
            raise EvidenceError("gate spool differs from its process receipt")
    payload = {
        "schema_version": 1,
        "kind": "pmux_gate_execution",
        "gate": normalized_gate,
        "outcome": outcome,
        "elapsed_seconds": elapsed,
        "identity_sha256": receipt["receipt_sha256"],
        "receipt_path": receipt_path.name,
        "stdout_path": stdout_path.name,
        "stderr_path": stderr_path.name,
        "process_receipt": receipt,
    }
    return append_private_jsonl(
        ledger,
        payload,
        expected_ordinal=expected_ordinal,
        expected_prior_sha256=expected_prior_sha256,
    )


def append_gate_skip(
    ledger: pathlib.Path,
    skip_path: pathlib.Path,
    *,
    gate: str,
    expected_ordinal: int,
    expected_prior_sha256: str | None,
) -> str:
    normalized_gate = _bounded_command_label(gate, description="gate name")
    expected_skip, stdout_path, stderr_path = _gate_artifact_paths(
        ledger, normalized_gate, "skip.json"
    )
    if skip_path != expected_skip:
        raise EvidenceError("gate skip path is not canonical")
    record = validate_gate_skip_record(load_json(skip_path))
    if record["gate"] != normalized_gate:
        raise EvidenceError("gate skip name was substituted")
    for stream, path in (("stdout", stdout_path), ("stderr", stderr_path)):
        size, digest = _stable_regular_size_sha256(
            path, description=f"skipped gate {stream} spool", maximum_bytes=8_388_608
        )
        if size != record[f"{stream}_size"] or digest != record[f"{stream}_sha256"]:
            raise EvidenceError("skipped gate spool differs from its record")
    payload = {
        "schema_version": 1,
        "kind": "pmux_gate_skip",
        "gate": normalized_gate,
        "outcome": record["outcome"],
        "elapsed_seconds": 0,
        "identity_sha256": record["skip_sha256"],
        "skip_path": skip_path.name,
        "stdout_path": stdout_path.name,
        "stderr_path": stderr_path.name,
        "skip_record": record,
    }
    return append_private_jsonl(
        ledger,
        payload,
        expected_ordinal=expected_ordinal,
        expected_prior_sha256=expected_prior_sha256,
    )


def bind_gate_evidence_ledger(
    result: Mapping[str, Any],
    ledger: pathlib.Path,
    *,
    expected_count: int,
    expected_tail_sha256: str,
) -> dict[str, Any]:
    """Bind every summary row to one retained executed-or-skipped gate row."""

    count = _exact_int(
        expected_count, description="gate evidence expected count", maximum=100_000
    )
    tail = _exact_digest(
        expected_tail_sha256, description="gate evidence external tail"
    )
    rows = result.get("gates")
    if not isinstance(rows, list) or len(rows) != count:
        raise EvidenceError(
            "gate result count differs from its external evidence count"
        )
    payload = _stable_regular_bytes(
        ledger,
        description="gate evidence ledger",
        maximum_bytes=MAX_JSON_EVIDENCE_BYTES,
    )
    observed_count, observed_tail = _validate_ledger_records(payload)
    if observed_count != count or observed_tail != tail:
        raise EvidenceError("gate evidence ledger differs from its external anchor")
    normalized: list[dict[str, Any]] = []
    for ordinal, (line, summary_row) in enumerate(
        zip(payload.splitlines(), rows, strict=True), start=1
    ):
        chained = strict_json_loads(line, description=f"gate evidence row {ordinal}")
        row = chained["payload"]
        kind = row.get("kind") if isinstance(row, Mapping) else None
        common = {
            "schema_version",
            "kind",
            "gate",
            "outcome",
            "elapsed_seconds",
            "identity_sha256",
            "stdout_path",
            "stderr_path",
        }
        if kind == "pmux_gate_execution":
            _exact_object(
                row,
                common | {"receipt_path", "process_receipt"},
                description=f"executed gate evidence row {ordinal}",
            )
            primary_name = row.get("receipt_path")
            raw_receipt = row.get("process_receipt")
            if not isinstance(raw_receipt, Mapping):
                raise EvidenceError("executed gate process receipt is malformed")
            rendered_receipt = (
                bounded_process.dump_execution_receipt(raw_receipt)
                if raw_receipt.get("kind") == "pmux_bounded_process"
                else bounded_process.dump_failure_receipt(raw_receipt)
            )
            receipt = _validated_bounded_process_receipt(rendered_receipt)
            if row.get("identity_sha256") != receipt["receipt_sha256"]:
                raise EvidenceError("executed gate receipt digest was substituted")
            retained = _validated_bounded_process_receipt(
                _stable_regular_bytes(
                    ledger.parent / str(primary_name),
                    description="retained gate receipt",
                    maximum_bytes=4 * 1024 * 1024,
                )
            )
            if retained != receipt:
                raise EvidenceError("retained gate receipt was substituted")
            maximum_bytes = receipt["maximum_output_bytes"]
            for stream in ("stdout", "stderr"):
                size, digest = _stable_regular_size_sha256(
                    ledger.parent / str(row.get(f"{stream}_path")),
                    description=f"retained gate {stream}",
                    maximum_bytes=maximum_bytes,
                )
                if (
                    size != receipt[f"{stream}_size"]
                    or digest != receipt[f"{stream}_sha256"]
                ):
                    raise EvidenceError("retained gate spool was substituted")
        elif kind == "pmux_gate_skip":
            _exact_object(
                row,
                common | {"skip_path", "skip_record"},
                description=f"skipped gate evidence row {ordinal}",
            )
            primary_name = row.get("skip_path")
            raw_skip = row.get("skip_record")
            if not isinstance(raw_skip, Mapping):
                raise EvidenceError("gate skip record is malformed")
            skip = validate_gate_skip_record(raw_skip)
            if row.get("identity_sha256") != skip["skip_sha256"]:
                raise EvidenceError("gate skip digest was substituted")
            retained = validate_gate_skip_record(
                load_json(ledger.parent / str(primary_name))
            )
            if retained != skip:
                raise EvidenceError("retained gate skip record was substituted")
            for stream in ("stdout", "stderr"):
                size, digest = _stable_regular_size_sha256(
                    ledger.parent / str(row.get(f"{stream}_path")),
                    description=f"retained skipped gate {stream}",
                    maximum_bytes=8_388_608,
                )
                if size != skip[f"{stream}_size"] or digest != skip[f"{stream}_sha256"]:
                    raise EvidenceError("retained skipped gate spool was substituted")
        else:
            raise EvidenceError("gate evidence row kind is invalid")
        name = row.get("gate")
        outcome = row.get("outcome")
        elapsed = _exact_int(
            row.get("elapsed_seconds"),
            description=f"gate evidence elapsed seconds {ordinal}",
            maximum=86_400,
        )
        identity = _exact_digest(
            row.get("identity_sha256"),
            description=f"gate evidence identity {ordinal}",
        )
        expected_artifact_names = {
            f"{name}.log",
            f"{name}.stderr",
            f"{name}.receipt.json"
            if kind == "pmux_gate_execution"
            else f"{name}.skip.json",
        }
        observed_artifact_names = {
            str(row.get("stdout_path")),
            str(row.get("stderr_path")),
            str(primary_name),
        }
        if observed_artifact_names != expected_artifact_names:
            raise EvidenceError("gate artifact paths are not canonical")
        expected_summary = {
            "name": name,
            "outcome": outcome,
            "elapsed_seconds": elapsed,
            "command_sha256": identity,
        }
        if summary_row != expected_summary:
            raise EvidenceError("gate summary differs from its exact evidence row")
        normalized.append(
            {
                "ordinal": ordinal,
                "name": name,
                "kind": kind,
                "outcome": outcome,
                "identity_sha256": identity,
            }
        )
    body = dict(result)
    body.update(
        {
            "gate_evidence_count": count,
            "gate_evidence_tail_sha256": tail,
            "gate_evidence_ledger_bytes": len(payload),
            "gate_evidence_ledger_sha256": hashlib.sha256(payload).hexdigest(),
            "gate_evidence_rows_sha256": canonical_json_sha256(
                normalized, domain="pmux.evidence.gate-evidence-rows.v1"
            ),
            "all_gate_evidence_verified": True,
        }
    )
    return body


def validate_suite_result(
    result: Mapping[str, Any], gate_manifest: Mapping[str, Any]
) -> dict[str, Any]:
    manifest = validate_platform_gate_manifest(gate_manifest)
    expected_fields = {
        "schema_version",
        "status",
        "failure_count",
        "gate_count",
        "platform",
        "expected_manifest_sha256",
        "gates",
        "gate_evidence_count",
        "gate_evidence_tail_sha256",
        "gate_evidence_ledger_bytes",
        "gate_evidence_ledger_sha256",
        "gate_evidence_rows_sha256",
        "all_gate_evidence_verified",
    }
    _exact_object(result, expected_fields, description="Linux suite result")
    if _exact_int(result.get("schema_version"), description="suite result schema") != 1:
        raise EvidenceError("Linux suite result schema is invalid")
    gate_count = _exact_int(
        result.get("gate_count"), description="suite gate count", maximum=100_000
    )
    failure_count = _exact_int(
        result.get("failure_count"),
        description="suite failure count",
        maximum=gate_count,
    )
    if (
        gate_count != manifest["gate_count"]
        or result.get("platform") != manifest["platform"]
        or result.get("status") != ("pass" if failure_count == 0 else "fail")
        or result.get("expected_manifest_sha256")
        != canonical_json_sha256(
            manifest, domain="pmux.evidence.platform-gate-manifest.v1"
        )
        or _exact_int(
            result.get("gate_evidence_count"),
            description="suite gate evidence count",
            maximum=100_000,
        )
        != gate_count
        or not _exact_bool(
            result.get("all_gate_evidence_verified"),
            description="suite gate evidence verdict",
        )
    ):
        raise EvidenceError("Linux suite result binding is invalid")
    for field in (
        "gate_evidence_tail_sha256",
        "gate_evidence_ledger_sha256",
        "gate_evidence_rows_sha256",
    ):
        _exact_digest(result.get(field), description=f"suite result {field}")
    _exact_int(
        result.get("gate_evidence_ledger_bytes"),
        description="suite gate evidence ledger size",
        maximum=MAX_JSON_EVIDENCE_BYTES,
    )
    rows = result.get("gates")
    if not isinstance(rows, list) or len(rows) != gate_count:
        raise EvidenceError("Linux suite result gate rows are incomplete")
    observed_failures = 0
    normalized_rows: list[dict[str, Any]] = []
    for index, (row, declared) in enumerate(
        zip(rows, manifest["gates"], strict=True), start=1
    ):
        _exact_object(
            row,
            {"name", "outcome", "elapsed_seconds", "command_sha256"},
            description=f"suite gate row {index}",
        )
        outcome = row.get("outcome")
        if outcome != "PASS":
            if (
                not isinstance(outcome, str)
                or re.fullmatch(r"FAIL\((?:[0-9]+|SKIPPED_PREREQUISITE)\)", outcome)
                is None
            ):
                raise EvidenceError("suite gate outcome is invalid")
            observed_failures += 1
        if row.get("name") != declared["name"]:
            raise EvidenceError("suite gate order differs from its manifest")
        normalized_rows.append(
            {
                "name": declared["name"],
                "outcome": outcome,
                "elapsed_seconds": _exact_int(
                    row.get("elapsed_seconds"),
                    description=f"suite gate elapsed seconds {index}",
                    maximum=86_400,
                ),
                "command_sha256": _exact_digest(
                    row.get("command_sha256"),
                    description=f"suite gate identity {index}",
                ),
            }
        )
    if observed_failures != failure_count:
        raise EvidenceError("suite gate failures disagree with the result")
    return {**dict(result), "gates": normalized_rows}


_UDS_PROBE_ENVIRONMENT = {
    "HOME": "/nonexistent-pmux-root-home",
    "LANG": "C",
    "LC_ALL": "C",
    "PATH": "/usr/sbin:/usr/bin:/bin",
    "PYTHONDONTWRITEBYTECODE": "1",
    "TZ": "UTC",
}
_UDS_CHILD_ENVIRONMENT = {
    "LANG": "C",
    "LC_ALL": "C",
    "PATH": "/usr/sbin:/usr/bin:/bin",
    "PYTHONDONTWRITEBYTECODE": "1",
    "TZ": "UTC",
}
_UDS_DAEMON_ENVIRONMENT = {
    "LANG": "C",
    "LC_ALL": "C",
    "PATH": "/usr/sbin:/usr/bin:/bin",
    "PYTHONDONTWRITEBYTECODE": "1",
    "TZ": "UTC",
}
_EACCES_JSON_LINE = b'{"denied":true,"errno_name":"EACCES","errno_number":13}\n'


def _receipt_output_is(receipt: Mapping[str, Any], stream: str, payload: bytes) -> bool:
    return (
        receipt.get(f"{stream}_size") == len(payload)
        and receipt.get(f"{stream}_sha256") == hashlib.sha256(payload).hexdigest()
    )


def _verify_uds_receipt_context(
    receipt: Mapping[str, Any],
    *,
    cwd: pathlib.Path,
    environment: Mapping[str, str],
    timeout_seconds: int,
    drain_timeout_seconds: int,
    maximum_output_bytes: int,
    description: str,
) -> dict[str, Any]:
    try:
        validated = bounded_process.verify_receipt_context(
            receipt,
            cwd=cwd,
            environment=environment,
            stdin_bytes=None,
        )
    except bounded_process.BoundedProcessError as error:
        raise EvidenceError(f"{description} context is invalid") from error
    if (
        validated["exit_code"] != 0
        or validated["timeout_seconds"] != timeout_seconds
        or validated["drain_timeout_seconds"] != drain_timeout_seconds
        or validated["maximum_output_bytes"] != maximum_output_bytes
        or not _receipt_output_is(validated, "stderr", b"")
    ):
        raise EvidenceError(f"{description} bounds or result are invalid")
    return validated


def _runuser_payload(receipt: Mapping[str, Any], user_name: str) -> list[str] | None:
    argv = receipt["argv"]
    executable = receipt["executable"]["path"]
    prefix = [
        executable,
        "-u",
        user_name,
        "--",
        "/usr/bin/env",
        "-i",
        f"HOME=/home/{user_name}",
        f"LOGNAME={user_name}",
        "PATH=/usr/local/bin:/usr/bin:/bin",
        f"USER={user_name}",
    ]
    if (
        pathlib.PurePosixPath(executable).name != "runuser"
        or argv[: len(prefix)] != prefix
    ):
        return None
    return argv[len(prefix) :]


def verify_uds_report(
    report: Mapping[str, Any],
    binary_manifest: Mapping[str, Any],
    probe_receipt: Mapping[str, Any],
    binary_manifest_path: pathlib.Path,
) -> dict[str, Any]:
    manifest = validate_release_binary_manifest_schema(binary_manifest)
    binaries = manifest["binaries"]
    expected_manifest_sha256 = canonical_json_sha256(
        manifest, domain="pmux.evidence.release-binary-manifest.v1"
    )
    expected_fields = {
        "schema_version",
        "status",
        "release_binary_manifest_sha256",
        "pmuxd_sha256",
        "pmux_sha256",
        "pmuxd_process",
        "pmuxd_managed_receipt",
        "managed_process_implementation",
        "daemon_exit_code",
        "socket_identity",
        "socket_parent_device",
        "socket_parent_inode",
        "socket_parent_uid",
        "socket_parent_gid",
        "socket_parent_mode",
        "socket_revalidated",
        "runtime_parent_device",
        "runtime_parent_inode",
        "runtime_parent_uid",
        "runtime_parent_gid",
        "runtime_parent_mode",
        "runtime_parent_revalidated",
        "owner_exit_code",
        "owner_process_receipt",
        "intruder_exit_code",
        "intruder_process_receipt",
        "intruder_denial",
        "candidate_write_exit_code",
        "candidate_write_process_receipt",
        "candidate_write_denial",
        "candidate_manifest_revalidated",
        "protocol_version",
        "server_version",
        "different_uid_denied",
        "process_session_empty",
        "residual_processes",
        "socket_removed",
        "runtime_entries_after_shutdown",
        "private_probe_tree_removed",
        "failure_type",
        "failure_message",
        "report_sha256",
    }
    _exact_object(report, expected_fields, description="UDS permissions report")
    if _exact_int(report.get("schema_version"), description="UDS report schema") != 3:
        raise EvidenceError("UDS permissions report schema is unsupported")
    process = report.get("pmuxd_process")
    process_valid = isinstance(process, Mapping) and set(process) == {
        "pid",
        "process_group",
        "session",
        "start_ticks",
    }
    if process_valid:
        process_valid = all(
            type(process.get(field)) is int and process[field] > 0
            for field in ("pid", "process_group", "session", "start_ticks")
        )
        process_valid = process_valid and (
            process["pid"] == process["process_group"] == process["session"]
        )
    try:
        managed_receipt = managed_process.validate_managed_execution_receipt(
            report.get("pmuxd_managed_receipt")
        )
    except bounded_process.BoundedProcessError as error:
        raise EvidenceError("pmuxd managed-process receipt is invalid") from error
    if (
        report.get("managed_process_implementation")
        != revalidate_managed_process_authority()
    ):
        raise EvidenceError("managed-process implementation identity changed")
    managed_environment = bounded_process.environment_identity(_UDS_DAEMON_ENVIRONMENT)
    expected_daemon_argv = [
        managed_receipt["executable"]["path"],
        "-u",
        "pmux",
        "--",
        "/usr/bin/env",
        "-i",
        "HOME=/home/pmux",
        "LOGNAME=pmux",
        "PATH=/usr/local/bin:/usr/bin:/bin",
        "USER=pmux",
        binaries["pmuxd"]["path"],
        "serve",
        "--socket",
        None,
        "--runtime-parent",
        None,
    ]
    managed_argv = managed_receipt["argv"]
    managed_context_valid = all(
        (
            pathlib.PurePosixPath(managed_receipt["executable"]["path"]).name
            == "runuser",
            len(managed_argv) == len(expected_daemon_argv),
            managed_argv[:13] == expected_daemon_argv[:13],
            isinstance(managed_argv[13], str),
            isinstance(managed_argv[15], str),
            managed_receipt["environment"] == managed_environment,
            managed_receipt["timeout_seconds"] == 90,
            managed_receipt["graceful_stop_timeout_seconds"] == 20,
            managed_receipt["drain_timeout_seconds"] == 10,
            managed_receipt["maximum_output_bytes"] == 16 * 1024 * 1024,
            managed_receipt["stop_request"]["signal"] == int(signal.SIGTERM),
            report.get("daemon_exit_code") == managed_receipt["exit_code"],
        )
    )
    managed_daemon_in_ledger = process_valid and any(
        record["pid"] == process["pid"]
        and record["pgid"] == process["process_group"]
        and record["sid"] == process["session"]
        and record["started"].startswith("linux:")
        and record["started"].rsplit(":", 1)[-1] == str(process["start_ticks"])
        and record["reaped"] is True
        for record in managed_receipt["process_ledger"]
    )
    managed_context_valid = managed_context_valid and managed_daemon_in_ledger
    socket_identity = report.get("socket_identity")
    socket_valid = isinstance(socket_identity, Mapping) and set(socket_identity) == {
        "device",
        "inode",
        "uid",
        "gid",
        "mode",
    }
    if socket_valid:
        socket_valid = (
            all(
                type(socket_identity.get(field)) is int and socket_identity[field] >= 0
                for field in ("device", "inode", "uid", "gid")
            )
            and socket_identity["inode"] > 0
            and socket_identity["mode"] == "0600"
        )
    owner_receipt_raw = report.get("owner_process_receipt")
    intruder_receipt_raw = report.get("intruder_process_receipt")
    candidate_write_receipt_raw = report.get("candidate_write_process_receipt")
    try:
        owner_receipt_validated = bounded_process.validate_execution_receipt(
            owner_receipt_raw
        )
        intruder_receipt_validated = bounded_process.validate_execution_receipt(
            intruder_receipt_raw
        )
        candidate_write_receipt_validated = bounded_process.validate_execution_receipt(
            candidate_write_receipt_raw
        )
    except bounded_process.BoundedProcessError as error:
        raise EvidenceError("UDS client process receipt is invalid") from error
    outer_receipt_validated: dict[str, Any]
    try:
        outer_receipt_validated = bounded_process.validate_execution_receipt(
            probe_receipt
        )
    except bounded_process.BoundedProcessError as error:
        raise EvidenceError("outer UDS probe process receipt is invalid") from error

    outer_argv = outer_receipt_validated["argv"]
    outer_script = (
        pathlib.Path(outer_argv[1])
        if len(outer_argv) == 4 and pathlib.Path(outer_argv[1]).is_absolute()
        else None
    )
    workspace = (
        outer_script.parents[2]
        if outer_script is not None
        and len(outer_script.parents) >= 3
        and outer_script.parts[-3:] == ("tools", "linux-docker", "permissions_probe.py")
        else None
    )
    if workspace is None:
        raise EvidenceError("outer UDS probe argv does not name the tracked probe")
    managed_context_valid = managed_context_valid and (
        managed_receipt["cwd"] == str(workspace)
    )
    expected_manifest_path = _canonical_absolute_path(
        str(binary_manifest_path), description="UDS binary manifest path"
    )
    report_argument = pathlib.Path(outer_argv[2])
    report_parent = report_argument.parent
    outer_argv_valid = (
        outer_argv[0] == outer_receipt_validated["executable"]["path"]
        and pathlib.PurePosixPath(outer_argv[0]).name.startswith("python3")
        and report_argument.is_absolute()
        and report_argument.name == "uds-permissions.json"
        and report_parent.parent == pathlib.Path("/var/tmp")
        and re.fullmatch(r"pmux-root-evidence\.[A-Za-z0-9]{8}", report_parent.name)
        is not None
        and outer_argv[3] == str(expected_manifest_path)
    )
    outer_receipt = _verify_uds_receipt_context(
        outer_receipt_validated,
        cwd=workspace,
        environment=_UDS_PROBE_ENVIRONMENT,
        timeout_seconds=90,
        drain_timeout_seconds=10,
        maximum_output_bytes=16 * 1024 * 1024,
        description="outer UDS probe receipt",
    )
    owner_receipt = _verify_uds_receipt_context(
        owner_receipt_validated,
        cwd=workspace,
        environment=_UDS_CHILD_ENVIRONMENT,
        timeout_seconds=15,
        drain_timeout_seconds=5,
        maximum_output_bytes=1024 * 1024,
        description="owner UDS client receipt",
    )
    intruder_receipt = _verify_uds_receipt_context(
        intruder_receipt_validated,
        cwd=workspace,
        environment=_UDS_CHILD_ENVIRONMENT,
        timeout_seconds=15,
        drain_timeout_seconds=5,
        maximum_output_bytes=1024 * 1024,
        description="intruder UDS client receipt",
    )
    candidate_write_receipt = _verify_uds_receipt_context(
        candidate_write_receipt_validated,
        cwd=workspace,
        environment=_UDS_CHILD_ENVIRONMENT,
        timeout_seconds=15,
        drain_timeout_seconds=5,
        maximum_output_bytes=1024 * 1024,
        description="candidate mutation-denial receipt",
    )
    denial = report.get("intruder_denial")
    denial_valid = isinstance(denial, Mapping) and denial == {
        "denied": True,
        "errno_name": "EACCES",
        "errno_number": errno.EACCES,
    }
    candidate_denial = report.get("candidate_write_denial")
    candidate_denial_valid = isinstance(
        candidate_denial, Mapping
    ) and candidate_denial == {
        "denied": True,
        "errno_name": "EACCES",
        "errno_number": errno.EACCES,
    }
    body = dict(report)
    report_sha256 = body.pop("report_sha256")
    digest_valid = isinstance(
        report_sha256, str
    ) and report_sha256 == canonical_json_sha256(
        body, domain="pmux.evidence.uds-permissions-report.v3"
    )
    publication_tail = (
        f"{report_sha256}\n".encode("ascii")
        if isinstance(report_sha256, str)
        and re.fullmatch(r"[0-9a-f]{64}", report_sha256)
        else b""
    )
    socket_path: str | None = None
    owner_payload = _runuser_payload(owner_receipt, "pmux")
    if owner_payload is not None and len(owner_payload) == 6:
        socket_path = owner_payload[2]
    expected_socket = (
        isinstance(socket_path, str)
        and re.fullmatch(r"/var/tmp/pmux-uds-[0-9a-f]{32}/pmux\.sock", socket_path)
        is not None
    )
    managed_context_valid = managed_context_valid and all(
        (
            managed_argv[13] == socket_path,
            isinstance(socket_path, str),
            managed_argv[15]
            == str(pathlib.PurePosixPath(socket_path).parent / "runtimes")
            if isinstance(socket_path, str)
            else False,
        )
    )
    intruder_payload = _runuser_payload(intruder_receipt, "intruder")
    candidate_payload = _runuser_payload(candidate_write_receipt, "pmux")
    owner_argv_valid = owner_payload == [
        binaries["pmux"]["path"],
        "--socket",
        socket_path,
        "--output",
        "json",
        "ping",
    ]
    intruder_argv_valid = intruder_payload == [
        "/usr/bin/python3",
        str(outer_script),
        "--connect-denied",
        socket_path,
    ]
    candidate_argv_valid = candidate_payload == [
        "/usr/bin/python3",
        str(outer_script),
        "--write-denied",
        binaries["pmuxd"]["path"],
    ]
    daemon_in_outer_ledger = process_valid and any(
        record["pid"] == process["pid"]
        and record["pgid"] == process["process_group"]
        and record["sid"] == process["session"]
        and record["started"].startswith("linux:")
        and record["started"].rsplit(":", 1)[-1] == str(process["start_ticks"])
        and record["reaped"] is True
        for record in outer_receipt["process_ledger"]
    )
    server_version = report.get("server_version")
    server_version_valid = (
        isinstance(server_version, str)
        and bool(server_version)
        and len(server_version.encode("utf-8")) <= 1024
        and not any(character in server_version for character in "\0\r\n")
    )
    immutable_candidate_valid = (
        int(manifest["directory_mode"], 8) & 0o222 == 0
        and all(
            int(binaries[name]["mode"], 8) & 0o222 == 0
            for name in REQUIRED_RELEASE_BINARIES
        )
        and manifest["directory_uid"] != report.get("socket_parent_uid")
    )
    verified = all(
        (
            report.get("status") == "pass",
            process_valid,
            managed_context_valid,
            daemon_in_outer_ledger,
            socket_valid,
            digest_valid,
            outer_argv_valid,
            _receipt_output_is(outer_receipt, "stdout", publication_tail),
            report.get("different_uid_denied") is True,
            report.get("process_session_empty") is True,
            report.get("private_probe_tree_removed") is True,
            report.get("socket_removed") is True,
            report.get("socket_revalidated") is True,
            report.get("runtime_parent_revalidated") is True,
            report.get("residual_processes") == [],
            report.get("runtime_entries_after_shutdown") == [],
            report.get("failure_type") is None,
            report.get("failure_message") is None,
            type(report.get("daemon_exit_code")) is int,
            report.get("owner_exit_code") == 0,
            report.get("intruder_exit_code") == 0,
            report.get("candidate_write_exit_code") == 0,
            report.get("protocol_version") == 1,
            server_version_valid,
            report.get("socket_parent_mode") == "0700",
            type(report.get("socket_parent_device")) is int,
            type(report.get("socket_parent_inode")) is int
            and report["socket_parent_inode"] > 0,
            type(report.get("socket_parent_uid")) is int,
            type(report.get("socket_parent_gid")) is int,
            report.get("socket_parent_device") == socket_identity.get("device"),
            socket_valid
            and report.get("socket_parent_uid") == socket_identity.get("uid"),
            socket_valid
            and report.get("socket_parent_gid") == socket_identity.get("gid"),
            report.get("runtime_parent_mode") == "0700",
            type(report.get("runtime_parent_device")) is int,
            type(report.get("runtime_parent_inode")) is int
            and report["runtime_parent_inode"] > 0,
            type(report.get("runtime_parent_uid")) is int,
            type(report.get("runtime_parent_gid")) is int,
            report.get("runtime_parent_inode") != report.get("socket_parent_inode"),
            report.get("runtime_parent_device") == report.get("socket_parent_device"),
            report.get("runtime_parent_uid") == report.get("socket_parent_uid"),
            report.get("runtime_parent_gid") == report.get("socket_parent_gid"),
            denial_valid,
            candidate_denial_valid,
            report.get("candidate_manifest_revalidated") is True,
            expected_socket,
            owner_argv_valid,
            intruder_argv_valid,
            candidate_argv_valid,
            _receipt_output_is(intruder_receipt, "stdout", _EACCES_JSON_LINE),
            _receipt_output_is(candidate_write_receipt, "stdout", _EACCES_JSON_LINE),
            owner_receipt["stdout_size"] > 0,
            immutable_candidate_valid,
            report.get("release_binary_manifest_sha256") == expected_manifest_sha256,
            report.get("pmuxd_sha256") == binaries["pmuxd"]["sha256"],
            report.get("pmux_sha256") == binaries["pmux"]["sha256"],
        )
    )
    return {
        "schema_version": 1,
        "verified": verified,
        "release_binary_manifest_sha256": expected_manifest_sha256,
        "uds_report_sha256": report_sha256,
        "owner_receipt_sha256": owner_receipt["receipt_sha256"],
        "intruder_receipt_sha256": intruder_receipt["receipt_sha256"],
        "candidate_write_receipt_sha256": candidate_write_receipt["receipt_sha256"],
        "outer_probe_receipt_sha256": outer_receipt["receipt_sha256"],
        "server_version": server_version,
    }


def load_retained_process_receipt(
    receipt_path: pathlib.Path,
    stdout_path: pathlib.Path,
    stderr_path: pathlib.Path,
) -> dict[str, Any]:
    receipt = _validated_bounded_process_receipt(
        _stable_regular_bytes(
            receipt_path,
            description="retained process receipt",
            maximum_bytes=4 * 1024 * 1024,
        )
    )
    for stream, path in (("stdout", stdout_path), ("stderr", stderr_path)):
        size, digest = _stable_regular_size_sha256(
            path,
            description=f"retained process {stream}",
            maximum_bytes=receipt["maximum_output_bytes"],
        )
        if size != receipt[f"{stream}_size"] or digest != receipt[f"{stream}_sha256"]:
            raise EvidenceError(f"retained process {stream} differs from its receipt")
    return receipt


def _require_json_object_from_stdin() -> dict[str, Any]:
    payload = sys.stdin.buffer.read(MAX_JSON_EVIDENCE_BYTES + 1)
    if len(payload) > MAX_JSON_EVIDENCE_BYTES:
        raise EvidenceError("stdin JSON exceeds the evidence bound")
    value = strict_json_loads(payload, description="stdin JSON")
    if not isinstance(value, dict):
        raise EvidenceError("stdin JSON must be an object")
    return value


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    prepare = subparsers.add_parser("prepare-output")
    prepare.add_argument("path", type=pathlib.Path)

    secure = subparsers.add_parser("secure-tree")
    secure.add_argument("path", type=pathlib.Path)

    tree_manifest = subparsers.add_parser("tree-manifest")
    tree_manifest.add_argument("root", type=pathlib.Path)
    tree_manifest.add_argument("output", type=pathlib.Path)

    tree_verify = subparsers.add_parser("tree-verify")
    tree_verify.add_argument("root", type=pathlib.Path)
    tree_verify.add_argument("manifest", type=pathlib.Path)
    tree_verify.add_argument("--output", type=pathlib.Path)

    write_json = subparsers.add_parser("write-json")
    write_json.add_argument("path", type=pathlib.Path)

    transfer = subparsers.add_parser("transfer-private")
    transfer.add_argument("source", type=pathlib.Path)
    transfer.add_argument("destination", type=pathlib.Path)
    transfer.add_argument("destination_uid", type=int)
    transfer.add_argument("destination_gid", type=int)
    transfer.add_argument("maximum_bytes", type=int)

    append = subparsers.add_parser("append-resource")
    append.add_argument("ledger", type=pathlib.Path)
    append.add_argument("kind")
    append.add_argument("name")
    append.add_argument("object_id")
    append.add_argument(
        "state",
        choices=(
            "planned",
            "created",
            "bound",
            "removed",
            "cleanup_failed",
            "ownership_unconfirmed",
        ),
    )
    append.add_argument("--expected-ordinal", required=True, type=int)
    append.add_argument("--expected-prior-sha256", required=True)

    append_command = subparsers.add_parser("append-command")
    append_command.add_argument("ledger", type=pathlib.Path)
    append_command.add_argument("receipt", type=pathlib.Path)
    append_command.add_argument("label")
    append_command.add_argument("scope")
    append_command.add_argument("--expected-ordinal", required=True, type=int)
    append_command.add_argument("--expected-prior-sha256", required=True)

    command_report = subparsers.add_parser("command-ledger-report")
    command_report.add_argument("ledger", type=pathlib.Path)
    command_report.add_argument("expected_count", type=int)
    command_report.add_argument("expected_tail_sha256")
    command_report.add_argument("output", type=pathlib.Path)

    append_gate = subparsers.add_parser("append-gate")
    append_gate.add_argument("ledger", type=pathlib.Path)
    append_gate.add_argument("receipt", type=pathlib.Path)
    append_gate.add_argument("gate")
    append_gate.add_argument("outcome")
    append_gate.add_argument("elapsed_seconds", type=int)
    append_gate.add_argument("--expected-ordinal", required=True, type=int)
    append_gate.add_argument("--expected-prior-sha256", required=True)

    append_skip = subparsers.add_parser("append-gate-skip")
    append_skip.add_argument("ledger", type=pathlib.Path)
    append_skip.add_argument("skip", type=pathlib.Path)
    append_skip.add_argument("gate")
    append_skip.add_argument("--expected-ordinal", required=True, type=int)
    append_skip.add_argument("--expected-prior-sha256", required=True)

    publish_skip = subparsers.add_parser("publish-gate-skip")
    publish_skip.add_argument("artifact_root", type=pathlib.Path)
    publish_skip.add_argument("gate")

    binary_capture = subparsers.add_parser("binary-capture")
    binary_capture.add_argument("directory", type=pathlib.Path)
    binary_capture.add_argument("output", type=pathlib.Path)
    binary_capture.add_argument("--expected-owner-uid", type=int)

    binary_verify = subparsers.add_parser("binary-verify")
    binary_verify.add_argument("manifest", type=pathlib.Path)
    binary_verify.add_argument("--output", type=pathlib.Path)

    binary_repro = subparsers.add_parser("binary-repro-compare")
    binary_repro.add_argument("candidate_manifest", type=pathlib.Path)
    binary_repro.add_argument("reproduced_directory", type=pathlib.Path)
    binary_repro.add_argument("output", type=pathlib.Path)

    binary_stage = subparsers.add_parser("binary-repro-stage")
    binary_stage.add_argument("source_directory", type=pathlib.Path)
    binary_stage.add_argument("destination_directory", type=pathlib.Path)
    binary_stage.add_argument("output", type=pathlib.Path)

    platform_parser = subparsers.add_parser("platform-report")
    platform_parser.add_argument("requested")
    platform_parser.add_argument("inspect_log", type=pathlib.Path)
    platform_parser.add_argument("output", type=pathlib.Path)

    image_iid = subparsers.add_parser("image-iid")
    image_iid.add_argument("path", type=pathlib.Path)

    base_image = subparsers.add_parser("base-image")
    base_image.add_argument("value")

    base_index = subparsers.add_parser("base-index")
    base_index.add_argument("reference")
    base_index.add_argument("raw_manifest", type=pathlib.Path)
    base_index.add_argument("output", type=pathlib.Path)

    docker_transport = subparsers.add_parser("docker-transport")
    docker_transport.add_argument("socket_path", type=pathlib.Path)
    docker_transport.add_argument("output", type=pathlib.Path)

    docker_transport_stability = subparsers.add_parser("docker-transport-stability")
    docker_transport_stability.add_argument("before", type=pathlib.Path)
    docker_transport_stability.add_argument("after", type=pathlib.Path)
    docker_transport_stability.add_argument("output", type=pathlib.Path)

    docker_control = subparsers.add_parser("docker-control-plane")
    docker_control.add_argument("workspace", type=pathlib.Path)
    docker_control.add_argument("docker_version_receipt", type=pathlib.Path)
    docker_control.add_argument("docker_version_stdout", type=pathlib.Path)
    docker_control.add_argument("docker_version_stderr", type=pathlib.Path)
    docker_control.add_argument("buildx_version_receipt", type=pathlib.Path)
    docker_control.add_argument("buildx_version_stdout", type=pathlib.Path)
    docker_control.add_argument("buildx_version_stderr", type=pathlib.Path)
    docker_control.add_argument("plugin_inventory_receipt", type=pathlib.Path)
    docker_control.add_argument("plugin_inventory_stdout", type=pathlib.Path)
    docker_control.add_argument("plugin_inventory_stderr", type=pathlib.Path)
    docker_control.add_argument("transport_identity", type=pathlib.Path)
    docker_control.add_argument("output", type=pathlib.Path)

    gate_manifest = subparsers.add_parser("gate-manifest")
    gate_manifest.add_argument("declared", type=pathlib.Path)
    gate_manifest.add_argument("platform")
    gate_manifest.add_argument("output", type=pathlib.Path)

    uds_binding = subparsers.add_parser("uds-binding")
    uds_binding.add_argument("report", type=pathlib.Path)
    uds_binding.add_argument("binary_manifest", type=pathlib.Path)
    uds_binding.add_argument("probe_receipt", type=pathlib.Path)
    uds_binding.add_argument("probe_stdout", type=pathlib.Path)
    uds_binding.add_argument("probe_stderr", type=pathlib.Path)
    uds_binding.add_argument("output", type=pathlib.Path)

    system = subparsers.add_parser("system")
    system.add_argument("workspace", type=pathlib.Path)
    system.add_argument("expected_source_sha256")
    system.add_argument("expected_platform")
    system.add_argument("output", type=pathlib.Path)

    stability = subparsers.add_parser("source-stability")
    stability.add_argument("before", type=pathlib.Path)
    stability.add_argument("after", type=pathlib.Path)
    stability.add_argument("expected_source_sha256")
    stability.add_argument("output", type=pathlib.Path)

    revision_stability = subparsers.add_parser("revision-stability")
    revision_stability.add_argument("before", type=pathlib.Path)
    revision_stability.add_argument("after", type=pathlib.Path)
    revision_stability.add_argument("output", type=pathlib.Path)

    binding = subparsers.add_parser("cell-binding")
    binding.add_argument("host_source", type=pathlib.Path)
    binding.add_argument("host_revision_before", type=pathlib.Path)
    binding.add_argument("host_revision_after", type=pathlib.Path)
    binding.add_argument("host_revision_stability", type=pathlib.Path)
    binding.add_argument("container_system", type=pathlib.Path)
    binding.add_argument("image_binaries", type=pathlib.Path)
    binding.add_argument("binaries_before", type=pathlib.Path)
    binding.add_argument("binaries_after", type=pathlib.Path)
    binding.add_argument("reproduced_binaries", type=pathlib.Path)
    binding.add_argument("reproduction_comparison", type=pathlib.Path)
    binding.add_argument("uds_binding", type=pathlib.Path)
    binding.add_argument("suite_result", type=pathlib.Path)
    binding.add_argument("gate_manifest", type=pathlib.Path)
    binding.add_argument("expected_source_sha256")
    binding.add_argument("expected_platform")
    binding.add_argument("expected_base_image")
    binding.add_argument("output", type=pathlib.Path)

    result = subparsers.add_parser("suite-result")
    result.add_argument("summary", type=pathlib.Path)
    result.add_argument("failures", type=int)
    result.add_argument("manifest", type=pathlib.Path)
    result.add_argument("gate_ledger", type=pathlib.Path)
    result.add_argument("gate_ledger_count", type=int)
    result.add_argument("gate_ledger_tail_sha256")
    result.add_argument("output", type=pathlib.Path)
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    options = _parser().parse_args(arguments)
    if options.command == "prepare-output":
        prepare_empty_private_directory(options.path)
    elif options.command == "secure-tree":
        secure_private_tree(options.path)
    elif options.command == "tree-manifest":
        root = options.root.resolve(strict=True)
        excluded: frozenset[str] = frozenset()
        try:
            relative_output = options.output.relative_to(root).as_posix()
        except ValueError:
            pass
        else:
            if options.output.exists() or options.output.is_symlink():
                raise EvidenceError("self-excluded tree-manifest output already exists")
            excluded = frozenset((relative_output,))
        atomic_write_json(
            options.output, regular_tree_manifest(root, excluded_paths=excluded)
        )
    elif options.command == "tree-verify":
        root = options.root.resolve(strict=True)
        manifest_path = options.manifest.resolve(strict=True)
        try:
            expected_exclusion = frozenset(
                (manifest_path.relative_to(root).as_posix(),)
            )
        except ValueError:
            expected_exclusion = frozenset()
        verified = verify_regular_tree_manifest(
            root,
            load_json(manifest_path),
            expected_excluded_paths=expected_exclusion,
        )
        if options.output is not None:
            atomic_write_json(
                options.output,
                {
                    "schema_version": 1,
                    "verified": True,
                    "tree_sha256": verified["tree_sha256"],
                    "manifest_sha256": canonical_json_sha256(
                        verified, domain="pmux.evidence.regular-tree-manifest.v1"
                    ),
                },
            )
    elif options.command == "write-json":
        atomic_write_json(options.path, _require_json_object_from_stdin())
    elif options.command == "transfer-private":
        print(
            transfer_private_artifact_to_uid(
                options.source,
                options.destination,
                destination_uid=options.destination_uid,
                destination_gid=options.destination_gid,
                maximum_bytes=options.maximum_bytes,
            )["sha256"]
        )
    elif options.command == "append-resource":
        if options.state == "planned":
            if options.object_id != "pending":
                raise EvidenceError(
                    "planned resource identity must be the literal 'pending'"
                )
            validate_planned_docker_resource(options.kind, options.name)
            record = {
                "schema_version": 1,
                "kind": options.kind,
                "name": options.name,
                "object_id": options.object_id,
                "state": options.state,
            }
        elif (
            options.state in {"cleanup_failed", "ownership_unconfirmed"}
            and options.object_id == "unknown"
        ):
            validate_planned_docker_resource(options.kind, options.name)
            record = {
                "schema_version": 1,
                "kind": options.kind,
                "name": options.name,
                "object_id": options.object_id,
                "state": options.state,
            }
        else:
            identity = validate_docker_resource(
                DockerResourceIdentity(options.kind, options.name, options.object_id)
            )
            record = {
                "schema_version": 1,
                "kind": identity.kind,
                "name": identity.name,
                "object_id": identity.object_id,
                "state": options.state,
            }
        prior = (
            None
            if options.expected_prior_sha256 == "START"
            else options.expected_prior_sha256
        )
        digest = append_private_jsonl(
            options.ledger,
            record,
            expected_ordinal=options.expected_ordinal,
            expected_prior_sha256=prior,
        )
        print(digest)
    elif options.command == "append-command":
        prior = (
            None
            if options.expected_prior_sha256 == "START"
            else options.expected_prior_sha256
        )
        print(
            append_bounded_command_receipt(
                options.ledger,
                options.receipt,
                label=options.label,
                scope=options.scope,
                expected_ordinal=options.expected_ordinal,
                expected_prior_sha256=prior,
            )
        )
    elif options.command == "command-ledger-report":
        report = bounded_command_ledger_report(
            options.ledger,
            expected_count=options.expected_count,
            expected_tail_sha256=options.expected_tail_sha256,
        )
        atomic_write_json(
            options.output,
            report,
        )
        if not report["all_commands_bounded"]:
            return 1
    elif options.command == "append-gate":
        prior = (
            None
            if options.expected_prior_sha256 == "START"
            else options.expected_prior_sha256
        )
        print(
            append_gate_execution(
                options.ledger,
                options.receipt,
                gate=options.gate,
                outcome=options.outcome,
                elapsed_seconds=options.elapsed_seconds,
                expected_ordinal=options.expected_ordinal,
                expected_prior_sha256=prior,
            )
        )
    elif options.command == "append-gate-skip":
        prior = (
            None
            if options.expected_prior_sha256 == "START"
            else options.expected_prior_sha256
        )
        print(
            append_gate_skip(
                options.ledger,
                options.skip,
                gate=options.gate,
                expected_ordinal=options.expected_ordinal,
                expected_prior_sha256=prior,
            )
        )
    elif options.command == "publish-gate-skip":
        skip_path = publish_gate_skip(options.artifact_root, options.gate)
        print(validate_gate_skip_record(load_json(skip_path))["skip_sha256"])
    elif options.command == "binary-capture":
        atomic_write_json(
            options.output,
            release_binary_manifest(
                options.directory,
                expected_owner_uid=options.expected_owner_uid,
            ),
        )
    elif options.command == "binary-verify":
        verified = verify_release_binary_manifest(load_json(options.manifest))
        if options.output is not None:
            atomic_write_json(options.output, verified)
    elif options.command == "binary-repro-compare":
        atomic_write_json(
            options.output,
            compare_reproduced_release_binaries(
                load_json(options.candidate_manifest),
                options.reproduced_directory,
            ),
        )
    elif options.command == "binary-repro-stage":
        atomic_write_json(
            options.output,
            stage_reproduced_release_binaries(
                options.source_directory, options.destination_directory
            ),
        )
    elif options.command == "platform-report":
        report = platform_report(
            options.requested, options.inspect_log.read_text(encoding="utf-8")
        )
        atomic_write_json(options.output, report)
        if not report["supported"]:
            return 1
    elif options.command == "image-iid":
        print(read_image_iid(options.path))
    elif options.command == "base-image":
        print(validate_base_image(options.value))
    elif options.command == "base-index":
        atomic_write_json(
            options.output,
            verify_base_image_index(options.reference, options.raw_manifest),
        )
    elif options.command == "docker-transport":
        atomic_write_json(
            options.output, docker_transport_identity(options.socket_path)
        )
    elif options.command == "docker-transport-stability":
        atomic_write_json(
            options.output,
            compare_docker_transport_identities(
                load_json(options.before), load_json(options.after)
            ),
        )
    elif options.command == "docker-control-plane":
        atomic_write_json(
            options.output,
            docker_control_plane_report(
                workspace=options.workspace,
                docker_version_receipt=options.docker_version_receipt,
                docker_version_stdout=options.docker_version_stdout,
                docker_version_stderr=options.docker_version_stderr,
                buildx_version_receipt=options.buildx_version_receipt,
                buildx_version_stdout=options.buildx_version_stdout,
                buildx_version_stderr=options.buildx_version_stderr,
                plugin_inventory_receipt=options.plugin_inventory_receipt,
                plugin_inventory_stdout=options.plugin_inventory_stdout,
                plugin_inventory_stderr=options.plugin_inventory_stderr,
                transport_identity=load_json(options.transport_identity),
            ),
        )
    elif options.command == "gate-manifest":
        atomic_write_json(
            options.output,
            platform_gate_manifest(load_json(options.declared), options.platform),
        )
    elif options.command == "uds-binding":
        report = verify_uds_report(
            load_json(options.report),
            load_json(options.binary_manifest),
            load_retained_process_receipt(
                options.probe_receipt, options.probe_stdout, options.probe_stderr
            ),
            options.binary_manifest,
        )
        atomic_write_json(options.output, report)
        if not report["verified"]:
            return 1
    elif options.command == "system":
        atomic_write_json(
            options.output,
            runtime_system_manifest(
                options.workspace,
                options.expected_source_sha256,
                options.expected_platform,
            ),
        )
    elif options.command == "source-stability":
        report = compare_source_manifests(
            load_json(options.before),
            load_json(options.after),
            options.expected_source_sha256,
        )
        atomic_write_json(options.output, report)
        if not report["verified"]:
            return 1
    elif options.command == "revision-stability":
        atomic_write_json(
            options.output,
            compare_workspace_revision_captures(
                load_json(options.before), load_json(options.after)
            ),
        )
    elif options.command == "cell-binding":
        report = verify_cell_binding(
            host_source=load_json(options.host_source),
            host_revision_before=load_json(options.host_revision_before),
            host_revision_after=load_json(options.host_revision_after),
            host_revision_stability=load_json(options.host_revision_stability),
            container_system=load_json(options.container_system),
            image_binaries=load_json(options.image_binaries),
            binaries_before=load_json(options.binaries_before),
            binaries_after=load_json(options.binaries_after),
            reproduced_binaries=load_json(options.reproduced_binaries),
            reproduction_comparison=load_json(options.reproduction_comparison),
            uds_binding=load_json(options.uds_binding),
            suite_result=load_json(options.suite_result),
            gate_manifest=load_json(options.gate_manifest),
            expected_source_sha256=options.expected_source_sha256,
            expected_platform=options.expected_platform,
            expected_base_image=options.expected_base_image,
        )
        atomic_write_json(options.output, report)
        if not report["verified"]:
            return 1
    elif options.command == "suite-result":
        parsed = parse_gate_summary(
            options.summary, options.failures, load_json(options.manifest)
        )
        atomic_write_json(
            options.output,
            bind_gate_evidence_ledger(
                parsed,
                options.gate_ledger,
                expected_count=options.gate_ledger_count,
                expected_tail_sha256=options.gate_ledger_tail_sha256,
            ),
        )
    else:  # pragma: no cover - argparse makes this unreachable.
        raise EvidenceError(f"unsupported command: {options.command}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (EvidenceError, SourceIdentityError) as error:
        print(f"linux-docker evidence: {error}", file=sys.stderr)
        raise SystemExit(2) from error
