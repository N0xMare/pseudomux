#!/usr/bin/env python3
"""Build, install, and inspect the shipped pmux language-client packages.

This is packaging evidence only. It deliberately does not connect to pmuxd or
duplicate any protocol/service semantics. Every build and consumer install is
confined to one identity-fenced temporary directory and runs with package
scripts, indexes, audits, funding checks, and caches disabled.
"""

from __future__ import annotations

import argparse
import base64
import csv
import gzip
import hashlib
import json
import os
import re
import secrets
import stat
import struct
import sys
import tarfile
import tempfile
import tomllib
import types
import zipfile
from contextlib import AbstractContextManager, ExitStack, contextmanager
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, BinaryIO, Final, Iterable, Iterator, Mapping, Sequence


SCHEMA_VERSION: Final = 1
SOURCE_DATE_EPOCH: Final = "946684800"
MAX_ARTIFACT_BYTES: Final = 64 * 1024 * 1024
MAX_ARCHIVE_ENTRIES: Final = 512
MAX_ARCHIVE_ENTRY_BYTES: Final = 16 * 1024 * 1024
MAX_ARCHIVE_DECOMPRESSED_BYTES: Final = 64 * 1024 * 1024
MAX_ARCHIVE_METADATA_BYTES: Final = 1024 * 1024
MAX_RETAINED_METADATA_BYTES: Final = 2 * 1024 * 1024
MAX_TAR_FORMAT_OVERHEAD_BYTES: Final = 4 * 1024 * 1024
MAX_ZIP_CENTRAL_DIRECTORY_BYTES: Final = 4 * 1024 * 1024
ARCHIVE_READ_CHUNK_BYTES: Final = 128 * 1024
ZIP_EOCD: Final = struct.Struct("<4s4H2LH")
ZIP_EOCD_SIGNATURE: Final = b"PK\x05\x06"
MAX_ZIP_COMMENT_BYTES: Final = 65_535
COMMAND_TIMEOUT_SECONDS: Final = 180
COMMAND_DRAIN_TIMEOUT_SECONDS: Final = 15
MAX_COMMAND_OUTPUT_BYTES: Final = 8 * 1024 * 1024
MAX_BOUNDED_PROCESS_AUTHORITY_BYTES: Final = 4 * 1024 * 1024
BOUNDED_PROCESS_RELATIVE_PATH: Final = "tools/evidence_common/bounded_process.py"
BOUNDED_PROCESS_AUTHORITY_PATH: Final = (
    Path(__file__).resolve().parents[1] / "evidence_common" / "bounded_process.py"
)
# Recorded diagnostic, not a gate: the digest of the shared bounded-process
# authority as observed at import, so receipts can carry it verbatim. Nothing
# compares it against a hand-maintained literal.
EXPECTED_BOUNDED_PROCESS_SHA256: Final = hashlib.sha256(
    BOUNDED_PROCESS_AUTHORITY_PATH.read_bytes()
).hexdigest()
MAX_TREE_ENTRIES: Final = 32_768
MAX_TREE_BYTES: Final = 256 * 1024 * 1024
MAX_TREE_DEPTH: Final = 32
TREE_PORTABLE_DOMAIN: Final = "pmux.package-smoke.portable-tree.v1"
TREE_WITNESS_DOMAIN: Final = "pmux.package-smoke.host-tree-witness.v1"
DECLARED_CLOSURE_DOMAIN: Final = "pmux.package-smoke.declared-closure.v1"
DECLARED_FILE_PORTABLE_DOMAIN: Final = "pmux.package-smoke.declared-file.v1"
DECLARED_FILE_WITNESS_DOMAIN: Final = "pmux.package-smoke.declared-file-witness.v1"
SUPPORT_CLOSURE_DOMAIN: Final = "pmux.package-smoke.support-closure.v1"
MAX_CLOSURE_MANIFEST_BYTES: Final = 1024 * 1024
MAX_DECLARED_INPUTS: Final = 64
HEX_SHA256: Final = re.compile(r"[0-9a-f]{64}")
EXPECTED_DECLARED_INPUTS: Final = {
    "typescript": {
        "node_executable": ("file", "native_executable"),
        "npm_executable": ("file", "interpreter_script"),
        "npm_support_tree": ("tree", "tool_support_tree"),
        "typescript_compiler": ("file", "interpreter_script"),
        "typescript_dependency_tree": ("tree", "dependency_tree"),
        "node_types_dependency_tree": ("tree", "dependency_tree"),
        "undici_types_dependency_tree": ("tree", "dependency_tree"),
        "support_closure": ("file", "support_manifest"),
    },
    "python": {
        "python_executable": ("file", "native_executable"),
        "python_stdlib_tree": ("tree", "runtime_support_tree"),
        "python_dynload_tree": ("tree", "runtime_extension_tree"),
        "python_build_support_tree": ("tree", "tool_support_tree"),
        "support_closure": ("file", "support_manifest"),
    },
}
# The complete distribution inventory the declared `python_build_support_tree`
# must carry, in the sorted order the isolated tool report publishes them in.
# This is the INTERPRETER CONTRACT for the Python package gate, and it is stated
# once rather than three times: `clients/python/pyproject.toml` declares
# `build-backend = "setuptools.build_meta"` with `requires = ["setuptools>=61"]`,
# `PYTHON_BUILD_WHEEL_SCRIPT` below imports that backend, and the wheel is built
# with the index disabled -- so setuptools has to arrive in the declared tree or
# not at all. Python 3.12 stopped bundling it with `ensurepip`, which makes
# "the host happens to have it" a host property and not a contract; a caller
# whose tree is missing either name is refused by `validate_python_tool_report`
# naming exactly which.
PYTHON_BUILD_SUPPORT_DISTRIBUTIONS: Final = ("pip", "setuptools")
PYTHON_ISOLATED_MODULE_BOOTSTRAP: Final = """
import sys
roots = sys.argv[1:4]
module = sys.argv[4]
arguments = sys.argv[5:]
sys.path[:] = roots
import runpy
sys.argv[:] = [module, *arguments]
runpy.run_module(module, run_name="__main__", alter_sys=True)
""".strip()
PYTHON_ISOLATED_SCRIPT_BOOTSTRAP: Final = """
import sys
roots = sys.argv[1:4]
target = sys.argv[4]
arguments = sys.argv[5:]
sys.path[:] = roots
import runpy
sys.argv[:] = [target, *arguments]
runpy.run_path(target, run_name="__main__")
""".strip()
PYTHON_ISOLATED_CODE_BOOTSTRAP: Final = """
import sys
roots = sys.argv[1:4]
payload = sys.argv[4]
arguments = sys.argv[5:]
sys.path[:] = roots
sys.argv[:] = ["-c", *arguments]
exec(compile(payload, "<pmux-package-tool-report>", "exec"), {"__name__": "__main__"})
""".strip()
PYTHON_BUILD_WHEEL_SCRIPT: Final = """
import contextlib
import json
import sys
from setuptools import build_meta

with contextlib.redirect_stdout(sys.stderr):
    filename = build_meta.build_wheel(sys.argv[1])
print(json.dumps({"filename": filename}, sort_keys=True, separators=(",", ":")))
""".strip()
PYTHON_TOOL_REPORT_SCRIPT: Final = """
import importlib.metadata
import json
import pathlib
import sys

support = pathlib.Path(sys.argv[1]).resolve(strict=True)
sys_path_before = list(sys.path)
root_distributions = sorted(
    {
        distribution.metadata["Name"].lower().replace("_", "-"): distribution.version
        for distribution in importlib.metadata.distributions(path=[str(support)])
    }.items()
)
root_versions = dict(root_distributions)
import pip
import setuptools

module_files = {
    "pip": str(pathlib.Path(pip.__file__).resolve(strict=True)),
    "setuptools": str(pathlib.Path(setuptools.__file__).resolve(strict=True)),
}
sys_path_after = list(sys.path)
vendor_distributions = []
for path in sys_path_after[len(sys_path_before):]:
    vendor_distributions.append({
        "path": path,
        "distributions": sorted(
            {
                distribution.metadata["Name"].lower().replace("_", "-"): distribution.version
                for distribution in importlib.metadata.distributions(path=[path])
            }.items()
        ),
    })
print(json.dumps({
    "executable": sys.executable,
    "python": sys.version.split()[0],
    "sys_path_before": sys_path_before,
    "sys_path_after": sys_path_after,
    "isolation": {
        "isolated": sys.flags.isolated,
        "ignore_environment": sys.flags.ignore_environment,
        "no_site": sys.flags.no_site,
    },
    "module_files": module_files,
    "distributions": root_distributions,
    "vendor_distributions": vendor_distributions,
    "pip": root_versions.get("pip"),
    "setuptools": root_versions.get("setuptools"),
    "build": root_versions.get("build"),
    "wheel": root_versions.get("wheel"),
    "ruff": root_versions.get("ruff"),
}, sort_keys=True, separators=(",", ":")))
""".strip()

TYPESCRIPT_REQUIRED_FILES: Final = frozenset(
    {
        "package/package.json",
        "package/README.md",
        "package/dist/index.js",
        "package/dist/index.js.map",
        "package/dist/index.d.ts",
        "package/dist/index.d.ts.map",
        "package/dist/client.js",
        "package/dist/client.js.map",
        "package/dist/client.d.ts",
        "package/dist/client.d.ts.map",
        "package/dist/protocol.js",
        "package/dist/protocol.js.map",
        "package/dist/protocol.d.ts",
        "package/dist/protocol.d.ts.map",
        "package/dist/smithers.js",
        "package/dist/smithers.js.map",
        "package/dist/smithers.d.ts",
        "package/dist/smithers.d.ts.map",
    }
)
PYTHON_REQUIRED_PACKAGE_FILES: Final = frozenset(
    {
        "pmux_client/__init__.py",
        "pmux_client/client.py",
        "pmux_client/protocol.py",
        "pmux_client/smithers.py",
        "pmux_client/py.typed",
    }
)


class SmokeError(RuntimeError):
    """A deterministic package-gate failure."""


class PackageCommandFailure(SmokeError):
    """One command failed while retaining its exact bounded-process receipt."""

    def __init__(self, message: str, receipt: Mapping[str, Any]) -> None:
        super().__init__(message)
        self.receipt = dict(receipt)


@dataclass(frozen=True)
class StatIdentity:
    device: int
    inode: int
    uid: int
    gid: int
    mode: int
    nlink: int
    size: int
    mtime_ns: int
    ctime_ns: int

    @classmethod
    def capture(cls, metadata: os.stat_result) -> StatIdentity:
        return cls(
            device=metadata.st_dev,
            inode=metadata.st_ino,
            uid=metadata.st_uid,
            gid=metadata.st_gid,
            mode=metadata.st_mode,
            nlink=metadata.st_nlink,
            size=metadata.st_size,
            mtime_ns=metadata.st_mtime_ns,
            ctime_ns=metadata.st_ctime_ns,
        )

    def stable_directory_identity(self) -> tuple[int, int, int, int, int]:
        return (self.device, self.inode, self.uid, self.gid, stat.S_IMODE(self.mode))

    def as_witness(self) -> dict[str, int]:
        return {
            "device": self.device,
            "inode": self.inode,
            "uid": self.uid,
            "gid": self.gid,
            "mode": stat.S_IMODE(self.mode),
            "nlink": self.nlink,
            "size": self.size,
            "mtime_ns": self.mtime_ns,
            "ctime_ns": self.ctime_ns,
        }


@dataclass(frozen=True)
class TreeSnapshot:
    portable: Mapping[str, Mapping[str, Any]]
    witness: Mapping[str, Mapping[str, Any]]
    portable_sha256: str
    witness_sha256: str
    entry_count: int
    total_bytes: int

    def summary(self) -> dict[str, Any]:
        return {
            "entry_count": self.entry_count,
            "total_bytes": self.total_bytes,
            "portable_sha256": self.portable_sha256,
            "witness_sha256": self.witness_sha256,
        }


def _directory_open_flags() -> int:
    return os.O_RDONLY | os.O_DIRECTORY | getattr(os, "O_NOFOLLOW", 0)


def _regular_open_flags() -> int:
    return os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)


def _safe_child_name(name: str) -> None:
    if not name or name in {".", ".."} or "/" in name or "\x00" in name:
        raise SmokeError(f"unsafe filesystem member name: {name!r}")


def _open_directory_path_no_follow(path: Path) -> int:
    absolute = path.absolute()
    if not absolute.is_absolute():
        raise SmokeError(f"directory path is not absolute: {path}")
    parts = absolute.parts
    descriptor = os.open(parts[0], _directory_open_flags())
    try:
        for part in parts[1:]:
            _safe_child_name(part)
            next_descriptor = os.open(
                part,
                _directory_open_flags(),
                dir_fd=descriptor,
            )
            os.close(descriptor)
            descriptor = next_descriptor
        metadata = os.fstat(descriptor)
        if not stat.S_ISDIR(metadata.st_mode):
            raise SmokeError(f"path is not a direct directory: {absolute}")
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def _same_object(first: StatIdentity, second: StatIdentity) -> bool:
    return first.device == second.device and first.inode == second.inode


class AnchoredDirectory(AbstractContextManager["AnchoredDirectory"]):
    """Retain a no-follow descriptor and revalidate its absolute membership."""

    def __init__(self, path: Path, descriptor: int | None = None) -> None:
        absolute = path.absolute()
        self.path = (
            absolute
            if absolute == Path(absolute.anchor)
            else absolute.parent.resolve(strict=True) / absolute.name
        )
        self._fd = (
            _open_directory_path_no_follow(self.path)
            if descriptor is None
            else descriptor
        )
        metadata = os.fstat(self._fd)
        if not stat.S_ISDIR(metadata.st_mode):
            os.close(self._fd)
            raise SmokeError(f"anchored path is not a directory: {self.path}")
        self.initial = StatIdentity.capture(metadata)
        self._closed = False

    @property
    def fd(self) -> int:
        if self._closed:
            raise SmokeError(f"directory anchor is closed: {self.path}")
        return self._fd

    def verify_path(self, *, exact_metadata: StatIdentity | None = None) -> None:
        retained = StatIdentity.capture(os.fstat(self.fd))
        current_fd = _open_directory_path_no_follow(self.path)
        try:
            current = StatIdentity.capture(os.fstat(current_fd))
        finally:
            os.close(current_fd)
        if not _same_object(retained, current):
            raise SmokeError(f"anchored directory membership changed: {self.path}")
        if (
            retained.stable_directory_identity()
            != self.initial.stable_directory_identity()
        ):
            raise SmokeError(f"anchored directory owner or mode changed: {self.path}")
        if exact_metadata is not None and (
            retained != exact_metadata or current != exact_metadata
        ):
            raise SmokeError(f"anchored directory metadata changed: {self.path}")

    def snapshot(
        self,
        *,
        excluded_top_level: frozenset[str] = frozenset(),
        max_entries: int = MAX_TREE_ENTRIES,
        max_bytes: int = MAX_TREE_BYTES,
        max_depth: int = MAX_TREE_DEPTH,
    ) -> TreeSnapshot:
        self.verify_path()
        snapshot = _tree_snapshot_from_descriptor(
            self.fd,
            excluded_top_level=excluded_top_level,
            max_entries=max_entries,
            max_bytes=max_bytes,
            max_depth=max_depth,
        )
        self.verify_path(exact_metadata=StatIdentity.capture(os.fstat(self.fd)))
        return snapshot

    def __enter__(self) -> AnchoredDirectory:
        return self

    def close(self) -> None:
        if not self._closed:
            os.close(self._fd)
            self._closed = True

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        self.close()
        return False


def _clear_directory_descriptor(descriptor: int) -> None:
    names = sorted(entry.name for entry in os.scandir(descriptor))
    for name in names:
        _safe_child_name(name)
        before = StatIdentity.capture(
            os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        )
        if stat.S_ISDIR(before.mode):
            child = os.open(name, _directory_open_flags(), dir_fd=descriptor)
            try:
                opened = StatIdentity.capture(os.fstat(child))
                if not _same_object(before, opened):
                    raise SmokeError(
                        f"temporary cleanup directory changed before open: {name}"
                    )
                _clear_directory_descriptor(child)
                os.fsync(child)
                after = StatIdentity.capture(
                    os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                )
                if not _same_object(opened, after):
                    raise SmokeError(
                        f"temporary cleanup directory changed during traversal: {name}"
                    )
            finally:
                os.close(child)
            os.rmdir(name, dir_fd=descriptor)
        else:
            os.unlink(name, dir_fd=descriptor)


class OwnedTemporaryRoot(AbstractContextManager[Path]):
    """Create and remove one exact private root only through retained dirfds."""

    def __init__(self, prefix: str) -> None:
        if not prefix or "/" in prefix or "\x00" in prefix:
            raise SmokeError("temporary-root prefix is unsafe")
        self._parent_path = Path(tempfile.gettempdir()).resolve(strict=True)
        self._parent = AnchoredDirectory(self._parent_path)
        self._name = ""
        for _ in range(128):
            candidate = f"{prefix}{secrets.token_hex(16)}"
            try:
                os.mkdir(candidate, mode=0o700, dir_fd=self._parent.fd)
            except FileExistsError:
                continue
            self._name = candidate
            break
        if not self._name:
            self._parent.close()
            raise SmokeError("could not reserve a unique package-smoke root")
        root_fd = os.open(self._name, _directory_open_flags(), dir_fd=self._parent.fd)
        self.path = self._parent_path / self._name
        self._root = AnchoredDirectory(self.path, root_fd)
        metadata = StatIdentity.capture(os.fstat(self._root.fd))
        if metadata.uid != os.geteuid() or stat.S_IMODE(metadata.mode) != 0o700:
            self._root.close()
            self._parent.close()
            raise SmokeError("temporary root is not private and owner-controlled")
        self._cleaned = False

    @property
    def anchor(self) -> AnchoredDirectory:
        return self._root

    def __enter__(self) -> Path:
        self._root.verify_path()
        return self.path

    def cleanup(self) -> None:
        if self._cleaned:
            return
        self._parent.verify_path()
        self._root.verify_path()
        path_metadata = StatIdentity.capture(
            os.stat(self._name, dir_fd=self._parent.fd, follow_symlinks=False)
        )
        retained = StatIdentity.capture(os.fstat(self._root.fd))
        if not _same_object(path_metadata, retained):
            raise SmokeError(
                f"refusing to clean a replaced package-smoke temporary root: {self.path}"
            )
        _clear_directory_descriptor(self._root.fd)
        os.fsync(self._root.fd)
        path_after = StatIdentity.capture(
            os.stat(self._name, dir_fd=self._parent.fd, follow_symlinks=False)
        )
        if not _same_object(path_after, retained):
            raise SmokeError(
                f"temporary root changed during descriptor cleanup: {self.path}"
            )
        self._root.close()
        os.rmdir(self._name, dir_fd=self._parent.fd)
        os.fsync(self._parent.fd)
        try:
            os.stat(self._name, dir_fd=self._parent.fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise SmokeError(
                f"package-smoke temporary root survived cleanup: {self.path}"
            )
        self._cleaned = True

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        try:
            self.cleanup()
        finally:
            self._root.close()
            self._parent.close()
        return False


def create_private_child_directory(
    parent: AnchoredDirectory,
    name: str,
) -> AnchoredDirectory:
    _safe_child_name(name)
    os.mkdir(name, mode=0o700, dir_fd=parent.fd)
    os.fsync(parent.fd)
    descriptor = os.open(name, _directory_open_flags(), dir_fd=parent.fd)
    child = AnchoredDirectory(parent.path / name, descriptor)
    metadata = StatIdentity.capture(os.fstat(child.fd))
    if metadata.uid != os.geteuid() or stat.S_IMODE(metadata.mode) != 0o700:
        child.close()
        raise SmokeError(f"private child directory is not owner-only: {name}")
    return child


def open_direct_child_directory(
    parent: AnchoredDirectory,
    name: str,
) -> AnchoredDirectory:
    _safe_child_name(name)
    before = StatIdentity.capture(
        os.stat(name, dir_fd=parent.fd, follow_symlinks=False)
    )
    if not stat.S_ISDIR(before.mode):
        raise SmokeError(f"direct child is not a directory: {name}")
    descriptor = os.open(name, _directory_open_flags(), dir_fd=parent.fd)
    opened = StatIdentity.capture(os.fstat(descriptor))
    if before != opened:
        os.close(descriptor)
        raise SmokeError(f"direct child changed before descriptor open: {name}")
    return AnchoredDirectory(parent.path / name, descriptor)


def write_new_private_file(
    parent: AnchoredDirectory,
    name: str,
    payload: bytes,
    *,
    mode: int = 0o600,
) -> None:
    _safe_child_name(name)
    descriptor = os.open(
        name,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        mode,
        dir_fd=parent.fd,
    )
    try:
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise SmokeError(f"private file write stopped early: {name}")
            view = view[written:]
        os.fchmod(descriptor, mode)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.fsync(parent.fd)


@contextmanager
def private_dependency_alias(
    parent: AnchoredDirectory,
    name: str,
    target: Path,
) -> Iterator[None]:
    _safe_child_name(name)
    canonical_target = target.resolve(strict=True)
    os.symlink(
        str(canonical_target),
        name,
        dir_fd=parent.fd,
        target_is_directory=True,
    )
    os.fsync(parent.fd)
    try:
        metadata = os.stat(name, dir_fd=parent.fd, follow_symlinks=False)
        if not stat.S_ISLNK(metadata.st_mode) or os.readlink(
            name, dir_fd=parent.fd
        ) != str(canonical_target):
            raise SmokeError("temporary dependency alias was replaced before use")
        yield
        metadata = os.stat(name, dir_fd=parent.fd, follow_symlinks=False)
        if not stat.S_ISLNK(metadata.st_mode) or os.readlink(
            name, dir_fd=parent.fd
        ) != str(canonical_target):
            raise SmokeError("temporary dependency alias changed during use")
    finally:
        try:
            metadata = os.stat(name, dir_fd=parent.fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            if stat.S_ISLNK(metadata.st_mode):
                os.unlink(name, dir_fd=parent.fd)
                os.fsync(parent.fd)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def strict_json_loads(payload: str | bytes, *, label: str) -> Any:
    """Decode JSON while rejecting duplicate keys and non-finite numbers."""

    def reject_constant(value: str) -> None:
        raise SmokeError(f"{label} contains non-finite JSON number {value!r}")

    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        decoded: dict[str, Any] = {}
        for key, value in pairs:
            if key in decoded:
                raise SmokeError(f"{label} repeats JSON object key {key!r}")
            decoded[key] = value
        return decoded

    try:
        return json.loads(
            payload,
            object_pairs_hook=unique_object,
            parse_constant=reject_constant,
        )
    except SmokeError:
        raise
    except (json.JSONDecodeError, RecursionError, UnicodeDecodeError) as error:
        raise SmokeError(f"{label} is not strict JSON: {error}") from error


def npm_artifact_identity(path: Path | AnchoredRegularFile) -> dict[str, str]:
    """Compute the exact legacy SHA-1 and modern SHA-512 npm identities."""

    sha1 = hashlib.sha1(usedforsecurity=False)
    sha512 = hashlib.sha512()
    with artifact_anchor(path) as anchored:
        with anchored.open_binary() as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                sha1.update(chunk)
                sha512.update(chunk)
    return {
        "shasum": sha1.hexdigest(),
        "integrity": "sha512-" + base64.b64encode(sha512.digest()).decode("ascii"),
    }


def verify_npm_artifact_identity(
    pack_entry: Mapping[str, Any], artifact: Path | AnchoredRegularFile
) -> dict[str, str]:
    computed = npm_artifact_identity(artifact)
    if pack_entry.get("shasum") != computed["shasum"]:
        raise SmokeError("npm pack shasum does not bind the created artifact")
    if pack_entry.get("integrity") != computed["integrity"]:
        raise SmokeError("npm pack integrity does not bind the created artifact")
    return computed


def canonical_json_digest(value: Any) -> str:
    encoded = json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return sha256_bytes(encoded)


def domain_json_digest(value: Any, *, domain: str) -> str:
    if not domain or "\x00" in domain:
        raise SmokeError("JSON digest domain is invalid")
    encoded = json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    digest = hashlib.sha256()
    digest.update(domain.encode("ascii"))
    digest.update(b"\x00")
    digest.update(encoded)
    return digest.hexdigest()


class AnchoredRegularFile(AbstractContextManager["AnchoredRegularFile"]):
    """Retain one direct regular file and its no-follow parent membership."""

    def __init__(self, path: Path, *, maximum_bytes: int = MAX_ARTIFACT_BYTES) -> None:
        absolute = path.absolute()
        self.path = absolute.parent.resolve(strict=True) / absolute.name
        self._parent = AnchoredDirectory(self.path.parent)
        _safe_child_name(self.path.name)
        try:
            self._fd = os.open(
                self.path.name,
                _regular_open_flags(),
                dir_fd=self._parent.fd,
            )
        except BaseException:
            self._parent.close()
            raise
        self._closed = False
        metadata = StatIdentity.capture(os.fstat(self._fd))
        if (
            not stat.S_ISREG(metadata.mode)
            or metadata.uid != os.geteuid()
            or metadata.nlink != 1
            or metadata.size < 0
            or metadata.size > maximum_bytes
            or stat.S_IMODE(metadata.mode) & 0o022
        ):
            self.close()
            raise SmokeError(
                f"anchored artifact is not a bounded owner-controlled direct file: {self.path}"
            )
        self.initial = metadata
        self.maximum_bytes = maximum_bytes
        self._initial_sha256 = self._hash_descriptor()
        self.verify()

    @property
    def fd(self) -> int:
        if self._closed:
            raise SmokeError(f"file anchor is closed: {self.path}")
        return self._fd

    def _hash_descriptor(self) -> str:
        digest = hashlib.sha256()
        offset = 0
        while offset < self.maximum_bytes + 1:
            chunk = os.pread(
                self.fd,
                min(1024 * 1024, self.maximum_bytes + 1 - offset),
                offset,
            )
            if not chunk:
                break
            digest.update(chunk)
            offset += len(chunk)
        if offset > self.maximum_bytes or os.pread(self.fd, 1, offset):
            raise SmokeError(f"anchored file exceeds its byte bound: {self.path}")
        return digest.hexdigest()

    def verify(self) -> None:
        self._parent.verify_path()
        retained = StatIdentity.capture(os.fstat(self.fd))
        current = StatIdentity.capture(
            os.stat(self.path.name, dir_fd=self._parent.fd, follow_symlinks=False)
        )
        if (
            retained != self.initial
            or current != self.initial
            or self._hash_descriptor() != self._initial_sha256
        ):
            raise SmokeError(f"anchored file identity or content changed: {self.path}")

    def open_binary(self) -> BinaryIO:
        self.verify()
        duplicate = os.dup(self.fd)
        os.lseek(duplicate, 0, os.SEEK_SET)
        return os.fdopen(duplicate, "rb")

    def sha256(self) -> str:
        self.verify()
        return self._initial_sha256

    def identity(self) -> dict[str, Any]:
        self.verify()
        return {
            **self.initial.as_witness(),
            "sha256": self._initial_sha256,
        }

    def __enter__(self) -> AnchoredRegularFile:
        return self

    def close(self) -> None:
        if not self._closed:
            os.close(self._fd)
            self._closed = True
            self._parent.close()

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        self.close()
        return False


@contextmanager
def artifact_anchor(
    source: Path | AnchoredRegularFile,
) -> Iterator[AnchoredRegularFile]:
    if isinstance(source, AnchoredRegularFile):
        source.verify()
        yield source
        source.verify()
        return
    with AnchoredRegularFile(source) as anchored:
        yield anchored


def _sha256_string(value: object) -> bool:
    return isinstance(value, str) and HEX_SHA256.fullmatch(value) is not None


def _declared_file_digests(anchor: AnchoredRegularFile) -> tuple[str, str]:
    identity = anchor.identity()
    portable = {
        "mode": identity["mode"],
        "bytes": identity["size"],
        "sha256": identity["sha256"],
    }
    return (
        domain_json_digest(portable, domain=DECLARED_FILE_PORTABLE_DOMAIN),
        domain_json_digest(identity, domain=DECLARED_FILE_WITNESS_DOMAIN),
    )


def declared_input_record(gate: str, role: str, path: Path) -> dict[str, str]:
    """Hash one candidate-selected materialized role using verifier semantics."""

    expected = EXPECTED_DECLARED_INPUTS.get(gate, {}).get(role)
    if expected is None:
        raise SmokeError("declared package input gate or role is invalid")
    if not isinstance(path, Path) or not path.is_absolute():
        raise SmokeError("declared package input path is not absolute")
    kind, usage = expected
    if kind == "file":
        with AnchoredRegularFile(path, maximum_bytes=MAX_TREE_BYTES) as anchor:
            portable, witness = _declared_file_digests(anchor)
            canonical_path = anchor.path
    else:
        with AnchoredDirectory(path) as anchor:
            snapshot = anchor.snapshot()
            portable = snapshot.portable_sha256
            witness = snapshot.witness_sha256
            canonical_path = anchor.path
    return {
        "role": role,
        "kind": kind,
        "usage": usage,
        "path": str(canonical_path),
        "portable_sha256": portable,
        "witness_sha256": witness,
    }


def candidate_support_closure_payload(
    gate: str,
    *,
    candidate_sha256: str,
    source_sha256: str,
    previous_anchor_sha256: str,
    attestation_sha256: str,
) -> dict[str, Any]:
    """Construct the exact candidate-owned package support anchor payload."""

    if gate not in EXPECTED_DECLARED_INPUTS or not all(
        _sha256_string(value)
        for value in (
            candidate_sha256,
            source_sha256,
            previous_anchor_sha256,
            attestation_sha256,
        )
    ):
        raise SmokeError("candidate support closure builder inputs are invalid")
    payload: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "kind": "pmux_gate_a_package_support_closure",
        "gate": gate,
        "candidate_manifest_sha256": candidate_sha256,
        "source_manifest_sha256": source_sha256,
        "previous_anchor_sha256": previous_anchor_sha256,
        "attestation_sha256": attestation_sha256,
    }
    payload["support_closure_sha256"] = domain_json_digest(
        payload,
        domain=SUPPORT_CLOSURE_DOMAIN,
    )
    return payload


def declared_closure_payload(
    gate: str,
    *,
    candidate_sha256: str,
    source_sha256: str,
    previous_anchor_sha256: str,
    inputs: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    """Construct an exact, canonical-JSON-serializable declared closure."""

    if gate not in EXPECTED_DECLARED_INPUTS or not all(
        _sha256_string(value)
        for value in (candidate_sha256, source_sha256, previous_anchor_sha256)
    ):
        raise SmokeError("declared closure builder anchors are invalid")
    exact_keys = {
        "role",
        "kind",
        "usage",
        "path",
        "portable_sha256",
        "witness_sha256",
    }
    normalized: list[dict[str, Any]] = []
    roles: set[str] = set()
    paths: set[str] = set()
    for raw in inputs:
        if not isinstance(raw, Mapping) or set(raw) != exact_keys:
            raise SmokeError("declared closure builder input schema is not exact")
        record = dict(raw)
        role = record.get("role")
        path = record.get("path")
        expected = EXPECTED_DECLARED_INPUTS[gate].get(role)
        canonical_path: Path | None = None
        if isinstance(path, str) and Path(path).is_absolute():
            absolute = Path(path).absolute()
            try:
                canonical_path = absolute.parent.resolve(strict=True) / absolute.name
            except OSError as error:
                raise SmokeError(
                    "declared closure builder input parent is unavailable"
                ) from error
        if (
            not isinstance(role, str)
            or role in roles
            or expected != (record.get("kind"), record.get("usage"))
            or not isinstance(path, str)
            or not Path(path).is_absolute()
            or canonical_path != Path(path)
            or path in paths
            or not _sha256_string(record.get("portable_sha256"))
            or not _sha256_string(record.get("witness_sha256"))
        ):
            raise SmokeError("declared closure builder input fields are invalid")
        roles.add(role)
        paths.add(path)
        normalized.append(record)
    if roles != set(EXPECTED_DECLARED_INPUTS[gate]):
        raise SmokeError("declared closure builder role inventory is not exact")
    normalized.sort(key=lambda record: record["role"])
    _validate_role_path_layout(
        gate,
        {record["role"]: Path(record["path"]) for record in normalized},
    )
    payload: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "kind": "pmux_package_smoke_declared_closure",
        "gate": gate,
        "candidate_manifest_sha256": candidate_sha256,
        "source_manifest_sha256": source_sha256,
        "previous_anchor_sha256": previous_anchor_sha256,
        "inputs": normalized,
    }
    payload["closure_sha256"] = domain_json_digest(
        payload,
        domain=DECLARED_CLOSURE_DOMAIN,
    )
    return payload


@dataclass
class DeclaredInput:
    role: str
    kind: str
    usage: str
    path: Path
    expected_portable_sha256: str
    expected_witness_sha256: str
    file: AnchoredRegularFile | None = None
    directory: AnchoredDirectory | None = None

    def verify(self) -> None:
        if self.kind == "file":
            if self.file is None:
                raise SmokeError(f"declared file input was not opened: {self.role}")
            portable, witness = _declared_file_digests(self.file)
        elif self.kind == "tree":
            if self.directory is None:
                raise SmokeError(f"declared tree input was not opened: {self.role}")
            snapshot = self.directory.snapshot()
            portable = snapshot.portable_sha256
            witness = snapshot.witness_sha256
        else:
            raise SmokeError(f"declared input kind is invalid: {self.kind}")
        if (
            portable != self.expected_portable_sha256
            or witness != self.expected_witness_sha256
        ):
            raise SmokeError(f"declared input changed or is misbound: {self.role}")

    def close(self) -> None:
        if self.file is not None:
            self.file.close()
        if self.directory is not None:
            self.directory.close()


class DeclaredClosure(AbstractContextManager["DeclaredClosure"]):
    def __init__(
        self,
        manifest_file: AnchoredRegularFile,
        manifest: Mapping[str, Any],
        inputs: Sequence[DeclaredInput],
        raw_sha256: str,
        support_attestation_sha256: str,
        support_closure_sha256: str,
    ) -> None:
        self.manifest_file = manifest_file
        self.manifest = dict(manifest)
        self.inputs = tuple(inputs)
        self.raw_sha256 = raw_sha256
        self.support_attestation_sha256 = support_attestation_sha256
        self.support_closure_sha256 = support_closure_sha256
        self._closed = False

    @property
    def digest(self) -> str:
        value = self.manifest.get("closure_sha256")
        if not isinstance(value, str):
            raise SmokeError("declared closure digest is unavailable")
        return value

    def verify(self) -> None:
        if self._closed:
            raise SmokeError("declared closure is already closed")
        self.manifest_file.verify()
        for declared in self.inputs:
            declared.verify()

    def input(self, role: str) -> DeclaredInput:
        matches = [declared for declared in self.inputs if declared.role == role]
        if len(matches) != 1:
            raise SmokeError(f"declared closure does not contain exact role {role}")
        matches[0].verify()
        return matches[0]

    def report(self) -> dict[str, Any]:
        self.verify()
        return {
            "candidate_manifest_sha256": self.manifest["candidate_manifest_sha256"],
            "source_manifest_sha256": self.manifest["source_manifest_sha256"],
            "previous_anchor_sha256": self.manifest["previous_anchor_sha256"],
            "closure_sha256": self.digest,
            "closure_file_sha256": self.raw_sha256,
            "support_attestation_sha256": self.support_attestation_sha256,
            "support_closure_sha256": self.support_closure_sha256,
            "inputs": [
                {
                    "role": declared.role,
                    "kind": declared.kind,
                    "usage": declared.usage,
                    "portable_sha256": declared.expected_portable_sha256,
                    "witness_sha256": declared.expected_witness_sha256,
                }
                for declared in sorted(self.inputs, key=lambda item: item.role)
            ],
        }

    def __enter__(self) -> DeclaredClosure:
        self.verify()
        return self

    def close(self) -> None:
        if self._closed:
            return
        for declared in reversed(self.inputs):
            declared.close()
        self.manifest_file.close()
        self._closed = True

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        self.close()
        return False


def load_declared_closure(
    gate: str,
    *,
    required_paths: Mapping[str, Path] | None = None,
) -> DeclaredClosure:
    closure_path_value = os.environ.get("PMUX_PACKAGE_SMOKE_CLOSURE_FILE")
    expected_file_sha256 = os.environ.get("PMUX_PACKAGE_SMOKE_CLOSURE_SHA256")
    expected_candidate = os.environ.get("PMUX_PACKAGE_SMOKE_CANDIDATE_SHA256")
    expected_source = os.environ.get("PMUX_PACKAGE_SMOKE_SOURCE_SHA256")
    expected_previous = os.environ.get("PMUX_PACKAGE_SMOKE_PREVIOUS_ANCHOR_SHA256")
    if not closure_path_value or not Path(closure_path_value).is_absolute():
        raise SmokeError("candidate did not supply an absolute declared-closure file")
    for label, value in (
        ("closure file", expected_file_sha256),
        ("candidate", expected_candidate),
        ("source", expected_source),
        ("previous command", expected_previous),
    ):
        if not _sha256_string(value):
            raise SmokeError(f"candidate did not supply an exact {label} anchor")

    manifest_file = AnchoredRegularFile(
        Path(closure_path_value),
        maximum_bytes=MAX_CLOSURE_MANIFEST_BYTES,
    )
    if stat.S_IMODE(manifest_file.initial.mode) != 0o600:
        manifest_file.close()
        raise SmokeError("declared closure manifest is not mode 0600")
    inputs: list[DeclaredInput] = []
    try:
        with manifest_file.open_binary() as stream:
            raw = stream.read(MAX_CLOSURE_MANIFEST_BYTES + 1)
        if len(raw) > MAX_CLOSURE_MANIFEST_BYTES:
            raise SmokeError("declared closure manifest exceeds its byte bound")
        raw_sha256 = sha256_bytes(raw)
        if raw_sha256 != expected_file_sha256:
            raise SmokeError("declared closure file differs from its external anchor")
        manifest = strict_json_loads(raw, label="declared package closure")
        exact_keys = {
            "schema_version",
            "kind",
            "gate",
            "candidate_manifest_sha256",
            "source_manifest_sha256",
            "previous_anchor_sha256",
            "inputs",
            "closure_sha256",
        }
        if not isinstance(manifest, dict) or set(manifest) != exact_keys:
            raise SmokeError("declared package closure schema is not exact")
        if (
            type(manifest.get("schema_version")) is not int
            or manifest["schema_version"] != SCHEMA_VERSION
            or manifest.get("kind") != "pmux_package_smoke_declared_closure"
            or manifest.get("gate") != gate
            or manifest.get("candidate_manifest_sha256") != expected_candidate
            or manifest.get("source_manifest_sha256") != expected_source
            or manifest.get("previous_anchor_sha256") != expected_previous
            or not _sha256_string(manifest.get("closure_sha256"))
        ):
            raise SmokeError("declared package closure anchors are invalid")
        payload = dict(manifest)
        supplied_digest = payload.pop("closure_sha256")
        expected_digest = domain_json_digest(payload, domain=DECLARED_CLOSURE_DOMAIN)
        if supplied_digest != expected_digest:
            raise SmokeError("declared package closure digest is invalid")
        raw_inputs = manifest.get("inputs")
        if (
            not isinstance(raw_inputs, list)
            or not 1 <= len(raw_inputs) <= MAX_DECLARED_INPUTS
        ):
            raise SmokeError("declared package closure input list is invalid")
        roles: set[str] = set()
        paths: set[Path] = set()
        for raw_input in raw_inputs:
            input_keys = {
                "role",
                "kind",
                "usage",
                "path",
                "portable_sha256",
                "witness_sha256",
            }
            if not isinstance(raw_input, dict) or set(raw_input) != input_keys:
                raise SmokeError("declared package input schema is not exact")
            role = raw_input.get("role")
            kind = raw_input.get("kind")
            usage = raw_input.get("usage")
            path_value = raw_input.get("path")
            if (
                not isinstance(role, str)
                or re.fullmatch(r"[a-z][a-z0-9_]{0,63}", role) is None
                or role in roles
                or gate not in EXPECTED_DECLARED_INPUTS
                or EXPECTED_DECLARED_INPUTS[gate].get(role) != (kind, usage)
                or not isinstance(path_value, str)
                or not Path(path_value).is_absolute()
                or not _sha256_string(raw_input.get("portable_sha256"))
                or not _sha256_string(raw_input.get("witness_sha256"))
            ):
                raise SmokeError("declared package input fields are invalid")
            declared_path = Path(path_value)
            if declared_path in paths:
                raise SmokeError("declared package closure repeats an input path")
            roles.add(role)
            paths.add(declared_path)
            declared = DeclaredInput(
                role=role,
                kind=kind,
                usage=str(usage),
                path=declared_path,
                expected_portable_sha256=raw_input["portable_sha256"],
                expected_witness_sha256=raw_input["witness_sha256"],
            )
            if kind == "file":
                declared.file = AnchoredRegularFile(
                    declared_path,
                    maximum_bytes=MAX_TREE_BYTES,
                )
            else:
                declared.directory = AnchoredDirectory(declared_path)
            inputs.append(declared)
        expected_roles = set(EXPECTED_DECLARED_INPUTS[gate])
        if required_paths is not None and set(required_paths) != expected_roles - {
            "support_closure"
        }:
            raise SmokeError("package gate required-path inventory is incomplete")
        if roles != expected_roles:
            raise SmokeError(
                "declared package closure roles are not exact: "
                f"{sorted(roles ^ expected_roles)}"
            )
        by_role = {declared.role: declared for declared in inputs}
        if required_paths is not None:
            for role, expected_path in required_paths.items():
                declared = by_role[role]
                expected_canonical = (
                    expected_path.absolute().parent.resolve(strict=True)
                    / expected_path.absolute().name
                )
                if declared.path != expected_canonical:
                    raise SmokeError(f"declared package input path differs for {role}")
        support = by_role["support_closure"]
        if support.file is None or stat.S_IMODE(support.file.initial.mode) != 0o600:
            raise SmokeError("candidate support closure is not a private direct file")
        with support.file.open_binary() as stream:
            support_payload = stream.read(MAX_CLOSURE_MANIFEST_BYTES + 1)
        if len(support_payload) > MAX_CLOSURE_MANIFEST_BYTES:
            raise SmokeError("candidate support closure exceeds its byte bound")
        support_manifest = strict_json_loads(
            support_payload,
            label="candidate package support closure",
        )
        support_keys = {
            "schema_version",
            "kind",
            "gate",
            "candidate_manifest_sha256",
            "source_manifest_sha256",
            "previous_anchor_sha256",
            "attestation_sha256",
            "support_closure_sha256",
        }
        if (
            not isinstance(support_manifest, dict)
            or set(support_manifest) != support_keys
        ):
            raise SmokeError("candidate support closure schema is not exact")
        if (
            type(support_manifest.get("schema_version")) is not int
            or support_manifest["schema_version"] != SCHEMA_VERSION
            or support_manifest.get("kind") != "pmux_gate_a_package_support_closure"
            or support_manifest.get("gate") != gate
            or support_manifest.get("candidate_manifest_sha256") != expected_candidate
            or support_manifest.get("source_manifest_sha256") != expected_source
            or support_manifest.get("previous_anchor_sha256") != expected_previous
            or not _sha256_string(support_manifest.get("attestation_sha256"))
            or not _sha256_string(support_manifest.get("support_closure_sha256"))
        ):
            raise SmokeError("candidate support closure anchors are invalid")
        support_for_hash = dict(support_manifest)
        support_digest = support_for_hash.pop("support_closure_sha256")
        if support_digest != domain_json_digest(
            support_for_hash,
            domain=SUPPORT_CLOSURE_DOMAIN,
        ):
            raise SmokeError("candidate support closure digest is invalid")
        support.file.verify()
        closure = DeclaredClosure(
            manifest_file,
            manifest,
            inputs,
            raw_sha256,
            str(support_manifest["attestation_sha256"]),
            str(support_manifest["support_closure_sha256"]),
        )
        closure.verify()
        validate_declared_layout(closure, gate)
        return closure
    except BaseException:
        for declared in reversed(inputs):
            declared.close()
        manifest_file.close()
        raise


def safe_archive_name(raw_name: str) -> str:
    if not raw_name or "\x00" in raw_name or "\\" in raw_name:
        raise SmokeError(f"unsafe archive path: {raw_name!r}")
    name = raw_name.rstrip("/")
    if not name:
        raise SmokeError(f"unsafe archive path: {raw_name!r}")
    path = PurePosixPath(name)
    drive_like = (
        bool(path.parts)
        and len(path.parts[0]) == 2
        and path.parts[0][0].isalpha()
        and path.parts[0][1] == ":"
    )
    if (
        path.is_absolute()
        or drive_like
        or any(part in {"", ".", ".."} for part in path.parts)
    ):
        raise SmokeError(f"unsafe archive path: {raw_name!r}")
    normalized = path.as_posix()
    if normalized != name:
        raise SmokeError(f"non-canonical archive path: {raw_name!r}")
    return normalized


def derived_archive_directories(files: Iterable[str]) -> frozenset[str]:
    directories: set[str] = set()
    for name in files:
        parts = PurePosixPath(name).parts
        for index in range(1, len(parts)):
            directories.add(PurePosixPath(*parts[:index]).as_posix())
    return frozenset(directories)


def preflight_zip_directory(path: Path | AnchoredRegularFile) -> int:
    """Bound the central-directory entry count before ZipFile allocates it."""

    with artifact_anchor(path) as anchored:
        file_size = anchored.initial.size
        if file_size < ZIP_EOCD.size:
            raise SmokeError("Python wheel is missing its ZIP end record")
        tail_size = min(file_size, ZIP_EOCD.size + MAX_ZIP_COMMENT_BYTES)
        with anchored.open_binary() as stream:
            stream.seek(file_size - tail_size)
            tail = stream.read(tail_size)

    end_record: tuple[int, int, int, int, int, int, int] | None = None
    position = tail.rfind(ZIP_EOCD_SIGNATURE)
    while position >= 0:
        if position + ZIP_EOCD.size <= len(tail):
            unpacked = ZIP_EOCD.unpack_from(tail, position)
            (
                _,
                disk_number,
                central_disk,
                entries_on_disk,
                entries_total,
                central_size,
                central_offset,
                comment_size,
            ) = unpacked
            if position + ZIP_EOCD.size + comment_size == len(tail):
                end_record = (
                    disk_number,
                    central_disk,
                    entries_on_disk,
                    entries_total,
                    central_size,
                    central_offset,
                    file_size - tail_size + position,
                )
                break
        position = tail.rfind(ZIP_EOCD_SIGNATURE, 0, position)
    if end_record is None:
        raise SmokeError("Python wheel has an invalid ZIP end record")

    (
        disk_number,
        central_disk,
        entries_on_disk,
        entries_total,
        central_size,
        central_offset,
        end_offset,
    ) = end_record
    if (
        entries_on_disk == 0xFFFF
        or entries_total == 0xFFFF
        or central_size == 0xFFFFFFFF
        or central_offset == 0xFFFFFFFF
    ):
        raise SmokeError(
            "Python wheel ZIP64 metadata exceeds the bounded wheel profile"
        )
    if disk_number != 0 or central_disk != 0 or entries_on_disk != entries_total:
        raise SmokeError("Python wheel uses unsupported multi-disk ZIP metadata")
    if entries_total > MAX_ARCHIVE_ENTRIES:
        raise SmokeError("Python wheel contains too many entries")
    if central_size > MAX_ZIP_CENTRAL_DIRECTORY_BYTES:
        raise SmokeError("Python wheel central directory exceeds its byte bound")
    if central_offset + central_size != end_offset:
        raise SmokeError("Python wheel central-directory bounds are inconsistent")
    return entries_total


class BoundedArchiveStream:
    """Cap all bytes obtained from an outer decompression stream."""

    def __init__(self, stream: BinaryIO, *, limit: int, label: str) -> None:
        self._stream = stream
        self._limit = limit
        self._label = label
        self.bytes_read = 0

    def read(self, size: int = -1) -> bytes:
        remaining = self._limit - self.bytes_read
        bounded_size = remaining + 1 if size < 0 else min(size, remaining + 1)
        payload = self._stream.read(bounded_size)
        self.bytes_read += len(payload)
        if self.bytes_read > self._limit:
            raise SmokeError(
                f"{self._label} exceeds its decompressed stream byte bound"
            )
        return payload


def checked_archive_total(current: int, declared_size: int, *, label: str) -> int:
    if isinstance(declared_size, bool) or not isinstance(declared_size, int):
        raise SmokeError(f"{label} has a non-integer declared size")
    if declared_size < 0 or declared_size > MAX_ARCHIVE_ENTRY_BYTES:
        raise SmokeError(f"{label} exceeds the per-entry decompressed byte bound")
    updated = current + declared_size
    if updated > MAX_ARCHIVE_DECOMPRESSED_BYTES:
        raise SmokeError("archive exceeds the cumulative decompressed byte bound")
    return updated


def stream_archive_entry(
    stream: BinaryIO,
    *,
    declared_size: int,
    label: str,
    retain: bool,
) -> tuple[dict[str, Any], bytes | None]:
    """Hash one bounded entry without retaining ordinary archive payloads."""

    digest = hashlib.sha256()
    retained = bytearray() if retain else None
    observed = 0
    while observed < declared_size:
        requested = min(ARCHIVE_READ_CHUNK_BYTES, declared_size - observed)
        chunk = stream.read(requested)
        if not chunk:
            break
        observed += len(chunk)
        if observed > declared_size:
            raise SmokeError(f"{label} expands past its declared size")
        digest.update(chunk)
        if retained is not None:
            retained.extend(chunk)
    if observed != declared_size:
        raise SmokeError(f"{label} ended before its declared size")
    if stream.read(1):
        raise SmokeError(f"{label} expands past its declared size")
    return (
        {"bytes": observed, "sha256": digest.hexdigest()},
        bytes(retained) if retained is not None else None,
    )


@dataclass
class _TreeScanState:
    portable: dict[str, dict[str, Any]]
    witness: dict[str, dict[str, Any]]
    entry_count: int
    total_bytes: int
    maximum_entries: int
    maximum_bytes: int
    maximum_depth: int


def _validate_tree_metadata(
    identity: StatIdentity,
    *,
    label: str,
    regular: bool,
) -> None:
    if identity.uid != os.geteuid():
        raise SmokeError(f"tree member is not owned by the effective user: {label}")
    if stat.S_IMODE(identity.mode) & 0o022:
        raise SmokeError(f"tree member is group/world writable: {label}")
    if regular and identity.nlink != 1:
        raise SmokeError(f"tree member is multiply linked: {label}")


def _record_tree_entry(
    state: _TreeScanState,
    *,
    relative: str,
    identity: StatIdentity,
    kind: str,
    sha256: str | None = None,
) -> None:
    state.entry_count += 1
    if state.entry_count > state.maximum_entries:
        raise SmokeError("filesystem tree exceeds its entry-count bound")
    portable: dict[str, Any] = {
        "type": kind,
        "mode": stat.S_IMODE(identity.mode),
    }
    witness: dict[str, Any] = {**portable, **identity.as_witness()}
    if kind == "file":
        portable["bytes"] = identity.size
        portable["sha256"] = sha256
        witness["bytes"] = identity.size
        witness["sha256"] = sha256
    state.portable[relative] = portable
    state.witness[relative] = witness


def _hash_regular_descriptor(
    descriptor: int,
    *,
    size: int,
    state: _TreeScanState,
    label: str,
) -> str:
    if size < 0 or size > state.maximum_bytes - state.total_bytes:
        raise SmokeError(f"filesystem tree exceeds its cumulative byte bound: {label}")
    digest = hashlib.sha256()
    observed = 0
    while observed < size:
        chunk = os.pread(descriptor, min(1024 * 1024, size - observed), observed)
        if not chunk:
            break
        observed += len(chunk)
        digest.update(chunk)
    if observed != size or os.pread(descriptor, 1, observed):
        raise SmokeError(f"tree file changed size while being read: {label}")
    state.total_bytes += observed
    return digest.hexdigest()


def _scan_tree_directory(
    descriptor: int,
    *,
    relative: str,
    depth: int,
    state: _TreeScanState,
    excluded_top_level: frozenset[str],
) -> None:
    if depth > state.maximum_depth:
        raise SmokeError("filesystem tree exceeds its depth bound")
    start = StatIdentity.capture(os.fstat(descriptor))
    if not stat.S_ISDIR(start.mode):
        raise SmokeError(f"tree member is not a directory: {relative}")
    _validate_tree_metadata(start, label=relative, regular=False)
    _record_tree_entry(
        state,
        relative=relative,
        identity=start,
        kind="directory",
    )
    try:
        names_before = sorted(entry.name for entry in os.scandir(descriptor))
    except OSError as error:
        raise SmokeError(
            f"could not enumerate filesystem tree at {relative}"
        ) from error
    for name in names_before:
        _safe_child_name(name)
        if depth == 0 and name in excluded_top_level:
            continue
        child_relative = name if relative == "." else f"{relative}/{name}"
        try:
            before = StatIdentity.capture(
                os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            )
        except OSError as error:
            raise SmokeError(
                f"tree member disappeared before inspection: {child_relative}"
            ) from error
        if stat.S_ISLNK(before.mode):
            raise SmokeError(f"filesystem tree contains a symlink: {child_relative}")
        if stat.S_ISDIR(before.mode):
            _validate_tree_metadata(before, label=child_relative, regular=False)
            try:
                child_fd = os.open(name, _directory_open_flags(), dir_fd=descriptor)
            except OSError as error:
                raise SmokeError(
                    f"tree directory could not be opened without following: {child_relative}"
                ) from error
            try:
                opened = StatIdentity.capture(os.fstat(child_fd))
                if before != opened:
                    raise SmokeError(
                        f"tree directory changed before descriptor open: {child_relative}"
                    )
                _scan_tree_directory(
                    child_fd,
                    relative=child_relative,
                    depth=depth + 1,
                    state=state,
                    excluded_top_level=frozenset(),
                )
                after_open = StatIdentity.capture(os.fstat(child_fd))
                after_path = StatIdentity.capture(
                    os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                )
                if after_open != opened or after_path != opened:
                    raise SmokeError(
                        f"tree directory changed during traversal: {child_relative}"
                    )
            finally:
                os.close(child_fd)
            continue
        if not stat.S_ISREG(before.mode):
            raise SmokeError(
                f"filesystem tree contains a special node: {child_relative}"
            )
        _validate_tree_metadata(before, label=child_relative, regular=True)
        try:
            child_fd = os.open(name, _regular_open_flags(), dir_fd=descriptor)
        except OSError as error:
            raise SmokeError(
                f"tree file could not be opened without following: {child_relative}"
            ) from error
        try:
            opened = StatIdentity.capture(os.fstat(child_fd))
            if opened != before:
                raise SmokeError(
                    f"tree file changed before descriptor open: {child_relative}"
                )
            digest = _hash_regular_descriptor(
                child_fd,
                size=opened.size,
                state=state,
                label=child_relative,
            )
            after_open = StatIdentity.capture(os.fstat(child_fd))
            after_path = StatIdentity.capture(
                os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            )
            if after_open != opened or after_path != opened:
                raise SmokeError(f"tree file changed during read: {child_relative}")
        finally:
            os.close(child_fd)
        _record_tree_entry(
            state,
            relative=child_relative,
            identity=opened,
            kind="file",
            sha256=digest,
        )
    try:
        names_after = sorted(entry.name for entry in os.scandir(descriptor))
    except OSError as error:
        raise SmokeError(
            f"could not re-enumerate filesystem tree at {relative}"
        ) from error
    end = StatIdentity.capture(os.fstat(descriptor))
    if names_after != names_before or end != start:
        raise SmokeError(
            f"filesystem tree membership changed during traversal: {relative}"
        )


def _tree_snapshot_from_descriptor(
    descriptor: int,
    *,
    excluded_top_level: frozenset[str],
    max_entries: int,
    max_bytes: int,
    max_depth: int,
) -> TreeSnapshot:
    if (
        type(max_entries) is not int
        or max_entries < 1
        or type(max_bytes) is not int
        or max_bytes < 0
        or type(max_depth) is not int
        or max_depth < 0
    ):
        raise SmokeError("filesystem tree bounds are invalid")
    for name in excluded_top_level:
        _safe_child_name(name)
    state = _TreeScanState(
        portable={},
        witness={},
        entry_count=0,
        total_bytes=0,
        maximum_entries=max_entries,
        maximum_bytes=max_bytes,
        maximum_depth=max_depth,
    )
    _scan_tree_directory(
        descriptor,
        relative=".",
        depth=0,
        state=state,
        excluded_top_level=excluded_top_level,
    )
    return TreeSnapshot(
        portable=state.portable,
        witness=state.witness,
        portable_sha256=domain_json_digest(
            state.portable,
            domain=TREE_PORTABLE_DOMAIN,
        ),
        witness_sha256=domain_json_digest(
            state.witness,
            domain=TREE_WITNESS_DOMAIN,
        ),
        entry_count=state.entry_count,
        total_bytes=state.total_bytes,
    )


def tree_snapshot(
    root: Path,
    *,
    excluded_top_level: frozenset[str] = frozenset(),
    max_entries: int = MAX_TREE_ENTRIES,
    max_bytes: int = MAX_TREE_BYTES,
    max_depth: int = MAX_TREE_DEPTH,
) -> TreeSnapshot:
    with AnchoredDirectory(root) as anchor:
        return anchor.snapshot(
            excluded_top_level=excluded_top_level,
            max_entries=max_entries,
            max_bytes=max_bytes,
            max_depth=max_depth,
        )


def file_manifest(
    root: Path, *, excluded_top_level: frozenset[str] = frozenset()
) -> dict[str, Any]:
    """Capture one bounded direct tree through no-follow descriptors."""

    return dict(tree_snapshot(root, excluded_top_level=excluded_top_level).portable)


def client_tree_snapshot(workspace: Path) -> dict[str, Any]:
    typescript = tree_snapshot(
        workspace / "clients/typescript",
        excluded_top_level=frozenset({"node_modules"}),
    )
    python = tree_snapshot(workspace / "clients/python")
    return {
        "typescript": {
            "portable": typescript.portable,
            "witness": typescript.witness,
        },
        "python": {
            "portable": python.portable,
            "witness": python.witness,
        },
    }


def assert_snapshot_unchanged(
    before: Mapping[str, Any], after: Mapping[str, Any]
) -> None:
    if before == after:
        return
    changed = sorted(set(before) ^ set(after))
    for key in sorted(set(before) & set(after)):
        if before[key] != after[key]:
            changed.append(key)
    raise SmokeError(
        f"client package source/residue changed during the gate: {changed[:12]}"
    )


def deterministic_environment(
    temp_root: Path, *, executables: Sequence[Path]
) -> dict[str, str]:
    home = temp_root / "home"
    cache = temp_root / "cache"
    scratch = temp_root / "tmp"
    config = temp_root / "config"
    npm_user_config = config / "npm-user.rc"
    npm_global_config = config / "npm-global.rc"
    with AnchoredDirectory(temp_root) as root:
        for name in ("home", "cache", "tmp"):
            with create_private_child_directory(root, name):
                pass
        with create_private_child_directory(root, "config") as config_anchor:
            write_new_private_file(config_anchor, "npm-user.rc", b"")
            write_new_private_file(config_anchor, "npm-global.rc", b"")
    path_entries = list(
        dict.fromkeys([*(str(path.parent) for path in executables), "/usr/bin", "/bin"])
    )
    # Construct an allowlist instead of inheriting the caller environment. In
    # particular, provider tokens, package-registry credentials, proxies,
    # SSH agents, language injection hooks, and user configuration paths never
    # reach a package build or installed-artifact consumer.
    return {
        "CI": "1",
        "PATH": os.pathsep.join(path_entries),
        "HOME": str(home),
        "TMPDIR": str(scratch),
        "TMP": str(scratch),
        "TEMP": str(scratch),
        "XDG_CACHE_HOME": str(cache / "xdg"),
        "XDG_CONFIG_HOME": str(config),
        "SOURCE_DATE_EPOCH": SOURCE_DATE_EPOCH,
        "TZ": "UTC",
        "LC_ALL": "C",
        "LANG": "C",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0",
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONHASHSEED": "0",
        "PYTHONNOUSERSITE": "1",
        "PIP_CONFIG_FILE": os.devnull,
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "PIP_NO_CACHE_DIR": "1",
        "PIP_NO_INDEX": "1",
        "PIP_NO_INPUT": "1",
        "npm_config_audit": "false",
        "npm_config_cache": str(cache / "npm"),
        "npm_config_color": "false",
        "npm_config_fund": "false",
        "npm_config_globalconfig": str(npm_global_config),
        "npm_config_ignore_scripts": "true",
        "npm_config_loglevel": "error",
        "npm_config_offline": "true",
        "npm_config_package_lock": "false",
        "npm_config_progress": "false",
        "npm_config_registry": "http://127.0.0.1:9/",
        "npm_config_update_notifier": "false",
        "npm_config_userconfig": str(npm_user_config),
    }


class BoundedProcessAuthority(AbstractContextManager["BoundedProcessAuthority"]):
    """Load and retain the reviewed shared finite-command implementation."""

    def __init__(self) -> None:
        self._anchor = AnchoredRegularFile(
            BOUNDED_PROCESS_AUTHORITY_PATH,
            maximum_bytes=MAX_BOUNDED_PROCESS_AUTHORITY_BYTES,
        )
        self._closed = False
        try:
            with self._anchor.open_binary() as stream:
                payload = stream.read(MAX_BOUNDED_PROCESS_AUTHORITY_BYTES + 1)
            if not payload or len(payload) > MAX_BOUNDED_PROCESS_AUTHORITY_BYTES:
                raise SmokeError(
                    "shared bounded-process authority exceeds its byte bound"
                )
            digest = sha256_bytes(payload)
            module_name = f"_pmux_package_bounded_process_{secrets.token_hex(16)}"
            module = types.ModuleType(module_name)
            module.__file__ = str(self._anchor.path)
            module.__package__ = ""
            sys.modules[module_name] = module
            try:
                code = compile(
                    payload,
                    str(self._anchor.path),
                    "exec",
                    dont_inherit=True,
                )
                exec(code, module.__dict__)
            except BaseException as error:
                raise SmokeError(
                    "shared bounded-process authority could not be loaded"
                ) from error
            finally:
                if sys.modules.get(module_name) is module:
                    del sys.modules[module_name]

            required = (
                "bind_executable",
                "run",
                "validate_execution_receipt",
                "validate_failure_receipt",
                "verify_receipt_context",
                "BoundedProcessError",
                "BoundedProcessFailure",
            )
            if any(not hasattr(module, name) for name in required):
                raise SmokeError(
                    "shared bounded-process authority is missing its required finite-command interface"
                )
            self.module = module
            self._implementation = {
                "path": BOUNDED_PROCESS_RELATIVE_PATH,
                "bytes": len(payload),
                "sha256": digest,
            }
            self.verify()
        except BaseException:
            self._anchor.close()
            self._closed = True
            raise

    def verify(self) -> None:
        if self._closed:
            raise SmokeError("shared bounded-process authority is already closed")
        self._anchor.verify()
        if self._anchor.sha256() != self._implementation["sha256"]:
            raise SmokeError(
                "shared bounded-process authority changed after it was loaded"
            )

    def report(self) -> dict[str, Any]:
        self.verify()
        return dict(self._implementation)

    def __enter__(self) -> BoundedProcessAuthority:
        self.verify()
        return self

    def close(self) -> None:
        if not self._closed:
            try:
                self.verify()
            finally:
                self._anchor.close()
                self._closed = True

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        self.close()
        return False


@dataclass(frozen=True)
class PackageCommandResult:
    exit_code: int
    stdout: str
    stderr: str
    process_ledger: tuple[Mapping[str, Any], ...]
    receipt: Mapping[str, Any]


def _result_matches_receipt(result: Any, receipt: Mapping[str, Any]) -> bool:
    return (
        type(result.exit_code) is int
        and result.exit_code == receipt.get("exit_code")
        and type(result.stdout) is bytes
        and len(result.stdout) == receipt.get("stdout_size")
        and sha256_bytes(result.stdout) == receipt.get("stdout_sha256")
        and type(result.stderr) is bytes
        and len(result.stderr) == receipt.get("stderr_size")
        and sha256_bytes(result.stderr) == receipt.get("stderr_sha256")
        and isinstance(result.process_ledger, tuple)
        and list(result.process_ledger) == receipt.get("process_ledger")
    )


def run_checked(
    command: Sequence[str],
    *,
    cwd: Path,
    environment: Mapping[str, str],
    label: str,
    timeout: int = COMMAND_TIMEOUT_SECONDS,
    drain_timeout: int | None = None,
    maximum_output_bytes: int = MAX_COMMAND_OUTPUT_BYTES,
    stdin_bytes: bytes | None = None,
    authority: BoundedProcessAuthority | None = None,
) -> PackageCommandResult:
    owned_authority = authority is None
    active_authority = authority or BoundedProcessAuthority()
    try:
        active_authority.verify()
        if (
            not command
            or not all(isinstance(value, str) and value for value in command)
            or not isinstance(label, str)
            or not label
            or any(character in label for character in "\0\r\n")
        ):
            raise SmokeError("package command contract is invalid")
        canonical_cwd = cwd.resolve(strict=True)
        command_environment = dict(environment)
        effective_drain_timeout = (
            min(COMMAND_DRAIN_TIMEOUT_SECONDS, timeout)
            if drain_timeout is None
            else drain_timeout
        )
        module = active_authority.module
        try:
            executable = module.bind_executable(Path(command[0]))
            argv = [executable.path, *command[1:]]
            result = module.run(
                executable,
                argv,
                cwd=canonical_cwd,
                environment=command_environment,
                timeout_seconds=timeout,
                drain_timeout_seconds=effective_drain_timeout,
                maximum_output_bytes=maximum_output_bytes,
                description=label,
                stdin_bytes=stdin_bytes,
            )
        except module.BoundedProcessFailure as error:
            failed = error.result
            try:
                receipt = module.validate_failure_receipt(failed.receipt)
                receipt = module.verify_receipt_context(
                    receipt,
                    cwd=canonical_cwd,
                    environment=command_environment,
                    stdin_bytes=stdin_bytes,
                )
            except module.BoundedProcessError as validation_error:
                raise SmokeError(
                    f"{label} returned an invalid bounded-process failure receipt"
                ) from validation_error
            if (
                not _result_matches_receipt(failed, receipt)
                or failed.reason != receipt.get("failure_reason")
                or failed.cleanup_complete != receipt.get("cleanup_complete")
                or failed.output_complete != receipt.get("output_complete")
            ):
                raise SmokeError(
                    f"{label} bounded-process failure result differs from its receipt"
                )
            active_authority.verify()
            raise PackageCommandFailure(
                f"{label} failed boundedly ({failed.reason}); receipt "
                f"{receipt['receipt_sha256']}",
                receipt,
            ) from error
        except module.BoundedProcessError as error:
            raise SmokeError(
                f"{label} could not be run by the bounded-process authority"
            ) from error

        try:
            receipt = module.validate_execution_receipt(result.receipt)
            receipt = module.verify_receipt_context(
                receipt,
                cwd=canonical_cwd,
                environment=command_environment,
                stdin_bytes=stdin_bytes,
            )
        except module.BoundedProcessError as error:
            raise SmokeError(
                f"{label} returned an invalid bounded-process execution receipt"
            ) from error
        if not _result_matches_receipt(result, receipt):
            raise SmokeError(f"{label} result differs from its execution receipt")
        if result.exit_code != 0:
            raise PackageCommandFailure(
                f"{label} failed with status {result.exit_code}; receipt "
                f"{receipt['receipt_sha256']}",
                receipt,
            )
        try:
            stdout = result.stdout.decode("utf-8", errors="strict")
            stderr = result.stderr.decode("utf-8", errors="strict")
        except UnicodeDecodeError as error:
            raise PackageCommandFailure(
                f"{label} emitted non-UTF-8 command output; receipt "
                f"{receipt['receipt_sha256']}",
                receipt,
            ) from error
        active_authority.verify()
        return PackageCommandResult(
            exit_code=result.exit_code,
            stdout=stdout,
            stderr=stderr,
            process_ledger=tuple(result.process_ledger),
            receipt=receipt,
        )
    finally:
        try:
            active_authority.verify()
        finally:
            if owned_authority:
                active_authority.close()


class PackageCommandRunner(AbstractContextManager["PackageCommandRunner"]):
    """Fence declared inputs around every command and retain exact receipts."""

    def __init__(self, closure: DeclaredClosure) -> None:
        self.closure = closure
        self.receipts: list[Mapping[str, Any]] = []
        self.authority = BoundedProcessAuthority()
        self._closed = False

    def __enter__(self) -> PackageCommandRunner:
        self.authority.verify()
        self.closure.verify()
        return self

    def close(self) -> None:
        if not self._closed:
            try:
                self.closure.verify()
                self.authority.verify()
            finally:
                self.authority.close()
                self._closed = True

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        self.close()
        return False

    def implementation_report(self) -> dict[str, Any]:
        if self._closed:
            raise SmokeError("package command runner is already closed")
        return self.authority.report()

    def run(
        self,
        command: Sequence[str],
        *,
        cwd: Path,
        environment: Mapping[str, str],
        label: str,
        timeout: int = COMMAND_TIMEOUT_SECONDS,
    ) -> PackageCommandResult:
        if self._closed:
            raise SmokeError("package command runner is already closed")
        self.closure.verify()
        try:
            result = run_checked(
                command,
                cwd=cwd,
                environment=environment,
                label=label,
                timeout=timeout,
                authority=self.authority,
            )
        finally:
            self.closure.verify()
        receipt = getattr(result, "receipt", None)
        if not isinstance(receipt, Mapping):
            raise SmokeError(
                "package command did not return a shared bounded-process receipt"
            )
        self.receipts.append(dict(receipt))
        return result

    def version(
        self,
        command: Sequence[str],
        *,
        cwd: Path,
        environment: Mapping[str, str],
        label: str,
    ) -> str:
        result = self.run(
            command,
            cwd=cwd,
            environment=environment,
            label=label,
        )
        value = result.stdout.strip()
        if not value or "\n" in value:
            raise SmokeError(f"{label} returned an invalid version: {value!r}")
        return value


def copy_direct_file(source: Path, destination: Path) -> None:
    with AnchoredRegularFile(source, maximum_bytes=MAX_TREE_BYTES) as source_file:
        with AnchoredDirectory(destination.parent) as destination_parent:
            _safe_child_name(destination.name)
            mode = stat.S_IMODE(source_file.initial.mode) & 0o755
            descriptor = os.open(
                destination.name,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
                mode,
                dir_fd=destination_parent.fd,
            )
            try:
                offset = 0
                while offset < source_file.initial.size:
                    chunk = os.pread(
                        source_file.fd,
                        min(1024 * 1024, source_file.initial.size - offset),
                        offset,
                    )
                    if not chunk:
                        raise SmokeError(f"package input changed during copy: {source}")
                    view = memoryview(chunk)
                    while view:
                        written = os.write(descriptor, view)
                        if written <= 0:
                            raise SmokeError(
                                f"package destination stopped accepting bytes: {destination}"
                            )
                        view = view[written:]
                    offset += len(chunk)
                os.fchmod(descriptor, mode)
                os.fsync(descriptor)
            except BaseException:
                os.close(descriptor)
                try:
                    os.unlink(destination.name, dir_fd=destination_parent.fd)
                except OSError:
                    pass
                raise
            os.close(descriptor)
            os.fsync(destination_parent.fd)
            source_file.verify()


def _copy_direct_tree_descriptor(
    source_descriptor: int,
    destination_descriptor: int,
    *,
    relative: str,
) -> None:
    names = sorted(entry.name for entry in os.scandir(source_descriptor))
    for name in names:
        _safe_child_name(name)
        child_relative = name if relative == "." else f"{relative}/{name}"
        source_identity = StatIdentity.capture(
            os.stat(name, dir_fd=source_descriptor, follow_symlinks=False)
        )
        if stat.S_ISDIR(source_identity.mode):
            os.mkdir(name, mode=0o700, dir_fd=destination_descriptor)
            source_child = os.open(
                name,
                _directory_open_flags(),
                dir_fd=source_descriptor,
            )
            destination_child = os.open(
                name,
                _directory_open_flags(),
                dir_fd=destination_descriptor,
            )
            try:
                if StatIdentity.capture(os.fstat(source_child)) != source_identity:
                    raise SmokeError(
                        f"package source directory changed before copy: {child_relative}"
                    )
                _copy_direct_tree_descriptor(
                    source_child,
                    destination_child,
                    relative=child_relative,
                )
                os.fchmod(
                    destination_child,
                    stat.S_IMODE(source_identity.mode) & 0o755,
                )
                os.fsync(destination_child)
                if StatIdentity.capture(os.fstat(source_child)) != source_identity:
                    raise SmokeError(
                        f"package source directory changed during copy: {child_relative}"
                    )
            finally:
                os.close(destination_child)
                os.close(source_child)
            continue
        if not stat.S_ISREG(source_identity.mode) or source_identity.nlink != 1:
            raise SmokeError(f"package input contains an unsafe node: {child_relative}")
        source_file = os.open(
            name,
            _regular_open_flags(),
            dir_fd=source_descriptor,
        )
        destination_file = os.open(
            name,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            stat.S_IMODE(source_identity.mode) & 0o755,
            dir_fd=destination_descriptor,
        )
        try:
            if StatIdentity.capture(os.fstat(source_file)) != source_identity:
                raise SmokeError(
                    f"package source file changed before copy: {child_relative}"
                )
            offset = 0
            while offset < source_identity.size:
                chunk = os.pread(
                    source_file,
                    min(1024 * 1024, source_identity.size - offset),
                    offset,
                )
                if not chunk:
                    raise SmokeError(
                        f"package source file ended during copy: {child_relative}"
                    )
                view = memoryview(chunk)
                while view:
                    written = os.write(destination_file, view)
                    if written <= 0:
                        raise SmokeError(
                            f"package destination file stopped during copy: {child_relative}"
                        )
                    view = view[written:]
                offset += len(chunk)
            os.fchmod(destination_file, stat.S_IMODE(source_identity.mode) & 0o755)
            os.fsync(destination_file)
            if StatIdentity.capture(os.fstat(source_file)) != source_identity:
                raise SmokeError(
                    f"package source file changed during copy: {child_relative}"
                )
        finally:
            os.close(destination_file)
            os.close(source_file)


def copy_direct_tree(source: Path, destination: Path) -> None:
    with AnchoredDirectory(source) as source_anchor:
        before = source_anchor.snapshot()
        with AnchoredDirectory(destination.parent) as destination_parent:
            _safe_child_name(destination.name)
            os.mkdir(destination.name, mode=0o700, dir_fd=destination_parent.fd)
            destination_fd = os.open(
                destination.name,
                _directory_open_flags(),
                dir_fd=destination_parent.fd,
            )
            try:
                _copy_direct_tree_descriptor(
                    source_anchor.fd,
                    destination_fd,
                    relative=".",
                )
                os.fsync(destination_fd)
            finally:
                os.close(destination_fd)
            os.fsync(destination_parent.fd)
        after = source_anchor.snapshot()
        if before != after:
            raise SmokeError(f"package source tree changed during copy: {source}")


def _validate_typescript_archive_anchored(
    path: AnchoredRegularFile, source_manifest: Mapping[str, Any]
) -> dict[str, Any]:
    if path.initial.size > MAX_ARTIFACT_BYTES:
        raise SmokeError("TypeScript package artifact exceeds the 64 MiB gate bound")
    entries: dict[str, dict[str, Any]] = {}
    packaged_manifest_payload: bytes | None = None
    declared_total = 0
    retained_total = 0
    member_count = 0
    try:
        with path.open_binary() as compressed:
            with gzip.GzipFile(fileobj=compressed, mode="rb") as decompressed:
                bounded = BoundedArchiveStream(
                    decompressed,
                    limit=(
                        MAX_ARCHIVE_DECOMPRESSED_BYTES + MAX_TAR_FORMAT_OVERHEAD_BYTES
                    ),
                    label="TypeScript package archive",
                )
                with tarfile.open(fileobj=bounded, mode="r|") as archive:
                    for member in archive:
                        member_count += 1
                        if member_count > MAX_ARCHIVE_ENTRIES:
                            raise SmokeError(
                                "TypeScript package archive contains too many entries"
                            )
                        name = safe_archive_name(member.name)
                        if name in entries:
                            raise SmokeError(
                                f"TypeScript package archive repeats {name}"
                            )
                        if (
                            member.issym()
                            or member.islnk()
                            or not (member.isdir() or member.isfile())
                        ):
                            raise SmokeError(
                                "TypeScript package archive contains an unsafe node: "
                                f"{name}"
                            )
                        if member.isdir():
                            if member.size != 0:
                                raise SmokeError(
                                    "TypeScript package directory has payload bytes: "
                                    f"{name}"
                                )
                            entries[name] = {"type": "directory"}
                            continue

                        declared_total = checked_archive_total(
                            declared_total,
                            member.size,
                            label=f"TypeScript package entry {name}",
                        )
                        retain = name == "package/package.json"
                        if retain:
                            if member.size > MAX_ARCHIVE_METADATA_BYTES:
                                raise SmokeError(
                                    "TypeScript package metadata exceeds its byte bound"
                                )
                            retained_total += member.size
                            if retained_total > MAX_RETAINED_METADATA_BYTES:
                                raise SmokeError(
                                    "TypeScript package retained metadata exceeds its byte bound"
                                )
                        stream = archive.extractfile(member)
                        if stream is None:
                            raise SmokeError(
                                f"TypeScript package entry is unreadable: {name}"
                            )
                        with stream:
                            identity, retained = stream_archive_entry(
                                stream,
                                declared_size=member.size,
                                label=f"TypeScript package entry {name}",
                                retain=retain,
                            )
                        entries[name] = identity
                        if retain:
                            packaged_manifest_payload = retained
                # Consume any concatenated gzip members or trailing tar padding so
                # the outer decompression cap also covers bytes hidden after EOF.
                while bounded.read(ARCHIVE_READ_CHUNK_BYTES):
                    pass
    except SmokeError:
        raise
    except (EOFError, OSError, tarfile.TarError) as error:
        raise SmokeError(f"TypeScript package archive is invalid: {error}") from error

    missing = sorted(TYPESCRIPT_REQUIRED_FILES - entries.keys())
    if missing:
        raise SmokeError(
            f"TypeScript package archive is missing required files: {missing}"
        )
    forbidden = [
        name
        for name in entries
        if name.startswith(("package/src/", "package/tests/", "package/node_modules/"))
        or "__pycache__" in name
    ]
    if forbidden:
        raise SmokeError(
            f"TypeScript package archive contains source/test residue: {forbidden}"
        )
    packaged_files = {
        name for name, identity in entries.items() if "sha256" in identity
    }
    if packaged_files != TYPESCRIPT_REQUIRED_FILES:
        raise SmokeError(
            "TypeScript package archive file closure differs from the exact public artifact: "
            f"{sorted(packaged_files ^ TYPESCRIPT_REQUIRED_FILES)}"
        )
    packaged_directories = {
        name
        for name, identity in entries.items()
        if identity.get("type") == "directory"
    }
    if not packaged_directories.issubset(
        derived_archive_directories(TYPESCRIPT_REQUIRED_FILES)
    ):
        raise SmokeError("TypeScript package archive contains an extraneous directory")

    if packaged_manifest_payload is None:
        raise SmokeError("TypeScript package manifest payload is unavailable")
    packaged = strict_json_loads(
        packaged_manifest_payload,
        label="TypeScript packaged package.json",
    )
    if not isinstance(packaged, dict):
        raise SmokeError("TypeScript packaged package.json is not an object")
    for key in (
        "name",
        "version",
        "type",
        "main",
        "types",
        "exports",
        "engines",
        "files",
    ):
        if packaged.get(key) != source_manifest.get(key):
            raise SmokeError(f"TypeScript package metadata changed field {key}")
    if packaged.get("name") != "pmux-client" or packaged.get("type") != "module":
        raise SmokeError("TypeScript package name/type is not the public ESM contract")
    if (
        packaged.get("main") != "dist/index.js"
        or packaged.get("types") != "dist/index.d.ts"
    ):
        raise SmokeError(
            "TypeScript package entrypoints do not target the built artifact"
        )
    if packaged.get("dependencies") not in (None, {}):
        raise SmokeError("TypeScript client acquired an unexpected runtime dependency")
    path.verify()
    return {
        "entry_count": len(entries),
        "entries_sha256": canonical_json_digest(entries),
        "required_files": sorted(TYPESCRIPT_REQUIRED_FILES),
        "entries": entries,
    }


def validate_typescript_archive(
    path: Path | AnchoredRegularFile,
    source_manifest: Mapping[str, Any],
) -> dict[str, Any]:
    with artifact_anchor(path) as anchored:
        return _validate_typescript_archive_anchored(anchored, source_manifest)


def wheel_dist_info(entries: Iterable[str], version: str) -> str:
    normalized_version = version.replace("-", "_")
    candidates = sorted(
        name.rsplit("/", 1)[0]
        for name in entries
        if name.endswith(".dist-info/METADATA")
    )
    expected = f"pmux_client-{normalized_version}.dist-info"
    if candidates != [expected]:
        raise SmokeError(
            f"Python wheel has an unexpected dist-info directory: {candidates}, expected {expected}"
        )
    return expected


def parse_rfc822_metadata(payload: bytes) -> dict[str, list[str]]:
    text = payload.decode("utf-8")
    parsed: dict[str, list[str]] = {}
    current: str | None = None
    for line in text.splitlines():
        if not line:
            break
        if line[:1].isspace() and current is not None:
            parsed[current][-1] += "\n" + line
            continue
        key, separator, value = line.partition(":")
        if not separator:
            raise SmokeError("Python wheel metadata contains a malformed header")
        current = key
        parsed.setdefault(key, []).append(value.strip())
    return parsed


def validate_wheel_record(
    file_entries: Mapping[str, Mapping[str, Any]],
    record_payload: bytes,
    dist_info: str,
) -> None:
    record_name = f"{dist_info}/RECORD"
    try:
        record_text = record_payload.decode("utf-8")
    except UnicodeDecodeError as error:
        raise SmokeError("Python wheel RECORD is missing or invalid UTF-8") from error
    rows: dict[str, tuple[str, str]] = {}
    for row in csv.reader(record_text.splitlines()):
        if len(row) != 3:
            raise SmokeError("Python wheel RECORD contains a malformed row")
        name = safe_archive_name(row[0])
        if name in rows:
            raise SmokeError(f"Python wheel RECORD repeats {name}")
        rows[name] = (row[1], row[2])
    if set(rows) != set(file_entries):
        raise SmokeError(
            "Python wheel RECORD does not enumerate every archive file exactly once"
        )
    for name, identity in file_entries.items():
        digest, size = rows[name]
        if name == record_name:
            if digest or size:
                raise SmokeError(
                    "Python wheel RECORD must leave its own hash and size empty"
                )
            continue
        sha256_hex = identity.get("sha256")
        byte_count = identity.get("bytes")
        if not isinstance(sha256_hex, str) or not isinstance(byte_count, int):
            raise SmokeError(f"Python wheel has an invalid identity for {name}")
        try:
            expected = (
                base64.urlsafe_b64encode(bytes.fromhex(sha256_hex))
                .rstrip(b"=")
                .decode("ascii")
            )
        except ValueError as error:
            raise SmokeError(
                f"Python wheel has an invalid digest for {name}"
            ) from error
        if digest != f"sha256={expected}" or size != str(byte_count):
            raise SmokeError(f"Python wheel RECORD does not bind {name}")


def _validate_python_wheel_anchored(
    path: AnchoredRegularFile, *, source_project: Mapping[str, Any]
) -> dict[str, Any]:
    if path.initial.size > MAX_ARTIFACT_BYTES:
        raise SmokeError("Python wheel exceeds the 64 MiB gate bound")
    declared_entry_count = preflight_zip_directory(path)
    entries: dict[str, dict[str, Any]] = {}
    file_entries: dict[str, dict[str, Any]] = {}
    metadata_payloads: dict[str, bytes] = {}
    declared_total = 0
    retained_total = 0
    try:
        with path.open_binary() as wheel_stream:
            archive = zipfile.ZipFile(wheel_stream)
            with archive:
                infos = archive.infolist()
                if len(infos) != declared_entry_count:
                    raise SmokeError(
                        "Python wheel central-directory entry count changed"
                    )
                for info in infos:
                    name = safe_archive_name(info.filename)
                    if name in entries:
                        raise SmokeError(f"Python wheel repeats {name}")
                    if info.flag_bits & 0x1:
                        raise SmokeError(
                            f"Python wheel contains an encrypted entry: {name}"
                        )

                    unix_mode = (info.external_attr >> 16) & 0xFFFF
                    file_type = stat.S_IFMT(unix_mode)
                    if info.is_dir():
                        if file_type not in (0, stat.S_IFDIR):
                            raise SmokeError(
                                f"Python wheel directory has a non-directory mode: {name}"
                            )
                        if info.file_size != 0:
                            raise SmokeError(
                                f"Python wheel directory has payload bytes: {name}"
                            )
                        entries[name] = {"type": "directory"}
                        continue
                    if file_type not in (0, stat.S_IFREG):
                        raise SmokeError(
                            f"Python wheel contains a non-regular entry: {name}"
                        )

                    declared_total = checked_archive_total(
                        declared_total,
                        info.file_size,
                        label=f"Python wheel entry {name}",
                    )
                    retain = name.endswith(
                        (
                            ".dist-info/METADATA",
                            ".dist-info/WHEEL",
                            ".dist-info/RECORD",
                        )
                    )
                    if retain:
                        if info.file_size > MAX_ARCHIVE_METADATA_BYTES:
                            raise SmokeError(
                                f"Python wheel metadata exceeds its byte bound: {name}"
                            )
                        retained_total += info.file_size
                        if retained_total > MAX_RETAINED_METADATA_BYTES:
                            raise SmokeError(
                                "Python wheel retained metadata exceeds its byte bound"
                            )
                    with archive.open(info, mode="r") as stream:
                        identity, retained = stream_archive_entry(
                            stream,
                            declared_size=info.file_size,
                            label=f"Python wheel entry {name}",
                            retain=retain,
                        )
                    entries[name] = identity
                    file_entries[name] = identity
                    if retained is not None:
                        metadata_payloads[name] = retained
    except SmokeError:
        raise
    except (
        EOFError,
        NotImplementedError,
        OSError,
        RuntimeError,
        zipfile.BadZipFile,
    ) as error:
        raise SmokeError(f"Python wheel archive is invalid: {error}") from error

    missing = sorted(PYTHON_REQUIRED_PACKAGE_FILES - entries.keys())
    if missing:
        raise SmokeError(f"Python wheel is missing package files: {missing}")
    forbidden = [
        name
        for name in entries
        if name.startswith("tests/")
        or "/tests/" in name
        or "__pycache__" in name
        or name.endswith((".pyc", ".pyo"))
    ]
    if forbidden:
        raise SmokeError(f"Python wheel contains test/cache residue: {forbidden}")

    version = str(source_project["version"])
    dist_info = wheel_dist_info(entries, version)
    metadata_name = f"{dist_info}/METADATA"
    wheel_name = f"{dist_info}/WHEEL"
    for required in (metadata_name, wheel_name, f"{dist_info}/RECORD"):
        if required not in metadata_payloads:
            raise SmokeError(f"Python wheel is missing {required}")
    expected_files = PYTHON_REQUIRED_PACKAGE_FILES | frozenset(
        {
            metadata_name,
            wheel_name,
            f"{dist_info}/RECORD",
            f"{dist_info}/top_level.txt",
        }
    )
    if set(file_entries) != expected_files:
        raise SmokeError(
            "Python wheel file closure differs from the exact public artifact: "
            f"{sorted(set(file_entries) ^ expected_files)}"
        )
    wheel_directories = {
        name
        for name, identity in entries.items()
        if identity.get("type") == "directory"
    }
    if not wheel_directories.issubset(derived_archive_directories(expected_files)):
        raise SmokeError("Python wheel contains an extraneous directory")
    metadata = parse_rfc822_metadata(metadata_payloads[metadata_name])
    if metadata.get("Name") != [source_project["name"]]:
        raise SmokeError("Python wheel Name does not match pyproject.toml")
    if metadata.get("Version") != [version]:
        raise SmokeError("Python wheel Version does not match pyproject.toml")
    if metadata.get("Requires-Python") != [source_project["requires-python"]]:
        raise SmokeError("Python wheel Requires-Python does not match pyproject.toml")
    if metadata.get("Requires-Dist"):
        raise SmokeError(
            "dependency-free Python client acquired Requires-Dist metadata"
        )
    wheel_metadata = parse_rfc822_metadata(metadata_payloads[wheel_name])
    if wheel_metadata.get("Root-Is-Purelib") != ["true"]:
        raise SmokeError("Python wheel is not marked as a purelib artifact")
    if "py3-none-any" not in wheel_metadata.get("Tag", []):
        raise SmokeError(
            "Python wheel does not advertise the expected py3-none-any tag"
        )
    validate_wheel_record(
        file_entries,
        metadata_payloads[f"{dist_info}/RECORD"],
        dist_info,
    )
    path.verify()
    return {
        "entry_count": len(entries),
        "entries_sha256": canonical_json_digest(entries),
        "required_files": sorted(PYTHON_REQUIRED_PACKAGE_FILES),
        "dist_info": dist_info,
        "entries": entries,
    }


def validate_python_wheel(
    path: Path | AnchoredRegularFile,
    *,
    source_project: Mapping[str, Any],
) -> dict[str, Any]:
    with artifact_anchor(path) as anchored:
        return _validate_python_wheel_anchored(
            anchored,
            source_project=source_project,
        )


def _portable_files(snapshot: TreeSnapshot) -> dict[str, Mapping[str, Any]]:
    return {
        name: identity
        for name, identity in snapshot.portable.items()
        if identity.get("type") == "file"
    }


def _portable_directories(snapshot: TreeSnapshot) -> frozenset[str]:
    return frozenset(
        name
        for name, identity in snapshot.portable.items()
        if identity.get("type") == "directory"
    )


def validate_installed_typescript_closure(
    snapshot: TreeSnapshot,
    archive_entries: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    expected_files = {
        name.removeprefix("package/"): identity
        for name, identity in archive_entries.items()
        if name.startswith("package/") and "sha256" in identity
    }
    actual_files = _portable_files(snapshot)
    if set(actual_files) != set(expected_files):
        raise SmokeError(
            "installed TypeScript tree file closure differs from the tarball: "
            f"{sorted(set(actual_files) ^ set(expected_files))}"
        )
    for name, expected in expected_files.items():
        actual = actual_files[name]
        if actual.get("bytes") != expected.get("bytes") or actual.get(
            "sha256"
        ) != expected.get("sha256"):
            raise SmokeError(
                f"installed TypeScript file differs from the tarball: {name}"
            )
    expected_directories = frozenset({"."}) | derived_archive_directories(
        expected_files
    )
    if _portable_directories(snapshot) != expected_directories:
        raise SmokeError("installed TypeScript tree directory closure is not exact")
    normalized = {
        name: {
            "bytes": identity["bytes"],
            "sha256": identity["sha256"],
        }
        for name, identity in sorted(actual_files.items())
    }
    return {
        **snapshot.summary(),
        "normalized_closure_sha256": domain_json_digest(
            normalized,
            domain="pmux.package-smoke.installed-typescript.v1",
        ),
    }


def _read_small_direct_file(path: Path, *, maximum_bytes: int, label: str) -> bytes:
    with AnchoredRegularFile(path, maximum_bytes=maximum_bytes) as anchored:
        with anchored.open_binary() as stream:
            payload = stream.read(maximum_bytes + 1)
        if len(payload) > maximum_bytes:
            raise SmokeError(f"{label} exceeds its byte bound")
        anchored.verify()
        return payload


def validate_installed_python_closure(
    installed: AnchoredDirectory,
    snapshot: TreeSnapshot,
    archive_entries: Mapping[str, Mapping[str, Any]],
    *,
    dist_info: str,
    artifact: AnchoredRegularFile,
) -> dict[str, Any]:
    archive_files = {
        name: identity
        for name, identity in archive_entries.items()
        if "sha256" in identity
    }
    generated_files = frozenset(
        {
            f"{dist_info}/INSTALLER",
            f"{dist_info}/REQUESTED",
            f"{dist_info}/direct_url.json",
        }
    )
    actual_files = _portable_files(snapshot)
    expected_names = set(archive_files) | generated_files
    if set(actual_files) != expected_names:
        raise SmokeError(
            "installed Python tree file closure differs from wheel plus exact pip metadata: "
            f"{sorted(set(actual_files) ^ expected_names)}"
        )
    installed_record_name = f"{dist_info}/RECORD"
    for name, expected in archive_files.items():
        if name == installed_record_name:
            continue
        actual = actual_files[name]
        if actual.get("bytes") != expected.get("bytes") or actual.get(
            "sha256"
        ) != expected.get("sha256"):
            raise SmokeError(f"installed Python file differs from the wheel: {name}")
    expected_directories = frozenset({"."}) | derived_archive_directories(
        expected_names
    )
    if _portable_directories(snapshot) != expected_directories:
        raise SmokeError("installed Python tree directory closure is not exact")

    installer = _read_small_direct_file(
        installed.path / dist_info / "INSTALLER",
        maximum_bytes=64,
        label="pip INSTALLER metadata",
    )
    requested = _read_small_direct_file(
        installed.path / dist_info / "REQUESTED",
        maximum_bytes=64,
        label="pip REQUESTED metadata",
    )
    direct_url_payload = _read_small_direct_file(
        installed.path / dist_info / "direct_url.json",
        maximum_bytes=4096,
        label="pip direct_url metadata",
    )
    installed_record_payload = _read_small_direct_file(
        installed.path / installed_record_name,
        maximum_bytes=MAX_ARCHIVE_METADATA_BYTES,
        label="installed pip RECORD metadata",
    )
    if installer != b"pip\n" or requested != b"":
        raise SmokeError("pip-generated installed metadata is not exact")
    direct_url = strict_json_loads(direct_url_payload, label="pip direct_url metadata")
    artifact_sha256 = artifact.sha256()
    expected_direct_url = {
        "archive_info": {
            "hash": f"sha256={artifact_sha256}",
            "hashes": {"sha256": artifact_sha256},
        },
        "url": artifact.path.as_uri(),
    }
    if direct_url != expected_direct_url:
        raise SmokeError("pip direct_url metadata does not bind the exact wheel")
    validate_wheel_record(
        actual_files,
        installed_record_payload,
        dist_info,
    )
    installed.verify_path()
    artifact.verify()
    after_metadata = installed.snapshot()
    if after_metadata != snapshot:
        raise SmokeError("installed Python tree changed while metadata was verified")

    normalized_files = {
        name: {
            "bytes": identity["bytes"],
            "sha256": identity["sha256"],
        }
        for name, identity in sorted(actual_files.items())
        if name not in {f"{dist_info}/direct_url.json", installed_record_name}
    }
    normalized_files[f"{dist_info}/direct_url.json"] = {
        "archive_sha256": artifact_sha256,
        "url": "$ARTIFACT",
    }
    normalized_files[installed_record_name] = {
        "semantic": "exact-installed-file-closure-record-v1"
    }
    return {
        **snapshot.summary(),
        "normalized_closure_sha256": domain_json_digest(
            normalized_files,
            domain="pmux.package-smoke.installed-python.v1",
        ),
    }


def validate_consumer_report(
    report: Any,
    *,
    language: str,
) -> dict[str, Any]:
    common = {
        "api",
        "client_constructed_without_io",
        "protocol_version",
        "turn_id",
    }
    expected_keys = (
        common | {"smithers_transport_constructed_without_io"}
        if language == "typescript"
        else common | {"py_typed"}
    )
    if not isinstance(report, dict) or set(report) != expected_keys:
        raise SmokeError(f"installed {language} consumer report schema is not exact")
    if (
        report.get("api") != "native_pmux_v1"
        or report.get("client_constructed_without_io") is not True
        or type(report.get("protocol_version")) is not int
        or report["protocol_version"] != 1
        or not isinstance(report.get("turn_id"), str)
        or re.fullmatch(
            r"[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}",
            report["turn_id"],
        )
        is None
    ):
        raise SmokeError(f"installed {language} consumer report values are invalid")
    if language == "typescript":
        if report.get("smithers_transport_constructed_without_io") is not True:
            raise SmokeError(
                "installed TypeScript Smithers transport was not constructed"
            )
    elif language == "python":
        if report.get("py_typed") is not True:
            raise SmokeError("installed Python package did not report py.typed")
    else:
        raise SmokeError(f"unknown consumer report language: {language}")
    return dict(report)


def assert_tree_anchor_unchanged(
    expected: TreeSnapshot,
    anchor: AnchoredDirectory,
    *,
    label: str,
) -> None:
    if anchor.snapshot() != expected:
        raise SmokeError(f"{label} changed after it was frozen")


def _canonical_declared_path(declared: DeclaredInput) -> Path:
    if declared.file is not None:
        return declared.file.path
    if declared.directory is not None:
        return declared.directory.path
    raise SmokeError(f"declared input is not anchored: {declared.role}")


def _path_is_within(path: Path, root: Path) -> bool:
    return path != root and path.is_relative_to(root)


def _is_canonical_existing_path_within(
    value: object,
    root: Path,
    *,
    directory: bool,
) -> bool:
    if not isinstance(value, str):
        return False
    try:
        resolved = Path(value).resolve(strict=True)
    except OSError:
        return False
    expected_kind = resolved.is_dir() if directory else resolved.is_file()
    return expected_kind and str(resolved) == value and _path_is_within(resolved, root)


def validate_declared_layout(closure: DeclaredClosure, gate: str) -> None:
    """Require the candidate's materialized role roots to be unambiguous.

    Content hashes alone are insufficient if two logical roles alias or nest in
    one mutable tree.  The only permitted containment relationships are the
    interpreter scripts that are intentionally members of their declared tool
    trees.
    """

    paths = {
        role: _canonical_declared_path(closure.input(role))
        for role in EXPECTED_DECLARED_INPUTS[gate]
    }
    _validate_role_path_layout(gate, paths)


def _validate_role_path_layout(gate: str, paths: Mapping[str, Path]) -> None:
    if gate not in EXPECTED_DECLARED_INPUTS or set(paths) != set(
        EXPECTED_DECLARED_INPUTS[gate]
    ):
        raise SmokeError("declared package role-path inventory is invalid")
    tree_roles = [
        role
        for role, (kind, _usage) in EXPECTED_DECLARED_INPUTS[gate].items()
        if kind == "tree"
    ]
    for index, first_role in enumerate(tree_roles):
        for second_role in tree_roles[index + 1 :]:
            first = paths[first_role]
            second = paths[second_role]
            if (
                first == second
                or _path_is_within(first, second)
                or _path_is_within(second, first)
            ):
                raise SmokeError(
                    "declared package support trees overlap: "
                    f"{first_role}, {second_role}"
                )

    permitted_parents = {
        "typescript": {
            "npm_executable": "npm_support_tree",
            "typescript_compiler": "typescript_dependency_tree",
        },
        "python": {},
    }[gate]
    for role, (kind, _usage) in EXPECTED_DECLARED_INPUTS[gate].items():
        if kind != "file":
            continue
        containers = [
            tree_role
            for tree_role in tree_roles
            if _path_is_within(paths[role], paths[tree_role])
        ]
        expected_parent = permitted_parents.get(role)
        if containers != ([] if expected_parent is None else [expected_parent]):
            raise SmokeError(f"declared package file containment is invalid for {role}")


def validate_python_tool_report(
    report: object,
    *,
    python: Path,
    stdlib: Path,
    dynload: Path,
    build_support: Path,
) -> dict[str, Any]:
    """Validate the isolated interpreter and its complete materialized tools."""

    # The two required names come from the declared contract, not from a fourth
    # copy of them; `build`, `wheel` and `ruff` are the names this report must
    # carry as ABSENT, so they stay written out here.
    tool_keys = {
        "executable",
        "python",
        "sys_path_before",
        "sys_path_after",
        "isolation",
        "module_files",
        "distributions",
        "vendor_distributions",
        "build",
        "wheel",
        "ruff",
        *PYTHON_BUILD_SUPPORT_DISTRIBUTIONS,
    }
    if not isinstance(report, dict) or set(report) != tool_keys:
        raise SmokeError("Python package tool report schema is not exact")
    executable = report.get("executable")
    python_version = report.get("python")
    if (
        not isinstance(executable, str)
        or executable != str(python)
        or not isinstance(python_version, str)
        or not python_version
        or any(character in python_version for character in "\0\r\n")
        or report.get("sys_path_before")
        != [str(stdlib), str(dynload), str(build_support)]
    ):
        raise SmokeError("Python package tool report runtime is not exact")
    isolation = report.get("isolation")
    if (
        not isinstance(isolation, dict)
        or set(isolation) != {"isolated", "ignore_environment", "no_site"}
        or any(type(isolation[key]) is not int for key in isolation)
        or isolation != {"isolated": 1, "ignore_environment": 1, "no_site": 1}
    ):
        raise SmokeError("Python package tool report isolation is not exact")

    build_support_versions = [
        report.get(name) for name in PYTHON_BUILD_SUPPORT_DISTRIBUTIONS
    ]
    missing = [
        name
        for name, value in zip(
            PYTHON_BUILD_SUPPORT_DISTRIBUTIONS, build_support_versions, strict=True
        )
        if value is None
    ]
    if missing:
        # Named, because this is the one failure a caller can fix without
        # reading the source: their declared build-support tree does not carry
        # the distribution the wheel backend needs, and every other message here
        # would have said only "is not exact".
        raise SmokeError(
            "declared Python build-support tree publishes no metadata for: "
            + ", ".join(missing)
        )
    if not all(
        isinstance(value, str)
        and value
        and not any(character in value for character in "\0\r\n")
        for value in build_support_versions
    ) or any(report.get(name) is not None for name in ("build", "wheel", "ruff")):
        raise SmokeError("Python wheel build-tool inventory is not exact")

    module_files = report.get("module_files")
    if (
        not isinstance(module_files, dict)
        or set(module_files) != set(PYTHON_BUILD_SUPPORT_DISTRIBUTIONS)
        or not all(
            _is_canonical_existing_path_within(
                value,
                build_support,
                directory=False,
            )
            for value in module_files.values()
        )
    ):
        raise SmokeError("Python build-tool module origins are not exact")
    expected_distributions = [
        [name, version]
        for name, version in zip(
            PYTHON_BUILD_SUPPORT_DISTRIBUTIONS, build_support_versions, strict=True
        )
    ]
    distributions = report.get("distributions")
    if (
        not isinstance(distributions, list)
        or distributions != expected_distributions
        or any(
            not isinstance(item, list)
            or len(item) != 2
            or not all(isinstance(value, str) and value for value in item)
            for item in distributions
        )
    ):
        raise SmokeError("Python isolated build-tool distribution closure is not exact")
    sys_path_after = report.get("sys_path_after")
    expected_prefix = [str(stdlib), str(dynload), str(build_support)]
    if (
        not isinstance(sys_path_after, list)
        or not all(isinstance(value, str) for value in sys_path_after)
        or sys_path_after[: len(expected_prefix)] != expected_prefix
        or len(set(sys_path_after)) != len(sys_path_after)
    ):
        raise SmokeError("Python build-tool post-import path is not exact")
    vendor_paths = sys_path_after[len(expected_prefix) :]
    if not vendor_paths or not all(
        _is_canonical_existing_path_within(
            value,
            build_support,
            directory=True,
        )
        for value in vendor_paths
    ):
        raise SmokeError("Python build-tool vendor paths are not materialized")
    vendor_distributions = report.get("vendor_distributions")
    if not isinstance(vendor_distributions, list) or len(vendor_distributions) != len(
        vendor_paths
    ):
        raise SmokeError("Python vendored distribution inventory is not exact")
    seen_vendor_names: set[str] = set()
    for expected_path, vendor in zip(
        vendor_paths,
        vendor_distributions,
        strict=True,
    ):
        if (
            not isinstance(vendor, dict)
            or set(vendor) != {"path", "distributions"}
            or vendor.get("path") != expected_path
            or not isinstance(vendor.get("distributions"), list)
            or not vendor["distributions"]
        ):
            raise SmokeError("Python vendored distribution record is not exact")
        names: list[str] = []
        for item in vendor["distributions"]:
            if (
                not isinstance(item, list)
                or len(item) != 2
                or not all(isinstance(value, str) and value for value in item)
                or item[0] in seen_vendor_names
            ):
                raise SmokeError("Python vendored distribution values are not exact")
            seen_vendor_names.add(item[0])
            names.append(item[0])
        if names != sorted(names):
            raise SmokeError("Python vendored distributions are not ordered")
    if "wheel" not in seen_vendor_names:
        raise SmokeError("setuptools vendored wheel support is unavailable")
    return dict(report)


def build_typescript_package(workspace: Path) -> dict[str, Any]:
    before = client_tree_snapshot(workspace)
    source = workspace / "clients/typescript"
    with AnchoredDirectory(source) as source_anchor:
        source_before = source_anchor.snapshot(
            excluded_top_level=frozenset({"node_modules"})
        )
        package_manifest = strict_json_loads(
            _read_small_direct_file(
                source / "package.json",
                maximum_bytes=MAX_ARCHIVE_METADATA_BYTES,
                label="TypeScript source package.json",
            ),
            label="TypeScript source package.json",
        )
        if not isinstance(package_manifest, dict):
            raise SmokeError("TypeScript source package.json is not an object")

        with load_declared_closure("typescript") as closure:
            node = _canonical_declared_path(closure.input("node_executable"))
            npm = _canonical_declared_path(closure.input("npm_executable"))
            npm_support = _canonical_declared_path(closure.input("npm_support_tree"))
            tsc = _canonical_declared_path(closure.input("typescript_compiler"))
            typescript_tree = _canonical_declared_path(
                closure.input("typescript_dependency_tree")
            )
            type_roots = _canonical_declared_path(
                closure.input("node_types_dependency_tree")
            )
            undici_types = _canonical_declared_path(
                closure.input("undici_types_dependency_tree")
            )
            if (
                typescript_tree
                != (source / "node_modules/typescript").resolve(strict=True)
                or type_roots != (source / "node_modules/@types").resolve(strict=True)
                or undici_types
                != (source / "node_modules/undici-types").resolve(strict=True)
            ):
                raise SmokeError(
                    "declared TypeScript dependency trees do not match the locked repository inputs"
                )
            if not npm.is_relative_to(npm_support):
                raise SmokeError(
                    "declared npm CLI is outside the exact npm support tree"
                )
            if not tsc.is_relative_to(typescript_tree):
                raise SmokeError(
                    "declared TypeScript compiler is outside its dependency tree"
                )
            node_input = closure.input("node_executable")
            if node_input.file is None or not (
                stat.S_IMODE(node_input.file.initial.mode) & stat.S_IXUSR
            ):
                raise SmokeError("declared Node runtime is not owner-executable")

            owned = OwnedTemporaryRoot("pmux-typescript-package-")
            with owned as temporary, ExitStack() as stack:
                environment = deterministic_environment(
                    temporary,
                    executables=(node,),
                )
                stage_anchor = stack.enter_context(
                    create_private_child_directory(owned.anchor, "stage")
                )
                artifacts_anchor = stack.enter_context(
                    create_private_child_directory(owned.anchor, "artifacts")
                )
                consumer_anchor = stack.enter_context(
                    create_private_child_directory(owned.anchor, "consumer")
                )
                stage = stage_anchor.path
                artifacts = artifacts_anchor.path
                consumer = consumer_anchor.path
                runner = stack.enter_context(PackageCommandRunner(closure))

                for name in (
                    "package.json",
                    "package-lock.json",
                    "README.md",
                    "tsconfig.json",
                ):
                    copy_direct_file(source / name, stage / name)
                copy_direct_tree(source / "src", stage / "src")
                stage_input = stage_anchor.snapshot()
                with private_dependency_alias(
                    stage_anchor,
                    "node_modules",
                    source / "node_modules",
                ):
                    runner.run(
                        [
                            str(node),
                            str(tsc),
                            "--project",
                            str(stage / "tsconfig.json"),
                            "--pretty",
                            "false",
                        ],
                        cwd=stage,
                        environment=environment,
                        label="isolated TypeScript package build",
                    )
                stage_built = stage_anchor.snapshot()

                pack = runner.run(
                    [
                        str(node),
                        str(npm),
                        "pack",
                        ".",
                        "--json",
                        "--offline",
                        "--ignore-scripts",
                        "--no-audit",
                        "--no-fund",
                        "--pack-destination",
                        str(artifacts),
                    ],
                    cwd=stage,
                    environment=environment,
                    label="actual TypeScript npm package creation",
                )
                assert_tree_anchor_unchanged(
                    stage_built,
                    stage_anchor,
                    label="TypeScript staging tree after pack",
                )
                pack_report = strict_json_loads(pack.stdout, label="npm pack report")
                if not isinstance(pack_report, list) or len(pack_report) != 1:
                    raise SmokeError("npm pack did not create exactly one artifact")
                pack_entry = pack_report[0]
                if not isinstance(pack_entry, dict):
                    raise SmokeError("npm pack artifact report is not an object")
                filename = pack_entry.get("filename")
                expected_filename = f"{package_manifest.get('name')}-{package_manifest.get('version')}.tgz"
                if (
                    not isinstance(filename, str)
                    or Path(filename).name != filename
                    or filename != expected_filename
                ):
                    raise SmokeError(
                        "npm pack returned an unexpected artifact filename"
                    )
                artifact_tree = artifacts_anchor.snapshot()
                artifact_files = _portable_files(artifact_tree)
                if set(artifact_files) != {filename}:
                    raise SmokeError("npm pack artifact-directory closure is not exact")

                with AnchoredRegularFile(artifacts / filename) as artifact_anchor:
                    npm_identity = verify_npm_artifact_identity(
                        pack_entry,
                        artifact_anchor,
                    )
                    archive = validate_typescript_archive(
                        artifact_anchor,
                        package_manifest,
                    )
                    archive_entries = archive.pop("entries")
                    artifact_anchor.verify()
                    artifact_frozen = artifacts_anchor.snapshot()

                    consumer_manifest = json.dumps(
                        {
                            "name": "pmux-package-smoke-consumer",
                            "private": True,
                            "type": "module",
                        },
                        sort_keys=True,
                        separators=(",", ":"),
                    ).encode("utf-8")
                    write_new_private_file(
                        consumer_anchor,
                        "package.json",
                        consumer_manifest,
                    )
                    runner.run(
                        [
                            str(node),
                            str(npm),
                            "install",
                            "--offline",
                            "--ignore-scripts",
                            "--no-audit",
                            "--no-fund",
                            "--package-lock=false",
                            "--save=false",
                            str(artifact_anchor.path),
                        ],
                        cwd=consumer,
                        environment=environment,
                        label="isolated TypeScript tarball install",
                    )
                    artifact_anchor.verify()
                    assert_tree_anchor_unchanged(
                        artifact_frozen,
                        artifacts_anchor,
                        label="TypeScript artifact directory through install",
                    )
                    assert_tree_anchor_unchanged(
                        stage_built,
                        stage_anchor,
                        label="TypeScript staging tree through install",
                    )

                    node_modules_anchor = stack.enter_context(
                        open_direct_child_directory(consumer_anchor, "node_modules")
                    )
                    installed_anchor = stack.enter_context(
                        open_direct_child_directory(
                            node_modules_anchor,
                            "pmux-client",
                        )
                    )
                    installed_snapshot = installed_anchor.snapshot()
                    installed_closure = validate_installed_typescript_closure(
                        installed_snapshot,
                        archive_entries,
                    )
                    fixtures = workspace / "tools/package-smoke/fixtures"
                    copy_direct_file(
                        fixtures / "typescript-consumer.mjs",
                        consumer / "consumer.mjs",
                    )
                    copy_direct_file(
                        fixtures / "typescript-consumer.ts",
                        consumer / "consumer.ts",
                    )
                    consumer_ready = consumer_anchor.snapshot()
                    runtime = runner.run(
                        [str(node), str(consumer / "consumer.mjs")],
                        cwd=consumer,
                        environment=environment,
                        label="installed TypeScript runtime import",
                    )
                    runtime_report = validate_consumer_report(
                        strict_json_loads(
                            runtime.stdout,
                            label="installed TypeScript consumer report",
                        ),
                        language="typescript",
                    )
                    runner.run(
                        [
                            str(node),
                            str(tsc),
                            "--noEmit",
                            "--strict",
                            "--target",
                            "ES2022",
                            "--module",
                            "NodeNext",
                            "--moduleResolution",
                            "NodeNext",
                            "--types",
                            "node",
                            "--typeRoots",
                            str(type_roots),
                            str(consumer / "consumer.ts"),
                        ],
                        cwd=consumer,
                        environment=environment,
                        label="installed TypeScript declaration consumer",
                    )
                    node_version = runner.version(
                        [str(node), "--version"],
                        cwd=consumer,
                        environment=environment,
                        label="node version",
                    )
                    npm_version = runner.version(
                        [str(node), str(npm), "--version"],
                        cwd=consumer,
                        environment=environment,
                        label="npm version",
                    )
                    typescript_version = runner.version(
                        [str(node), str(tsc), "--version"],
                        cwd=consumer,
                        environment=environment,
                        label="TypeScript version",
                    )
                    assert_tree_anchor_unchanged(
                        installed_snapshot,
                        installed_anchor,
                        label="installed TypeScript package",
                    )
                    assert_tree_anchor_unchanged(
                        consumer_ready,
                        consumer_anchor,
                        label="TypeScript consumer tree",
                    )
                    assert_tree_anchor_unchanged(
                        artifact_frozen,
                        artifacts_anchor,
                        label="TypeScript artifact directory",
                    )
                    assert_tree_anchor_unchanged(
                        stage_built,
                        stage_anchor,
                        label="TypeScript staging tree",
                    )
                    closure_report = closure.report()
                    result = {
                        "schema_version": SCHEMA_VERSION,
                        "gate": "typescript_package_artifact",
                        "package": {
                            "name": package_manifest["name"],
                            "version": package_manifest["version"],
                        },
                        "declared_closure": closure_report,
                        "source_input": stage_input.summary(),
                        "built_stage": stage_built.summary(),
                        "artifact": {
                            "filename": filename,
                            "bytes": artifact_anchor.initial.size,
                            "sha256": artifact_anchor.sha256(),
                            "npm_sha1": npm_identity["shasum"],
                            "npm_integrity": npm_identity["integrity"],
                            **archive,
                        },
                        "installed": installed_closure,
                        "consumer": runtime_report,
                        "toolchain": {
                            "node": node_version,
                            "npm": npm_version,
                            "typescript": typescript_version,
                        },
                        "bounded_process_implementation": runner.implementation_report(),
                        "command_receipts": runner.receipts,
                        "dependency_acquisition": "registry_disabled_by_npm_offline",
                        "socket_network_sandbox": "not_applied_or_claimed",
                        "credential_environment": "explicit_allowlist_only",
                        "package_scripts": "disabled",
                        "audit_and_fund_requests": "disabled",
                    }

        source_after = source_anchor.snapshot(
            excluded_top_level=frozenset({"node_modules"})
        )
        if source_after != source_before:
            raise SmokeError("TypeScript source tree changed during package gate")

    assert_snapshot_unchanged(before, client_tree_snapshot(workspace))
    result["temporary_state_removed"] = True
    result["repository_client_trees_unchanged"] = True
    return result


def build_python_package(workspace: Path) -> dict[str, Any]:
    before = client_tree_snapshot(workspace)
    source = workspace / "clients/python"
    with AnchoredDirectory(source) as source_anchor:
        source_before = source_anchor.snapshot()
        pyproject = tomllib.loads(
            _read_small_direct_file(
                source / "pyproject.toml",
                maximum_bytes=MAX_ARCHIVE_METADATA_BYTES,
                label="Python source pyproject.toml",
            ).decode("utf-8")
        )
        project = pyproject["project"]
        if not isinstance(project, dict):
            raise SmokeError("Python source project metadata is not a table")

        with load_declared_closure("python") as closure:
            python_input = closure.input("python_executable")
            python = _canonical_declared_path(python_input)
            python_stdlib = _canonical_declared_path(
                closure.input("python_stdlib_tree")
            )
            python_dynload = _canonical_declared_path(
                closure.input("python_dynload_tree")
            )
            python_build_support = _canonical_declared_path(
                closure.input("python_build_support_tree")
            )
            if len({python_stdlib, python_dynload, python_build_support}) != 3:
                raise SmokeError("declared Python support trees are not distinct")
            if python_input.file is None or not (
                stat.S_IMODE(python_input.file.initial.mode) & stat.S_IXUSR
            ):
                raise SmokeError("declared Python runtime is not owner-executable")

            owned = OwnedTemporaryRoot("pmux-python-package-")
            with owned as temporary, ExitStack() as stack:
                environment = deterministic_environment(
                    temporary,
                    executables=(python,),
                )
                stage_anchor = stack.enter_context(
                    create_private_child_directory(owned.anchor, "stage")
                )
                artifacts_anchor = stack.enter_context(
                    create_private_child_directory(owned.anchor, "artifacts")
                )
                installed_anchor = stack.enter_context(
                    create_private_child_directory(owned.anchor, "installed")
                )
                consumer_anchor = stack.enter_context(
                    create_private_child_directory(owned.anchor, "consumer")
                )
                stage = stage_anchor.path
                artifacts = artifacts_anchor.path
                installed = installed_anchor.path
                consumer = consumer_anchor.path
                runner = stack.enter_context(PackageCommandRunner(closure))

                for name in ("pyproject.toml", "README.md"):
                    copy_direct_file(source / name, stage / name)
                copy_direct_tree(source / "pmux_client", stage / "pmux_client")
                stage_input = stage_anchor.snapshot()
                wheel_build = runner.run(
                    [
                        str(python),
                        "-I",
                        "-S",
                        "-B",
                        "-c",
                        PYTHON_ISOLATED_CODE_BOOTSTRAP,
                        str(python_stdlib),
                        str(python_dynload),
                        str(python_build_support),
                        PYTHON_BUILD_WHEEL_SCRIPT,
                        str(artifacts),
                    ],
                    cwd=stage,
                    environment=environment,
                    label="isolated setuptools PEP 517 wheel creation",
                )
                stage_built = stage_anchor.snapshot()
                artifact_tree = artifacts_anchor.snapshot()
                artifact_files = _portable_files(artifact_tree)
                normalized_name = str(project["name"]).replace("-", "_")
                normalized_version = str(project["version"]).replace("-", "_")
                expected_filename = (
                    f"{normalized_name}-{normalized_version}-py3-none-any.whl"
                )
                wheel_build_report = strict_json_loads(
                    wheel_build.stdout,
                    label="setuptools wheel build report",
                )
                if wheel_build_report != {"filename": expected_filename}:
                    raise SmokeError(
                        "setuptools returned an inexact wheel build report"
                    )
                if set(artifact_files) != {expected_filename}:
                    raise SmokeError(
                        "setuptools wheel artifact-directory closure or filename is not exact"
                    )

                with AnchoredRegularFile(
                    artifacts / expected_filename
                ) as artifact_anchor:
                    archive = validate_python_wheel(
                        artifact_anchor,
                        source_project=project,
                    )
                    archive_entries = archive.pop("entries")
                    artifact_frozen = artifacts_anchor.snapshot()
                    runner.run(
                        [
                            str(python),
                            "-I",
                            "-S",
                            "-B",
                            "-c",
                            PYTHON_ISOLATED_MODULE_BOOTSTRAP,
                            str(python_stdlib),
                            str(python_dynload),
                            str(python_build_support),
                            "pip",
                            "install",
                            "--no-index",
                            "--no-deps",
                            "--no-compile",
                            "--disable-pip-version-check",
                            "--target",
                            str(installed),
                            str(artifact_anchor.path),
                        ],
                        cwd=temporary,
                        environment=environment,
                        label="isolated Python wheel install",
                    )
                    artifact_anchor.verify()
                    assert_tree_anchor_unchanged(
                        artifact_frozen,
                        artifacts_anchor,
                        label="Python artifact directory through install",
                    )
                    assert_tree_anchor_unchanged(
                        stage_built,
                        stage_anchor,
                        label="Python staging tree through install",
                    )
                    installed_snapshot = installed_anchor.snapshot()
                    installed_closure = validate_installed_python_closure(
                        installed_anchor,
                        installed_snapshot,
                        archive_entries,
                        dist_info=str(archive["dist_info"]),
                        artifact=artifact_anchor,
                    )

                    fixture = (
                        workspace / "tools/package-smoke/fixtures/python-consumer.py"
                    )
                    copy_direct_file(fixture, consumer / "consumer.py")
                    consumer_ready = consumer_anchor.snapshot()
                    imported = runner.run(
                        [
                            str(python),
                            "-I",
                            "-S",
                            "-B",
                            "-c",
                            PYTHON_ISOLATED_SCRIPT_BOOTSTRAP,
                            str(installed),
                            str(python_stdlib),
                            str(python_dynload),
                            str(consumer / "consumer.py"),
                            str(installed),
                            str(project["name"]),
                            str(project["version"]),
                        ],
                        cwd=consumer,
                        environment=environment,
                        label="installed Python artifact import",
                    )
                    import_report = validate_consumer_report(
                        strict_json_loads(
                            imported.stdout,
                            label="installed Python consumer report",
                        ),
                        language="python",
                    )
                    tool_result = runner.run(
                        [
                            str(python),
                            "-I",
                            "-S",
                            "-B",
                            "-c",
                            PYTHON_ISOLATED_CODE_BOOTSTRAP,
                            str(python_stdlib),
                            str(python_dynload),
                            str(python_build_support),
                            PYTHON_TOOL_REPORT_SCRIPT,
                            str(python_build_support),
                        ],
                        cwd=consumer,
                        environment=environment,
                        label="Python package tool closure report",
                    )
                    tool_report = validate_python_tool_report(
                        strict_json_loads(
                            tool_result.stdout,
                            label="Python package tool closure report",
                        ),
                        python=python,
                        stdlib=python_stdlib,
                        dynload=python_dynload,
                        build_support=python_build_support,
                    )

                    assert_tree_anchor_unchanged(
                        installed_snapshot,
                        installed_anchor,
                        label="installed Python package",
                    )
                    assert_tree_anchor_unchanged(
                        consumer_ready,
                        consumer_anchor,
                        label="Python consumer tree",
                    )
                    assert_tree_anchor_unchanged(
                        artifact_frozen,
                        artifacts_anchor,
                        label="Python artifact directory",
                    )
                    assert_tree_anchor_unchanged(
                        stage_built,
                        stage_anchor,
                        label="Python staging tree",
                    )
                    closure_report = closure.report()
                    result = {
                        "schema_version": SCHEMA_VERSION,
                        "gate": "python_package_artifact",
                        "package": {
                            "name": project["name"],
                            "version": project["version"],
                        },
                        "declared_closure": closure_report,
                        "source_input": stage_input.summary(),
                        "built_stage": stage_built.summary(),
                        "artifact": {
                            "filename": expected_filename,
                            "bytes": artifact_anchor.initial.size,
                            "sha256": artifact_anchor.sha256(),
                            **archive,
                        },
                        "installed": installed_closure,
                        "consumer": import_report,
                        "toolchain": tool_report,
                        "bounded_process_implementation": runner.implementation_report(),
                        "command_receipts": runner.receipts,
                        "dependency_acquisition": (
                            "direct_setuptools_backend_and_pip_index_disabled"
                        ),
                        "socket_network_sandbox": "not_applied_or_claimed",
                        "credential_environment": "explicit_allowlist_only",
                        "dependency_resolution": "disabled",
                        "bytecode_generation": "disabled",
                    }

        source_after = source_anchor.snapshot()
        if source_after != source_before:
            raise SmokeError("Python source tree changed during package gate")

    assert_snapshot_unchanged(before, client_tree_snapshot(workspace))
    result["temporary_state_removed"] = True
    result["repository_client_trees_unchanged"] = True
    return result


def workspace_root() -> Path:
    return Path(__file__).resolve().parents[2]


def parse_args(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build, install, and inspect actual pmux client package artifacts"
    )
    parser.add_argument("gate", choices=("typescript", "python"))
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    options = parse_args(sys.argv[1:] if arguments is None else arguments)
    workspace = workspace_root()
    try:
        if options.gate == "typescript":
            report: Any = build_typescript_package(workspace)
        elif options.gate == "python":
            report = build_python_package(workspace)
    except (KeyError, OSError, SmokeError, ValueError) as error:
        print(f"package-smoke: {error}", file=sys.stderr)
        return 1
    print(
        json.dumps(
            report,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
