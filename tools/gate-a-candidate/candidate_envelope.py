#!/usr/bin/env python3
"""Bind one Gate A source tree and release-binary set across ordered gates.

This is an evidence envelope only.  Source and executable identity are owned by
the existing canonical Linux-evidence primitives; this module adds the local
Gate A ordering, Cargo target binding, and immutable checkpoint chain.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import pathlib
import re
import shutil
import stat
import sys
import tarfile
import tomllib
import types
from dataclasses import dataclass
from collections.abc import Callable, Mapping, Sequence
from typing import Any

sys.dont_write_bytecode = True

TOOLS_ROOT = pathlib.Path(__file__).resolve().parents[1]
LINUX_EVIDENCE_ROOT = TOOLS_ROOT / "linux-docker"
EVIDENCE_COMMON_ROOT = TOOLS_ROOT / "evidence_common"
PACKAGE_SMOKE_ROOT = TOOLS_ROOT / "package-smoke"
MAX_AUTHORITY_BYTES = 8 * 1024 * 1024

# Tools the gate pins and installs BESIDE the workspace instead of taking from
# whatever a host carries: `cargo-fuzz`, pinned at 0.13.2, and `cargo-mutants`,
# pinned at 27.1.0. Both are version-asserted by their own `gate_b` cell and by
# their own script.
#
# The relative path is DERIVED from the tool's name rather than written out.
# There are four readers of it -- this file, `tools/gate-a/run_gate.py`,
# `scripts/gate-a-fuzz.sh` and `scripts/gate-a-mutants.sh` -- and the last time
# it was a literal, one of the three then-existing readers did not know it and
# `--phase gate_b` aborted on a host that had the pinned binary in place.
# `tools/gate-a/tests/test_run_gate.py` scans the repository and refuses any
# reader that spells it a different way.
WORKSPACE_TOOLS_ROOT = ".context/tools"
WORKSPACE_TOOLS = ("cargo_fuzz", "cargo_mutants")


def _workspace_tool_path(name: str) -> str:
    """Where the gate installs one pinned tool, relative to the workspace."""

    binary = name.replace("_", "-")
    return f"{WORKSPACE_TOOLS_ROOT}/{binary}/bin/{binary}"


class CandidateEnvelopeError(RuntimeError):
    """The candidate could not be bound or verified without ambiguity."""


def _exact_authority_bytes(
    path: pathlib.Path, description: str
) -> tuple[bytes, tuple[int, ...]]:
    try:
        before = path.lstat()
    except OSError as error:
        raise CandidateEnvelopeError(f"{description} is unavailable") from error
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
    if (
        stat.S_ISLNK(before.st_mode)
        or not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or stat.S_IMODE(before.st_mode) & 0o7000
        or not 1 <= before.st_size <= MAX_AUTHORITY_BYTES
    ):
        raise CandidateEnvelopeError(f"{description} is not one exact authority file")
    descriptor = -1
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        opened = os.fstat(descriptor)
        if any(getattr(opened, field) != getattr(before, field) for field in fields):
            raise CandidateEnvelopeError(f"{description} changed before read")
        payload = bytearray()
        while len(payload) < opened.st_size:
            chunk = os.read(descriptor, min(64 * 1024, opened.st_size - len(payload)))
            if not chunk:
                raise CandidateEnvelopeError(f"{description} ended before its bound")
            payload.extend(chunk)
        if os.read(descriptor, 1):
            raise CandidateEnvelopeError(f"{description} exceeded its bound")
        after = os.fstat(descriptor)
        if any(getattr(after, field) != getattr(opened, field) for field in fields):
            raise CandidateEnvelopeError(f"{description} changed while read")
    except OSError as error:
        raise CandidateEnvelopeError(f"{description} could not be read") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    final = path.lstat()
    if any(getattr(final, field) != getattr(after, field) for field in fields):
        raise CandidateEnvelopeError(f"{description} path changed after read")
    return bytes(payload), tuple(getattr(after, field) for field in fields)


def _load_exact_authority(
    path: pathlib.Path,
    description: str,
    *,
    aliases: Mapping[str, types.ModuleType] | None = None,
) -> tuple[types.ModuleType, dict[str, Any], tuple[int, ...]]:
    payload, witness = _exact_authority_bytes(path, description)
    digest = hashlib.sha256(payload).hexdigest()
    module_name = f"_pmux_candidate_{path.stem}_{os.urandom(8).hex()}"
    module = types.ModuleType(module_name)
    module.__file__ = str(path)
    module.__package__ = ""
    saved: dict[str, types.ModuleType | None] = {}
    sys.modules[module_name] = module
    for name, authority in (aliases or {}).items():
        saved[name] = sys.modules.get(name)
        sys.modules[name] = authority
    try:
        exec(compile(payload, str(path), "exec", dont_inherit=True), module.__dict__)
    except Exception as error:
        raise CandidateEnvelopeError(f"{description} could not load") from error
    finally:
        for name, prior in saved.items():
            if prior is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = prior
    identity = {
        "path": str(path.relative_to(TOOLS_ROOT.parent)),
        "size": len(payload),
        "sha256": digest,
    }
    return module, identity, witness


def _revalidate_exact_authority(
    path: pathlib.Path,
    description: str,
    expected_identity: Mapping[str, Any],
    expected_witness: tuple[int, ...],
) -> dict[str, Any]:
    payload, witness = _exact_authority_bytes(path, description)
    identity = {
        "path": str(path.relative_to(TOOLS_ROOT.parent)),
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }
    if witness != expected_witness or identity != dict(expected_identity):
        raise CandidateEnvelopeError(f"{description} changed after module load")
    return identity


source_digest, SOURCE_DIGEST_AUTHORITY, _SOURCE_DIGEST_WITNESS = _load_exact_authority(
    LINUX_EVIDENCE_ROOT / "source_digest.py", "source-digest authority"
)
bounded_process = source_digest.bounded_process
BOUNDED_PROCESS_AUTHORITY = dict(source_digest._BOUNDED_PROCESS_IDENTITY)
evidence, EVIDENCE_AUTHORITY, _EVIDENCE_WITNESS = _load_exact_authority(
    LINUX_EVIDENCE_ROOT / "evidence.py",
    "Linux evidence authority",
    aliases={"source_digest": source_digest},
)
managed_process, MANAGED_PROCESS_AUTHORITY, _MANAGED_PROCESS_WITNESS = (
    _load_exact_authority(
        EVIDENCE_COMMON_ROOT / "managed_process.py",
        "managed-process authority",
        aliases={"bounded_process": bounded_process},
    )
)
package_smoke, PACKAGE_SMOKE_AUTHORITY, _PACKAGE_SMOKE_WITNESS = _load_exact_authority(
    PACKAGE_SMOKE_ROOT / "package_smoke.py", "package-smoke authority"
)


def _evidence_authorities() -> dict[str, Any]:
    bounded = source_digest._revalidate_bounded_process_authority()
    if bounded != BOUNDED_PROCESS_AUTHORITY:
        raise CandidateEnvelopeError("bounded-process authority changed")
    managed = _revalidate_exact_authority(
        EVIDENCE_COMMON_ROOT / "managed_process.py",
        "managed-process authority",
        MANAGED_PROCESS_AUTHORITY,
        _MANAGED_PROCESS_WITNESS,
    )
    return {
        "bounded_process": bounded,
        "managed_process": managed,
        "source_digest": _revalidate_exact_authority(
            LINUX_EVIDENCE_ROOT / "source_digest.py",
            "source-digest authority",
            SOURCE_DIGEST_AUTHORITY,
            _SOURCE_DIGEST_WITNESS,
        ),
        "evidence": _revalidate_exact_authority(
            LINUX_EVIDENCE_ROOT / "evidence.py",
            "Linux evidence authority",
            EVIDENCE_AUTHORITY,
            _EVIDENCE_WITNESS,
        ),
        "package_smoke": _revalidate_exact_authority(
            PACKAGE_SMOKE_ROOT / "package_smoke.py",
            "package-smoke authority",
            PACKAGE_SMOKE_AUTHORITY,
            _PACKAGE_SMOKE_WITNESS,
        ),
    }


def _validate_evidence_authorities(value: object) -> dict[str, Any]:
    expected = _evidence_authorities()
    if not isinstance(value, Mapping) or set(value) != set(expected):
        raise CandidateEnvelopeError("evidence-authority inventory is not exact")
    normalized: dict[str, Any] = {}
    for name, exact in expected.items():
        raw = value.get(name)
        if not isinstance(raw, Mapping) or set(raw) != {"path", "size", "sha256"}:
            raise CandidateEnvelopeError(f"evidence authority is malformed: {name}")
        if dict(raw) != exact:
            raise CandidateEnvelopeError(f"evidence authority differs: {name}")
        normalized[name] = dict(raw)
    return normalized


CANDIDATE_FILE = "candidate.json"
FINAL_AUDIT_FILE = "final-audit.json"
PHASE_MANIFEST_FILE = pathlib.Path(__file__).resolve().with_name("phase-manifest.json")
PHASES = ("gate_a", "gate_b", "gate_c", "gate_d", "gate_e", "gate_f", "residue")
CHECKPOINTS = tuple(
    label for phase in PHASES for label in (f"{phase}_before", f"{phase}_after")
)
PHASE_BEFORE_LABEL = {phase: f"{phase}_before" for phase in PHASES}
PHASE_AFTER_LABEL = {phase: f"{phase}_after" for phase in PHASES}
MAX_CARGO_METADATA_BYTES = 16 * 1024 * 1024
MAX_CARGO_BUILD_BYTES = 64 * 1024 * 1024
MAX_CARGO_METADATA_SECONDS = 60
MAX_CARGO_BUILD_SECONDS = 3600
SCHEMA_VERSION = 1
RELEASE_BUILD_COMMAND = (
    "build",
    "--locked",
    "--workspace",
    "--release",
    "--bins",
    "--message-format=json-render-diagnostics",
)
VALIDATION_TARGET_NAME = "gate-a-validation"
VALIDATION_CHILD_NAMES = (
    "cargo-home",
    "cargo-target",
    "typescript-dist",
    "fuzz",
    "fuzz-evidence",
    "home",
    "tmp",
)
TYPESCRIPT_STAGE_FILES = (
    "client.d.ts",
    "client.d.ts.map",
    "client.js",
    "client.js.map",
    "index.d.ts",
    "index.d.ts.map",
    "index.js",
    "index.js.map",
    "package.json",
    "protocol.d.ts",
    "protocol.d.ts.map",
    "protocol.js",
    "protocol.js.map",
    "smithers.d.ts",
    "smithers.d.ts.map",
    "smithers.js",
    "smithers.js.map",
)
MAX_TOOL_OUTPUT_BYTES = 1024 * 1024
TOOLCHAIN = "1.88.0"
NIGHTLY_TOOLCHAIN = "nightly-2026-03-26"
CARGO_LOCK_MANIFESTS = (
    ("Cargo.lock", "Cargo.toml"),
    ("fuzz/Cargo.lock", "fuzz/Cargo.toml"),
    ("vendor/rmux-client/Cargo.lock", "vendor/rmux-client/Cargo.toml"),
    ("vendor/rmux-server/Cargo.lock", "vendor/rmux-server/Cargo.toml"),
)
MAX_CARGO_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_CARGO_PACKAGE_FILE_BYTES = 512 * 1024 * 1024
MAX_CARGO_PACKAGE_BYTES = 2 * 1024 * 1024 * 1024
MAX_CARGO_PACKAGE_FILES = 100_000
MAX_CARGO_CLOSURE_BYTES = 8 * 1024 * 1024 * 1024
HASH_DOMAINS = {
    "build_environment": "pmux.gate-a.build-environment.v1",
    "release_build": "pmux.gate-a.release-build.v1",
    "build_context": "pmux.gate-a.build-context.v1",
    "source_guard": "pmux.gate-a.source-guard.v1",
    "source_manifest": "pmux.gate-a.source-manifest.v1",
    "source_revision_identity": "pmux.gate-a.source-revision-identity.v1",
    "binary_manifest": "pmux.gate-a.release-binary-manifest.v1",
    "phase_manifest": "pmux.gate-a.phase-manifest.v1",
    "observation": "pmux.gate-a.candidate-observation.v1",
    "candidate": "pmux.gate-a.candidate.v1",
    "checkpoint": "pmux.gate-a.checkpoint.v1",
    "phase_report": "pmux.gate-a.phase-report.v1",
    "final_audit": "pmux.gate-a.final-audit.v1",
    "typescript_stage": "pmux.gate-a.typescript-stage.v1",
    "process_ledger": "pmux.gate-a.process-ledger.v1",
}


MetadataLoader = Callable[
    [pathlib.Path, pathlib.Path, Mapping[str, Any]], Mapping[str, Any]
]
BuildRunner = Callable[
    [pathlib.Path, pathlib.Path, Sequence[str], Mapping[str, str]], Mapping[str, Any]
]
CommandRunner = Callable[
    [pathlib.Path, Sequence[str], Mapping[str, str], int, int], "CommandExecution"
]
RuntimeIdentityLoader = Callable[[pathlib.Path, pathlib.Path], Mapping[str, Any]]
RevisionCaptureLoader = Callable[[pathlib.Path], Mapping[str, Any]]


@dataclass(frozen=True)
class CommandExecution:
    exit_code: int
    stdout: bytes
    stderr: bytes = b""
    process_ledger: tuple[Mapping[str, Any], ...] = ()


def _exact_keys(
    value: Mapping[str, Any], expected: frozenset[str], description: str
) -> None:
    actual = frozenset(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        raise CandidateEnvelopeError(
            f"{description} fields are not exact: missing={missing}, extra={extra}"
        )


def _is_exact_int(
    value: object, *, minimum: int | None = None, maximum: int | None = None
) -> bool:
    if type(value) is not int:
        return False
    if minimum is not None and value < minimum:
        return False
    return maximum is None or value <= maximum


def _is_sha256(value: object) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _validated_process_ledger(
    raw: Sequence[Mapping[str, Any]],
    description: str,
    *,
    require_nonempty: bool = False,
) -> list[dict[str, Any]]:
    try:
        records = bounded_process.validate_process_ledger(
            raw, require_nonempty=require_nonempty
        )
    except bounded_process.BoundedProcessError as error:
        raise CandidateEnvelopeError(
            f"{description} process ledger is invalid: {error}"
        ) from error
    return [dict(record) for record in records]


def _directory_identity(
    path: pathlib.Path,
    description: str,
    *,
    reject_group_other_write: bool = False,
    include_temporal: bool = False,
    include_nlink: bool = True,
) -> dict[str, Any]:
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise CandidateEnvelopeError(f"{description} is missing: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise CandidateEnvelopeError(f"{description} must be a real directory: {path}")
    if metadata.st_uid != os.geteuid():
        raise CandidateEnvelopeError(
            f"{description} must be owned by the invoking user: {path}"
        )
    if stat.S_IMODE(metadata.st_mode) & 0o7000:
        raise CandidateEnvelopeError(
            f"{description} has unsupported special mode bits: {path}"
        )
    if reject_group_other_write and stat.S_IMODE(metadata.st_mode) & 0o022:
        raise CandidateEnvelopeError(f"{description} is group/world writable: {path}")
    identity = {
        "path": str(path),
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "uid": metadata.st_uid,
        "gid": metadata.st_gid,
        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
    }
    if include_nlink:
        identity["nlink"] = metadata.st_nlink
    if include_temporal:
        identity.update(
            {
                "mtime_ns": metadata.st_mtime_ns,
                "ctime_ns": metadata.st_ctime_ns,
            }
        )
    return identity


def _require_canonical_workspace(workspace: pathlib.Path) -> pathlib.Path:
    if not workspace.is_absolute():
        raise CandidateEnvelopeError("workspace must be an absolute path")
    identity = _directory_identity(workspace, "workspace")
    canonical = workspace.resolve(strict=True)
    if canonical != workspace:
        raise CandidateEnvelopeError("workspace must already be canonical")
    current = pathlib.Path.cwd().resolve(strict=True)
    if current != workspace:
        raise CandidateEnvelopeError(
            f"candidate command must run from the exact workspace: {current} != {workspace}"
        )
    shell_pwd = os.environ.get("PWD")
    if shell_pwd != str(workspace):
        raise CandidateEnvelopeError(
            "PWD must name the exact canonical workspace without aliases"
        )
    if identity != _directory_identity(workspace, "workspace"):
        raise CandidateEnvelopeError("workspace identity changed during validation")
    return workspace


def _run_bounded_process(
    argv: Sequence[str],
    *,
    cwd: pathlib.Path | None,
    environment: Mapping[str, str],
    timeout_seconds: int,
    maximum_output_bytes: int,
    description: str,
) -> CommandExecution:
    if not argv:
        raise CandidateEnvelopeError(f"{description} command is empty")
    try:
        executable = bounded_process.bind_executable(pathlib.Path(argv[0]))
        result = bounded_process.run(
            executable,
            argv,
            cwd=cwd,
            environment=environment,
            timeout_seconds=timeout_seconds,
            drain_timeout_seconds=min(timeout_seconds, 30),
            maximum_output_bytes=maximum_output_bytes,
            description=description,
        )
    except bounded_process.BoundedProcessError as error:
        raise CandidateEnvelopeError(str(error)) from error
    return CommandExecution(
        exit_code=result.exit_code,
        stdout=result.stdout,
        stderr=result.stderr,
        process_ledger=result.process_ledger,
    )


_FORBIDDEN_CHILD_STATE_NAMES = frozenset(
    {
        "TMP",
        "TEMP",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    }
)


def _empty_execution_state(
    environment: Mapping[str, str], description: str
) -> dict[str, Any]:
    forbidden = sorted(_FORBIDDEN_CHILD_STATE_NAMES.intersection(environment))
    if forbidden:
        raise CandidateEnvelopeError(
            f"{description} exposes alternate child state paths: {forbidden}"
        )
    raw_home = environment.get("HOME")
    raw_tmp = environment.get("TMPDIR")
    if not isinstance(raw_home, str) or not isinstance(raw_tmp, str):
        raise CandidateEnvelopeError(
            f"{description} has no exact private HOME and TMPDIR"
        )
    home = pathlib.Path(raw_home)
    temporary = pathlib.Path(raw_tmp)
    if (
        not home.is_absolute()
        or not temporary.is_absolute()
        or home.name != "home"
        or temporary.name != "tmp"
        or home.parent != temporary.parent
        or home.resolve(strict=True) != home
        or temporary.resolve(strict=True) != temporary
    ):
        raise CandidateEnvelopeError(
            f"{description} HOME and TMPDIR are not canonical validation children"
        )
    validation_root = home.parent
    if validation_root.resolve(strict=True) != validation_root:
        raise CandidateEnvelopeError(f"{description} validation root is not canonical")
    root_identity = _directory_identity(
        validation_root,
        f"{description} validation root",
        reject_group_other_write=True,
        include_nlink=False,
    )
    identities: dict[str, Any] = {}
    for name, path in (("home", home), ("tmp", temporary)):
        identity = _directory_identity(
            path,
            f"{description} {name}",
            reject_group_other_write=True,
        )
        if identity["mode"] != "0700" or identity["nlink"] != 2:
            raise CandidateEnvelopeError(
                f"{description} {name} must be one empty mode-0700 directory"
            )
        try:
            manifest = evidence.regular_tree_manifest(path)
        except evidence.EvidenceError as error:
            raise CandidateEnvelopeError(str(error)) from error
        if manifest["file_count"] != 0 or manifest["directory_count"] != 0:
            raise CandidateEnvelopeError(
                f"{description} left private state in {name}: {path}"
            )
        identities[name] = identity
    return {
        "validation_root": root_identity,
        "home": identities["home"],
        "tmp": identities["tmp"],
    }


def _run_bounded_command(
    argv: Sequence[str],
    *,
    cwd: pathlib.Path | None,
    environment: Mapping[str, str] | None,
    timeout_seconds: int,
    maximum_output_bytes: int,
    description: str,
) -> CommandExecution:
    if environment is None:
        raise CandidateEnvelopeError(
            f"{description} requires one exact sanitized environment"
        )
    state_before = _empty_execution_state(environment, description)
    try:
        result = _run_bounded_process(
            argv,
            cwd=cwd,
            environment=environment,
            timeout_seconds=timeout_seconds,
            maximum_output_bytes=maximum_output_bytes,
            description=description,
        )
    except BaseException as operation_error:
        try:
            state_after_failure = _empty_execution_state(environment, description)
            if state_after_failure != state_before:
                raise CandidateEnvelopeError(
                    f"{description} replaced a private state directory"
                )
        except CandidateEnvelopeError as state_error:
            raise CandidateEnvelopeError(
                f"{description} failed and left invalid private state"
            ) from state_error
        raise operation_error
    state_after = _empty_execution_state(environment, description)
    if state_after != state_before:
        raise CandidateEnvelopeError(
            f"{description} replaced a private state directory"
        )
    return result


def _run_cargo_metadata(
    workspace: pathlib.Path,
    expected_target: pathlib.Path,
    runtime_identity: Mapping[str, Any],
) -> Mapping[str, Any]:
    environment = _exact_process_environment(runtime_identity, expected_target)
    cargo = runtime_identity["tool_paths"]["cargo"]
    result = _run_bounded_command(
        [
            cargo,
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ],
        cwd=workspace,
        environment=environment,
        timeout_seconds=MAX_CARGO_METADATA_SECONDS,
        maximum_output_bytes=MAX_CARGO_METADATA_BYTES,
        description="cargo metadata",
    )
    if result.exit_code != 0:
        raise CandidateEnvelopeError(
            f"cargo metadata failed with status {result.exit_code}"
        )
    try:
        payload = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CandidateEnvelopeError(
            "cargo metadata returned malformed JSON"
        ) from error
    if not isinstance(payload, dict):
        raise CandidateEnvelopeError("cargo metadata did not return an object")
    return payload


def _parse_release_build_output(
    workspace: pathlib.Path,
    expected_target: pathlib.Path,
    payload: bytes,
    returncode: int,
    command: Sequence[str],
) -> dict[str, Any]:
    if returncode != 0:
        raise CandidateEnvelopeError(f"release build failed with status {returncode}")
    if len(payload) > MAX_CARGO_BUILD_BYTES:
        raise CandidateEnvelopeError("release build JSON exceeded its evidence bound")
    found: dict[str, dict[str, Any]] = {}
    required = frozenset(evidence.REQUIRED_RELEASE_BINARIES)
    for raw_line in payload.splitlines():
        if len(raw_line) > MAX_CARGO_METADATA_BYTES:
            raise CandidateEnvelopeError("one release build JSON record is oversized")
        try:
            message = json.loads(raw_line.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise CandidateEnvelopeError(
                "release build returned malformed JSON"
            ) from error
        if (
            not isinstance(message, dict)
            or message.get("reason") != "compiler-artifact"
        ):
            continue
        target = message.get("target")
        if not isinstance(target, dict) or "bin" not in target.get("kind", []):
            continue
        name = target.get("name")
        executable = message.get("executable")
        if not isinstance(name, str) or not isinstance(executable, str):
            raise CandidateEnvelopeError("release bin artifact receipt is malformed")
        if name not in required:
            raise CandidateEnvelopeError(
                f"release build emitted unexpected bin: {name}"
            )
        expected_path = expected_target / "release" / name
        if executable != str(expected_path):
            raise CandidateEnvelopeError(
                f"release build emitted {name} outside exact $PWD/target/release"
            )
        record = {
            "name": name,
            "path": executable,
            "package_id": message.get("package_id"),
            "fresh": message.get("fresh"),
        }
        if not isinstance(record["package_id"], str) or not isinstance(
            record["fresh"], bool
        ):
            raise CandidateEnvelopeError("release bin artifact metadata is malformed")
        if record["fresh"]:
            raise CandidateEnvelopeError(
                f"release build returned a stale fresh-only receipt for {name}"
            )
        if name in found and found[name] != record:
            raise CandidateEnvelopeError(
                f"release build emitted conflicting bin: {name}"
            )
        found[name] = record
    if frozenset(found) != required:
        raise CandidateEnvelopeError(
            "release build did not emit the exact eight executable receipts"
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "pmux_gate_a_release_build",
        "command": list(command),
        "workspace": str(workspace),
        "cargo_target_dir": str(expected_target),
        "status": "PASS",
        "exit_code": 0,
        "executables": [found[name] for name in evidence.REQUIRED_RELEASE_BINARIES],
    }


def _run_release_build(
    workspace: pathlib.Path,
    expected_target: pathlib.Path,
    command: Sequence[str],
    environment: Mapping[str, str],
) -> Mapping[str, Any]:
    result = _run_bounded_command(
        command,
        cwd=workspace,
        environment=environment,
        timeout_seconds=MAX_CARGO_BUILD_SECONDS,
        maximum_output_bytes=MAX_CARGO_BUILD_BYTES,
        description="release build",
    )
    receipt = _parse_release_build_output(
        workspace, expected_target, result.stdout, result.exit_code, command
    )
    process_ledger = _validated_process_ledger(
        result.process_ledger, "release build", require_nonempty=True
    )
    return {
        **receipt,
        "stdout_size": len(result.stdout),
        "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
        "stderr_size": len(result.stderr),
        "stderr_sha256": hashlib.sha256(result.stderr).hexdigest(),
        "process_ledger": process_ledger,
        "process_ledger_sha256": evidence.canonical_json_sha256(
            process_ledger, domain=HASH_DOMAINS["process_ledger"]
        ),
    }


def _execution_receipt(
    result: CommandExecution, argv: Sequence[str], description: str
) -> dict[str, Any]:
    process_ledger = _validated_process_ledger(
        result.process_ledger, description, require_nonempty=True
    )
    return {
        "argv": list(argv),
        "exit_code": result.exit_code,
        "stdout_size": len(result.stdout),
        "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
        "stderr_size": len(result.stderr),
        "stderr_sha256": hashlib.sha256(result.stderr).hexdigest(),
        "process_ledger": process_ledger,
        "process_ledger_sha256": evidence.canonical_json_sha256(
            process_ledger, domain=HASH_DOMAINS["process_ledger"]
        ),
    }


def _tool_output(
    argv: Sequence[str], *, environment: Mapping[str, str]
) -> tuple[str, dict[str, Any]]:
    result = _run_bounded_command(
        argv,
        cwd=None,
        environment=environment,
        timeout_seconds=30,
        maximum_output_bytes=MAX_TOOL_OUTPUT_BYTES,
        description="tool version probe",
    )
    if result.exit_code != 0:
        raise CandidateEnvelopeError(f"tool version probe was not bounded: {argv[0]}")
    if result.stdout and result.stderr:
        raise CandidateEnvelopeError(
            f"tool version probe used both output streams: {argv[0]}"
        )
    payload = result.stdout or result.stderr
    try:
        value = payload.decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise CandidateEnvelopeError("tool version output is not UTF-8") from error
    if not value:
        raise CandidateEnvelopeError("tool version output is empty")
    return value, _execution_receipt(result, argv, "tool version probe")


def _resolved_tool(name: str) -> pathlib.Path:
    invoked = shutil.which(name)
    if invoked is None:
        raise CandidateEnvelopeError(f"required validation tool is unavailable: {name}")
    resolved = pathlib.Path(invoked).resolve(strict=True)
    if not resolved.is_absolute():
        raise CandidateEnvelopeError(f"resolved tool path is not absolute: {name}")
    return resolved


def _rustup_tool(
    rustup: pathlib.Path,
    toolchain: str,
    name: str,
    environment: Mapping[str, str],
    resolution_probes: list[dict[str, Any]],
) -> pathlib.Path:
    output, receipt = _tool_output(
        (str(rustup), "which", "--toolchain", toolchain, name),
        environment=environment,
    )
    resolution_probes.append(
        {
            "toolchain": toolchain,
            "tool": name,
            "resolved_path": output,
            "execution": receipt,
        }
    )
    path = pathlib.Path(output).resolve(strict=True)
    if not path.is_absolute():
        raise CandidateEnvelopeError(f"rustup returned a relative tool path: {name}")
    return path


def _stable_tool_identity(path: pathlib.Path) -> dict[str, Any]:
    try:
        with evidence._anchored_directory(
            path.parent, description="validation tool parent"
        ) as opened_parent:
            before = os.stat(path.name, dir_fd=opened_parent.fd, follow_symlinks=False)
            mode = stat.S_IMODE(before.st_mode)
            if (
                stat.S_ISLNK(before.st_mode)
                or not stat.S_ISREG(before.st_mode)
                or mode & 0o500 != 0o500
                or mode & 0o022
                or (mode & 0o7000 and before.st_uid != 0)
            ):
                raise CandidateEnvelopeError(
                    f"validation tool identity is unsafe: {path}"
                )
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
            descriptor = os.open(
                path.name,
                os.O_RDONLY
                | getattr(os, "O_CLOEXEC", 0)
                | getattr(os, "O_NOFOLLOW", 0),
                dir_fd=opened_parent.fd,
            )
            try:
                opened = os.fstat(descriptor)
                if any(
                    getattr(opened, field) != getattr(before, field) for field in fields
                ):
                    raise CandidateEnvelopeError(
                        f"validation tool changed before hashing: {path}"
                    )
                digest = hashlib.sha256()
                while True:
                    chunk = os.read(descriptor, 1024 * 1024)
                    if not chunk:
                        break
                    digest.update(chunk)
                after = os.fstat(descriptor)
            finally:
                os.close(descriptor)
            linked_after = os.stat(
                path.name, dir_fd=opened_parent.fd, follow_symlinks=False
            )
            if any(
                getattr(after, field) != getattr(opened, field)
                or getattr(linked_after, field) != getattr(opened, field)
                for field in fields
            ):
                raise CandidateEnvelopeError(
                    f"validation tool changed while hashing: {path}"
                )
            evidence._revalidate_directory_chain(
                opened_parent, description="validation tool parent"
            )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
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


def _stable_file_bytes(
    path: pathlib.Path, *, description: str, maximum_bytes: int
) -> bytes:
    try:
        before = path.lstat()
    except FileNotFoundError as error:
        raise CandidateEnvelopeError(f"{description} is missing: {path}") from error
    if (
        stat.S_ISLNK(before.st_mode)
        or not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or before.st_size > maximum_bytes
    ):
        raise CandidateEnvelopeError(f"{description} is not one bounded regular file")
    try:
        return evidence._stable_regular_bytes(
            path,
            description=description,
            maximum_bytes=maximum_bytes,
            before=before,
        )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error


def _cargo_lock_packages(workspace: pathlib.Path) -> dict[str, Any]:
    packages: dict[tuple[str, str], dict[str, str]] = {}
    locks: list[dict[str, Any]] = []
    for lock_relative, manifest_relative in CARGO_LOCK_MANIFESTS:
        lock_path = workspace / lock_relative
        manifest_path = workspace / manifest_relative
        lock_bytes = _stable_file_bytes(
            lock_path,
            description=f"Cargo lock {lock_relative}",
            maximum_bytes=16 * 1024 * 1024,
        )
        manifest_bytes = _stable_file_bytes(
            manifest_path,
            description=f"Cargo manifest {manifest_relative}",
            maximum_bytes=16 * 1024 * 1024,
        )
        try:
            parsed = tomllib.loads(lock_bytes.decode("utf-8"))
        except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
            raise CandidateEnvelopeError(
                f"Cargo lock is malformed: {lock_relative}"
            ) from error
        raw_packages = parsed.get("package")
        if not _is_exact_int(
            parsed.get("version"), minimum=4, maximum=4
        ) or not isinstance(raw_packages, list):
            raise CandidateEnvelopeError(f"Cargo lock has no packages: {lock_relative}")
        registry_count = 0
        for raw_package in raw_packages:
            if not isinstance(raw_package, dict):
                raise CandidateEnvelopeError(
                    f"Cargo lock package is malformed: {lock_relative}"
                )
            source = raw_package.get("source")
            if source is None:
                continue
            name = raw_package.get("name")
            version = raw_package.get("version")
            checksum = raw_package.get("checksum")
            if (
                source != "registry+https://github.com/rust-lang/crates.io-index"
                or not isinstance(name, str)
                or re.fullmatch(r"[A-Za-z0-9_-]+", name) is None
                or not isinstance(version, str)
                or re.fullmatch(r"[A-Za-z0-9.+-]+", version) is None
                or not _is_sha256(checksum)
            ):
                raise CandidateEnvelopeError(
                    f"Cargo lock registry package is unsupported: {lock_relative}"
                )
            key = (name, version)
            record = {
                "name": name,
                "version": version,
                "source": source,
                "checksum": checksum,
            }
            prior = packages.get(key)
            if prior is not None and prior != record:
                raise CandidateEnvelopeError(
                    f"Cargo lock union conflicts for {name} {version}"
                )
            packages[key] = record
            registry_count += 1
        locks.append(
            {
                "lock_path": lock_relative,
                "manifest_path": manifest_relative,
                "lock_sha256": hashlib.sha256(lock_bytes).hexdigest(),
                "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
                "lock_format_version": 4,
                "registry_package_count": registry_count,
            }
        )
    return {
        "locks": locks,
        "packages": [packages[key] for key in sorted(packages)],
    }


def _cargo_archive_manifest(
    payload: bytes, *, package_directory: str
) -> dict[str, Any]:
    files: dict[str, str] = {}
    directories: set[str] = set()
    total_bytes = 0
    seen_members: set[str] = set()
    try:
        with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
            for member in archive:
                pure = pathlib.PurePosixPath(member.name)
                if (
                    pure.is_absolute()
                    or member.name != pure.as_posix()
                    or any(part in {"", ".", ".."} for part in pure.parts)
                    or not pure.parts
                    or pure.parts[0] != package_directory
                    or member.name in seen_members
                ):
                    raise CandidateEnvelopeError(
                        "Cargo crate archive has unsafe or duplicate membership"
                    )
                seen_members.add(member.name)
                if member.isdir():
                    if len(pure.parts) > 1:
                        directories.add(
                            pathlib.PurePosixPath(*pure.parts[1:]).as_posix()
                        )
                    continue
                if not member.isreg() or len(pure.parts) < 2:
                    raise CandidateEnvelopeError(
                        "Cargo crate archive contains a non-regular member"
                    )
                relative = pathlib.PurePosixPath(*pure.parts[1:]).as_posix()
                parent = pathlib.PurePosixPath(relative).parent
                while parent != pathlib.PurePosixPath("."):
                    directories.add(parent.as_posix())
                    parent = parent.parent
                if relative in files or member.size < 0:
                    raise CandidateEnvelopeError(
                        "Cargo crate archive repeats a package file"
                    )
                if member.size > MAX_CARGO_PACKAGE_FILE_BYTES:
                    raise CandidateEnvelopeError(
                        "Cargo crate archive member exceeded its size bound"
                    )
                if len(files) >= MAX_CARGO_PACKAGE_FILES:
                    raise CandidateEnvelopeError(
                        "Cargo crate archive exceeded its file-count bound"
                    )
                total_bytes += member.size
                if total_bytes > MAX_CARGO_PACKAGE_BYTES:
                    raise CandidateEnvelopeError(
                        "Cargo crate archive exceeded its unpacked-size bound"
                    )
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise CandidateEnvelopeError(
                        "Cargo crate archive file cannot be read"
                    )
                digest = hashlib.sha256()
                observed = 0
                while True:
                    chunk = extracted.read(1024 * 1024)
                    if not chunk:
                        break
                    observed += len(chunk)
                    if observed > member.size:
                        raise CandidateEnvelopeError(
                            "Cargo crate archive expanded beyond its header"
                        )
                    digest.update(chunk)
                if observed != member.size:
                    raise CandidateEnvelopeError(
                        "Cargo crate archive file is truncated"
                    )
                files[relative] = digest.hexdigest()
    except (tarfile.TarError, EOFError, OSError) as error:
        raise CandidateEnvelopeError("Cargo crate archive is malformed") from error
    if not files:
        raise CandidateEnvelopeError("Cargo crate archive contains no files")
    aggregate = hashlib.sha256()
    aggregate.update(b"pmux-cargo-package-files-v1\0")
    for relative, digest in sorted(files.items()):
        rendered = json.dumps(
            {"path": relative, "sha256": digest},
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        aggregate.update(len(rendered).to_bytes(4, "big"))
        aggregate.update(rendered)
    return {
        "files": files,
        "directories": sorted(directories),
        "file_count": len(files),
        "unpacked_size": total_bytes,
        "files_sha256": aggregate.hexdigest(),
    }


def _cargo_source_matches_archive(
    source: pathlib.Path, archive_manifest: Mapping[str, Any]
) -> None:
    raw_files = archive_manifest.get("files")
    if not isinstance(raw_files, dict):
        raise CandidateEnvelopeError("Cargo archive file manifest is malformed")
    try:
        manifest = evidence.regular_tree_manifest(
            source, excluded_paths=frozenset({".cargo-ok"})
        )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    actual_files = {entry["path"]: entry["sha256"] for entry in manifest["files"]}
    actual_directories = sorted(entry["path"] for entry in manifest["directories"])
    if actual_files != raw_files or actual_directories != archive_manifest.get(
        "directories"
    ):
        raise CandidateEnvelopeError("ambient Cargo source differs from its archive")
    try:
        (source / ".cargo-ok").lstat()
    except FileNotFoundError:
        pass
    else:
        _stable_file_bytes(
            source / ".cargo-ok",
            description="Cargo cache extraction marker",
            maximum_bytes=1024,
        )


def _write_cargo_checksum(
    package_root: pathlib.Path,
    files: Mapping[str, str],
    package_checksum: str,
) -> None:
    payload = (
        json.dumps(
            {"files": dict(files), "package": package_checksum},
            allow_nan=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        + b"\n"
    )
    try:
        evidence.atomic_write_bytes(package_root / ".cargo-checksum.json", payload)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error


def _cargo_package_checksum(
    package_root: pathlib.Path, expected_package_checksum: str
) -> dict[str, str]:
    checksum_path = package_root / ".cargo-checksum.json"
    checksum_bytes = _stable_file_bytes(
        checksum_path,
        description="Cargo package checksum",
        maximum_bytes=64 * 1024 * 1024,
    )
    try:
        checksum = evidence.strict_json_loads(
            checksum_bytes, description="Cargo package checksum"
        )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    if (
        not isinstance(checksum, dict)
        or set(checksum) != {"files", "package"}
        or checksum.get("package") != expected_package_checksum
        or not isinstance(checksum.get("files"), dict)
    ):
        raise CandidateEnvelopeError("Cargo package checksum schema is invalid")
    files: dict[str, str] = {}
    for relative, digest in checksum["files"].items():
        if (
            not isinstance(relative, str)
            or pathlib.PurePosixPath(relative).is_absolute()
            or relative != pathlib.PurePosixPath(relative).as_posix()
            or any(
                part in {"", ".", ".."}
                for part in pathlib.PurePosixPath(relative).parts
            )
            or not _is_sha256(digest)
        ):
            raise CandidateEnvelopeError("Cargo package file checksum is invalid")
        payload = _stable_file_bytes(
            package_root / relative,
            description="Cargo package source",
            maximum_bytes=512 * 1024 * 1024,
        )
        if hashlib.sha256(payload).hexdigest() != digest:
            raise CandidateEnvelopeError(
                f"Cargo package source checksum differs: {relative}"
            )
        files[relative] = digest
    try:
        manifest = evidence.regular_tree_manifest(package_root)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    actual_files = {entry["path"] for entry in manifest["files"]}
    if actual_files != {*files, ".cargo-checksum.json"}:
        raise CandidateEnvelopeError("Cargo package source membership is not exact")
    return files


def _cargo_home_host_witness(
    cargo_home: pathlib.Path,
) -> list[dict[str, Any]]:
    excluded = {".package-cache", ".package-cache-mutate"}
    try:
        with evidence._anchored_directory(
            cargo_home, description="private Cargo home"
        ) as opened:
            snapshot, _contents = evidence._anchored_tree_capture(
                opened.fd,
                relative_prefix="",
                excluded_paths=frozenset(excluded),
                read_files=False,
            )
            evidence._revalidate_directory_chain(
                opened, description="private Cargo home"
            )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    return [
        {
            "path": relative,
            "device": metadata.st_dev,
            "inode": metadata.st_ino,
            "uid": metadata.st_uid,
            "gid": metadata.st_gid,
            "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
            "nlink": metadata.st_nlink,
            "size": metadata.st_size,
            "mtime_ns": metadata.st_mtime_ns,
            "ctime_ns": metadata.st_ctime_ns,
            "kind": "directory" if stat.S_ISDIR(metadata.st_mode) else "file",
        }
        for relative, metadata in sorted(snapshot.items())
    ]


def _private_cargo_config_bytes(cargo_home: pathlib.Path) -> bytes:
    return (
        '[source.crates-io]\nreplace-with = "pmux-vendored"\n\n'
        '[source.pmux-vendored]\ndirectory = "'
        + str(cargo_home / "vendor").replace("\\", "\\\\").replace('"', '\\"')
        + '"\n\n[net]\noffline = true\n'
    ).encode("utf-8")


def _write_private_cargo_config(cargo_home: pathlib.Path) -> None:
    config = _private_cargo_config_bytes(cargo_home)
    try:
        evidence.atomic_write_bytes(cargo_home / "config.toml", config)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    (cargo_home / "config.toml").chmod(0o400)


def _stage_private_cargo_home(
    workspace: pathlib.Path,
    ambient_cargo_home: pathlib.Path,
    private_cargo_home: pathlib.Path,
) -> dict[str, Any]:
    lock_union = _cargo_lock_packages(workspace)
    attestation_path = private_cargo_home / "pmux-cargo-closure.json"
    if not any(private_cargo_home.iterdir()):
        try:
            evidence.prepare_empty_private_directory(private_cargo_home / "vendor")
        except evidence.EvidenceError as error:
            raise CandidateEnvelopeError(str(error)) from error
        package_records: list[dict[str, Any]] = []
        closure_unpacked_bytes = 0
        for package in lock_union["packages"]:
            name = package["name"]
            version = package["version"]
            archive_name = f"{name}-{version}.crate"
            source_name = f"{name}-{version}"
            archives = sorted(
                path
                for cache in (ambient_cargo_home / "registry" / "cache").glob("*")
                for path in cache.glob(archive_name)
                if path.is_file()
            )
            sources = sorted(
                path
                for source in (ambient_cargo_home / "registry" / "src").glob("*")
                for path in source.glob(source_name)
                if path.is_dir()
            )
            if not archives or not sources:
                raise CandidateEnvelopeError(
                    f"ambient Cargo cache is incomplete for {name} {version}"
                )
            archive_matches: list[tuple[pathlib.Path, str, dict[str, Any]]] = []
            for archive in archives:
                payload = _stable_file_bytes(
                    archive,
                    description="Cargo crate archive",
                    maximum_bytes=MAX_CARGO_ARCHIVE_BYTES,
                )
                digest = hashlib.sha256(payload).hexdigest()
                if digest == package["checksum"]:
                    archive_matches.append(
                        (
                            archive,
                            digest,
                            _cargo_archive_manifest(
                                payload, package_directory=source_name
                            ),
                        )
                    )
            source_matches: list[pathlib.Path] = []
            archive_manifest = archive_matches[0][2] if archive_matches else None
            for source in sources:
                try:
                    if archive_manifest is None:
                        break
                    _cargo_source_matches_archive(source, archive_manifest)
                except CandidateEnvelopeError:
                    continue
                source_matches.append(source)
            if not archive_matches or not source_matches:
                raise CandidateEnvelopeError(
                    f"Cargo cache checksum differs for {name} {version}"
                )
            selected_archive, archive_sha256, archive_manifest = archive_matches[0]
            selected_source = source_matches[0]
            closure_unpacked_bytes += archive_manifest["unpacked_size"]
            if closure_unpacked_bytes > MAX_CARGO_CLOSURE_BYTES:
                raise CandidateEnvelopeError(
                    "private Cargo closure exceeded its unpacked-size bound"
                )
            destination = private_cargo_home / "vendor" / source_name
            try:
                shutil.copytree(
                    selected_source,
                    destination,
                    symlinks=False,
                    ignore=lambda _directory, names: (
                        {".cargo-ok"} if ".cargo-ok" in names else set()
                    ),
                )
                destination.chmod(0o700)
            except OSError as error:
                raise CandidateEnvelopeError(
                    f"could not stage Cargo package {name} {version}"
                ) from error
            _write_cargo_checksum(
                destination,
                archive_manifest["files"],
                package["checksum"],
            )
            _cargo_package_checksum(destination, package["checksum"])
            package_records.append(
                {
                    **package,
                    "archive_path": str(selected_archive),
                    "archive_sha256": archive_sha256,
                    "source_path": str(selected_source),
                    "vendor_directory": source_name,
                    "file_count": archive_manifest["file_count"],
                    "unpacked_size": archive_manifest["unpacked_size"],
                    "files_sha256": archive_manifest["files_sha256"],
                }
            )
        _write_private_cargo_config(private_cargo_home)
        for cache_name in (".package-cache", ".package-cache-mutate"):
            cache_path = private_cargo_home / cache_name
            descriptor = os.open(
                cache_path,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
                0o600,
            )
            os.close(descriptor)
        attestation = {
            "schema_version": SCHEMA_VERSION,
            "kind": "pmux_private_cargo_closure",
            "lock_union": lock_union,
            "packages": package_records,
        }
        try:
            evidence.atomic_write_json(attestation_path, attestation)
        except evidence.EvidenceError as error:
            raise CandidateEnvelopeError(str(error)) from error
        attestation_path.chmod(0o400)
        for current, directories, files in os.walk(private_cargo_home / "vendor"):
            for name in directories:
                (pathlib.Path(current) / name).chmod(0o500)
            for name in files:
                path = pathlib.Path(current) / name
                mode = stat.S_IMODE(path.lstat().st_mode)
                path.chmod(0o500 if mode & 0o111 else 0o400)
        (private_cargo_home / "vendor").chmod(0o500)
    attestation_bytes = _stable_file_bytes(
        attestation_path,
        description="private Cargo closure attestation",
        maximum_bytes=64 * 1024 * 1024,
    )
    try:
        attestation = evidence.strict_json_loads(
            attestation_bytes, description="private Cargo closure attestation"
        )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    if (
        not isinstance(attestation, dict)
        or set(attestation) != {"schema_version", "kind", "lock_union", "packages"}
        or not _is_exact_int(
            attestation.get("schema_version"),
            minimum=SCHEMA_VERSION,
            maximum=SCHEMA_VERSION,
        )
        or attestation.get("kind") != "pmux_private_cargo_closure"
        or attestation.get("lock_union") != lock_union
        or not isinstance(attestation.get("packages"), list)
        or len(attestation["packages"]) != len(lock_union["packages"])
    ):
        raise CandidateEnvelopeError("private Cargo closure attestation is invalid")
    for expected, package in zip(
        lock_union["packages"], attestation["packages"], strict=True
    ):
        if (
            not isinstance(package, dict)
            or set(package)
            != {
                "name",
                "version",
                "source",
                "checksum",
                "archive_path",
                "archive_sha256",
                "source_path",
                "vendor_directory",
                "file_count",
                "unpacked_size",
                "files_sha256",
            }
            or {key: package[key] for key in expected} != expected
            or package.get("archive_sha256") != expected["checksum"]
            or package.get("vendor_directory")
            != f"{expected['name']}-{expected['version']}"
            or not _is_exact_int(package.get("file_count"), minimum=1)
            or not _is_exact_int(package.get("unpacked_size"), minimum=0)
            or not _is_sha256(package.get("files_sha256"))
        ):
            raise CandidateEnvelopeError("private Cargo package attestation is invalid")
        _cargo_package_checksum(
            private_cargo_home / "vendor" / package["vendor_directory"],
            expected["checksum"],
        )
    expected_membership = {
        ".package-cache",
        ".package-cache-mutate",
        "config.toml",
        "pmux-cargo-closure.json",
        "vendor",
    }
    if {child.name for child in private_cargo_home.iterdir()} != expected_membership:
        raise CandidateEnvelopeError("private Cargo home membership changed")
    for cache_name in (".package-cache", ".package-cache-mutate"):
        cache = private_cargo_home / cache_name
        metadata = cache.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_size != 0
            or stat.S_IMODE(metadata.st_mode) != 0o600
        ):
            raise CandidateEnvelopeError("private Cargo lock state is invalid")
    try:
        manifest = evidence.regular_tree_manifest(
            private_cargo_home,
            excluded_paths=frozenset({".package-cache", ".package-cache-mutate"}),
        )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    return {
        "attestation": attestation,
        "attestation_sha256": hashlib.sha256(attestation_bytes).hexdigest(),
        "tree_manifest": manifest,
        "host_witness": _cargo_home_host_witness(private_cargo_home),
    }


def _tool_record(
    path: pathlib.Path,
    version_arguments: Sequence[str],
    environment: Mapping[str, str],
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    identity = _stable_tool_identity(path)
    record: dict[str, Any] = {
        "path": str(path),
        "identity": identity,
    }
    if version_arguments:
        version, receipt = _tool_output(
            (str(path), *version_arguments), environment=environment
        )
        record["version"] = version
        return record, receipt
    else:
        record["version"] = None
        return record, None


def _sanitized_environment_values(
    validation_root: pathlib.Path,
    cargo_home: pathlib.Path,
    rustup_home: pathlib.Path,
    path_directories: Sequence[str],
    *,
    rustc: pathlib.Path | None = None,
    rustdoc: pathlib.Path | None = None,
) -> dict[str, str]:
    selected = {
        "HOME": str(validation_root / "home"),
        "CARGO_HOME": str(cargo_home),
        "RUSTUP_HOME": str(rustup_home),
        "PATH": os.pathsep.join(dict.fromkeys(path_directories)),
        "LANG": "C",
        "LC_ALL": "C",
        "TMPDIR": str(validation_root / "tmp"),
        "CARGO_NET_OFFLINE": "true",
        "CARGO_INCREMENTAL": "0",
        "CARGO_TERM_COLOR": "never",
        "RUSTUP_TOOLCHAIN": TOOLCHAIN,
        "NODE_DISABLE_COMPILE_CACHE": "1",
    }
    if rustc is not None:
        selected["RUSTC"] = str(rustc)
    if rustdoc is not None:
        selected["RUSTDOC"] = str(rustdoc)
    return selected


def _toolchain_identity(
    validation_root: pathlib.Path,
    cargo_home: pathlib.Path,
    rustup_home: pathlib.Path,
) -> tuple[dict[str, Any], dict[str, str], list[dict[str, Any]]]:
    rustup = _resolved_tool("rustup")
    bootstrap_directories = [
        str(rustup.parent),
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ]
    bootstrap_environment = _sanitized_environment_values(
        validation_root,
        cargo_home,
        rustup_home,
        bootstrap_directories,
    )
    resolution_probes: list[dict[str, Any]] = []
    tool_paths: dict[str, pathlib.Path] = {
        "rustup": rustup,
        "cargo": _rustup_tool(
            rustup, TOOLCHAIN, "cargo", bootstrap_environment, resolution_probes
        ),
        "rustc": _rustup_tool(
            rustup, TOOLCHAIN, "rustc", bootstrap_environment, resolution_probes
        ),
        "rustfmt": _rustup_tool(
            rustup, TOOLCHAIN, "rustfmt", bootstrap_environment, resolution_probes
        ),
        "rustdoc": _rustup_tool(
            rustup, TOOLCHAIN, "rustdoc", bootstrap_environment, resolution_probes
        ),
        "nightly_cargo": _rustup_tool(
            rustup,
            NIGHTLY_TOOLCHAIN,
            "cargo",
            bootstrap_environment,
            resolution_probes,
        ),
        "nightly_rustc": _rustup_tool(
            rustup,
            NIGHTLY_TOOLCHAIN,
            "rustc",
            bootstrap_environment,
            resolution_probes,
        ),
    }
    toolchain_directory = tool_paths["cargo"].parent
    for name in ("cargo-clippy", "cargo-fmt", "clippy-driver"):
        path = (toolchain_directory / name).resolve(strict=True)
        tool_paths[name.replace("-", "_")] = path
    nightly_toolchain_directory = tool_paths["nightly_cargo"].parent
    for name in (
        "rustdoc",
        "rustfmt",
        "cargo-fmt",
        "cargo-clippy",
        "clippy-driver",
    ):
        path = (nightly_toolchain_directory / name).resolve(strict=True)
        tool_paths[f"nightly_{name.replace('-', '_')}"] = path
    for name in WORKSPACE_TOOLS:
        beside = pathlib.Path(_workspace_tool_path(name))
        tool_paths[name] = (pathlib.Path.cwd() / beside).resolve(strict=True)
    for name in (
        "python3",
        "node",
        "npm",
        "bash",
        "shellcheck",
        "git",
        "date",
        "sed",
        "shasum",
        "tee",
        "find",
        "sort",
        "uname",
        "awk",
        "ps",
        "rmdir",
        "mkdir",
        "chmod",
        "mktemp",
        "dirname",
        "cc",
        "clang",
        "ld",
        "xcrun",
        "env",
        "sh",
    ):
        placeholder = {"python3": "python"}.get(name, name)
        tool_paths[placeholder] = _resolved_tool(name)
    path_directories = list(
        dict.fromkeys(str(path.parent) for _name, path in sorted(tool_paths.items()))
    )
    for directory in ("/usr/bin", "/bin", "/usr/sbin", "/sbin"):
        if directory not in path_directories:
            path_directories.append(directory)
    selected_environment = _sanitized_environment_values(
        validation_root,
        cargo_home,
        rustup_home,
        path_directories,
        rustc=tool_paths["rustc"],
        rustdoc=tool_paths["rustdoc"],
    )
    records: dict[str, Any] = {}
    probe_receipts = list(resolution_probes)
    version_arguments = {
        "cargo": ("--version", "--verbose"),
        "rustc": ("--version", "--verbose"),
        "rustfmt": ("--version",),
        "rustdoc": ("--version",),
        "python": ("--version",),
        "node": ("--version",),
        "npm": ("--version",),
        "bash": ("--version",),
        "shellcheck": ("--version",),
        "cargo_fuzz": ("--version",),
        # `cargo-mutants` is a cargo SUBCOMMAND binary: bare `--version` is
        # parsed as `cargo --version` and refused with
        # `Found argument '--version' which wasn't expected`. The subcommand
        # word is part of its argv, exactly as `gate_b/cargo_mutants_version`
        # spells it.
        "cargo_mutants": ("mutants", "--version"),
        "cc": ("--version",),
        "clang": ("--version",),
        "ld": ("-v",),
        "nightly_cargo": ("--version", "--verbose"),
        "nightly_rustc": ("--version", "--verbose"),
    }
    for name, path in sorted(tool_paths.items()):
        record, receipt = _tool_record(
            path, version_arguments.get(name, ()), selected_environment
        )
        records[name] = record
        if receipt is not None:
            probe_receipts.append(
                {
                    "tool": name,
                    "version": record["version"],
                    "execution": receipt,
                }
            )
    return (
        {
            "schema_version": SCHEMA_VERSION,
            "rust_toolchain": TOOLCHAIN,
            "nightly_toolchain": NIGHTLY_TOOLCHAIN,
            "tools": records,
            "tool_paths": {
                name: str(path) for name, path in sorted(tool_paths.items())
            },
        },
        selected_environment,
        probe_receipts,
    )


_REJECTED_AMBIENT_BUILD_NAMES = frozenset(
    {
        "AR",
        "CC",
        "CFLAGS",
        "CPPFLAGS",
        "CXX",
        "DEVELOPER_DIR",
        "LDFLAGS",
        "MACOSX_DEPLOYMENT_TARGET",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTDOCFLAGS",
        "RUSTFLAGS",
        "SDKROOT",
    }
)


def _cargo_configuration_absence(
    workspace: pathlib.Path, cargo_home: pathlib.Path
) -> list[str]:
    paths: list[pathlib.Path] = []
    current = workspace
    while True:
        paths.extend(
            (current / ".cargo" / "config", current / ".cargo" / "config.toml")
        )
        if current.parent == current:
            break
        current = current.parent
    paths.extend((cargo_home / "config", cargo_home / "config.toml"))
    checked: list[str] = []
    for path in paths:
        try:
            path.lstat()
        except FileNotFoundError:
            checked.append(str(path))
            continue
        raise CandidateEnvelopeError(f"Cargo configuration is forbidden: {path}")
    return sorted(set(checked))


def _build_environment_identity(
    workspace: pathlib.Path,
    validation_root: pathlib.Path,
    toolchain: Mapping[str, Any],
    selected: Mapping[str, str],
    cargo_home: pathlib.Path,
    rustup_home: pathlib.Path,
) -> dict[str, Any]:
    conflicting = sorted(
        name
        for name in os.environ
        if name in _REJECTED_AMBIENT_BUILD_NAMES
        or name.startswith(("CARGO_PROFILE_", "DYLD_", "LD_"))
        or name.startswith("CARGO_TARGET_")
        and name != "CARGO_TARGET_DIR"
    )
    if conflicting:
        raise CandidateEnvelopeError(
            f"ambient build inputs must be unset: {conflicting}"
        )
    for name, directory in (("CARGO_HOME", cargo_home), ("RUSTUP_HOME", rustup_home)):
        identity = _directory_identity(
            directory,
            name,
            reject_group_other_write=True,
            include_temporal=True,
        )
        if (
            not directory.is_absolute()
            or directory.resolve(strict=True) != directory
            or identity["mode"] == "0000"
        ):
            raise CandidateEnvelopeError(f"{name} must be one canonical directory")
    configuration_absent = _cargo_configuration_absence(workspace, cargo_home)
    paths = toolchain.get("tool_paths")
    if not isinstance(paths, dict):
        raise CandidateEnvelopeError("toolchain path mapping is malformed")
    path_directories = list(
        dict.fromkeys(str(pathlib.Path(str(path)).parent) for path in paths.values())
    )
    for directory in ("/usr/bin", "/bin", "/usr/sbin", "/sbin"):
        if directory not in path_directories:
            path_directories.append(directory)
    expected_selected = _sanitized_environment_values(
        validation_root,
        cargo_home,
        rustup_home,
        path_directories,
        rustc=pathlib.Path(str(paths["rustc"])),
        rustdoc=pathlib.Path(str(paths["rustdoc"])),
    )
    if dict(selected) != expected_selected:
        raise CandidateEnvelopeError("sanitized build environment is not exact")
    state_identity = _empty_execution_state(selected, "build environment")
    body = {
        "schema_version": SCHEMA_VERSION,
        "selected_values": dict(selected),
        "private_state_identity": state_identity,
        "cargo_home_identity": _directory_identity(
            cargo_home,
            "Cargo home",
            reject_group_other_write=True,
            include_temporal=True,
        ),
        "rustup_home_identity": _directory_identity(
            rustup_home,
            "rustup home",
            reject_group_other_write=True,
            include_temporal=True,
        ),
        "configuration_paths_proven_absent": configuration_absent,
        "rejected_ambient_names": sorted(_REJECTED_AMBIENT_BUILD_NAMES),
        "replaced_ambient_state_names": sorted(
            {
                "HOME",
                "TMPDIR",
                "TMP",
                "TEMP",
                *_FORBIDDEN_CHILD_STATE_NAMES,
            }.intersection(os.environ)
        ),
    }
    return {
        **body,
        "identity_sha256": evidence.canonical_json_sha256(
            body, domain=HASH_DOMAINS["build_environment"]
        ),
    }


def _runtime_identity(
    workspace: pathlib.Path, validation_root: pathlib.Path
) -> dict[str, Any]:
    ambient_home = pathlib.Path(os.environ.get("HOME", ""))
    if (
        not ambient_home.is_absolute()
        or ambient_home.resolve(strict=True) != ambient_home
    ):
        raise CandidateEnvelopeError("ambient HOME must be one canonical directory")
    cargo_home = pathlib.Path(
        os.environ.get("CARGO_HOME", str(ambient_home / ".cargo"))
    )
    rustup_home = pathlib.Path(
        os.environ.get("RUSTUP_HOME", str(ambient_home / ".rustup"))
    )
    toolchain, selected = _toolchain_identity(validation_root, cargo_home, rustup_home)
    environment = _build_environment_identity(
        workspace,
        validation_root,
        toolchain,
        selected,
        cargo_home,
        rustup_home,
    )
    return {
        "toolchain": toolchain,
        "tool_paths": toolchain["tool_paths"],
        "selected_build_environment": environment,
        "evidence_authorities": _evidence_authorities(),
    }


def _exact_process_environment(
    runtime_identity: Mapping[str, Any], target_directory: pathlib.Path
) -> dict[str, str]:
    selected = runtime_identity.get("selected_build_environment", {}).get(
        "selected_values"
    )
    if not isinstance(selected, dict) or not all(
        isinstance(name, str) and isinstance(value, str)
        for name, value in selected.items()
    ):
        raise CandidateEnvelopeError("sanitized environment is malformed")
    return {**selected, "CARGO_TARGET_DIR": str(target_directory)}


def _captured_runtime_identity(build_context: Mapping[str, Any]) -> dict[str, Any]:
    runtime = {
        "toolchain": build_context.get("toolchain"),
        "tool_paths": build_context.get("tool_paths"),
        "selected_build_environment": build_context.get("selected_build_environment"),
        "evidence_authorities": build_context.get("evidence_authorities"),
    }
    if not all(isinstance(value, dict) for value in runtime.values()):
        raise CandidateEnvelopeError("captured runtime identity is malformed")
    runtime["evidence_authorities"] = _validate_evidence_authorities(
        runtime["evidence_authorities"]
    )
    return runtime


def _require_release_outputs_absent(expected_target: pathlib.Path) -> None:
    release = expected_target / "release"
    for name in evidence.REQUIRED_RELEASE_BINARIES:
        path = release / name
        try:
            path.lstat()
        except FileNotFoundError:
            continue
        raise CandidateEnvelopeError(
            f"exact release output must be absent before candidate build: {path}"
        )


def _validate_build_receipt(
    receipt: Mapping[str, Any],
    workspace: pathlib.Path,
    build_context: Mapping[str, Any],
) -> dict[str, Any]:
    _exact_keys(
        receipt,
        frozenset(
            {
                "schema_version",
                "kind",
                "command",
                "workspace",
                "cargo_target_dir",
                "status",
                "exit_code",
                "stdout_size",
                "stdout_sha256",
                "stderr_size",
                "stderr_sha256",
                "process_ledger",
                "process_ledger_sha256",
                "executables",
            }
        ),
        "release build receipt",
    )
    expected_target = workspace / "target"
    tool_paths = build_context.get("tool_paths")
    if not isinstance(tool_paths, dict) or not isinstance(tool_paths.get("cargo"), str):
        raise CandidateEnvelopeError("release build context has no exact Cargo path")
    expected_command = [tool_paths["cargo"], *RELEASE_BUILD_COMMAND]
    raw_process_ledger = receipt.get("process_ledger")
    if not isinstance(raw_process_ledger, list):
        raise CandidateEnvelopeError("release build process ledger is malformed")
    process_ledger = _validated_process_ledger(
        raw_process_ledger, "release build", require_nonempty=True
    )
    if (
        not _is_exact_int(
            receipt.get("schema_version"),
            minimum=SCHEMA_VERSION,
            maximum=SCHEMA_VERSION,
        )
        or receipt.get("kind") != "pmux_gate_a_release_build"
        or receipt.get("command") != expected_command
        or receipt.get("workspace") != str(workspace)
        or receipt.get("cargo_target_dir") != str(expected_target)
        or receipt.get("status") != "PASS"
        or not _is_exact_int(receipt.get("exit_code"), minimum=0, maximum=0)
        or type(receipt.get("stdout_size")) is not int
        or receipt["stdout_size"] < 0
        or receipt["stdout_size"] > MAX_CARGO_BUILD_BYTES
        or not _is_sha256(receipt.get("stdout_sha256"))
        or type(receipt.get("stderr_size")) is not int
        or receipt["stderr_size"] < 0
        or receipt["stderr_size"] > MAX_CARGO_BUILD_BYTES
        or receipt["stdout_size"] + receipt["stderr_size"] > MAX_CARGO_BUILD_BYTES
        or not _is_sha256(receipt.get("stderr_sha256"))
        or receipt.get("process_ledger_sha256")
        != evidence.canonical_json_sha256(
            process_ledger, domain=HASH_DOMAINS["process_ledger"]
        )
    ):
        raise CandidateEnvelopeError("release build receipt binding is invalid")
    executables = receipt.get("executables")
    if not isinstance(executables, list) or len(executables) != len(
        evidence.REQUIRED_RELEASE_BINARIES
    ):
        raise CandidateEnvelopeError("release build executable receipts are incomplete")
    expected_records: list[dict[str, Any]] = []
    for name, record in zip(
        evidence.REQUIRED_RELEASE_BINARIES, executables, strict=True
    ):
        if not isinstance(record, dict):
            raise CandidateEnvelopeError(
                "release build executable receipt is malformed"
            )
        _exact_keys(
            record,
            frozenset({"name", "path", "package_id", "fresh"}),
            f"release build executable {name}",
        )
        if (
            record.get("name") != name
            or record.get("path") != str(expected_target / "release" / name)
            or not isinstance(record.get("package_id"), str)
            or record.get("fresh") is not False
        ):
            raise CandidateEnvelopeError(
                f"release build executable receipt differs for {name}"
            )
        expected_records.append(dict(record))
    result = dict(receipt)
    result["executables"] = expected_records
    return result


def _cargo_layout(
    workspace: pathlib.Path,
    validation_root: pathlib.Path,
    metadata_loader: MetadataLoader,
    runtime_identity: Mapping[str, Any],
    *,
    require_empty_validation: bool,
) -> dict[str, Any]:
    expected_target = workspace / "target"
    if expected_target.resolve(strict=True) != expected_target:
        raise CandidateEnvelopeError("Cargo target directory must already be canonical")
    metadata = metadata_loader(workspace, expected_target, runtime_identity)
    metadata_workspace = metadata.get("workspace_root")
    metadata_target = metadata.get("target_directory")
    if metadata_workspace != str(workspace):
        raise CandidateEnvelopeError(
            "Cargo metadata workspace_root differs from the exact workspace"
        )
    if metadata_target != str(expected_target):
        raise CandidateEnvelopeError(
            "Cargo metadata target_directory differs from the forced $PWD/target"
        )
    if pathlib.Path(metadata_workspace).resolve(strict=True) != workspace:
        raise CandidateEnvelopeError("Cargo metadata workspace_root is not canonical")
    if pathlib.Path(metadata_target).resolve(strict=True) != expected_target:
        raise CandidateEnvelopeError("Cargo metadata target_directory is not canonical")
    target_identity = _directory_identity(
        expected_target,
        "Cargo target directory",
        reject_group_other_write=True,
        include_temporal=True,
    )
    if target_identity != _directory_identity(
        expected_target,
        "Cargo target directory",
        reject_group_other_write=True,
        include_temporal=True,
    ):
        raise CandidateEnvelopeError("Cargo target directory identity changed")
    if not validation_root.is_absolute() or validation_root.is_relative_to(workspace):
        raise CandidateEnvelopeError(
            "Gate A validation root must be absolute and outside the workspace"
        )
    if validation_root.resolve(strict=True) != validation_root:
        raise CandidateEnvelopeError("Gate A validation root must be canonical")
    validation_parent_identity = _directory_identity(
        validation_root.parent,
        "Gate A validation parent",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_identity = _directory_identity(
        validation_root,
        "Gate A validation root",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_children = {
        name: _directory_identity(
            validation_root / name,
            f"Gate A validation child {name}",
            reject_group_other_write=True,
            include_nlink=False,
        )
        for name in VALIDATION_CHILD_NAMES
    }
    if sorted(child.name for child in validation_root.parent.iterdir()) != [
        validation_root.name
    ]:
        raise CandidateEnvelopeError(
            "Gate A validation parent must contain only its validation root"
        )
    if sorted(child.name for child in validation_root.iterdir()) != sorted(
        validation_children
    ):
        raise CandidateEnvelopeError(
            "Gate A validation root membership is not the exact precreated set"
        )
    for name in validation_children:
        if not require_empty_validation and name not in {"home", "tmp"}:
            continue
        try:
            child_manifest = evidence.regular_tree_manifest(validation_root / name)
        except evidence.EvidenceError as error:
            raise CandidateEnvelopeError(str(error)) from error
        if (
            child_manifest.get("file_count") != 0
            or child_manifest.get("directory_count") != 0
        ):
            raise CandidateEnvelopeError(
                f"Gate A validation child must be empty at this boundary: {name}"
            )
    _empty_execution_state(
        _exact_process_environment(runtime_identity, expected_target),
        "Cargo layout",
    )
    return {
        "workspace_root": str(workspace),
        "target_directory": str(expected_target),
        "candidate_cargo_target_dir": str(expected_target),
        "validation_root_directory": str(validation_root),
        "validation_cargo_target_directory": str(validation_root / "cargo-target"),
        "target_identity": target_identity,
        "validation_parent_identity": validation_parent_identity,
        "validation_root_identity": validation_identity,
        "validation_child_identities": validation_children,
    }


def _require_forced_target_environment(workspace: pathlib.Path) -> pathlib.Path:
    return workspace / "target"


def _private_evidence_directory(
    path: pathlib.Path, expected: Mapping[str, Any] | None = None
) -> dict[str, Any]:
    if not path.is_absolute():
        raise CandidateEnvelopeError("evidence directory must be absolute")
    if path.resolve(strict=True) != path:
        raise CandidateEnvelopeError("evidence directory must already be canonical")
    # Publishing artifacts can change a directory's link count on macOS.  The
    # inode/owner/mode stay fixed, while exact membership is validated by the
    # ordered artifact loader.
    identity = _directory_identity(
        path, "candidate evidence directory", include_nlink=False
    )
    if identity["mode"] != "0700":
        raise CandidateEnvelopeError("candidate evidence directory must be mode 0700")
    if expected is not None and identity != expected:
        raise CandidateEnvelopeError("candidate evidence directory identity changed")
    return identity


def _private_evidence_parent(
    evidence_directory: pathlib.Path, expected: Mapping[str, Any] | None = None
) -> dict[str, Any]:
    parent = evidence_directory.parent
    identity = _directory_identity(
        parent,
        "candidate evidence parent",
        reject_group_other_write=True,
        include_temporal=True,
    )
    if identity["mode"] != "0700":
        raise CandidateEnvelopeError("candidate evidence parent must be mode 0700")
    if sorted(child.name for child in parent.iterdir()) != [evidence_directory.name]:
        raise CandidateEnvelopeError(
            "candidate evidence parent must contain only its evidence child"
        )
    if expected is not None and identity != expected:
        raise CandidateEnvelopeError("candidate evidence parent identity changed")
    return identity


def _load_private_bytes(
    path: pathlib.Path, description: str, maximum_bytes: int
) -> bytes:
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise CandidateEnvelopeError(f"{description} is missing: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise CandidateEnvelopeError(
            f"{description} must be a real regular file: {path}"
        )
    if metadata.st_uid != os.geteuid():
        raise CandidateEnvelopeError(f"{description} has the wrong owner: {path}")
    if metadata.st_nlink != 1:
        raise CandidateEnvelopeError(
            f"{description} has an ambiguous hard-link alias: {path}"
        )
    if stat.S_IMODE(metadata.st_mode) != evidence.PRIVATE_FILE_MODE:
        raise CandidateEnvelopeError(f"{description} must be mode 0600: {path}")
    try:
        return evidence._stable_regular_bytes(
            path,
            description=description,
            maximum_bytes=maximum_bytes,
            before=metadata,
        )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error


def _load_private_json(path: pathlib.Path, description: str) -> Mapping[str, Any]:
    payload_bytes = _load_private_bytes(
        path, description, evidence.MAX_JSON_EVIDENCE_BYTES
    )
    try:
        payload = evidence.strict_json_loads(payload_bytes, description=description)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    if not isinstance(payload, dict):
        raise CandidateEnvelopeError(f"{description} must contain a JSON object")
    return payload


def _sealed_payload(
    body: Mapping[str, Any], *, digest_field: str, domain: str
) -> dict[str, Any]:
    payload = dict(body)
    payload[digest_field] = evidence.canonical_json_sha256(body, domain=domain)
    return payload


def _verify_sealed_payload(
    payload: Mapping[str, Any], *, digest_field: str, description: str, domain: str
) -> str:
    digest = payload.get(digest_field)
    if not isinstance(digest, str):
        raise CandidateEnvelopeError(f"{description} digest is missing")
    body = dict(payload)
    del body[digest_field]
    expected = evidence.canonical_json_sha256(body, domain=domain)
    if digest != expected:
        raise CandidateEnvelopeError(f"{description} digest does not match its body")
    return digest


def _validate_expected_candidate_digest(value: str) -> str:
    if not _is_sha256(value):
        raise CandidateEnvelopeError(
            "expected candidate digest must be exactly 64 lowercase hexadecimal characters"
        )
    return value


def _validate_external_anchor(value: str, description: str) -> str:
    if not _is_sha256(value):
        raise CandidateEnvelopeError(
            f"{description} must be exactly 64 lowercase hexadecimal characters"
        )
    return value


def _phase_report_filename(phase: str) -> str:
    return f"phase-{phase}.json"


def _phase_log_filename(phase: str, ordinal: int, command_id: str, stream: str) -> str:
    if stream not in {"stdout", "stderr"}:
        raise CandidateEnvelopeError("phase log stream is invalid")
    return f"command-{phase}-{ordinal:02d}-{command_id}.{stream}.log"


def _load_phase_manifest() -> tuple[Mapping[str, Any], str]:
    try:
        manifest = evidence.load_json(PHASE_MANIFEST_FILE)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    if not isinstance(manifest, dict):
        raise CandidateEnvelopeError("phase command manifest must be an object")
    _exact_keys(
        manifest,
        frozenset(
            {
                "schema_version",
                "max_command_output_bytes",
                "phase_timeouts_seconds",
                "phases",
            }
        ),
        "phase command manifest",
    )
    phases = manifest.get("phases")
    timeouts = manifest.get("phase_timeouts_seconds")
    maximum_output = manifest.get("max_command_output_bytes")
    if not _is_exact_int(
        manifest.get("schema_version"), minimum=SCHEMA_VERSION, maximum=SCHEMA_VERSION
    ) or not isinstance(phases, dict):
        raise CandidateEnvelopeError("phase command manifest schema is invalid")
    if (
        not isinstance(timeouts, dict)
        or tuple(timeouts) != PHASES
        or any(
            type(timeouts.get(phase)) is not int
            or timeouts[phase] < 1
            or timeouts[phase] > 86_400
            for phase in PHASES
        )
        or type(maximum_output) is not int
        or maximum_output < 1024
        or maximum_output > 64 * 1024 * 1024
    ):
        raise CandidateEnvelopeError("phase command bounds are invalid")
    if tuple(phases) != PHASES:
        raise CandidateEnvelopeError("phase command manifest phase order is not exact")
    seen_ids: set[str] = set()
    for phase in PHASES:
        commands = phases.get(phase)
        if not isinstance(commands, list) or not commands:
            raise CandidateEnvelopeError(f"phase command list is empty: {phase}")
        for command in commands:
            if not isinstance(command, dict):
                raise CandidateEnvelopeError("phase command must be an object")
            command_keys = frozenset(command)
            base_keys = frozenset({"id", "cwd", "argv", "env", "stdout_equals"})
            if command_keys not in (
                base_keys,
                base_keys | {"stdout_sha256_line"},
            ):
                _exact_keys(command, base_keys, f"phase command in {phase}")
            command_id = command.get("id")
            cwd = command.get("cwd")
            argv = command.get("argv")
            environment = command.get("env")
            stdout_equals = command.get("stdout_equals")
            stdout_sha256_line = command.get("stdout_sha256_line", False)
            if (
                not isinstance(command_id, str)
                or not command_id
                or command_id in seen_ids
                or not isinstance(cwd, str)
                or not cwd
                or not isinstance(argv, list)
                or not argv
                or not all(isinstance(item, str) and item for item in argv)
                or not isinstance(environment, dict)
                or not all(
                    isinstance(name, str) and name and isinstance(value, str) and value
                    for name, value in environment.items()
                )
                or not (
                    stdout_equals is None
                    or isinstance(stdout_equals, str)
                    and len(stdout_equals.encode("utf-8")) <= 4096
                )
                or type(stdout_sha256_line) is not bool
                or stdout_sha256_line
                and stdout_equals is not None
            ):
                raise CandidateEnvelopeError("phase command manifest entry is invalid")
            seen_ids.add(command_id)
    return manifest, evidence.canonical_json_sha256(
        manifest, domain=HASH_DOMAINS["phase_manifest"]
    )


def _expanded_phase_commands(
    candidate: Mapping[str, Any], phase: str
) -> tuple[list[dict[str, Any]], str]:
    if phase not in PHASES:
        raise CandidateEnvelopeError(f"unknown candidate phase: {phase}")
    manifest, manifest_digest = _load_phase_manifest()
    replacements = {
        "{workspace}": str(candidate["workspace"]),
        "{target}": str(candidate["target_directory"]),
        "{release}": str(candidate["release_directory"]),
        "{validation}": str(candidate["validation_root_directory"]),
    }
    build_context = candidate.get("build_context")
    if not isinstance(build_context, dict) or not isinstance(
        build_context.get("tool_paths"), dict
    ):
        raise CandidateEnvelopeError("candidate tool-path binding is malformed")
    for name, path in build_context["tool_paths"].items():
        if not isinstance(name, str) or not isinstance(path, str):
            raise CandidateEnvelopeError("candidate tool-path binding is malformed")
        replacements[f"{{{name}}}"] = path
    nightly_cargo = build_context["tool_paths"].get("nightly_cargo")
    if not isinstance(nightly_cargo, str):
        raise CandidateEnvelopeError("candidate has no exact nightly Cargo path")
    replacements["{nightly_bin}"] = str(pathlib.Path(nightly_cargo).parent)

    def expand(value: str) -> str:
        result = value
        for token, replacement in replacements.items():
            result = result.replace(token, replacement)
        if "{" in result or "}" in result:
            raise CandidateEnvelopeError(
                f"phase command contains an unknown placeholder: {value}"
            )
        return result

    raw_commands = manifest["phases"][phase]
    timeout_seconds = manifest["phase_timeouts_seconds"][phase]
    maximum_output_bytes = manifest["max_command_output_bytes"]
    commands = [
        {
            "id": command["id"],
            "cwd": expand(command["cwd"]),
            "argv": [expand(value) for value in command["argv"]],
            "env": {
                name: expand(value) for name, value in sorted(command["env"].items())
            },
            "stdout_equals": command["stdout_equals"],
            "stdout_sha256_line": command.get("stdout_sha256_line", False),
            "timeout_seconds": timeout_seconds,
            "maximum_output_bytes": maximum_output_bytes,
        }
        for command in raw_commands
    ]
    cargo_path = build_context["tool_paths"].get("cargo")
    validation = pathlib.Path(str(candidate["validation_cargo_target_directory"]))
    release = pathlib.Path(str(candidate["release_directory"]))
    for command in commands:
        cwd = pathlib.Path(command["cwd"])
        if not cwd.is_absolute() or not cwd.is_relative_to(
            pathlib.Path(str(candidate["workspace"]))
        ):
            raise CandidateEnvelopeError("phase command cwd escaped the workspace")
        target_value = command["env"].get("CARGO_TARGET_DIR", str(validation))
        if command["argv"][0] == cargo_path:
            target_path = pathlib.Path(target_value)
            if target_path != validation and not target_path.is_relative_to(validation):
                raise CandidateEnvelopeError(
                    "phase Cargo command escaped the isolated validation target"
                )
            if target_path == pathlib.Path(str(candidate["target_directory"])) or (
                target_path == release or target_path.is_relative_to(release)
            ):
                raise CandidateEnvelopeError(
                    "phase Cargo command could write the frozen candidate"
                )
    return commands, manifest_digest


def _run_phase_command(
    cwd: pathlib.Path,
    argv: Sequence[str],
    environment: Mapping[str, str],
    timeout_seconds: int,
    maximum_output_bytes: int,
) -> CommandExecution:
    return _run_bounded_command(
        argv,
        cwd=cwd,
        environment=environment,
        timeout_seconds=timeout_seconds,
        maximum_output_bytes=maximum_output_bytes,
        description="phase command",
    )


def _workspace_revision_capture(
    workspace: pathlib.Path,
    loader: RevisionCaptureLoader = source_digest.workspace_revision_capture,
) -> dict[str, Any]:
    try:
        raw = loader(workspace)
        capture = source_digest.validate_workspace_revision_capture(raw)
    except source_digest.SourceIdentityError as error:
        raise CandidateEnvelopeError(str(error)) from error
    if capture["identity"]["workspace"] != str(workspace):
        raise CandidateEnvelopeError(
            "workspace revision capture names another workspace"
        )
    bounded = capture["bounded_process_implementation"]
    if bounded != BOUNDED_PROCESS_AUTHORITY:
        raise CandidateEnvelopeError(
            "workspace revision capture used another process authority"
        )
    return capture


def _same_revision_identity(
    expected: Mapping[str, Any], capture: Mapping[str, Any], description: str
) -> None:
    if capture.get("identity") != dict(expected):
        raise CandidateEnvelopeError(f"workspace revision changed {description}")


def _candidate_body(
    workspace: pathlib.Path,
    evidence_directory: pathlib.Path,
    build_receipt: Mapping[str, Any],
    build_context: Mapping[str, Any],
    metadata_loader: MetadataLoader,
    revision_capture_loader: RevisionCaptureLoader,
) -> dict[str, Any]:
    workspace = _require_canonical_workspace(workspace)
    evidence_identity = _private_evidence_directory(evidence_directory)
    evidence_parent_identity = _private_evidence_parent(evidence_directory)
    validated_build = _validate_build_receipt(build_receipt, workspace, build_context)
    validation_root_value = build_context.get("validation_root_directory")
    if not isinstance(validation_root_value, str):
        raise CandidateEnvelopeError("build context validation root is missing")
    validation_root = pathlib.Path(validation_root_value)
    captured_runtime = _captured_runtime_identity(build_context)
    cargo_before = _cargo_layout(
        workspace,
        validation_root,
        metadata_loader,
        captured_runtime,
        require_empty_validation=True,
    )
    revision_expected = build_context.get("source_revision_identity")
    build_revision_captures = build_context.get("source_revision_captures")
    if (
        not isinstance(revision_expected, Mapping)
        or not isinstance(build_revision_captures, Mapping)
        or set(build_revision_captures)
        != {
            "before_release_build",
            "after_release_build",
        }
    ):
        raise CandidateEnvelopeError("build revision evidence is incomplete")
    revision_expected = source_digest.validate_workspace_revision_identity(
        revision_expected
    )
    normalized_build_captures: dict[str, Any] = {}
    for label in ("before_release_build", "after_release_build"):
        capture = source_digest.validate_workspace_revision_capture(
            build_revision_captures[label]
        )
        _same_revision_identity(revision_expected, capture, f"at {label}")
        normalized_build_captures[label] = capture
    capture_before = _workspace_revision_capture(
        workspace, loader=revision_capture_loader
    )
    _same_revision_identity(
        revision_expected, capture_before, "before candidate capture"
    )
    source_guard_before = source_digest.workspace_source_guard(workspace)
    if build_context.get("source_guard") != source_guard_before:
        raise CandidateEnvelopeError(
            "source identity differs from the pre-build capture"
        )
    if build_context.get("cargo_layout") != cargo_before:
        raise CandidateEnvelopeError("Cargo layout differs from the pre-build capture")
    release_directory = workspace / "target" / "release"
    try:
        binaries = evidence.release_binary_manifest(release_directory)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    source_guard_after = source_digest.workspace_source_guard(workspace)
    if source_guard_after != source_guard_before:
        raise CandidateEnvelopeError("source identity changed during candidate capture")
    capture_after = _workspace_revision_capture(
        workspace, loader=revision_capture_loader
    )
    _same_revision_identity(
        revision_expected, capture_after, "during candidate capture"
    )
    try:
        if evidence.verify_release_binary_manifest(binaries) != binaries:
            raise CandidateEnvelopeError("release binary verification was inconsistent")
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    cargo_after = _cargo_layout(
        workspace,
        validation_root,
        metadata_loader,
        captured_runtime,
        require_empty_validation=True,
    )
    if cargo_after != cargo_before:
        raise CandidateEnvelopeError("Cargo layout changed during candidate capture")
    _private_evidence_directory(evidence_directory, evidence_identity)
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "pmux_gate_a_candidate",
        "workspace": str(workspace),
        "target_directory": str(workspace / "target"),
        "release_directory": str(release_directory),
        "validation_root_directory": str(validation_root),
        "validation_cargo_target_directory": str(validation_root / "cargo-target"),
        "evidence_directory": str(evidence_directory),
        "evidence_directory_identity": evidence_identity,
        "evidence_parent_identity": evidence_parent_identity,
        "cargo_layout": cargo_before,
        "release_build_receipt": validated_build,
        "release_build_receipt_sha256": evidence.canonical_json_sha256(
            validated_build, domain=HASH_DOMAINS["release_build"]
        ),
        "build_context": dict(build_context),
        "build_context_sha256": evidence.canonical_json_sha256(
            build_context, domain=HASH_DOMAINS["build_context"]
        ),
        "source_guard": source_guard_before,
        "source_guard_sha256": evidence.canonical_json_sha256(
            source_guard_before, domain=HASH_DOMAINS["source_guard"]
        ),
        "source_manifest": source_guard_before["manifest"],
        "source_manifest_sha256": evidence.canonical_json_sha256(
            source_guard_before["manifest"], domain=HASH_DOMAINS["source_manifest"]
        ),
        "source_revision_identity": revision_expected,
        "source_revision_identity_sha256": evidence.canonical_json_sha256(
            revision_expected, domain=HASH_DOMAINS["source_revision_identity"]
        ),
        "source_revision_captures": {
            **normalized_build_captures,
            "before_candidate_capture": capture_before,
            "after_candidate_capture": capture_after,
        },
        "release_binary_manifest": binaries,
        "release_binary_manifest_sha256": evidence.canonical_json_sha256(
            binaries, domain=HASH_DOMAINS["binary_manifest"]
        ),
        "checkpoint_order": list(CHECKPOINTS),
    }


def capture_candidate(
    workspace: pathlib.Path,
    evidence_directory: pathlib.Path,
    build_receipt: Mapping[str, Any],
    build_context: Mapping[str, Any],
    *,
    metadata_loader: MetadataLoader = _run_cargo_metadata,
    revision_capture_loader: RevisionCaptureLoader = (
        source_digest.workspace_revision_capture
    ),
) -> Mapping[str, Any]:
    try:
        evidence.prepare_empty_private_directory(evidence_directory.parent)
        evidence.prepare_empty_private_directory(evidence_directory)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    body = _candidate_body(
        workspace,
        evidence_directory,
        build_receipt,
        build_context,
        metadata_loader,
        revision_capture_loader,
    )
    candidate = _sealed_payload(
        body,
        digest_field="candidate_manifest_sha256",
        domain=HASH_DOMAINS["candidate"],
    )
    try:
        evidence.atomic_write_json(evidence_directory / CANDIDATE_FILE, candidate)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    loaded = _load_candidate(evidence_directory)
    if loaded != candidate:
        raise CandidateEnvelopeError("published candidate manifest changed")
    return loaded


def build_and_capture_candidate(
    workspace: pathlib.Path,
    evidence_directory: pathlib.Path,
    validation_root: pathlib.Path,
    *,
    metadata_loader: MetadataLoader = _run_cargo_metadata,
    build_runner: BuildRunner = _run_release_build,
    runtime_identity_loader: RuntimeIdentityLoader = _runtime_identity,
    revision_capture_loader: RevisionCaptureLoader = (
        source_digest.workspace_revision_capture
    ),
) -> Mapping[str, Any]:
    workspace = _require_canonical_workspace(workspace)
    expected_target = _require_forced_target_environment(workspace)
    try:
        expected_target.lstat()
    except FileNotFoundError:
        pass
    else:
        raise CandidateEnvelopeError(
            "candidate Cargo target must be absent before the release build"
        )
    if not validation_root.is_absolute() or validation_root.is_relative_to(workspace):
        raise CandidateEnvelopeError(
            "validation root must be absolute and outside the workspace"
        )
    if validation_root != pathlib.Path(os.path.normpath(str(validation_root))):
        raise CandidateEnvelopeError("validation root must already be normalized")
    try:
        validation_root.parent.lstat()
    except FileNotFoundError:
        pass
    else:
        raise CandidateEnvelopeError(
            "dedicated validation parent must be absent before candidate construction"
        )
    try:
        evidence.prepare_empty_private_directory(validation_root.parent)
        evidence.prepare_empty_private_directory(validation_root)
        for child in VALIDATION_CHILD_NAMES:
            evidence.prepare_empty_private_directory(validation_root / child)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    try:
        evidence.prepare_empty_private_directory(expected_target)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    target_before = _directory_identity(
        expected_target,
        "fresh candidate Cargo target",
        reject_group_other_write=True,
    )
    if list(expected_target.iterdir()):
        raise CandidateEnvelopeError("candidate Cargo target is not newly empty")
    revision_before = _workspace_revision_capture(
        workspace, loader=revision_capture_loader
    )
    revision_identity = revision_before["identity"]
    source_before = source_digest.workspace_source_guard(workspace)
    runtime_before = runtime_identity_loader(workspace, validation_root)
    toolchain_before = runtime_before["toolchain"]
    tool_paths_before = runtime_before["tool_paths"]
    environment_before = runtime_before["selected_build_environment"]
    authorities_before = _validate_evidence_authorities(
        runtime_before.get("evidence_authorities")
    )
    _require_release_outputs_absent(expected_target)
    if not isinstance(tool_paths_before, dict) or not isinstance(
        tool_paths_before.get("cargo"), str
    ):
        raise CandidateEnvelopeError("runtime identity has no exact Cargo path")
    build_command = [tool_paths_before["cargo"], *RELEASE_BUILD_COMMAND]
    build_environment = _exact_process_environment(runtime_before, expected_target)
    receipt = build_runner(
        workspace,
        expected_target,
        build_command,
        build_environment,
    )
    revision_after = _workspace_revision_capture(
        workspace, loader=revision_capture_loader
    )
    _same_revision_identity(revision_identity, revision_after, "across release build")
    source_after = source_digest.workspace_source_guard(workspace)
    if source_after != source_before:
        raise CandidateEnvelopeError("source identity changed across release build")
    runtime_after = runtime_identity_loader(workspace, validation_root)
    if runtime_after.get("toolchain") != toolchain_before:
        raise CandidateEnvelopeError("toolchain identity changed across release build")
    if runtime_after.get("selected_build_environment") != environment_before:
        raise CandidateEnvelopeError(
            "selected build environment changed across release build"
        )
    if runtime_after.get("tool_paths") != tool_paths_before:
        raise CandidateEnvelopeError("exact tool paths changed across release build")
    if (
        _validate_evidence_authorities(runtime_after.get("evidence_authorities"))
        != authorities_before
    ):
        raise CandidateEnvelopeError(
            "evidence authorities changed across release build"
        )
    target_after_build = _directory_identity(
        expected_target,
        "candidate Cargo target after build",
        reject_group_other_write=True,
    )
    stable_target_fields = ("path", "device", "inode", "uid", "gid", "mode")
    if any(
        target_after_build[field] != target_before[field]
        for field in stable_target_fields
    ):
        raise CandidateEnvelopeError("Cargo target directory was replaced across build")
    cargo_after = _cargo_layout(
        workspace,
        validation_root,
        metadata_loader,
        runtime_after,
        require_empty_validation=True,
    )
    build_context = {
        "schema_version": SCHEMA_VERSION,
        "validation_root_directory": str(validation_root),
        "source_guard": source_before,
        "cargo_layout": cargo_after,
        "toolchain": toolchain_before,
        "tool_paths": tool_paths_before,
        "selected_build_environment": environment_before,
        "evidence_authorities": authorities_before,
        "source_revision_identity": revision_identity,
        "source_revision_captures": {
            "before_release_build": revision_before,
            "after_release_build": revision_after,
        },
    }
    return capture_candidate(
        workspace,
        evidence_directory,
        receipt,
        build_context,
        metadata_loader=metadata_loader,
        revision_capture_loader=revision_capture_loader,
    )


def _load_candidate(
    evidence_directory: pathlib.Path,
    expected_candidate_sha256: str | None = None,
) -> Mapping[str, Any]:
    candidate = _load_private_json(
        evidence_directory / CANDIDATE_FILE, "candidate manifest"
    )
    _exact_keys(
        candidate,
        frozenset(
            {
                "schema_version",
                "kind",
                "workspace",
                "target_directory",
                "release_directory",
                "validation_root_directory",
                "validation_cargo_target_directory",
                "evidence_directory",
                "evidence_directory_identity",
                "evidence_parent_identity",
                "cargo_layout",
                "release_build_receipt",
                "release_build_receipt_sha256",
                "build_context",
                "build_context_sha256",
                "source_guard",
                "source_guard_sha256",
                "source_manifest",
                "source_manifest_sha256",
                "source_revision_identity",
                "source_revision_identity_sha256",
                "source_revision_captures",
                "release_binary_manifest",
                "release_binary_manifest_sha256",
                "checkpoint_order",
                "candidate_manifest_sha256",
            }
        ),
        "candidate manifest",
    )
    if not _is_exact_int(
        candidate.get("schema_version"), minimum=SCHEMA_VERSION, maximum=SCHEMA_VERSION
    ):
        raise CandidateEnvelopeError("candidate manifest schema is unsupported")
    if candidate.get("kind") != "pmux_gate_a_candidate":
        raise CandidateEnvelopeError("candidate manifest kind is invalid")
    if candidate.get("checkpoint_order") != list(CHECKPOINTS):
        raise CandidateEnvelopeError("candidate checkpoint order is not exact")
    candidate_digest = _verify_sealed_payload(
        candidate,
        digest_field="candidate_manifest_sha256",
        description="candidate manifest",
        domain=HASH_DOMAINS["candidate"],
    )
    if expected_candidate_sha256 is not None and candidate_digest != (
        _validate_expected_candidate_digest(expected_candidate_sha256)
    ):
        raise CandidateEnvelopeError(
            "candidate manifest differs from the externally carried digest"
        )
    source = candidate.get("source_manifest")
    source_guard = candidate.get("source_guard")
    binaries = candidate.get("release_binary_manifest")
    if (
        not isinstance(source, dict)
        or not isinstance(source_guard, dict)
        or not isinstance(binaries, dict)
    ):
        raise CandidateEnvelopeError("candidate identity manifests are malformed")
    if source_guard.get("manifest") != source:
        raise CandidateEnvelopeError("candidate source guard does not bind manifest")
    if candidate.get("source_guard_sha256") != evidence.canonical_json_sha256(
        source_guard, domain=HASH_DOMAINS["source_guard"]
    ):
        raise CandidateEnvelopeError("candidate source guard digest is invalid")
    build_receipt = candidate.get("release_build_receipt")
    if not isinstance(build_receipt, dict):
        raise CandidateEnvelopeError("candidate release build receipt is malformed")
    build_context = candidate.get("build_context")
    if (
        not isinstance(build_context, dict)
        or build_context.get("source_guard") != source_guard
    ):
        raise CandidateEnvelopeError("candidate build context is malformed")
    validated_build = _validate_build_receipt(
        build_receipt,
        pathlib.Path(str(candidate["workspace"])),
        build_context,
    )
    if candidate.get("release_build_receipt_sha256") != evidence.canonical_json_sha256(
        validated_build, domain=HASH_DOMAINS["release_build"]
    ):
        raise CandidateEnvelopeError(
            "candidate release build receipt digest is invalid"
        )
    if candidate.get("build_context_sha256") != evidence.canonical_json_sha256(
        build_context, domain=HASH_DOMAINS["build_context"]
    ):
        raise CandidateEnvelopeError("candidate build context digest is invalid")
    if candidate.get("source_manifest_sha256") != evidence.canonical_json_sha256(
        source, domain=HASH_DOMAINS["source_manifest"]
    ):
        raise CandidateEnvelopeError("candidate source manifest digest is invalid")
    revision_identity_raw = candidate.get("source_revision_identity")
    revision_captures_raw = candidate.get("source_revision_captures")
    try:
        revision_identity = source_digest.validate_workspace_revision_identity(
            revision_identity_raw
        )
    except source_digest.SourceIdentityError as error:
        raise CandidateEnvelopeError(
            "candidate revision identity is invalid"
        ) from error
    if candidate.get(
        "source_revision_identity_sha256"
    ) != evidence.canonical_json_sha256(
        revision_identity, domain=HASH_DOMAINS["source_revision_identity"]
    ):
        raise CandidateEnvelopeError("candidate revision identity digest is invalid")
    revision_labels = {
        "before_release_build",
        "after_release_build",
        "before_candidate_capture",
        "after_candidate_capture",
    }
    if (
        not isinstance(revision_captures_raw, Mapping)
        or set(revision_captures_raw) != revision_labels
    ):
        raise CandidateEnvelopeError("candidate revision capture schema is not exact")
    normalized_revision_captures: dict[str, Any] = {}
    for label in sorted(revision_labels):
        try:
            capture = source_digest.validate_workspace_revision_capture(
                revision_captures_raw[label]
            )
        except source_digest.SourceIdentityError as error:
            raise CandidateEnvelopeError(
                f"candidate revision capture is invalid: {label}"
            ) from error
        _same_revision_identity(revision_identity, capture, f"at {label}")
        bounded_authority = capture.get("bounded_process_implementation")
        if bounded_authority != BOUNDED_PROCESS_AUTHORITY:
            raise CandidateEnvelopeError(
                f"candidate revision capture used another process authority: {label}"
            )
        normalized_revision_captures[label] = capture
    if build_context.get("source_revision_identity") != revision_identity or (
        build_context.get("source_revision_captures")
        != {
            label: normalized_revision_captures[label]
            for label in ("before_release_build", "after_release_build")
        }
    ):
        raise CandidateEnvelopeError("candidate build revision evidence disagrees")
    if candidate.get(
        "release_binary_manifest_sha256"
    ) != evidence.canonical_json_sha256(
        binaries, domain=HASH_DOMAINS["binary_manifest"]
    ):
        raise CandidateEnvelopeError("candidate binary manifest digest is invalid")
    expected_evidence = candidate.get("evidence_directory_identity")
    expected_parent = candidate.get("evidence_parent_identity")
    if not isinstance(expected_evidence, dict) or not isinstance(expected_parent, dict):
        raise CandidateEnvelopeError(
            "candidate evidence-directory identity is malformed"
        )
    _private_evidence_directory(evidence_directory, expected_evidence)
    _private_evidence_parent(evidence_directory, expected_parent)
    if candidate.get("evidence_directory") != str(evidence_directory):
        raise CandidateEnvelopeError("candidate evidence-directory path changed")
    return candidate


def _typescript_stage_verifier_digest(manifest: Mapping[str, Any]) -> str:
    _exact_keys(
        manifest,
        frozenset(
            {
                "schema_version",
                "algorithm",
                "root",
                "excluded_paths",
                "directory_count",
                "file_count",
                "tree_sha256",
                "directories",
                "files",
            }
        ),
        "TypeScript stage tree manifest",
    )
    if (
        not _is_exact_int(manifest.get("schema_version"), minimum=2, maximum=2)
        or manifest.get("algorithm") != "pmux-artifact-tree-v2-sha256"
        or manifest.get("root") != "."
        or manifest.get("excluded_paths") != []
        or not _is_exact_int(manifest.get("directory_count"), minimum=0, maximum=0)
        or not _is_exact_int(
            manifest.get("file_count"),
            minimum=len(TYPESCRIPT_STAGE_FILES),
            maximum=len(TYPESCRIPT_STAGE_FILES),
        )
        or manifest.get("directories") != []
        or not isinstance(manifest.get("tree_sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", manifest["tree_sha256"]) is None
    ):
        raise CandidateEnvelopeError("TypeScript stage tree manifest is malformed")
    files = manifest.get("files")
    if not isinstance(files, list) or len(files) != len(TYPESCRIPT_STAGE_FILES):
        raise CandidateEnvelopeError("TypeScript stage file manifest is malformed")
    verifier_files: list[dict[str, str]] = []
    for expected_path, entry in zip(TYPESCRIPT_STAGE_FILES, files, strict=True):
        if not isinstance(entry, dict):
            raise CandidateEnvelopeError("TypeScript stage file entry is malformed")
        _exact_keys(
            entry,
            frozenset({"path", "size", "mode", "sha256"}),
            f"TypeScript stage file {expected_path}",
        )
        if (
            entry.get("path") != expected_path
            or type(entry.get("size")) is not int
            or entry["size"] < 0
            or entry.get("mode") != "0600"
            or not isinstance(entry.get("sha256"), str)
            or re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]) is None
        ):
            raise CandidateEnvelopeError(
                f"TypeScript stage file binding is invalid: {expected_path}"
            )
        if expected_path == "package.json":
            package_bytes = b'{"type":"module"}\n'
            if (
                entry["size"] != len(package_bytes)
                or entry["sha256"] != hashlib.sha256(package_bytes).hexdigest()
            ):
                raise CandidateEnvelopeError(
                    "TypeScript stage package.json is not the exact ESM scope"
                )
        verifier_files.append(
            {"relative_path": expected_path, "sha256": entry["sha256"]}
        )
    encoded = json.dumps(
        {"schema_version": 1, "files": verifier_files},
        separators=(",", ":"),
    ).encode("utf-8")
    digest = hashlib.sha256()
    digest.update(b"pmux-typescript-dist-stage-v1\0")
    digest.update(encoded)
    return digest.hexdigest()


def _capture_typescript_stage(
    candidate: Mapping[str, Any], verifier_output: bytes
) -> dict[str, Any]:
    if re.fullmatch(rb"[0-9a-f]{64}\n", verifier_output) is None:
        raise CandidateEnvelopeError(
            "TypeScript stage verifier did not emit one exact digest line"
        )
    stage_directory = (
        pathlib.Path(str(candidate["validation_root_directory"])) / "typescript-dist"
    )
    try:
        manifest = evidence.regular_tree_manifest(stage_directory)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    verifier_sha256 = _typescript_stage_verifier_digest(manifest)
    if verifier_output != f"{verifier_sha256}\n".encode("ascii"):
        raise CandidateEnvelopeError(
            "TypeScript stage verifier digest differs from the captured tree"
        )
    return _sealed_payload(
        {
            "schema_version": SCHEMA_VERSION,
            "kind": "pmux_gate_a_typescript_stage",
            "stage_directory": str(stage_directory),
            "verifier_sha256": verifier_sha256,
            "tree_manifest": manifest,
        },
        digest_field="typescript_stage_sha256",
        domain=HASH_DOMAINS["typescript_stage"],
    )


def _verify_typescript_stage(
    candidate: Mapping[str, Any], attestation: Mapping[str, Any]
) -> dict[str, Any]:
    if not isinstance(attestation, dict):
        raise CandidateEnvelopeError("TypeScript stage attestation is malformed")
    _exact_keys(
        attestation,
        frozenset(
            {
                "schema_version",
                "kind",
                "stage_directory",
                "verifier_sha256",
                "tree_manifest",
                "typescript_stage_sha256",
            }
        ),
        "TypeScript stage attestation",
    )
    expected_directory = (
        pathlib.Path(str(candidate["validation_root_directory"])) / "typescript-dist"
    )
    manifest = attestation.get("tree_manifest")
    if (
        not _is_exact_int(
            attestation.get("schema_version"),
            minimum=SCHEMA_VERSION,
            maximum=SCHEMA_VERSION,
        )
        or attestation.get("kind") != "pmux_gate_a_typescript_stage"
        or attestation.get("stage_directory") != str(expected_directory)
        or not isinstance(manifest, dict)
    ):
        raise CandidateEnvelopeError("TypeScript stage attestation binding is invalid")
    expected_verifier = _typescript_stage_verifier_digest(manifest)
    if attestation.get("verifier_sha256") != expected_verifier:
        raise CandidateEnvelopeError("TypeScript stage verifier binding is invalid")
    _verify_sealed_payload(
        attestation,
        digest_field="typescript_stage_sha256",
        description="TypeScript stage attestation",
        domain=HASH_DOMAINS["typescript_stage"],
    )
    try:
        evidence.verify_regular_tree_manifest(
            expected_directory,
            manifest,
            expected_excluded_paths=frozenset(),
        )
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    return dict(attestation)


def _observe_candidate(
    candidate: Mapping[str, Any],
    metadata_loader: MetadataLoader,
    runtime_identity_loader: RuntimeIdentityLoader = _runtime_identity,
    typescript_stage: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    workspace = pathlib.Path(str(candidate["workspace"]))
    workspace = _require_canonical_workspace(workspace)
    expected_target = workspace / "target"
    if candidate.get("target_directory") != str(expected_target):
        raise CandidateEnvelopeError("candidate target-directory binding is malformed")
    if candidate.get("release_directory") != str(expected_target / "release"):
        raise CandidateEnvelopeError("candidate release-directory binding is malformed")
    validation = pathlib.Path(str(candidate.get("validation_root_directory", "")))
    validation_cargo = validation / "cargo-target"
    if candidate.get("validation_cargo_target_directory") != str(validation_cargo):
        raise CandidateEnvelopeError("candidate validation-target binding is malformed")
    cargo_expected = candidate.get("cargo_layout")
    if not isinstance(cargo_expected, dict):
        raise CandidateEnvelopeError("candidate Cargo layout is malformed")
    target_before = _directory_identity(
        expected_target,
        "Cargo target directory",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_parent_before = _directory_identity(
        validation.parent,
        "Gate A validation parent",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_before = _directory_identity(
        validation,
        "Gate A validation root",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_children_before = {
        name: _directory_identity(
            validation / name,
            f"Gate A validation child {name}",
            reject_group_other_write=True,
            include_nlink=False,
        )
        for name in VALIDATION_CHILD_NAMES
    }
    if (
        target_before != cargo_expected.get("target_identity")
        or validation_parent_before != cargo_expected.get("validation_parent_identity")
        or validation_before != cargo_expected.get("validation_root_identity")
        or validation_children_before
        != cargo_expected.get("validation_child_identities")
    ):
        raise CandidateEnvelopeError("Cargo layout changed after candidate capture")
    runtime_identity = runtime_identity_loader(workspace, validation)
    build_context = candidate.get("build_context")
    if not isinstance(build_context, dict) or (
        runtime_identity.get("toolchain") != build_context.get("toolchain")
        or runtime_identity.get("tool_paths") != build_context.get("tool_paths")
        or runtime_identity.get("selected_build_environment")
        != build_context.get("selected_build_environment")
        or _validate_evidence_authorities(runtime_identity.get("evidence_authorities"))
        != _validate_evidence_authorities(build_context.get("evidence_authorities"))
    ):
        raise CandidateEnvelopeError(
            "toolchain or selected build environment changed after capture"
        )
    stage_before = (
        _verify_typescript_stage(candidate, typescript_stage)
        if typescript_stage is not None
        else None
    )
    source_guard = source_digest.workspace_source_guard(workspace)
    if source_guard != candidate.get("source_guard"):
        raise CandidateEnvelopeError("candidate source identity changed")
    binaries = candidate.get("release_binary_manifest")
    if not isinstance(binaries, dict):
        raise CandidateEnvelopeError("candidate binary manifest is malformed")
    try:
        current_binaries = evidence.verify_release_binary_manifest(binaries)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    target_after = _directory_identity(
        expected_target,
        "Cargo target directory",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_parent_after = _directory_identity(
        validation.parent,
        "Gate A validation parent",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_after = _directory_identity(
        validation,
        "Gate A validation root",
        reject_group_other_write=True,
        include_temporal=True,
    )
    validation_children_after = {
        name: _directory_identity(
            validation / name,
            f"Gate A validation child {name}",
            reject_group_other_write=True,
            include_nlink=False,
        )
        for name in VALIDATION_CHILD_NAMES
    }
    if (
        target_after != target_before
        or validation_parent_after != validation_parent_before
        or validation_after != validation_before
        or validation_children_after != validation_children_before
    ):
        raise CandidateEnvelopeError("Cargo layout changed during checkpoint")
    source_after = source_digest.workspace_source_guard(workspace)
    if source_after != source_guard:
        raise CandidateEnvelopeError("candidate source changed during checkpoint")
    try:
        if evidence.verify_release_binary_manifest(binaries) != current_binaries:
            raise CandidateEnvelopeError("candidate binaries changed during checkpoint")
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    stage_after = (
        _verify_typescript_stage(candidate, typescript_stage)
        if typescript_stage is not None
        else None
    )
    if stage_after != stage_before:
        raise CandidateEnvelopeError("TypeScript stage changed during checkpoint")
    observation = {
        "workspace": str(workspace),
        "target_directory": str(expected_target),
        "release_directory": str(expected_target / "release"),
        "cargo_layout": cargo_expected,
        "release_build_receipt_sha256": candidate["release_build_receipt_sha256"],
        "build_context_sha256": candidate["build_context_sha256"],
        "source_guard_sha256": candidate["source_guard_sha256"],
        "source_manifest_sha256": candidate["source_manifest_sha256"],
        "release_binary_manifest_sha256": candidate["release_binary_manifest_sha256"],
    }
    if stage_after is not None:
        observation["typescript_stage_sha256"] = stage_after["typescript_stage_sha256"]
    return observation


def _checkpoint_filename(ordinal: int, label: str) -> str:
    return f"checkpoint-{ordinal:02d}-{label}.json"


def _expected_observation(
    candidate: Mapping[str, Any],
    typescript_stage: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    observation = {
        "workspace": candidate["workspace"],
        "target_directory": candidate["target_directory"],
        "release_directory": candidate["release_directory"],
        "cargo_layout": candidate["cargo_layout"],
        "release_build_receipt_sha256": candidate["release_build_receipt_sha256"],
        "build_context_sha256": candidate["build_context_sha256"],
        "source_guard_sha256": candidate["source_guard_sha256"],
        "source_manifest_sha256": candidate["source_manifest_sha256"],
        "release_binary_manifest_sha256": candidate["release_binary_manifest_sha256"],
    }
    if typescript_stage is not None:
        observation["typescript_stage_sha256"] = typescript_stage[
            "typescript_stage_sha256"
        ]
    return observation


def _allowed_names(include_final: bool) -> frozenset[str]:
    names = {CANDIDATE_FILE}
    names.update(
        _checkpoint_filename(index, label)
        for index, label in enumerate(CHECKPOINTS, start=1)
    )
    names.update(_phase_report_filename(phase) for phase in PHASES)
    manifest, _digest = _load_phase_manifest()
    for phase in PHASES:
        for ordinal, command in enumerate(manifest["phases"][phase], start=1):
            for stream in ("stdout", "stderr"):
                names.add(_phase_log_filename(phase, ordinal, command["id"], stream))
    if include_final:
        names.add(FINAL_AUDIT_FILE)
    return frozenset(names)


def _reject_unknown_evidence(
    evidence_directory: pathlib.Path, *, allow_final: bool
) -> None:
    allowed = _allowed_names(allow_final)
    actual = {child.name for child in evidence_directory.iterdir()}
    unknown = sorted(actual - allowed)
    if unknown:
        raise CandidateEnvelopeError(
            f"candidate evidence directory contains unknown artifacts: {unknown}"
        )


def _load_phase_report(
    evidence_directory: pathlib.Path,
    candidate: Mapping[str, Any],
    phase: str,
) -> Mapping[str, Any]:
    report = _load_private_json(
        evidence_directory / _phase_report_filename(phase),
        f"phase report {phase}",
    )
    _exact_keys(
        report,
        frozenset(
            {
                "schema_version",
                "kind",
                "phase",
                "candidate_manifest_sha256",
                "phase_manifest_sha256",
                "previous_anchor_sha256",
                "before_observation",
                "commands",
                "after_observation",
                "frozen_outputs",
                "status",
                "phase_report_sha256",
            }
        ),
        f"phase report {phase}",
    )
    commands, manifest_digest = _expanded_phase_commands(candidate, phase)
    frozen_outputs = report.get("frozen_outputs")
    if not isinstance(frozen_outputs, dict):
        raise CandidateEnvelopeError(f"phase report {phase} outputs are malformed")
    _exact_keys(
        frozen_outputs,
        frozenset({"typescript_stage"}),
        f"phase report {phase} frozen outputs",
    )
    raw_typescript_stage = frozen_outputs.get("typescript_stage")
    if not isinstance(raw_typescript_stage, dict):
        raise CandidateEnvelopeError(
            f"phase report {phase} has no TypeScript stage attestation"
        )
    typescript_stage = _verify_typescript_stage(candidate, raw_typescript_stage)
    if phase != "gate_a":
        gate_a_report = _load_phase_report(evidence_directory, candidate, "gate_a")
        if gate_a_report["frozen_outputs"]["typescript_stage"] != typescript_stage:
            raise CandidateEnvelopeError(
                f"phase report {phase} changed the frozen TypeScript stage"
            )
    expected_before_observation = _expected_observation(
        candidate, None if phase == "gate_a" else typescript_stage
    )
    expected_after_observation = _expected_observation(candidate, typescript_stage)
    if (
        not _is_exact_int(
            report.get("schema_version"),
            minimum=SCHEMA_VERSION,
            maximum=SCHEMA_VERSION,
        )
        or report.get("kind") != "pmux_gate_a_candidate_phase"
        or report.get("phase") != phase
        or report.get("candidate_manifest_sha256")
        != candidate["candidate_manifest_sha256"]
        or report.get("phase_manifest_sha256") != manifest_digest
        or report.get("before_observation") != expected_before_observation
        or report.get("after_observation") != expected_after_observation
        or report.get("status") != "PASS"
    ):
        raise CandidateEnvelopeError(f"phase report {phase} binding is invalid")
    command_records = report.get("commands")
    if not isinstance(command_records, list) or len(command_records) != len(commands):
        raise CandidateEnvelopeError(f"phase report {phase} command count is invalid")
    stage_is_frozen = phase != "gate_a"
    for command_ordinal, (expected, actual) in enumerate(
        zip(commands, command_records, strict=True), start=1
    ):
        if not isinstance(actual, dict):
            raise CandidateEnvelopeError(f"phase report {phase} command is malformed")
        _exact_keys(
            actual,
            frozenset(
                {
                    "id",
                    "cwd",
                    "argv",
                    "env",
                    "effective_environment_sha256",
                    "timeout_seconds",
                    "maximum_output_bytes",
                    "stdout_equals",
                    "stdout_sha256_line",
                    "status",
                    "exit_code",
                    "stdout_log",
                    "stdout_size",
                    "stdout_sha256",
                    "stderr_log",
                    "stderr_size",
                    "stderr_sha256",
                    "process_ledger",
                    "process_ledger_sha256",
                    "candidate_observation_sha256",
                }
            ),
            f"phase report command {expected['id']}",
        )
        build_context = candidate["build_context"]
        base_environment = _exact_process_environment(
            {"selected_build_environment": build_context["selected_build_environment"]},
            pathlib.Path(str(candidate["validation_cargo_target_directory"])),
        )
        effective_environment = {**base_environment, **expected["env"]}
        expected_stdout_log = _phase_log_filename(
            phase, command_ordinal, expected["id"], "stdout"
        )
        expected_stderr_log = _phase_log_filename(
            phase, command_ordinal, expected["id"], "stderr"
        )
        stdout = _load_private_bytes(
            evidence_directory / expected_stdout_log,
            f"phase stdout log {expected['id']}",
            expected["maximum_output_bytes"],
        )
        stderr = _load_private_bytes(
            evidence_directory / expected_stderr_log,
            f"phase stderr log {expected['id']}",
            expected["maximum_output_bytes"],
        )
        raw_process_ledger = actual.get("process_ledger")
        if not isinstance(raw_process_ledger, list):
            raise CandidateEnvelopeError(
                f"phase report process ledger is malformed: {expected['id']}"
            )
        process_ledger = _validated_process_ledger(
            raw_process_ledger,
            f"phase command {expected['id']}",
            require_nonempty=True,
        )
        if phase == "gate_a" and expected["id"] == "typescript_stage_verify":
            stage_is_frozen = True
        expected_command_observation = _expected_observation(
            candidate, typescript_stage if stage_is_frozen else None
        )
        if (
            actual.get("id") != expected["id"]
            or actual.get("cwd") != expected["cwd"]
            or actual.get("argv") != expected["argv"]
            or actual.get("env") != expected["env"]
            or actual.get("effective_environment_sha256")
            != evidence.canonical_json_sha256(
                effective_environment,
                domain="pmux.gate-a.command-environment.v1",
            )
            or not _is_exact_int(
                actual.get("timeout_seconds"),
                minimum=expected["timeout_seconds"],
                maximum=expected["timeout_seconds"],
            )
            or not _is_exact_int(
                actual.get("maximum_output_bytes"),
                minimum=expected["maximum_output_bytes"],
                maximum=expected["maximum_output_bytes"],
            )
            or actual.get("stdout_equals") != expected["stdout_equals"]
            or actual.get("stdout_sha256_line") is not expected["stdout_sha256_line"]
            or actual.get("status") != "PASS"
            or type(actual.get("exit_code")) is not int
            or actual.get("exit_code") != 0
            or actual.get("stdout_log") != expected_stdout_log
            or type(actual.get("stdout_size")) is not int
            or actual.get("stdout_size") != len(stdout)
            or actual.get("stdout_sha256") != hashlib.sha256(stdout).hexdigest()
            or actual.get("stderr_log") != expected_stderr_log
            or type(actual.get("stderr_size")) is not int
            or actual.get("stderr_size") != len(stderr)
            or actual.get("stderr_sha256") != hashlib.sha256(stderr).hexdigest()
            or actual.get("process_ledger_sha256")
            != evidence.canonical_json_sha256(
                process_ledger, domain=HASH_DOMAINS["process_ledger"]
            )
            or len(stdout) + len(stderr) > expected["maximum_output_bytes"]
            or actual.get("candidate_observation_sha256")
            != evidence.canonical_json_sha256(
                expected_command_observation, domain=HASH_DOMAINS["observation"]
            )
        ):
            raise CandidateEnvelopeError(
                f"phase report command did not pass exactly: {expected['id']}"
            )
        if expected["stdout_equals"] is not None and stdout != expected[
            "stdout_equals"
        ].encode("utf-8"):
            raise CandidateEnvelopeError(
                f"phase report stdout proof differs: {expected['id']}"
            )
        if (
            expected["stdout_sha256_line"]
            and re.fullmatch(rb"[0-9a-f]{64}\n", stdout) is None
        ):
            raise CandidateEnvelopeError(
                f"phase report stdout digest proof differs: {expected['id']}"
            )
        if expected["id"] == "typescript_stage_verify" and stdout != (
            f"{typescript_stage['verifier_sha256']}\n".encode("ascii")
        ):
            raise CandidateEnvelopeError(
                "phase report TypeScript verifier output differs from its stage"
            )
    _verify_sealed_payload(
        report,
        digest_field="phase_report_sha256",
        description=f"phase report {phase}",
        domain=HASH_DOMAINS["phase_report"],
    )
    return report


def _phase_report_for_after_label(
    evidence_directory: pathlib.Path,
    candidate: Mapping[str, Any],
    label: str,
) -> Mapping[str, Any] | None:
    for phase, after_label in PHASE_AFTER_LABEL.items():
        if label == after_label:
            return _load_phase_report(evidence_directory, candidate, phase)
    return None


def _typescript_stage_for_label(
    evidence_directory: pathlib.Path,
    candidate: Mapping[str, Any],
    label: str,
) -> Mapping[str, Any] | None:
    if label == "gate_a_before":
        return None
    gate_a_report = _load_phase_report(evidence_directory, candidate, "gate_a")
    stage = gate_a_report["frozen_outputs"]["typescript_stage"]
    if not isinstance(stage, dict):
        raise CandidateEnvelopeError("Gate A TypeScript stage binding is malformed")
    return stage


def _load_receipts(
    evidence_directory: pathlib.Path, candidate: Mapping[str, Any]
) -> list[Mapping[str, Any]]:
    receipts: list[Mapping[str, Any]] = []
    missing_seen = False
    previous_receipt_digest: str | None = None
    previous_anchor = candidate["candidate_manifest_sha256"]
    candidate_digest = candidate["candidate_manifest_sha256"]
    for ordinal, label in enumerate(CHECKPOINTS, start=1):
        path = evidence_directory / _checkpoint_filename(ordinal, label)
        if not path.exists():
            missing_seen = True
            continue
        if missing_seen:
            raise CandidateEnvelopeError("candidate checkpoint sequence contains a gap")
        receipt = _load_private_json(path, f"checkpoint {label}")
        _exact_keys(
            receipt,
            frozenset(
                {
                    "schema_version",
                    "kind",
                    "ordinal",
                    "label",
                    "candidate_manifest_sha256",
                    "previous_receipt_sha256",
                    "previous_anchor_sha256",
                    "phase_report_sha256",
                    "observation",
                    "receipt_sha256",
                }
            ),
            f"checkpoint {label}",
        )
        if (
            not _is_exact_int(
                receipt.get("schema_version"),
                minimum=SCHEMA_VERSION,
                maximum=SCHEMA_VERSION,
            )
            or receipt.get("kind") != "pmux_gate_a_candidate_checkpoint"
            or not _is_exact_int(
                receipt.get("ordinal"), minimum=ordinal, maximum=ordinal
            )
            or receipt.get("label") != label
            or receipt.get("candidate_manifest_sha256") != candidate_digest
            or receipt.get("previous_receipt_sha256") != previous_receipt_digest
        ):
            raise CandidateEnvelopeError(f"checkpoint {label} binding is invalid")
        phase_report = _phase_report_for_after_label(
            evidence_directory, candidate, label
        )
        expected_phase_digest = None
        expected_anchor = previous_anchor
        if phase_report is not None:
            if phase_report.get("previous_anchor_sha256") != previous_anchor:
                raise CandidateEnvelopeError(
                    f"phase report before {label} is not externally chained"
                )
            expected_phase_digest = phase_report["phase_report_sha256"]
            expected_anchor = expected_phase_digest
        if (
            receipt.get("phase_report_sha256") != expected_phase_digest
            or receipt.get("previous_anchor_sha256") != expected_anchor
        ):
            raise CandidateEnvelopeError(f"checkpoint {label} anchor is invalid")
        digest = _verify_sealed_payload(
            receipt,
            digest_field="receipt_sha256",
            description=f"checkpoint {label}",
            domain=HASH_DOMAINS["checkpoint"],
        )
        expected_observation = _expected_observation(
            candidate,
            _typescript_stage_for_label(evidence_directory, candidate, label),
        )
        if receipt.get("observation") != expected_observation:
            raise CandidateEnvelopeError(
                f"checkpoint {label} does not bind the exact candidate"
            )
        receipts.append(receipt)
        previous_receipt_digest = digest
        previous_anchor = digest
    for phase, before_label in PHASE_BEFORE_LABEL.items():
        path = evidence_directory / _phase_report_filename(phase)
        try:
            path.lstat()
        except FileNotFoundError:
            continue
        labels = [receipt["label"] for receipt in receipts]
        if before_label not in labels:
            raise CandidateEnvelopeError(
                f"phase report {phase} exists before its opening checkpoint"
            )
        report = _load_phase_report(evidence_directory, candidate, phase)
        before_receipt = receipts[labels.index(before_label)]
        if report.get("previous_anchor_sha256") != before_receipt["receipt_sha256"]:
            raise CandidateEnvelopeError(f"phase report {phase} anchor is invalid")
    return receipts


def record_checkpoint(
    workspace: pathlib.Path,
    evidence_directory: pathlib.Path,
    label: str,
    expected_candidate_sha256: str,
    expected_prior_sha256: str,
    *,
    metadata_loader: MetadataLoader = _run_cargo_metadata,
    runtime_identity_loader: RuntimeIdentityLoader = _runtime_identity,
) -> Mapping[str, Any]:
    if label not in CHECKPOINTS:
        raise CandidateEnvelopeError(f"unknown candidate checkpoint: {label}")
    workspace = _require_canonical_workspace(workspace)
    candidate = _load_candidate(evidence_directory, expected_candidate_sha256)
    if candidate.get("workspace") != str(workspace):
        raise CandidateEnvelopeError("checkpoint workspace differs from candidate")
    _reject_unknown_evidence(evidence_directory, allow_final=False)
    receipts = _load_receipts(evidence_directory, candidate)
    next_index = len(receipts)
    if next_index >= len(CHECKPOINTS) or CHECKPOINTS[next_index] != label:
        expected = CHECKPOINTS[next_index] if next_index < len(CHECKPOINTS) else None
        raise CandidateEnvelopeError(
            f"checkpoint is duplicate or out of order: expected {expected}, got {label}"
        )
    phase_report = _phase_report_for_after_label(evidence_directory, candidate, label)
    prior_anchor = (
        phase_report["phase_report_sha256"]
        if phase_report is not None
        else (
            receipts[-1]["receipt_sha256"]
            if receipts
            else candidate["candidate_manifest_sha256"]
        )
    )
    if (
        _validate_external_anchor(expected_prior_sha256, "expected prior anchor")
        != prior_anchor
    ):
        raise CandidateEnvelopeError(
            "checkpoint differs from the externally carried prior anchor"
        )
    for phase, before_label in PHASE_BEFORE_LABEL.items():
        if label != before_label:
            continue
        try:
            (evidence_directory / _phase_report_filename(phase)).lstat()
        except FileNotFoundError:
            break
        raise CandidateEnvelopeError(
            f"phase report {phase} exists before its opening checkpoint"
        )
    typescript_stage = _typescript_stage_for_label(evidence_directory, candidate, label)
    observation = _observe_candidate(
        candidate,
        metadata_loader,
        runtime_identity_loader,
        typescript_stage,
    )
    candidate_again = _load_candidate(evidence_directory, expected_candidate_sha256)
    if candidate_again != candidate:
        raise CandidateEnvelopeError("candidate manifest changed during checkpoint")
    _reject_unknown_evidence(evidence_directory, allow_final=False)
    ordinal = next_index + 1
    body = {
        "schema_version": SCHEMA_VERSION,
        "kind": "pmux_gate_a_candidate_checkpoint",
        "ordinal": ordinal,
        "label": label,
        "candidate_manifest_sha256": candidate["candidate_manifest_sha256"],
        "previous_receipt_sha256": (
            receipts[-1]["receipt_sha256"] if receipts else None
        ),
        "previous_anchor_sha256": prior_anchor,
        "phase_report_sha256": (
            phase_report["phase_report_sha256"] if phase_report is not None else None
        ),
        "observation": observation,
    }
    receipt = _sealed_payload(
        body,
        digest_field="receipt_sha256",
        domain=HASH_DOMAINS["checkpoint"],
    )
    path = evidence_directory / _checkpoint_filename(ordinal, label)
    try:
        evidence.atomic_write_json(path, receipt)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    loaded = _load_receipts(evidence_directory, candidate)
    if len(loaded) != ordinal or loaded[-1] != receipt:
        raise CandidateEnvelopeError("published checkpoint chain changed")
    return receipt


def run_phase(
    workspace: pathlib.Path,
    evidence_directory: pathlib.Path,
    phase: str,
    expected_candidate_sha256: str,
    expected_prior_sha256: str,
    *,
    metadata_loader: MetadataLoader = _run_cargo_metadata,
    runtime_identity_loader: RuntimeIdentityLoader = _runtime_identity,
    command_runner: CommandRunner = _run_phase_command,
) -> Mapping[str, Any]:
    if phase not in PHASES:
        raise CandidateEnvelopeError(f"unknown candidate phase: {phase}")
    workspace = _require_canonical_workspace(workspace)
    candidate = _load_candidate(evidence_directory, expected_candidate_sha256)
    if candidate.get("workspace") != str(workspace):
        raise CandidateEnvelopeError("phase workspace differs from candidate")
    _reject_unknown_evidence(evidence_directory, allow_final=False)
    receipts = _load_receipts(evidence_directory, candidate)
    expected_before = PHASE_BEFORE_LABEL[phase]
    if not receipts or receipts[-1].get("label") != expected_before:
        raise CandidateEnvelopeError(
            f"phase {phase} requires the exact {expected_before} checkpoint"
        )
    prior_anchor = receipts[-1]["receipt_sha256"]
    if (
        _validate_external_anchor(expected_prior_sha256, "expected prior anchor")
        != prior_anchor
    ):
        raise CandidateEnvelopeError(
            "phase differs from the externally carried prior anchor"
        )
    report_path = evidence_directory / _phase_report_filename(phase)
    try:
        report_path.lstat()
    except FileNotFoundError:
        pass
    else:
        raise CandidateEnvelopeError(f"refusing to replace phase report {phase}")

    commands, manifest_digest = _expanded_phase_commands(candidate, phase)
    typescript_stage = (
        None
        if phase == "gate_a"
        else _typescript_stage_for_label(evidence_directory, candidate, expected_before)
    )
    expected_before_observation = _expected_observation(candidate, typescript_stage)
    before_observation = _observe_candidate(
        candidate,
        metadata_loader,
        runtime_identity_loader,
        typescript_stage,
    )
    if before_observation != expected_before_observation:
        raise CandidateEnvelopeError(f"phase {phase} opened on another candidate")
    command_records: list[dict[str, Any]] = []
    build_context = candidate["build_context"]
    base_environment = _exact_process_environment(
        {"selected_build_environment": build_context["selected_build_environment"]},
        pathlib.Path(str(candidate["validation_cargo_target_directory"])),
    )
    for command_ordinal, command in enumerate(commands, start=1):
        pre_command = _observe_candidate(
            candidate,
            metadata_loader,
            runtime_identity_loader,
            typescript_stage,
        )
        if pre_command != _expected_observation(candidate, typescript_stage):
            raise CandidateEnvelopeError(
                f"candidate changed before phase command {command['id']}"
            )
        effective_environment = {**base_environment, **command["env"]}
        execution = command_runner(
            pathlib.Path(command["cwd"]),
            command["argv"],
            effective_environment,
            command["timeout_seconds"],
            command["maximum_output_bytes"],
        )
        if not isinstance(execution, CommandExecution):
            raise CandidateEnvelopeError(
                f"phase command runner returned an invalid result: {command['id']}"
            )
        if type(execution.exit_code) is not int or execution.exit_code != 0:
            raise CandidateEnvelopeError(
                f"phase command failed: {command['id']} status={execution.exit_code}"
            )
        if (
            not isinstance(execution.stdout, bytes)
            or not isinstance(execution.stderr, bytes)
            or len(execution.stdout) + len(execution.stderr)
            > command["maximum_output_bytes"]
        ):
            raise CandidateEnvelopeError(
                f"phase command output exceeded its bound: {command['id']}"
            )
        process_ledger = _validated_process_ledger(
            execution.process_ledger,
            f"phase command {command['id']}",
            require_nonempty=True,
        )
        expected_stdout = command["stdout_equals"]
        if expected_stdout is not None and execution.stdout != expected_stdout.encode(
            "utf-8"
        ):
            raise CandidateEnvelopeError(
                f"phase command stdout was not exact: {command['id']}"
            )
        if (
            command["stdout_sha256_line"]
            and re.fullmatch(rb"[0-9a-f]{64}\n", execution.stdout) is None
        ):
            raise CandidateEnvelopeError(
                f"phase command stdout was not one digest line: {command['id']}"
            )
        if command["id"] == "typescript_stage_verify":
            if phase != "gate_a" or typescript_stage is not None:
                raise CandidateEnvelopeError(
                    "TypeScript stage verifier appeared outside its freeze point"
                )
            typescript_stage = _capture_typescript_stage(candidate, execution.stdout)
        stdout_log = _phase_log_filename(
            phase, command_ordinal, command["id"], "stdout"
        )
        stderr_log = _phase_log_filename(
            phase, command_ordinal, command["id"], "stderr"
        )
        try:
            evidence.atomic_write_bytes(
                evidence_directory / stdout_log, execution.stdout
            )
            evidence.atomic_write_bytes(
                evidence_directory / stderr_log, execution.stderr
            )
        except evidence.EvidenceError as error:
            raise CandidateEnvelopeError(str(error)) from error
        post_command = _observe_candidate(
            candidate,
            metadata_loader,
            runtime_identity_loader,
            typescript_stage,
        )
        if post_command != _expected_observation(candidate, typescript_stage):
            raise CandidateEnvelopeError(
                f"candidate changed after phase command {command['id']}"
            )
        _reject_unknown_evidence(evidence_directory, allow_final=False)
        command_records.append(
            {
                "id": command["id"],
                "cwd": command["cwd"],
                "argv": command["argv"],
                "env": command["env"],
                "effective_environment_sha256": evidence.canonical_json_sha256(
                    effective_environment,
                    domain="pmux.gate-a.command-environment.v1",
                ),
                "timeout_seconds": command["timeout_seconds"],
                "maximum_output_bytes": command["maximum_output_bytes"],
                "stdout_equals": command["stdout_equals"],
                "stdout_sha256_line": command["stdout_sha256_line"],
                "status": "PASS",
                "exit_code": 0,
                "stdout_log": stdout_log,
                "stdout_size": len(execution.stdout),
                "stdout_sha256": hashlib.sha256(execution.stdout).hexdigest(),
                "stderr_log": stderr_log,
                "stderr_size": len(execution.stderr),
                "stderr_sha256": hashlib.sha256(execution.stderr).hexdigest(),
                "process_ledger": process_ledger,
                "process_ledger_sha256": evidence.canonical_json_sha256(
                    process_ledger, domain=HASH_DOMAINS["process_ledger"]
                ),
                "candidate_observation_sha256": evidence.canonical_json_sha256(
                    post_command, domain=HASH_DOMAINS["observation"]
                ),
            }
        )
    if typescript_stage is None:
        raise CandidateEnvelopeError(
            f"phase {phase} did not bind the TypeScript validation stage"
        )
    after_observation = _observe_candidate(
        candidate,
        metadata_loader,
        runtime_identity_loader,
        typescript_stage,
    )
    if after_observation != _expected_observation(candidate, typescript_stage):
        raise CandidateEnvelopeError(f"phase {phase} closed on another candidate")
    body = {
        "schema_version": SCHEMA_VERSION,
        "kind": "pmux_gate_a_candidate_phase",
        "phase": phase,
        "candidate_manifest_sha256": candidate["candidate_manifest_sha256"],
        "phase_manifest_sha256": manifest_digest,
        "previous_anchor_sha256": prior_anchor,
        "before_observation": before_observation,
        "commands": command_records,
        "after_observation": after_observation,
        "frozen_outputs": {"typescript_stage": typescript_stage},
        "status": "PASS",
    }
    report = _sealed_payload(
        body,
        digest_field="phase_report_sha256",
        domain=HASH_DOMAINS["phase_report"],
    )
    try:
        evidence.atomic_write_json(report_path, report)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    loaded = _load_phase_report(evidence_directory, candidate, phase)
    if loaded != report:
        raise CandidateEnvelopeError(f"published phase report changed: {phase}")
    _load_receipts(evidence_directory, candidate)
    return loaded


def _load_final_audit(
    evidence_directory: pathlib.Path,
    candidate: Mapping[str, Any],
    receipts: Sequence[Mapping[str, Any]],
) -> Mapping[str, Any]:
    audit = _load_private_json(
        evidence_directory / FINAL_AUDIT_FILE, "candidate final audit"
    )
    _exact_keys(
        audit,
        frozenset(
            {
                "schema_version",
                "kind",
                "candidate_manifest_sha256",
                "previous_anchor_sha256",
                "checkpoint_receipts",
                "phase_reports",
                "observation",
                "verdict",
                "final_audit_sha256",
            }
        ),
        "candidate final audit",
    )
    if (
        not _is_exact_int(
            audit.get("schema_version"),
            minimum=SCHEMA_VERSION,
            maximum=SCHEMA_VERSION,
        )
        or audit.get("kind") != "pmux_gate_a_candidate_final_audit"
        or audit.get("candidate_manifest_sha256")
        != candidate["candidate_manifest_sha256"]
        or audit.get("previous_anchor_sha256") != receipts[-1]["receipt_sha256"]
        or audit.get("verdict") != "verified"
    ):
        raise CandidateEnvelopeError("candidate final audit binding is invalid")
    expected_receipts = [
        {
            "ordinal": receipt["ordinal"],
            "label": receipt["label"],
            "receipt_sha256": receipt["receipt_sha256"],
        }
        for receipt in receipts
    ]
    if audit.get("checkpoint_receipts") != expected_receipts:
        raise CandidateEnvelopeError("candidate final audit checkpoint chain differs")
    expected_phases = [
        {
            "phase": phase,
            "phase_report_sha256": _load_phase_report(
                evidence_directory, candidate, phase
            )["phase_report_sha256"],
        }
        for phase in PHASES
    ]
    if audit.get("phase_reports") != expected_phases:
        raise CandidateEnvelopeError("candidate final audit phase reports differ")
    expected_observation = receipts[-1]["observation"]
    if audit.get("observation") != expected_observation:
        raise CandidateEnvelopeError("candidate final audit observation differs")
    _verify_sealed_payload(
        audit,
        digest_field="final_audit_sha256",
        description="candidate final audit",
        domain=HASH_DOMAINS["final_audit"],
    )
    return audit


def audit_candidate(
    workspace: pathlib.Path,
    evidence_directory: pathlib.Path,
    expected_candidate_sha256: str,
    expected_prior_sha256: str,
    *,
    metadata_loader: MetadataLoader = _run_cargo_metadata,
    runtime_identity_loader: RuntimeIdentityLoader = _runtime_identity,
) -> Mapping[str, Any]:
    workspace = _require_canonical_workspace(workspace)
    candidate = _load_candidate(evidence_directory, expected_candidate_sha256)
    if candidate.get("workspace") != str(workspace):
        raise CandidateEnvelopeError("audit workspace differs from candidate")
    _reject_unknown_evidence(evidence_directory, allow_final=True)
    if (evidence_directory / FINAL_AUDIT_FILE).exists():
        raise CandidateEnvelopeError("refusing to replace an existing final audit")
    receipts = _load_receipts(evidence_directory, candidate)
    if len(receipts) != len(CHECKPOINTS):
        raise CandidateEnvelopeError("candidate checkpoint sequence is incomplete")
    prior_anchor = receipts[-1]["receipt_sha256"]
    if (
        _validate_external_anchor(expected_prior_sha256, "expected prior anchor")
        != prior_anchor
    ):
        raise CandidateEnvelopeError(
            "final audit differs from the externally carried prior anchor"
        )
    typescript_stage = _typescript_stage_for_label(
        evidence_directory, candidate, CHECKPOINTS[-1]
    )
    observation = _observe_candidate(
        candidate,
        metadata_loader,
        runtime_identity_loader,
        typescript_stage,
    )
    candidate_again = _load_candidate(evidence_directory, expected_candidate_sha256)
    if candidate_again != candidate:
        raise CandidateEnvelopeError("candidate manifest changed during final audit")
    receipts_again = _load_receipts(evidence_directory, candidate)
    if receipts_again != receipts:
        raise CandidateEnvelopeError("checkpoint chain changed during final audit")
    body = {
        "schema_version": SCHEMA_VERSION,
        "kind": "pmux_gate_a_candidate_final_audit",
        "candidate_manifest_sha256": candidate["candidate_manifest_sha256"],
        "previous_anchor_sha256": prior_anchor,
        "checkpoint_receipts": [
            {
                "ordinal": receipt["ordinal"],
                "label": receipt["label"],
                "receipt_sha256": receipt["receipt_sha256"],
            }
            for receipt in receipts
        ],
        "phase_reports": [
            {
                "phase": phase,
                "phase_report_sha256": _load_phase_report(
                    evidence_directory, candidate, phase
                )["phase_report_sha256"],
            }
            for phase in PHASES
        ],
        "observation": observation,
        "verdict": "verified",
    }
    audit = _sealed_payload(
        body,
        digest_field="final_audit_sha256",
        domain=HASH_DOMAINS["final_audit"],
    )
    try:
        evidence.atomic_write_json(evidence_directory / FINAL_AUDIT_FILE, audit)
    except evidence.EvidenceError as error:
        raise CandidateEnvelopeError(str(error)) from error
    loaded = _load_final_audit(evidence_directory, candidate, receipts)
    if loaded != audit:
        raise CandidateEnvelopeError("published final audit changed")
    _reject_unknown_evidence(evidence_directory, allow_final=True)
    return loaded


def verify_final_candidate(
    workspace: pathlib.Path,
    evidence_directory: pathlib.Path,
    expected_candidate_sha256: str,
    expected_final_audit_sha256: str,
    *,
    metadata_loader: MetadataLoader = _run_cargo_metadata,
    runtime_identity_loader: RuntimeIdentityLoader = _runtime_identity,
) -> Mapping[str, Any]:
    workspace = _require_canonical_workspace(workspace)
    candidate = _load_candidate(evidence_directory, expected_candidate_sha256)
    if candidate.get("workspace") != str(workspace):
        raise CandidateEnvelopeError("verification workspace differs from candidate")
    _reject_unknown_evidence(evidence_directory, allow_final=True)
    receipts = _load_receipts(evidence_directory, candidate)
    if len(receipts) != len(CHECKPOINTS):
        raise CandidateEnvelopeError("candidate checkpoint sequence is incomplete")
    audit = _load_final_audit(evidence_directory, candidate, receipts)
    if (
        _validate_external_anchor(
            expected_final_audit_sha256, "expected final-audit anchor"
        )
        != audit["final_audit_sha256"]
    ):
        raise CandidateEnvelopeError(
            "final audit differs from the externally carried final anchor"
        )
    typescript_stage = _typescript_stage_for_label(
        evidence_directory, candidate, CHECKPOINTS[-1]
    )
    observation = _observe_candidate(
        candidate,
        metadata_loader,
        runtime_identity_loader,
        typescript_stage,
    )
    if audit.get("observation") != observation:
        raise CandidateEnvelopeError("final candidate observation changed")
    candidate_again = _load_candidate(evidence_directory, expected_candidate_sha256)
    receipts_again = _load_receipts(evidence_directory, candidate_again)
    audit_again = _load_final_audit(evidence_directory, candidate_again, receipts_again)
    if (
        candidate_again != candidate
        or receipts_again != receipts
        or audit_again != audit
    ):
        raise CandidateEnvelopeError("candidate evidence changed during verification")
    return audit


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Capture and reverify one exact local Gate A candidate"
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    capture = subparsers.add_parser("build-capture")
    capture.add_argument("--workspace", type=pathlib.Path, required=True)
    capture.add_argument("--evidence-dir", type=pathlib.Path, required=True)
    capture.add_argument("--validation-root", type=pathlib.Path, required=True)
    for command in ("audit", "verify"):
        operation = subparsers.add_parser(command)
        operation.add_argument("--workspace", type=pathlib.Path, required=True)
        operation.add_argument("--evidence-dir", type=pathlib.Path, required=True)
        operation.add_argument("--expected-candidate-sha256", required=True)
    audit_parser = subparsers.choices["audit"]
    audit_parser.add_argument("--expected-prior-sha256", required=True)
    verify_parser = subparsers.choices["verify"]
    verify_parser.add_argument("--expected-final-audit-sha256", required=True)
    checkpoint = subparsers.add_parser("checkpoint")
    checkpoint.add_argument("--workspace", type=pathlib.Path, required=True)
    checkpoint.add_argument("--evidence-dir", type=pathlib.Path, required=True)
    checkpoint.add_argument("--label", choices=CHECKPOINTS, required=True)
    checkpoint.add_argument("--expected-candidate-sha256", required=True)
    checkpoint.add_argument("--expected-prior-sha256", required=True)
    phase = subparsers.add_parser("run-phase")
    phase.add_argument("--workspace", type=pathlib.Path, required=True)
    phase.add_argument("--evidence-dir", type=pathlib.Path, required=True)
    phase.add_argument("--phase", choices=PHASES, required=True)
    phase.add_argument("--expected-candidate-sha256", required=True)
    phase.add_argument("--expected-prior-sha256", required=True)
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    options = _parser().parse_args(arguments)
    try:
        if options.command == "build-capture":
            result = build_and_capture_candidate(
                options.workspace,
                options.evidence_dir,
                options.validation_root,
                metadata_loader=_run_cargo_metadata,
                build_runner=_run_release_build,
                runtime_identity_loader=_runtime_identity,
            )
        elif options.command == "checkpoint":
            result = record_checkpoint(
                options.workspace,
                options.evidence_dir,
                options.label,
                options.expected_candidate_sha256,
                options.expected_prior_sha256,
            )
        elif options.command == "run-phase":
            result = run_phase(
                options.workspace,
                options.evidence_dir,
                options.phase,
                options.expected_candidate_sha256,
                options.expected_prior_sha256,
            )
        elif options.command == "audit":
            result = audit_candidate(
                options.workspace,
                options.evidence_dir,
                options.expected_candidate_sha256,
                options.expected_prior_sha256,
            )
        else:
            result = verify_final_candidate(
                options.workspace,
                options.evidence_dir,
                options.expected_candidate_sha256,
                options.expected_final_audit_sha256,
            )
    except (CandidateEnvelopeError, source_digest.SourceIdentityError) as error:
        print(f"gate-a-candidate: {error}", file=sys.stderr)
        return 1
    digest_field = {
        "build-capture": "candidate_manifest_sha256",
        "checkpoint": "receipt_sha256",
        "run-phase": "phase_report_sha256",
        "audit": "final_audit_sha256",
        "verify": "final_audit_sha256",
    }[options.command]
    print(result[digest_field])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
