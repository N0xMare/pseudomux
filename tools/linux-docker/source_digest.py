#!/usr/bin/env python3
"""Canonical, race-aware source identity for frozen pmux evidence.

This module intentionally hashes only the declared Docker build context.  It
rejects unknown top-level inputs, symlinks, special files, and mutations while
reading so a host digest cannot silently describe bytes that the image did not
receive (or vice versa).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import stat
import sys
import types
from collections.abc import Callable, Iterator, Mapping, Sequence
from dataclasses import dataclass
from typing import Any


class SourceIdentityError(RuntimeError):
    """The declared source tree could not be identified without ambiguity."""


TOOLS_ROOT = pathlib.Path(__file__).resolve().parents[1]
BOUNDED_PROCESS_RELATIVE_PATH = "tools/evidence_common/bounded_process.py"
MAX_BOUND_AUTHORITY_BYTES = 4 * 1024 * 1024


def _read_exact_authority(
    path: pathlib.Path,
) -> tuple[bytes, os.stat_result]:
    try:
        before = path.lstat()
    except OSError as error:
        raise SourceIdentityError(
            f"shared bounded-process authority is unavailable: {error}"
        ) from error
    if (
        stat.S_ISLNK(before.st_mode)
        or not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or stat.S_IMODE(before.st_mode) & 0o7000
        or not 1 <= before.st_size <= MAX_BOUND_AUTHORITY_BYTES
    ):
        raise SourceIdentityError(
            "shared bounded-process authority is not one exact file"
        )
    descriptor = -1
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
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
        if any(getattr(opened, field) != getattr(before, field) for field in fields):
            raise SourceIdentityError(
                "shared bounded-process authority changed before read"
            )
        payload = bytearray()
        while len(payload) < opened.st_size:
            chunk = os.read(descriptor, min(64 * 1024, opened.st_size - len(payload)))
            if not chunk:
                raise SourceIdentityError(
                    "shared bounded-process authority ended before its bound"
                )
            payload.extend(chunk)
        if os.read(descriptor, 1):
            raise SourceIdentityError(
                "shared bounded-process authority exceeded its bound"
            )
        after = os.fstat(descriptor)
        if any(getattr(after, field) != getattr(opened, field) for field in fields):
            raise SourceIdentityError(
                "shared bounded-process authority changed while reading"
            )
    except OSError as error:
        raise SourceIdentityError(
            f"shared bounded-process authority could not be read: {error}"
        ) from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    final = path.lstat()
    if any(getattr(final, field) != getattr(after, field) for field in fields):
        raise SourceIdentityError(
            "shared bounded-process authority path changed after read"
        )
    return bytes(payload), after


def _load_exact_bounded_process() -> tuple[
    types.ModuleType, dict[str, Any], tuple[int, ...]
]:
    path = TOOLS_ROOT / "evidence_common" / "bounded_process.py"
    payload, metadata = _read_exact_authority(path)
    digest = hashlib.sha256(payload).hexdigest()
    module_name = f"_pmux_bounded_process_authority_{os.urandom(16).hex()}"
    module = types.ModuleType(module_name)
    module.__file__ = str(path)
    module.__package__ = ""
    sys.modules[module_name] = module
    try:
        code = compile(payload, str(path), "exec", dont_inherit=True)
        exec(code, module.__dict__)
    except Exception as error:
        raise SourceIdentityError(
            f"shared bounded-process authority could not load: {error}"
        ) from error
    finally:
        if sys.modules.get(module_name) is module:
            del sys.modules[module_name]
    required = (
        "bind_executable",
        "run",
        "validate_execution_receipt",
        "environment_identity",
        "BoundedProcessError",
    )
    if any(not hasattr(module, name) for name in required):
        raise SourceIdentityError(
            "shared bounded-process authority is missing its required interface"
        )
    witness_fields = (
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
    witness = tuple(getattr(metadata, field) for field in witness_fields)
    identity = {
        "path": BOUNDED_PROCESS_RELATIVE_PATH,
        "size": len(payload),
        "sha256": digest,
    }
    return module, identity, witness


bounded_process, _BOUNDED_PROCESS_IDENTITY, _BOUNDED_PROCESS_WITNESS = (
    _load_exact_bounded_process()
)


def _revalidate_bounded_process_authority() -> dict[str, Any]:
    path = TOOLS_ROOT / "evidence_common" / "bounded_process.py"
    payload, metadata = _read_exact_authority(path)
    witness_fields = (
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
    if tuple(getattr(metadata, field) for field in witness_fields) != (
        _BOUNDED_PROCESS_WITNESS
    ):
        raise SourceIdentityError(
            "shared bounded-process authority changed after module load"
        )
    identity = {
        "path": BOUNDED_PROCESS_RELATIVE_PATH,
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }
    if identity != _BOUNDED_PROCESS_IDENTITY:
        raise SourceIdentityError(
            "shared bounded-process authority content changed after module load"
        )
    return identity


SOURCE_ALGORITHM = "pmux-source-v2-path-mode-size-content-sha256"
REVISION_ALGORITHM = "pmux-workspace-revision-v1"
REVISION_CAPTURE_ALGORITHM = "pmux-workspace-revision-capture-v1"
REVISION_CAPTURE_DOMAIN = "pmux-workspace-revision-capture-v1"
MAX_GIT_OUTPUT_BYTES = 64 * 1024 * 1024
MAX_GIT_EXECUTABLE_BYTES = 512 * 1024 * 1024
MAX_GIT_CONTROL_FILE_BYTES = 512 * 1024 * 1024
MAX_GIT_SHARED_INDEX_FILES = 128
GIT_COMMAND_TIMEOUT_SECONDS = 30
MAX_SAFE_INTEGER = 9_007_199_254_740_991

INCLUDED_ROOT_FILES = frozenset(
    {
        ".dockerignore",
        ".gitignore",
        "Cargo.lock",
        "Cargo.toml",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "README.md",
        "rust-toolchain.toml",
    }
)
INCLUDED_ROOT_DIRECTORIES = frozenset(
    {
        "bin",
        "clients",
        "docs",
        "evidence",
        "crates",
        "fuzz",
        "scripts",
        "tests",
        "tools",
        "vendor",
    }
)
EXCLUDED_DIRECTORY_NAMES = frozenset(
    {
        ".claude",
        ".context",
        ".direnv",
        ".git",
        ".idea",
        ".mypy_cache",
        ".pytest_cache",
        ".pseudomux",
        ".ruff_cache",
        ".venv",
        ".vscode",
        "__pycache__",
        "coverage",
        "dist",
        "node_modules",
        "target",
        "venv",
    }
)
EXCLUDED_FILE_NAMES = frozenset(
    {
        ".DS_Store",
        ".coverage",
        ".netrc",
        ".npmrc",
        "credentials",
        "credentials.toml",
    }
)
EXCLUDED_FILE_SUFFIXES = (
    ".db",
    ".fifo",
    ".key",
    ".log",
    ".p12",
    ".pem",
    ".pfx",
    ".pid",
    ".pyc",
    ".sock",
    ".socket",
    ".sqlite",
)


@dataclass(frozen=True)
class GitCommandOutcome:
    """One Git query plus the causal receipt produced by the bounded supervisor."""

    exit_code: int
    stdout: bytes
    receipt: Mapping[str, Any]


GitCommandRunner = Callable[
    [pathlib.Path, pathlib.Path, tuple[str, ...], int], GitCommandOutcome
]


@dataclass(frozen=True)
class _GitQuerySpec:
    label: str
    arguments: tuple[str, ...]
    maximum_stdout_bytes: int
    allowed_exit_codes: frozenset[int]


_GIT_QUERY_SPECS = (
    _GitQuerySpec(
        "head",
        ("rev-parse", "--verify", "HEAD^{commit}"),
        256,
        frozenset({0}),
    ),
    _GitQuerySpec(
        "symbolic_head",
        ("symbolic-ref", "-q", "HEAD"),
        4096,
        frozenset({0, 1}),
    ),
    _GitQuerySpec(
        "status_porcelain_v1_z",
        (
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ),
        MAX_GIT_OUTPUT_BYTES,
        frozenset({0}),
    ),
    _GitQuerySpec(
        "tracked_binary_diff",
        (
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "HEAD",
            "--",
        ),
        MAX_GIT_OUTPUT_BYTES,
        frozenset({0}),
    ),
    _GitQuerySpec("version", ("--version",), 4096, frozenset({0})),
)


@dataclass(frozen=True)
class _SourceSnapshot:
    directories: dict[str, os.stat_result]
    files: dict[str, os.stat_result]


_DIRECTORY_IDENTITY_FIELDS = (
    "st_dev",
    "st_ino",
    "st_uid",
    "st_gid",
    "st_mode",
    "st_mtime_ns",
    "st_ctime_ns",
    "st_nlink",
)
_FILE_IDENTITY_FIELDS = (
    *_DIRECTORY_IDENTITY_FIELDS,
    "st_size",
)


def _is_excluded_file(relative: pathlib.Path) -> bool:
    name = relative.name
    if name == ".env" or name.startswith(".env."):
        return True
    if name in EXCLUDED_FILE_NAMES or name.endswith(EXCLUDED_FILE_SUFFIXES):
        return True
    if ".sqlite-" in name or name.startswith("core."):
        return True
    if name.endswith(".jsonl"):
        parts = relative.parts
        return not (
            len(parts) >= 4
            and parts[:3] == ("crates", "claude", "tests")
            and parts[3] == "fixtures"
        ) and not (
            len(parts) >= 3
            and parts[:2] == ("tools", "phase0")
            and parts[2] == "fixtures"
        )
    if len(relative.parts) >= 2 and relative.parts[-2] == ".cargo":
        return name in {"credentials", "credentials.toml"}
    if name.startswith("id_rsa") or name.startswith("id_ed25519"):
        return True
    return False


def is_included(relative: pathlib.Path) -> bool:
    """Return whether a regular path is part of the declared source context."""

    if relative.is_absolute() or not relative.parts:
        return False
    if any(part in EXCLUDED_DIRECTORY_NAMES for part in relative.parts[:-1]):
        return False
    if _is_excluded_file(relative):
        return False
    if len(relative.parts) == 1:
        return relative.name in INCLUDED_ROOT_FILES
    return relative.parts[0] in INCLUDED_ROOT_DIRECTORIES


def _lstat_regular(path: pathlib.Path, *, description: str) -> os.stat_result:
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise SourceIdentityError(f"{description} disappeared: {path}") from error
    if stat.S_ISLNK(metadata.st_mode):
        raise SourceIdentityError(f"{description} is a symlink: {path}")
    if not stat.S_ISREG(metadata.st_mode):
        raise SourceIdentityError(f"{description} is not a regular file: {path}")
    if metadata.st_nlink != 1:
        raise SourceIdentityError(
            f"{description} has an ambiguous hard-link alias: {path}"
        )
    if stat.S_IMODE(metadata.st_mode) & 0o7000:
        raise SourceIdentityError(f"{description} has special mode bits: {path}")
    return metadata


def _read_stable_file(
    path: pathlib.Path, before: os.stat_result
) -> tuple[bytes, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise SourceIdentityError(
            f"could not open source file exactly: {path}: {error}"
        ) from error
    try:
        opened = os.fstat(descriptor)
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
        if any(
            getattr(opened, field) != getattr(before, field)
            for field in identity_fields
        ):
            raise SourceIdentityError(f"source file changed before read: {path}")
        chunks: list[bytes] = []
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
        after = os.fstat(descriptor)
        if any(
            getattr(after, field) != getattr(opened, field) for field in identity_fields
        ):
            raise SourceIdentityError(f"source file changed while read: {path}")
    finally:
        os.close(descriptor)
    final = _lstat_regular(path, description="source file")
    if any(getattr(final, field) != getattr(after, field) for field in identity_fields):
        raise SourceIdentityError(f"source path was replaced after read: {path}")
    data = b"".join(chunks)
    if len(data) != after.st_size:
        raise SourceIdentityError(f"source file size changed while read: {path}")
    return data, after


def _snapshot_declared_directory(
    root: pathlib.Path,
    directory: pathlib.Path,
    directories: dict[str, os.stat_result],
    files: dict[str, os.stat_result],
) -> None:
    relative_text = directory.relative_to(root).as_posix()
    try:
        metadata = directory.lstat()
    except FileNotFoundError as error:
        raise SourceIdentityError(
            f"declared source directory disappeared: {relative_text}"
        ) from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise SourceIdentityError(
            f"declared source directory changed identity: {relative_text}"
        )
    directories[relative_text] = metadata
    try:
        children = sorted(directory.iterdir(), key=lambda item: item.name)
    except OSError as error:
        raise SourceIdentityError(
            f"could not enumerate declared source directory: {relative_text}"
        ) from error
    for child in children:
        relative = child.relative_to(root)
        try:
            child_metadata = child.lstat()
        except FileNotFoundError as error:
            raise SourceIdentityError(
                f"source membership changed during traversal: {relative}"
            ) from error
        if stat.S_ISLNK(child_metadata.st_mode):
            raise SourceIdentityError(f"source tree contains a symlink: {relative}")
        if stat.S_ISDIR(child_metadata.st_mode):
            if child.name not in EXCLUDED_DIRECTORY_NAMES:
                _snapshot_declared_directory(root, child, directories, files)
            continue
        if stat.S_ISREG(child_metadata.st_mode):
            if is_included(relative):
                validated = _lstat_regular(child, description="source file")
                files[relative.as_posix()] = validated
            continue
        raise SourceIdentityError(f"source tree contains a special file: {relative}")


def _source_snapshot(root: pathlib.Path) -> _SourceSnapshot:
    root_metadata = root.lstat()
    if stat.S_ISLNK(root_metadata.st_mode) or not stat.S_ISDIR(root_metadata.st_mode):
        raise SourceIdentityError(
            "workspace root must be a real directory, not a symlink"
        )
    directories: dict[str, os.stat_result] = {}
    files: dict[str, os.stat_result] = {}
    try:
        children = sorted(root.iterdir(), key=lambda item: item.name)
    except OSError as error:
        raise SourceIdentityError("could not enumerate workspace root") from error
    for child in children:
        relative = pathlib.Path(child.name)
        try:
            metadata = child.lstat()
        except FileNotFoundError as error:
            raise SourceIdentityError(
                f"source membership changed during traversal: {relative}"
            ) from error
        if stat.S_ISLNK(metadata.st_mode):
            raise SourceIdentityError(
                f"top-level source entry is a symlink: {relative}"
            )
        if child.name in EXCLUDED_DIRECTORY_NAMES:
            if child.name == ".git" and stat.S_ISREG(metadata.st_mode):
                continue
            if not stat.S_ISDIR(metadata.st_mode):
                raise SourceIdentityError(
                    f"excluded directory name is not a directory: {relative}"
                )
            continue
        if child.name in INCLUDED_ROOT_FILES:
            files[relative.as_posix()] = _lstat_regular(
                child, description="declared root source file"
            )
            continue
        if child.name in INCLUDED_ROOT_DIRECTORIES:
            if not stat.S_ISDIR(metadata.st_mode):
                raise SourceIdentityError(
                    f"declared source root is not a directory: {relative}"
                )
            _snapshot_declared_directory(root, child, directories, files)
            continue
        if _is_excluded_file(relative):
            _lstat_regular(child, description="excluded root file")
            continue
        raise SourceIdentityError(
            f"unknown top-level input is outside the declared Docker context: {relative}"
        )
    return _SourceSnapshot(directories=directories, files=files)


def _compare_source_snapshots(before: _SourceSnapshot, after: _SourceSnapshot) -> None:
    if before.directories.keys() != after.directories.keys() or (
        before.files.keys() != after.files.keys()
    ):
        raise SourceIdentityError("source membership changed while hashing")
    for relative, original in before.directories.items():
        current = after.directories[relative]
        if any(
            getattr(current, field) != getattr(original, field)
            for field in _DIRECTORY_IDENTITY_FIELDS
        ):
            raise SourceIdentityError(
                f"source directory metadata changed while hashing: {relative}"
            )
    for relative, original in before.files.items():
        current = after.files[relative]
        if any(
            getattr(current, field) != getattr(original, field)
            for field in _FILE_IDENTITY_FIELDS
        ):
            raise SourceIdentityError(
                f"source file metadata changed while hashing: {relative}"
            )


def source_files(workspace: pathlib.Path) -> Iterator[pathlib.Path]:
    """Yield every declared source file in canonical path order."""

    root = workspace if workspace.is_absolute() else workspace.absolute()
    snapshot = _source_snapshot(root)
    for relative in sorted(snapshot.files):
        yield root / relative


def source_directories(
    workspace: pathlib.Path,
) -> Iterator[tuple[pathlib.Path, int, int, int]]:
    """Yield declared source directories and exact modes in path order."""

    root = workspace if workspace.is_absolute() else workspace.absolute()
    snapshot = _source_snapshot(root)
    for relative, metadata in sorted(snapshot.directories.items()):
        yield (
            root / relative,
            stat.S_IMODE(metadata.st_mode),
            metadata.st_dev,
            metadata.st_ino,
        )


def _metadata_identity(metadata: os.stat_result, *, file: bool) -> dict[str, Any]:
    fields = _FILE_IDENTITY_FIELDS if file else _DIRECTORY_IDENTITY_FIELDS
    return {field.removeprefix("st_"): getattr(metadata, field) for field in fields}


def _workspace_source_capture(
    workspace: pathlib.Path,
) -> tuple[dict[str, Any], dict[str, Any]]:
    root = workspace
    if not root.is_absolute():
        root = root.absolute()
    root_metadata = root.lstat()
    if stat.S_ISLNK(root_metadata.st_mode):
        raise SourceIdentityError("workspace path must not be a symlink")
    root = root.resolve(strict=True)
    root_before = root.lstat()

    aggregate = hashlib.sha256()
    aggregate.update(b"pmux-source-v2\0")
    directory_entries: list[dict[str, Any]] = []
    snapshot_before = _source_snapshot(root)
    for relative_text, metadata in sorted(snapshot_before.directories.items()):
        mode = stat.S_IMODE(metadata.st_mode)
        relative = relative_text.encode("utf-8")
        aggregate.update(b"D")
        aggregate.update(len(relative).to_bytes(4, "big"))
        aggregate.update(relative)
        aggregate.update(mode.to_bytes(4, "big"))
        directory_entries.append({"path": relative_text, "mode": f"{mode:04o}"})
    entries: list[dict[str, Any]] = []
    for relative_text, before in sorted(snapshot_before.files.items()):
        path = root / relative_text
        relative = relative_text.encode("utf-8")
        data, metadata = _read_stable_file(path, before)
        mode = stat.S_IMODE(metadata.st_mode)
        content_sha256 = hashlib.sha256(data).hexdigest()
        aggregate.update(b"F")
        aggregate.update(len(relative).to_bytes(4, "big"))
        aggregate.update(relative)
        aggregate.update(mode.to_bytes(4, "big"))
        aggregate.update(len(data).to_bytes(8, "big"))
        aggregate.update(bytes.fromhex(content_sha256))
        entries.append(
            {
                "path": relative_text,
                "size": len(data),
                "mode": f"{mode:04o}",
                "sha256": content_sha256,
            }
        )
    snapshot_after = _source_snapshot(root)
    _compare_source_snapshots(snapshot_before, snapshot_after)
    root_after = root.lstat()
    root_fields = _DIRECTORY_IDENTITY_FIELDS
    if any(
        getattr(root_after, field) != getattr(root_before, field)
        for field in root_fields
    ):
        raise SourceIdentityError("workspace root changed while hashing")
    manifest = {
        "schema_version": 1,
        "algorithm": SOURCE_ALGORITHM,
        "workspace_source_sha256": aggregate.hexdigest(),
        "workspace_file_count": len(entries),
        "workspace_directory_count": len(directory_entries),
        "directories": directory_entries,
        "files": entries,
    }
    filesystem_identity = {
        "root": _metadata_identity(root_after, file=False),
        "directories": [
            {
                "path": relative,
                "identity": _metadata_identity(metadata, file=False),
            }
            for relative, metadata in sorted(snapshot_after.directories.items())
        ],
        "files": [
            {
                "path": relative,
                "identity": _metadata_identity(metadata, file=True),
            }
            for relative, metadata in sorted(snapshot_after.files.items())
        ],
    }
    return manifest, filesystem_identity


def workspace_source_manifest(workspace: pathlib.Path) -> dict[str, Any]:
    """Return the complete canonical source manifest and aggregate digest."""

    manifest, _identity = _workspace_source_capture(workspace)
    return manifest


def workspace_source_guard(workspace: pathlib.Path) -> dict[str, Any]:
    """Capture canonical bytes plus transient filesystem identity for one host."""

    manifest, filesystem_identity = _workspace_source_capture(workspace)
    return {
        "schema_version": 1,
        "manifest": manifest,
        "filesystem_identity": filesystem_identity,
    }


def workspace_source_digest(workspace: pathlib.Path) -> tuple[str, int]:
    """Compatibility tuple used by evidence callers."""

    manifest = workspace_source_manifest(workspace)
    return manifest["workspace_source_sha256"], manifest["workspace_file_count"]


def _git_command_argv(
    git: pathlib.Path, workspace: pathlib.Path, arguments: Sequence[str]
) -> list[str]:
    return [
        str(git),
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "diff.external=",
        "-c",
        "core.attributesfile=/dev/null",
        "-c",
        "core.excludesfile=/dev/null",
        "-C",
        str(workspace),
        *arguments,
    ]


def _git_command_environment(git: pathlib.Path) -> dict[str, str]:
    return {
        "HOME": "/nonexistent-pmux-git-home",
        "PATH": str(git.parent),
        "GIT_ATTR_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_SYSTEM": "/dev/null",
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_TERMINAL_PROMPT": "0",
        "LANG": "C",
        "LC_ALL": "C",
    }


def _bounded_git_command(
    git: pathlib.Path,
    workspace: pathlib.Path,
    *arguments: str,
    maximum_stdout_bytes: int = MAX_GIT_OUTPUT_BYTES,
) -> GitCommandOutcome:
    """Run one configuration-isolated Git query with strict output/time bounds."""

    if type(maximum_stdout_bytes) is not int or not 1 <= maximum_stdout_bytes <= (
        MAX_GIT_OUTPUT_BYTES
    ):
        raise SourceIdentityError("Git stdout bound is invalid")
    command = _git_command_argv(git, workspace, arguments)
    environment = _git_command_environment(git)
    try:
        witness = bounded_process.bind_executable(git)
        result = bounded_process.run(
            witness,
            [witness.path, *command[1:]],
            cwd=workspace,
            environment=environment,
            timeout_seconds=GIT_COMMAND_TIMEOUT_SECONDS,
            drain_timeout_seconds=5,
            maximum_output_bytes=maximum_stdout_bytes + 64 * 1024,
            description="Git identity query",
        )
    except bounded_process.BoundedProcessError as error:
        raise SourceIdentityError(
            f"Git identity query was not bounded: {error}"
        ) from error
    if len(result.stdout) > maximum_stdout_bytes:
        raise SourceIdentityError(
            f"Git stdout exceeded its {maximum_stdout_bytes}-byte bound"
        )
    if result.stderr:
        raise SourceIdentityError("Git identity query emitted unexpected stderr")
    return_code = result.exit_code
    if return_code not in {0, 1}:
        diagnostic = result.stderr[:1024].decode("utf-8", errors="replace")
        raise SourceIdentityError(
            f"Git identity query failed with status {return_code}: {diagnostic}"
        )
    return GitCommandOutcome(
        exit_code=return_code,
        stdout=result.stdout,
        receipt=bounded_process.validate_execution_receipt(result.receipt),
    )


def _default_git_command_runner(
    git: pathlib.Path,
    workspace: pathlib.Path,
    arguments: tuple[str, ...],
    maximum_stdout_bytes: int,
) -> GitCommandOutcome:
    return _bounded_git_command(
        git,
        workspace,
        *arguments,
        maximum_stdout_bytes=maximum_stdout_bytes,
    )


def _git_executable_identity(git: pathlib.Path) -> dict[str, Any]:
    if not git.is_absolute():
        raise SourceIdentityError("Git executable path must be absolute")
    before = _lstat_regular(git, description="Git executable")
    if before.st_size < 1 or before.st_size > MAX_GIT_EXECUTABLE_BYTES:
        raise SourceIdentityError("Git executable size is outside its exact bound")
    payload, metadata = _read_stable_file(git, before)
    if stat.S_IMODE(metadata.st_mode) & 0o111 == 0:
        raise SourceIdentityError("Git executable is not executable")
    return {
        "path": str(git),
        "size": len(payload),
        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
        "sha256": hashlib.sha256(payload).hexdigest(),
    }


def _canonical_absolute_path_text(value: Any, *, description: str) -> pathlib.Path:
    if not isinstance(value, str):
        raise SourceIdentityError(f"{description} is not an exact path string")
    path = pathlib.Path(value)
    if (
        not path.is_absolute()
        or any(component in {"", ".", ".."} for component in path.parts[1:])
        or str(path) != value
    ):
        raise SourceIdentityError(f"{description} is not a canonical absolute path")
    return path


def _valid_git_ref(value: Any) -> bool:
    if not isinstance(value, str) or not value.startswith("refs/"):
        return False
    if (
        value.endswith(("/", ".", ".lock"))
        or "//" in value
        or ".." in value
        or "@{" in value
        or any(character in value for character in "\x00\x20~^:?*[\\\r\n")
    ):
        return False
    parts = pathlib.PurePosixPath(value).parts
    return all(
        part not in {"", ".", ".."} and not part.startswith(".") for part in parts
    )


def _control_node_identity(
    path: pathlib.Path, *, required: bool, description: str
) -> dict[str, Any] | None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        if not required:
            return None
        raise SourceIdentityError(f"{description} is missing: {path}")
    if stat.S_ISLNK(metadata.st_mode):
        raise SourceIdentityError(f"{description} is a symlink: {path}")
    common = {
        "path": str(path),
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "uid": metadata.st_uid,
        "gid": metadata.st_gid,
        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
        "nlink": metadata.st_nlink,
    }
    if stat.S_ISDIR(metadata.st_mode):
        # A DIRECTORY carries no timestamps here, and that is the fix for an
        # identity that included something which is not identity.
        #
        # `st_mtime_ns`/`st_ctime_ns` on a directory record when its ENTRY SET
        # last changed, which is not a property of the directory -- it is a
        # property of whatever last created or removed a name inside it. Every
        # reader-shaped Git command does exactly that: `git status` creates
        # `.git/index.lock` and unlinks it again even when it writes no index.
        # MEASURED on this host 2026-08-06: an external workspace poller adds
        # and removes that lock in two bursts about 130 ms apart every ~6 s, so
        # the `.git` directory's mtime moved 14 times in 30 s with nothing of
        # ours running, and the snapshot comparison at
        # `workspace_revision_capture` -- whose window is ~380 ms -- aborted a
        # capture 1 time in 20 on a tree whose `git status --porcelain` was
        # byte-stable throughout. Through `phase0_lib.observe_source_identity`,
        # which takes two captures around a whole source manifest, that surfaced
        # as `gate_f/phase0_self_tests` failing 2 of 12 isolated runs.
        #
        # What identifies a directory is `(device, inode)` plus its mode and
        # ownership, all of which are still here. What a directory CONTAINS is
        # identified by the entries this snapshot already binds one by one --
        # `files`, `directories`, `shared_indexes` -- each with its own
        # `(device, inode)`, and each regular file additionally by `sha256`. So
        # nothing this dropped was load-bearing: a mutation that slipped through
        # would have to leave every bound entry byte-identical, in which case
        # every claim the capture makes about the repository is still true.
        #
        # The two fields are dropped rather than merely excluded from the
        # comparison, because a field named `identity` that no consumer may
        # compare is an invitation to compare it. `phase0_lib.py:1176-1197`
        # narrowed a SECOND, wider window the same way and named this line as
        # the follow-up it did not take; this is that follow-up.
        return {**common, "kind": "directory"}
    common["mtime_ns"] = str(metadata.st_mtime_ns)
    common["ctime_ns"] = str(metadata.st_ctime_ns)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise SourceIdentityError(f"{description} is not one exact regular file")
    if metadata.st_size > MAX_GIT_CONTROL_FILE_BYTES:
        raise SourceIdentityError(f"{description} exceeds its exact byte bound")
    payload, final = _read_stable_file(path, metadata)
    if len(payload) != final.st_size:
        raise SourceIdentityError(f"{description} changed size while reading")
    return {
        **common,
        "kind": "file",
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }


def _control_file_bytes(path: pathlib.Path, *, description: str) -> bytes:
    metadata = _lstat_regular(path, description=description)
    if metadata.st_size > MAX_GIT_CONTROL_FILE_BYTES:
        raise SourceIdentityError(f"{description} exceeds its exact byte bound")
    payload, _final = _read_stable_file(path, metadata)
    return payload


def _resolve_control_directory(
    base: pathlib.Path, raw: str, *, description: str
) -> pathlib.Path:
    if "\x00" in raw or "\r" in raw or "\n" in raw:
        raise SourceIdentityError(f"{description} contains unsafe control bytes")
    candidate = pathlib.Path(raw)
    if not candidate.is_absolute():
        candidate = base / candidate
    try:
        resolved = candidate.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise SourceIdentityError(
            f"{description} cannot be resolved exactly"
        ) from error
    metadata = resolved.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise SourceIdentityError(f"{description} is not a real directory")
    return resolved


def _reject_external_config_includes(
    path: pathlib.Path, *, required: bool, description: str
) -> None:
    """Reject repository config that can import an unbound external input."""

    try:
        path.lstat()
    except FileNotFoundError:
        if required:
            raise SourceIdentityError(f"{description} is missing: {path}")
        return
    payload = _control_file_bytes(path, description=description)
    if re.search(
        rb"(?im)^[ \t]*\[[ \t]*include(?:if)?(?:[ \t\"]|\])",
        payload,
    ):
        raise SourceIdentityError(
            f"{description} uses unsupported external include configuration"
        )


def _shared_index_identities(git_dir: pathlib.Path) -> dict[str, dict[str, Any]]:
    """Bind every possible split-index backing file in the exact Git directory."""

    try:
        names = sorted(
            entry.name
            for entry in os.scandir(git_dir)
            if re.fullmatch(
                r"sharedindex\.[0-9a-f]{40}|sharedindex\.[0-9a-f]{64}", entry.name
            )
        )
    except OSError as error:
        raise SourceIdentityError("Git directory could not be enumerated") from error
    if len(names) > MAX_GIT_SHARED_INDEX_FILES:
        raise SourceIdentityError("Git shared-index membership exceeds its exact bound")
    result: dict[str, dict[str, Any]] = {}
    aggregate_size = 0
    for name in names:
        identity = _control_node_identity(
            git_dir / name,
            required=True,
            description="Git shared-index backing file",
        )
        if identity is None or identity["kind"] != "file":
            raise SourceIdentityError("Git shared-index backing node is not a file")
        aggregate_size += identity["size"]
        if aggregate_size > MAX_GIT_CONTROL_FILE_BYTES:
            raise SourceIdentityError(
                "Git shared-index backing files exceed their cumulative byte bound"
            )
        result[name] = identity
    return result


def _repository_control_snapshot(workspace: pathlib.Path) -> dict[str, Any]:
    """Capture host-only Git control inputs before/after all revision queries."""

    control = workspace / ".git"
    control_metadata = control.lstat()
    worktree_control = _control_node_identity(
        control, required=True, description="workspace Git control entry"
    )
    if stat.S_ISDIR(control_metadata.st_mode):
        git_dir = control.resolve(strict=True)
    elif stat.S_ISREG(control_metadata.st_mode):
        payload = _control_file_bytes(control, description="workspace Git control file")
        try:
            text = payload.decode("utf-8")
        except UnicodeDecodeError as error:
            raise SourceIdentityError(
                "workspace Git control file is not UTF-8"
            ) from error
        if (
            not text.endswith("\n")
            or text.count("\n") != 1
            or not text.startswith("gitdir: ")
        ):
            raise SourceIdentityError("workspace Git control file is malformed")
        git_dir = _resolve_control_directory(
            workspace,
            text.removeprefix("gitdir: ").removesuffix("\n"),
            description="workspace Git directory",
        )
    else:
        raise SourceIdentityError("workspace Git control entry has an unsupported type")

    common_marker = git_dir / "commondir"
    try:
        common_marker.lstat()
        common_marker_present = True
    except FileNotFoundError:
        common_marker_present = False
    if common_marker_present:
        marker_payload = _control_file_bytes(
            common_marker, description="Git common-directory marker"
        )
        try:
            marker = marker_payload.decode("utf-8").strip()
        except UnicodeDecodeError as error:
            raise SourceIdentityError(
                "Git common-directory marker is not UTF-8"
            ) from error
        common_dir = _resolve_control_directory(
            git_dir, marker, description="Git common directory"
        )
    else:
        common_dir = git_dir

    config_path = common_dir / "config"
    config_worktree_path = git_dir / "config.worktree"
    _reject_external_config_includes(
        config_path, required=False, description="Git repository config"
    )
    _reject_external_config_includes(
        config_worktree_path,
        required=False,
        description="Git worktree config",
    )

    head_path = git_dir / "HEAD"
    head_payload = _control_file_bytes(head_path, description="Git HEAD control file")
    try:
        head_text = head_payload.decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise SourceIdentityError("Git HEAD control file is not UTF-8") from error
    head_ref_path: pathlib.Path | None = None
    if head_text.startswith("ref: "):
        head_ref = head_text.removeprefix("ref: ")
        if not _valid_git_ref(head_ref):
            raise SourceIdentityError("Git HEAD control ref is malformed")
        head_ref_path = common_dir / pathlib.PurePosixPath(head_ref)

    files = {
        "head": _control_node_identity(
            head_path, required=True, description="Git HEAD"
        ),
        "index": _control_node_identity(
            git_dir / "index", required=False, description="Git index"
        ),
        "commondir": _control_node_identity(
            common_marker, required=False, description="Git common-directory marker"
        ),
        "config": _control_node_identity(
            config_path, required=False, description="Git repository config"
        ),
        "config_worktree": _control_node_identity(
            config_worktree_path,
            required=False,
            description="Git worktree config",
        ),
        "info_exclude": _control_node_identity(
            common_dir / "info" / "exclude",
            required=False,
            description="Git repository exclude file",
        ),
        "info_attributes": _control_node_identity(
            common_dir / "info" / "attributes",
            required=False,
            description="Git repository attributes file",
        ),
        "sparse_checkout": _control_node_identity(
            git_dir / "info" / "sparse-checkout",
            required=False,
            description="Git sparse-checkout file",
        ),
        "packed_refs": _control_node_identity(
            common_dir / "packed-refs", required=False, description="Git packed refs"
        ),
        "head_ref": (
            _control_node_identity(
                head_ref_path, required=False, description="Git loose HEAD ref"
            )
            if head_ref_path is not None
            else None
        ),
    }
    directories = {
        "objects": _control_node_identity(
            common_dir / "objects", required=True, description="Git objects directory"
        ),
        "refs": _control_node_identity(
            common_dir / "refs", required=True, description="Git refs directory"
        ),
    }
    shared_indexes = _shared_index_identities(git_dir)
    if shared_indexes:
        raise SourceIdentityError(
            "Git split-index mode is unsupported for revision provenance"
        )
    return {
        "workspace_root": _control_node_identity(
            workspace, required=True, description="workspace root"
        ),
        "worktree_control": worktree_control,
        "git_dir": _control_node_identity(
            git_dir, required=True, description="Git directory"
        ),
        "git_dir_parent": _control_node_identity(
            git_dir.parent, required=True, description="Git-directory parent"
        ),
        "common_dir": _control_node_identity(
            common_dir, required=True, description="Git common directory"
        ),
        "common_dir_parent": _control_node_identity(
            common_dir.parent, required=True, description="Git common-directory parent"
        ),
        "files": files,
        "shared_indexes": shared_indexes,
        "directories": directories,
    }


def _strict_git_text_line(payload: bytes, *, encoding: str, description: str) -> str:
    if (
        not payload.endswith(b"\n")
        or payload.count(b"\n") != 1
        or b"\r" in payload
        or b"\0" in payload
    ):
        raise SourceIdentityError(f"{description} is not one exact line")
    try:
        text = payload[:-1].decode(encoding)
    except UnicodeDecodeError as error:
        raise SourceIdentityError(f"{description} is not {encoding}") from error
    if not text:
        raise SourceIdentityError(f"{description} is empty")
    return text


def _revision_capture_sha256(payload: Mapping[str, Any]) -> str:
    try:
        encoded = json.dumps(
            payload,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise SourceIdentityError(
            "workspace revision capture is not canonical JSON"
        ) from error
    digest = hashlib.sha256()
    digest.update(b"pmux-canonical-json-sha256-v1\0")
    digest.update(REVISION_CAPTURE_DOMAIN.encode("ascii"))
    digest.update(b"\0")
    digest.update(encoded)
    return digest.hexdigest()


def workspace_revision_capture(
    workspace: pathlib.Path,
    *,
    git_executable: pathlib.Path | None = None,
    command_runner: GitCommandRunner | None = None,
) -> dict[str, Any]:
    """Capture host Git facts and the five exact bounded receipts that caused them."""

    root = workspace if workspace.is_absolute() else workspace.absolute()
    if root.is_symlink():
        raise SourceIdentityError("workspace path must not be a symlink")
    root = root.resolve(strict=True)
    discovered = shutil.which("git") if git_executable is None else str(git_executable)
    if not discovered:
        raise SourceIdentityError("Git executable is unavailable")
    git = pathlib.Path(discovered)
    if not git.is_absolute():
        git = git.absolute()
    git = git.resolve(strict=True)
    bounded_process_implementation = _revalidate_bounded_process_authority()
    repository_control = _repository_control_snapshot(root)
    git_identity = _git_executable_identity(git)
    run = _default_git_command_runner if command_runner is None else command_runner

    outcomes: dict[str, GitCommandOutcome] = {}
    for query in _GIT_QUERY_SPECS:
        outcome = run(git, root, query.arguments, query.maximum_stdout_bytes)
        if not isinstance(outcome, GitCommandOutcome):
            raise SourceIdentityError(
                "Git command runner returned an unreceipted result"
            )
        try:
            receipt = bounded_process.validate_execution_receipt(outcome.receipt)
        except bounded_process.BoundedProcessError as error:
            raise SourceIdentityError("Git command receipt is invalid") from error
        if (
            type(outcome.exit_code) is not int
            or type(outcome.stdout) is not bytes
            or outcome.exit_code != receipt["exit_code"]
            or len(outcome.stdout) != receipt["stdout_size"]
            or hashlib.sha256(outcome.stdout).hexdigest() != receipt["stdout_sha256"]
            or receipt["stderr_size"] != 0
            or receipt["stderr_sha256"] != hashlib.sha256(b"").hexdigest()
        ):
            raise SourceIdentityError("Git command result disagrees with its receipt")
        if outcome.exit_code not in query.allowed_exit_codes:
            raise SourceIdentityError(
                f"Git {query.label} query failed with status {outcome.exit_code}"
            )
        outcomes[query.label] = GitCommandOutcome(
            exit_code=outcome.exit_code,
            stdout=outcome.stdout,
            receipt=receipt,
        )

    head_outcome = outcomes["head"]
    head = _strict_git_text_line(
        head_outcome.stdout, encoding="ascii", description="workspace HEAD"
    )
    if not re_fullmatch_git_object_id(head):
        raise SourceIdentityError("workspace HEAD is not one exact object ID")

    symbolic_outcome = outcomes["symbolic_head"]
    if symbolic_outcome.exit_code == 0:
        head_ref = _strict_git_text_line(
            symbolic_outcome.stdout,
            encoding="utf-8",
            description="workspace symbolic HEAD",
        )
        if not _valid_git_ref(head_ref):
            raise SourceIdentityError("workspace symbolic HEAD is malformed")
        branch = (
            head_ref.removeprefix("refs/heads/")
            if head_ref.startswith("refs/heads/")
            else None
        )
        detached = False
    else:
        if symbolic_outcome.stdout:
            raise SourceIdentityError("detached symbolic-ref emitted unexpected output")
        head_ref = None
        branch = None
        detached = True

    status_payload = outcomes["status_porcelain_v1_z"].stdout
    diff_payload = outcomes["tracked_binary_diff"].stdout

    implementation = root / "tools" / "linux-docker" / "source_digest.py"
    implementation_before = _lstat_regular(
        implementation, description="source-digest implementation"
    )
    implementation_bytes, _metadata = _read_stable_file(
        implementation, implementation_before
    )
    git_version = _strict_git_text_line(
        outcomes["version"].stdout,
        encoding="utf-8",
        description="Git version",
    )
    if not git_version.startswith("git version ") or any(
        character in git_version for character in "\x00\r\n"
    ):
        raise SourceIdentityError("Git version output is malformed")
    if _git_executable_identity(git) != git_identity:
        raise SourceIdentityError("Git executable changed during revision capture")
    if _repository_control_snapshot(root) != repository_control:
        raise SourceIdentityError(
            "Git repository control identity changed during capture"
        )
    if _revalidate_bounded_process_authority() != bounded_process_implementation:
        raise SourceIdentityError(
            "shared bounded-process authority changed during revision capture"
        )

    identity = {
        "schema_version": 1,
        "algorithm": REVISION_ALGORITHM,
        "workspace": str(root),
        "head": head,
        "head_ref": head_ref,
        "branch": branch,
        "detached": detached,
        "status_porcelain_v1_z_sha256": hashlib.sha256(status_payload).hexdigest(),
        "status_porcelain_v1_z_bytes": len(status_payload),
        "tracked_binary_diff_sha256": hashlib.sha256(diff_payload).hexdigest(),
        "tracked_binary_diff_bytes": len(diff_payload),
        "source_digest_implementation": {
            "path": "tools/linux-docker/source_digest.py",
            "sha256": hashlib.sha256(implementation_bytes).hexdigest(),
        },
        "git": {**git_identity, "version": git_version},
        "repository_control": repository_control,
    }
    normalized_identity = validate_workspace_revision_identity(identity)
    body = {
        "schema_version": 1,
        "algorithm": REVISION_CAPTURE_ALGORITHM,
        "identity": normalized_identity,
        "bounded_process_implementation": bounded_process_implementation,
        "commands": [
            {"label": query.label, "receipt": dict(outcomes[query.label].receipt)}
            for query in _GIT_QUERY_SPECS
        ],
    }
    capture = {**body, "capture_sha256": _revision_capture_sha256(body)}
    return validate_workspace_revision_capture(capture)


def workspace_revision_identity(
    workspace: pathlib.Path,
    *,
    git_executable: pathlib.Path | None = None,
    command_runner: GitCommandRunner | None = None,
) -> dict[str, Any]:
    """Compatibility view of host revision facts, separate from portable source."""

    return workspace_revision_capture(
        workspace,
        git_executable=git_executable,
        command_runner=command_runner,
    )["identity"]


def re_fullmatch_git_object_id(value: Any) -> bool:
    return (
        isinstance(value, str)
        and re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", value) is not None
    )


def _validate_control_node(value: Any, *, description: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise SourceIdentityError(f"{description} identity is not an object")
    common_fields = {
        "path",
        "kind",
        "device",
        "inode",
        "uid",
        "gid",
        "mode",
        "nlink",
    }
    # Timestamps are a FILE field. See `_control_node_identity`: a directory's
    # mtime/ctime record when its entry set last moved, which any concurrent
    # `git status` does, so they are not identity and are not recorded. This
    # schema is exact in both directions, so a producer that started emitting
    # them on a directory again would be refused here rather than silently
    # reintroducing the abort.
    timestamp_fields = {"mtime_ns", "ctime_ns"}
    kind = value.get("kind")
    if kind == "directory":
        expected_fields = common_fields
    elif kind == "file":
        expected_fields = common_fields | timestamp_fields | {"size", "sha256"}
    else:
        expected_fields = set()
    if not expected_fields or set(value) != expected_fields:
        raise SourceIdentityError(f"{description} identity schema is not exact")
    _canonical_absolute_path_text(value.get("path"), description=f"{description} path")
    if (
        not isinstance(value.get("mode"), str)
        or re.fullmatch(r"0[0-7]{3}", value["mode"]) is None
    ):
        raise SourceIdentityError(f"{description} mode is invalid")
    for field in (
        "device",
        "inode",
        "uid",
        "gid",
        "nlink",
    ):
        if (
            type(value.get(field)) is not int
            or value[field] < 0
            or value[field] > MAX_SAFE_INTEGER
        ):
            raise SourceIdentityError(f"{description} integer is invalid: {field}")
    for field in sorted(timestamp_fields & expected_fields):
        if (
            not isinstance(value.get(field), str)
            or re.fullmatch(r"0|[1-9][0-9]*", value[field]) is None
        ):
            raise SourceIdentityError(f"{description} timestamp is invalid: {field}")
    if kind == "file" and (
        type(value.get("size")) is not int
        or value["size"] < 0
        or value["size"] > MAX_GIT_CONTROL_FILE_BYTES
        or not isinstance(value.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", value["sha256"]) is None
    ):
        raise SourceIdentityError(f"{description} file identity is invalid")
    return dict(value)


def _validate_repository_control(
    value: Any, *, workspace: pathlib.Path
) -> dict[str, Any]:
    top_fields = {
        "workspace_root",
        "worktree_control",
        "git_dir",
        "git_dir_parent",
        "common_dir",
        "common_dir_parent",
        "files",
        "shared_indexes",
        "directories",
    }
    if not isinstance(value, Mapping) or set(value) != top_fields:
        raise SourceIdentityError("Git repository-control schema is not exact")
    result: dict[str, Any] = {}
    for field in (
        "workspace_root",
        "worktree_control",
        "git_dir",
        "git_dir_parent",
        "common_dir",
        "common_dir_parent",
    ):
        result[field] = _validate_control_node(
            value[field], description=f"Git repository-control {field}"
        )
    if result["workspace_root"]["path"] != str(workspace):
        raise SourceIdentityError("Git repository-control workspace path disagrees")
    if result["worktree_control"]["path"] != str(workspace / ".git"):
        raise SourceIdentityError("Git repository-control entry path disagrees")
    for field in (
        "workspace_root",
        "git_dir",
        "git_dir_parent",
        "common_dir",
        "common_dir_parent",
    ):
        if result[field]["kind"] != "directory":
            raise SourceIdentityError(
                f"Git repository-control {field} is not a directory"
            )
    git_dir = pathlib.Path(result["git_dir"]["path"])
    common_dir = pathlib.Path(result["common_dir"]["path"])
    if result["git_dir_parent"]["path"] != str(git_dir.parent):
        raise SourceIdentityError("Git-directory parent path disagrees")
    if result["common_dir_parent"]["path"] != str(common_dir.parent):
        raise SourceIdentityError("Git common-directory parent path disagrees")
    if result["worktree_control"]["kind"] == "directory":
        if result["worktree_control"]["path"] != str(git_dir):
            raise SourceIdentityError("Git directory control path disagrees")
    elif result["worktree_control"]["kind"] != "file":
        raise SourceIdentityError("Git worktree control entry has invalid type")
    files = value.get("files")
    expected_files = {
        "head",
        "index",
        "commondir",
        "config",
        "config_worktree",
        "info_exclude",
        "info_attributes",
        "sparse_checkout",
        "packed_refs",
        "head_ref",
    }
    if not isinstance(files, Mapping) or set(files) != expected_files:
        raise SourceIdentityError("Git repository-control file schema is not exact")
    result_files: dict[str, Any] = {}
    for name in sorted(expected_files):
        item = files[name]
        if item is None:
            result_files[name] = None
            continue
        validated = _validate_control_node(
            item, description=f"Git repository-control file {name}"
        )
        if validated["kind"] != "file":
            raise SourceIdentityError(
                f"Git repository-control file {name} is not a file"
            )
        result_files[name] = validated
    if result_files["head"] is None:
        raise SourceIdentityError("Git repository-control HEAD is missing")
    exact_file_paths = {
        "head": git_dir / "HEAD",
        "index": git_dir / "index",
        "commondir": git_dir / "commondir",
        "config": common_dir / "config",
        "config_worktree": git_dir / "config.worktree",
        "info_exclude": common_dir / "info" / "exclude",
        "info_attributes": common_dir / "info" / "attributes",
        "sparse_checkout": git_dir / "info" / "sparse-checkout",
        "packed_refs": common_dir / "packed-refs",
    }
    for name, expected_path in exact_file_paths.items():
        item = result_files[name]
        if item is not None and item["path"] != str(expected_path):
            raise SourceIdentityError(
                f"Git repository-control file path disagrees: {name}"
            )
    head_ref = result_files["head_ref"]
    if head_ref is not None:
        head_ref_path = pathlib.Path(head_ref["path"])
        try:
            head_ref_relative = head_ref_path.relative_to(common_dir)
        except ValueError as error:
            raise SourceIdentityError(
                "Git loose HEAD ref escapes common directory"
            ) from error
        if (
            len(head_ref_relative.parts) < 2
            or head_ref_relative.parts[0] != "refs"
            or not _valid_git_ref(head_ref_relative.as_posix())
        ):
            raise SourceIdentityError("Git loose HEAD ref path is invalid")
    shared_indexes = value.get("shared_indexes")
    if (
        not isinstance(shared_indexes, Mapping)
        or len(shared_indexes) > MAX_GIT_SHARED_INDEX_FILES
    ):
        raise SourceIdentityError("Git shared-index schema is not exact")
    if shared_indexes:
        raise SourceIdentityError(
            "Git split-index mode is unsupported for revision provenance"
        )
    result_shared_indexes: dict[str, Any] = {}
    aggregate_shared_size = 0
    for name in sorted(shared_indexes):
        if (
            not isinstance(name, str)
            or re.fullmatch(
                r"sharedindex\.[0-9a-f]{40}|sharedindex\.[0-9a-f]{64}", name
            )
            is None
        ):
            raise SourceIdentityError("Git shared-index name is invalid")
        validated = _validate_control_node(
            shared_indexes[name], description=f"Git shared-index {name}"
        )
        if validated["kind"] != "file" or validated["path"] != str(git_dir / name):
            raise SourceIdentityError("Git shared-index identity is invalid")
        aggregate_shared_size += validated["size"]
        if aggregate_shared_size > MAX_GIT_CONTROL_FILE_BYTES:
            raise SourceIdentityError(
                "Git shared-index identities exceed their cumulative byte bound"
            )
        result_shared_indexes[name] = validated
    directories = value.get("directories")
    expected_directories = {"objects", "refs"}
    if not isinstance(directories, Mapping) or set(directories) != expected_directories:
        raise SourceIdentityError(
            "Git repository-control directory schema is not exact"
        )
    result_directories: dict[str, Any] = {}
    for name in sorted(expected_directories):
        validated = _validate_control_node(
            directories[name],
            description=f"Git repository-control directory {name}",
        )
        if validated["kind"] != "directory":
            raise SourceIdentityError(
                f"Git repository-control directory {name} is not a directory"
            )
        result_directories[name] = validated
    for name, expected_path in {
        "objects": common_dir / "objects",
        "refs": common_dir / "refs",
    }.items():
        if result_directories[name]["path"] != str(expected_path):
            raise SourceIdentityError(
                f"Git repository-control directory path disagrees: {name}"
            )
    result["files"] = result_files
    result["shared_indexes"] = result_shared_indexes
    result["directories"] = result_directories
    return result


def validate_workspace_revision_identity(
    identity: Mapping[str, Any],
    *,
    workspace: pathlib.Path | None = None,
    command_runner: GitCommandRunner | None = None,
) -> dict[str, Any]:
    """Validate the exact revision schema and optionally reobserve the workspace."""

    expected_fields = {
        "schema_version",
        "algorithm",
        "workspace",
        "head",
        "head_ref",
        "branch",
        "detached",
        "status_porcelain_v1_z_sha256",
        "status_porcelain_v1_z_bytes",
        "tracked_binary_diff_sha256",
        "tracked_binary_diff_bytes",
        "source_digest_implementation",
        "git",
        "repository_control",
    }
    if not isinstance(identity, Mapping) or set(identity) != expected_fields:
        raise SourceIdentityError("workspace revision identity schema is not exact")
    if (
        type(identity.get("schema_version")) is not int
        or identity["schema_version"] != 1
    ):
        raise SourceIdentityError("workspace revision schema version is invalid")
    if identity.get("algorithm") != REVISION_ALGORITHM:
        raise SourceIdentityError("workspace revision algorithm is invalid")
    workspace_path = _canonical_absolute_path_text(
        identity.get("workspace"), description="workspace revision path"
    )
    if not re_fullmatch_git_object_id(identity.get("head")):
        raise SourceIdentityError("workspace revision HEAD is invalid")
    detached = identity.get("detached")
    if type(detached) is not bool:
        raise SourceIdentityError("workspace revision detached flag is invalid")
    head_ref = identity.get("head_ref")
    branch = identity.get("branch")
    if detached:
        if head_ref is not None or branch is not None:
            raise SourceIdentityError("detached revision cannot name a ref or branch")
    else:
        if not _valid_git_ref(head_ref):
            raise SourceIdentityError("workspace revision ref is invalid")
        expected_branch = (
            head_ref.removeprefix("refs/heads/")
            if head_ref.startswith("refs/heads/")
            else None
        )
        if branch != expected_branch:
            raise SourceIdentityError(
                "workspace revision branch disagrees with its ref"
            )
    for field in ("status_porcelain_v1_z_sha256", "tracked_binary_diff_sha256"):
        if (
            not isinstance(identity.get(field), str)
            or re.fullmatch(r"[0-9a-f]{64}", identity[field]) is None
        ):
            raise SourceIdentityError(f"workspace revision digest is invalid: {field}")
    for field in ("status_porcelain_v1_z_bytes", "tracked_binary_diff_bytes"):
        if type(identity.get(field)) is not int or not 0 <= identity[field] <= min(
            MAX_SAFE_INTEGER, MAX_GIT_OUTPUT_BYTES
        ):
            raise SourceIdentityError(
                f"workspace revision byte count is invalid: {field}"
            )
    implementation = identity.get("source_digest_implementation")
    if not isinstance(implementation, Mapping) or set(implementation) != {
        "path",
        "sha256",
    }:
        raise SourceIdentityError("source-digest implementation identity is invalid")
    if (
        implementation.get("path") != "tools/linux-docker/source_digest.py"
        or not isinstance(implementation.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", implementation["sha256"]) is None
    ):
        raise SourceIdentityError("source-digest implementation binding is invalid")
    git = identity.get("git")
    if not isinstance(git, Mapping) or set(git) != {
        "path",
        "size",
        "mode",
        "sha256",
        "version",
    }:
        raise SourceIdentityError("Git executable identity schema is invalid")
    git_path = _canonical_absolute_path_text(
        git.get("path"), description="Git executable path identity"
    )
    if (
        type(git.get("size")) is not int
        or git["size"] <= 0
        or git["size"] > min(MAX_SAFE_INTEGER, MAX_GIT_EXECUTABLE_BYTES)
    ):
        raise SourceIdentityError("Git executable size identity is invalid")
    if (
        not isinstance(git.get("mode"), str)
        or re.fullmatch(r"0[0-7]{3}", git["mode"]) is None
    ):
        raise SourceIdentityError("Git executable mode identity is invalid")
    if int(git["mode"], 8) & 0o111 == 0:
        raise SourceIdentityError("Git executable mode is not executable")
    if (
        not isinstance(git.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", git["sha256"]) is None
    ):
        raise SourceIdentityError("Git executable digest identity is invalid")
    if (
        not isinstance(git.get("version"), str)
        or not git["version"].startswith("git version ")
        or any(character in git["version"] for character in "\x00\r\n")
    ):
        raise SourceIdentityError("Git version identity is invalid")
    repository_control = _validate_repository_control(
        identity.get("repository_control"), workspace=workspace_path
    )
    normalized = dict(identity)
    normalized["source_digest_implementation"] = dict(implementation)
    normalized["git"] = dict(git)
    normalized["repository_control"] = repository_control
    if workspace is not None:
        canonical_workspace = workspace.resolve(strict=True)
        if canonical_workspace != workspace_path:
            raise SourceIdentityError("workspace revision revalidation path disagrees")
        expected_git = {key: git[key] for key in ("path", "size", "mode", "sha256")}
        if _git_executable_identity(git_path) != expected_git:
            raise SourceIdentityError("Git executable identity changed")
        current = workspace_revision_identity(
            canonical_workspace,
            git_executable=git_path,
            command_runner=command_runner,
        )
        if current != normalized:
            raise SourceIdentityError("workspace revision identity changed")
    return normalized


def _git_receipt_stdout_matches(receipt: Mapping[str, Any], payload: bytes) -> bool:
    return (
        receipt["stdout_size"] == len(payload)
        and receipt["stdout_sha256"] == hashlib.sha256(payload).hexdigest()
    )


def _validate_bounded_process_implementation(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping) or set(value) != {"path", "size", "sha256"}:
        raise SourceIdentityError(
            "bounded-process implementation identity schema is not exact"
        )
    if (
        value.get("path") != BOUNDED_PROCESS_RELATIVE_PATH
        or type(value.get("size")) is not int
        or not 1 <= value["size"] <= MAX_BOUND_AUTHORITY_BYTES
        or not isinstance(value.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", value["sha256"]) is None
    ):
        raise SourceIdentityError("bounded-process implementation identity is invalid")
    return dict(value)


def validate_workspace_revision_capture(
    capture: Mapping[str, Any],
    *,
    workspace: pathlib.Path | None = None,
    command_runner: GitCommandRunner | None = None,
) -> dict[str, Any]:
    """Validate a revision fact plus the exact bounded Git executions behind it."""

    expected_fields = {
        "schema_version",
        "algorithm",
        "identity",
        "bounded_process_implementation",
        "commands",
        "capture_sha256",
    }
    if not isinstance(capture, Mapping) or set(capture) != expected_fields:
        raise SourceIdentityError("workspace revision capture schema is not exact")
    if (
        type(capture.get("schema_version")) is not int
        or capture["schema_version"] != 1
        or capture.get("algorithm") != REVISION_CAPTURE_ALGORITHM
    ):
        raise SourceIdentityError("workspace revision capture version is invalid")
    identity = validate_workspace_revision_identity(capture.get("identity"))
    bounded_process_implementation = _validate_bounded_process_implementation(
        capture.get("bounded_process_implementation")
    )
    commands = capture.get("commands")
    if not isinstance(commands, list) or len(commands) != len(_GIT_QUERY_SPECS):
        raise SourceIdentityError("workspace revision command inventory is not exact")

    workspace_path = pathlib.Path(identity["workspace"])
    git_path = pathlib.Path(identity["git"]["path"])
    expected_environment = bounded_process.environment_identity(
        _git_command_environment(git_path)
    )
    empty_sha256 = hashlib.sha256(b"").hexdigest()
    workspace_root = identity["repository_control"]["workspace_root"]
    normalized_commands: list[dict[str, Any]] = []
    common_executable: dict[str, Any] | None = None

    for row, query in zip(commands, _GIT_QUERY_SPECS, strict=True):
        if not isinstance(row, Mapping) or set(row) != {"label", "receipt"}:
            raise SourceIdentityError("workspace revision command row is not exact")
        if row.get("label") != query.label:
            raise SourceIdentityError("workspace revision command order is not exact")
        try:
            receipt = bounded_process.validate_execution_receipt(row.get("receipt"))
        except bounded_process.BoundedProcessError as error:
            raise SourceIdentityError(
                "workspace revision command receipt is invalid"
            ) from error

        expected_argv = _git_command_argv(git_path, workspace_path, query.arguments)
        standard_input = receipt["standard_input"]
        cwd_witness = receipt["cwd_witness"]
        executable = receipt["executable"]
        if (
            receipt["argv"] != expected_argv
            or receipt["cwd"] != str(workspace_path)
            or receipt["environment"] != expected_environment
            or receipt["timeout_seconds"] != GIT_COMMAND_TIMEOUT_SECONDS
            or receipt["drain_timeout_seconds"] != 5
            or receipt["maximum_output_bytes"] != query.maximum_stdout_bytes + 64 * 1024
            or receipt["exit_code"] not in query.allowed_exit_codes
            or receipt["stdout_size"] > query.maximum_stdout_bytes
            or receipt["stderr_size"] != 0
            or receipt["stderr_sha256"] != empty_sha256
            or not receipt["process_ledger"]
            or any(record["reaped"] is not True for record in receipt["process_ledger"])
            or standard_input["source"] != "none"
            or standard_input["present"] is not False
            or standard_input["size"] != 0
            or standard_input["sha256"] != empty_sha256
            or standard_input["source_descriptor"] is not None
        ):
            raise SourceIdentityError("workspace revision command binding is invalid")
        if (
            cwd_witness["path"] != workspace_root["path"]
            or cwd_witness["device"] != workspace_root["device"]
            or cwd_witness["inode"] != workspace_root["inode"]
            or cwd_witness["uid"] != workspace_root["uid"]
            or cwd_witness["gid"] != workspace_root["gid"]
            or stat.S_IMODE(cwd_witness["mode"]) != int(workspace_root["mode"], 8)
        ):
            raise SourceIdentityError(
                "workspace revision command cwd witness disagrees with repository control"
            )
        if (
            executable["path"] != identity["git"]["path"]
            or executable["size"] != identity["git"]["size"]
            or stat.S_IMODE(executable["mode"]) != int(identity["git"]["mode"], 8)
            or executable["sha256"] != identity["git"]["sha256"]
        ):
            raise SourceIdentityError(
                "workspace revision command Git executable binding is invalid"
            )
        if common_executable is None:
            common_executable = dict(executable)
        elif executable != common_executable:
            raise SourceIdentityError(
                "Git executable witness changed between revision commands"
            )

        if query.label == "head":
            output_matches = receipt["exit_code"] == 0 and _git_receipt_stdout_matches(
                receipt, identity["head"].encode("ascii") + b"\n"
            )
        elif query.label == "symbolic_head":
            if identity["detached"]:
                output_matches = receipt["exit_code"] == 1 and (
                    _git_receipt_stdout_matches(receipt, b"")
                )
            else:
                output_matches = receipt["exit_code"] == 0 and (
                    _git_receipt_stdout_matches(
                        receipt, identity["head_ref"].encode("utf-8") + b"\n"
                    )
                )
        elif query.label == "status_porcelain_v1_z":
            output_matches = receipt["exit_code"] == 0 and (
                receipt["stdout_size"] == identity["status_porcelain_v1_z_bytes"]
                and receipt["stdout_sha256"] == identity["status_porcelain_v1_z_sha256"]
            )
        elif query.label == "tracked_binary_diff":
            output_matches = receipt["exit_code"] == 0 and (
                receipt["stdout_size"] == identity["tracked_binary_diff_bytes"]
                and receipt["stdout_sha256"] == identity["tracked_binary_diff_sha256"]
            )
        else:
            output_matches = receipt["exit_code"] == 0 and (
                _git_receipt_stdout_matches(
                    receipt, identity["git"]["version"].encode("utf-8") + b"\n"
                )
            )
        if not output_matches:
            raise SourceIdentityError(
                f"workspace revision {query.label} output receipt is inconsistent"
            )
        normalized_commands.append({"label": query.label, "receipt": receipt})

    body = {
        "schema_version": 1,
        "algorithm": REVISION_CAPTURE_ALGORITHM,
        "identity": identity,
        "bounded_process_implementation": bounded_process_implementation,
        "commands": normalized_commands,
    }
    digest = capture.get("capture_sha256")
    if (
        not isinstance(digest, str)
        or re.fullmatch(r"[0-9a-f]{64}", digest) is None
        or digest != _revision_capture_sha256(body)
    ):
        raise SourceIdentityError("workspace revision capture digest does not match")
    normalized = {**body, "capture_sha256": digest}

    if workspace is not None:
        canonical_workspace = workspace.resolve(strict=True)
        if canonical_workspace != workspace_path:
            raise SourceIdentityError("workspace revision capture path disagrees")
        if _revalidate_bounded_process_authority() != bounded_process_implementation:
            raise SourceIdentityError("bounded-process implementation identity changed")
        current = workspace_revision_capture(
            canonical_workspace,
            git_executable=git_path,
            command_runner=command_runner,
        )
        if current["identity"] != identity:
            raise SourceIdentityError("workspace revision capture identity changed")
    return normalized


def validate_expected_digest(value: str) -> str:
    if not isinstance(value, str):
        raise SourceIdentityError(
            "expected source digest must be exactly 64 hexadecimal characters"
        )
    normalized = value.lower()
    if len(normalized) != 64 or any(
        character not in "0123456789abcdef" for character in normalized
    ):
        raise SourceIdentityError(
            "expected source digest must be exactly 64 hexadecimal characters"
        )
    return normalized


def _atomic_write(path: pathlib.Path, payload: bytes) -> None:
    # Imported lazily so this canonical module remains independently usable.
    from evidence import atomic_write_bytes

    atomic_write_bytes(path, payload)


def _parse_arguments(arguments: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("workspace", type=pathlib.Path)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--expected")
    parser.add_argument("--output", type=pathlib.Path)
    revision_mode = parser.add_mutually_exclusive_group()
    revision_mode.add_argument("--revision", action="store_true")
    revision_mode.add_argument("--revision-capture", action="store_true")
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    options = _parse_arguments(arguments)
    if options.revision or options.revision_capture:
        if options.expected is not None:
            raise SourceIdentityError(
                "--expected cannot be combined with a revision capture mode"
            )
        revision = (
            workspace_revision_capture(options.workspace)
            if options.revision_capture
            else workspace_revision_identity(options.workspace)
        )
        rendered = json.dumps(revision, separators=(",", ":"), sort_keys=True) + "\n"
        if options.output is not None:
            _atomic_write(options.output, rendered.encode("utf-8"))
        else:
            print(rendered, end="")
        return 0
    manifest = workspace_source_manifest(options.workspace)
    if options.expected is not None:
        expected = validate_expected_digest(options.expected)
        if manifest["workspace_source_sha256"] != expected:
            raise SourceIdentityError(
                "workspace source digest does not match the frozen candidate: "
                f"expected {expected}, got {manifest['workspace_source_sha256']}"
            )
    if options.json:
        rendered = json.dumps(manifest, separators=(",", ":"), sort_keys=True) + "\n"
    else:
        rendered = str(manifest["workspace_source_sha256"]) + "\n"
    if options.output is not None:
        _atomic_write(options.output, rendered.encode("utf-8"))
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
