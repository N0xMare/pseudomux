#!/usr/bin/env python3
"""Minimal Gate A driver: run every manifest cell, record every outcome.

A recorder, not a gate. It runs the ordered phase manifest, bounds each cell in
time and output, and emits one JSON receipt of exactly what happened. A failing
cell is recorded and the run continues: a failing Gate A number is far more
informative than another missing one.

Nonclaims (decision D9, docs/testing.md:32-53): no claim against a malicious
same-UID actor. No substitution detection, no marker vnode, no descendant-escape
supervision. The claims are bounded output, bounded wall clock, exact argv,
exact per-cell environment, and honest exit codes.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fnmatch
import hashlib
import json
import os
import pathlib
import platform
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time
from typing import Any

SCHEMA_VERSION = 1
SOURCE_ALGORITHM = "pmux-gate-a-source-v1-path-mode-size-content-sha256"
EXCERPT_BYTES = 4096
READ_CHUNK_BYTES = 65536
DEFAULT_MAX_OUTPUT_BYTES = 16 * 1024 * 1024
DEFAULT_CELL_TIMEOUT_SECONDS = 3600.0
TERMINATE_GRACE_SECONDS = 5.0
VERSION_PROBE_TIMEOUT_SECONDS = 30
# Local digest by choice: importing tools/linux-docker/source_digest.py drags in
# the host-Git apparatus that decision D6 de-scopes (and exec-loads
# bounded_process.py at import time), and advisory row 23 relocates that lane.
#
# THE SOURCE SET IS DERIVED, and the two lists it replaced are why. It was
# `SOURCE_ROOT_FILES` -- four root files -- plus `SOURCE_ROOT_DIRS` -- nine
# directories -- and it therefore hashed 951 files: 10 of them gitignored
# `.DS_Store` that Finder rewrites when anybody browses a directory, voiding a
# receipt for a reason that is not source at all, and none of the 12 tracked
# files outside the nine, which are `evidence/` (8 files, one of them the
# model-attempt ledger `gate_f/gate_driver_self_tests` reads), `LICENSE-APACHE`,
# `LICENSE-MIT`, `.gitignore` and `.dockerignore`. A digest that omits committed
# evidence and includes untracked noise is not the integrity claim its name
# makes.
#
# What it is derived FROM is `.gitignore`: the repository's own committed
# declaration of what is not source. Not `git ls-files`, because this driver is
# platform-neutral by construction and `docs/gate-c-linux-handoff.md` documents
# running it against a `git archive` export, which has no repository at all.
# `.gitignore` is a tracked file and survives that export.
GITIGNORE = ".gitignore"
# The one thing `.gitignore` cannot declare, because it is the machinery that
# reads it. Everything else that is not source says so in the file.
SOURCE_SKIP = ".git".split()
# docs/testing.md:383-390 makes the validation root and these three children a
# precondition, and the consumers check it: `dist-stage.mjs` refuses a stage
# whose root is not 0700, and the E2E harness asserts
# `PMUX_E2E_TYPESCRIPT_DIST_DIR must be owner-private`.
#
# This is a FLOOR, not the set. The set is DERIVED from the manifest by
# `validation_children` below, because a hand-written list here was narrower
# than its own docstring: `prepare_validation_root` says it creates "the
# documented validation tree owner-private, or refuse", and these three omit
# `cargo-target`, which twenty-one `gate_a` vendor cells write into through
# `CARGO_TARGET_DIR` and which `docs/testing.md:391-396` documents in the same
# breath as the other three. An operator who pre-created that one under an
# ordinary umask got no refusal at all -- exactly the case the docstring says
# it exists to catch, in the one child it never looked at. Keeping the three as
# a floor means a derivation that silently stops matching REFUSES rather than
# narrowing the guarantee back to nothing.
VALIDATION_CHILDREN = "fuzz fuzz-evidence typescript-dist".split()
VALIDATION_PLACEHOLDER = "{validation}"
OWNER_ONLY_DIRECTORY = 0o700
ENVIRONMENT_ALLOWLIST = """CARGO_HOME HOME LANG LC_ALL LOGNAME PATH RUSTUP_HOME SHELL
    SSL_CERT_FILE TMPDIR USER""".split()
TOOL_EXECUTABLES = {
    "bash": "bash", "cargo": "cargo", "cargo_fuzz": "cargo-fuzz", "node": "node",
    "cargo_mutants": "cargo-mutants", "python": sys.executable,
    "rustfmt": "rustfmt", "shellcheck": "shellcheck",
}  # fmt: skip
# Tools the gate installs BESIDE the workspace rather than onto PATH, checked
# before PATH. Each is version-pinned by the gate itself -- `cargo_fuzz_version`
# asserts the exact string `cargo-fuzz 0.13.2` and `cargo_mutants_version`
# asserts `cargo-mutants 27.1.0`, and `scripts/gate-a-fuzz.sh` and
# `scripts/gate-a-mutants.sh` each refuse anything else -- so they are installed
# under the workspace and never taken from whatever a host happens to carry.
#
# THE PATH IS DERIVED, NOT WRITTEN OUT, and that is the whole point of
# `workspace_tool_path`. This driver was once the THIRD reader of a
# hand-written `.context/tools/cargo-fuzz/bin/cargo-fuzz` and the only one that
# did not know it, so `--phase gate_b` aborted with
# `placeholder {cargo_fuzz} is unresolved` on a host that had the pinned binary
# sitting in the workspace. Adding a second pinned tool would have made four
# readers of two literals; instead every reader now computes the same one-line
# rule from the tool's own name.
# `tools/gate-a/tests/test_run_gate.py::every_reader_of_the_workspace_tool_root_derives_the_same_path`
# scans the repository and refuses any reader that spells it a different way.
WORKSPACE_TOOLS_ROOT = ".context/tools"


def workspace_tool_root(name: str) -> str:
    """What `cargo install --root` is given for one pinned tool.

    A SECOND derived spelling, and it has to exist: `docs/testing.md` documents
    the install commands that produce these binaries, and `cargo install --root`
    takes the directory whose `bin/` the binary lands in -- the parent of
    `workspace_tool_path`. That is a real spelling of the pinned-tool path, so it
    is derived here rather than written out there. The scan in
    `test_run_gate.py` admits exactly the two forms these two functions produce
    and still refuses every other, which is the point: the install command and
    the lookup cannot drift apart, because one is the other's prefix by
    construction.
    """

    return f"{WORKSPACE_TOOLS_ROOT}/{name.replace('_', '-')}"


def workspace_tool_path(name: str) -> str:
    """Where the gate installs one pinned tool, relative to the workspace."""

    binary = name.replace("_", "-")
    return f"{workspace_tool_root(name)}/bin/{binary}"


WORKSPACE_TOOLS = {
    name: workspace_tool_path(name) for name in "cargo_fuzz cargo_mutants".split()
}
# Why this receipt spells absolute paths where `evidence/`'s receipts spell
# `<REPO>` (`tools/evidence_common/portable_paths.py`), stated IN the artefact
# because a reader who finds one of each is owed the difference. This one is not
# descriptive: `scripts/path_b_done.py` opens `artefacts[].path` out of the
# pinned run that wraps this receipt, re-hashes it, and then compares THIS
# receipt's `workspace` against that runner's `worktree`. The two are written by
# two processes whose checkouts differ -- the driver runs inside the pinned
# worktree, the runner beside it -- so each would render its own `<REPO>` and
# the two spellings of one directory would stop comparing equal.
ABSOLUTE_PATHS_ARE_THE_CONTRACT = (
    "absolute on purpose: scripts/path_b_done.py re-opens the artefacts this "
    "receipt names and compares this workspace with the pinned runner's "
    "worktree, which is written by a different process in a different checkout"
)
RECEIPT_SCHEMA = {
    int: "schema_version wall_ms",
    str: "driver started_at completed_at workspace validation_root "
    "paths_are_absolute_because",
    list: "phases environment_base_keys cells",
    dict: "manifest host tools release summary "
    "source_digest_before source_digest_after",
    bool: "source_unchanged",
}
CELL_SCHEMA = {
    int: "index wall_ms",
    str: "id phase cwd started_at",
    list: "argv assertions failures",
    dict: "env exit_status stdout stderr",
    float: "timeout_seconds",
    bool: "passed",
}
STREAM_SCHEMA = {int: "bytes", str: "sha256 head tail", bool: "over_limit"}
FAULT_SEPARATOR = "; "


class GateDriverError(RuntimeError):
    """A malformed input or an unresolvable declaration. Always fatal."""


class UnresolvedPlaceholder(GateDriverError):
    """A tool placeholder with no override, no workspace copy, none on PATH.

    Its own class because it is the one fatal fault an operator can fix by
    passing another `--tool`, so every instance is collected and reported
    together rather than one per run.
    """


def _timestamp() -> str:
    now = dt.datetime.now(dt.timezone.utc)
    return now.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _file_sha256(path: pathlib.Path) -> tuple[str, int]:
    digest, size = hashlib.sha256(), 0
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def ignore_rules(root: pathlib.Path) -> list[tuple[str, bool, bool]]:
    """`.gitignore`, parsed into `(pattern, anchored, directory_only)`.

    Deliberately small, and it REFUSES what it does not implement rather than
    quietly not matching it. A digest that under-excludes hashes build output
    and reports a source change that is not one; a digest that silently
    mis-parses a re-inclusion would over-exclude and miss a real one. Both are
    receipts that say something the run did not establish, so an unsupported
    pattern is an error and whoever adds one comes here.
    """

    path = root / GITIGNORE
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise GateDriverError(
            f"the source declaration {GITIGNORE} is unreadable: {error}"
        )
    rules = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("!") or set("[]?") & set(line):
            raise GateDriverError(f"{GITIGNORE} pattern {line!r} is not supported here")
        directory_only = line.endswith("/")
        line = line.rstrip("/")
        if line.startswith("**/"):
            line = line[3:]
        anchored = "/" in line
        rules.append((line.lstrip("/"), anchored, directory_only))
    if not rules:
        raise GateDriverError(f"{GITIGNORE} declared nothing; the derivation is broken")
    return rules


def is_ignored(relative: str, is_directory: bool, rules) -> bool:
    """Whether `.gitignore` excludes this path, relative to the workspace."""

    for pattern, anchored, directory_only in rules:
        if directory_only and not is_directory:
            continue
        if anchored:
            if fnmatch.fnmatch(relative, pattern) or relative.startswith(pattern + "/"):
                return True
        elif any(fnmatch.fnmatch(part, pattern) for part in relative.split("/")):
            return True
    return False


def source_files(workspace: pathlib.Path) -> list[pathlib.Path]:
    """Every file the workspace declares to be source, DERIVED by subtraction."""

    root = workspace.resolve(strict=True)
    rules = ignore_rules(root)
    paths: list[pathlib.Path] = []
    for current, directories, files in os.walk(root):
        here = pathlib.Path(current)
        directories[:] = [
            d
            for d in directories
            if d not in SOURCE_SKIP
            and not is_ignored(str((here / d).relative_to(root)), True, rules)
        ]
        # `.git` is skipped by NAME and not only as a directory: in a worktree
        # it is a one-line file naming the real git dir, and pruning directories
        # alone hashed it -- a file whose content is an absolute path into
        # somebody's checkout, in a digest whose whole purpose is to be the same
        # number on two hosts.
        paths.extend(
            here / entry
            for entry in files
            if entry not in SOURCE_SKIP
            and not is_ignored(str((here / entry).relative_to(root)), False, rules)
        )
    return paths


def source_digest(workspace: pathlib.Path) -> dict[str, Any]:
    """Hash the canonical source tree: path, mode, size, content, sorted."""

    root = workspace.resolve(strict=True)
    paths = source_files(root)
    aggregate = hashlib.sha256(SOURCE_ALGORITHM.encode("utf-8") + b"\0")
    count = 0
    for path in sorted(paths):
        try:
            metadata = path.lstat()
        except OSError:
            continue
        if not stat.S_ISREG(metadata.st_mode) or path.name.endswith(".pyc"):
            continue
        content, size = _file_sha256(path)
        relative = str(path.relative_to(root)).encode("utf-8")
        aggregate.update(len(relative).to_bytes(4, "big") + relative)
        aggregate.update(stat.S_IMODE(metadata.st_mode).to_bytes(4, "big"))
        aggregate.update(size.to_bytes(8, "big") + bytes.fromhex(content))
        count += 1
    return {"algorithm": SOURCE_ALGORITHM, "sha256": aggregate.hexdigest(),
            "file_count": count}  # fmt: skip


def load_manifest(payload: bytes, where: str) -> dict[str, Any]:
    """Parse and structurally validate a phase manifest. Never lenient."""

    try:
        manifest = json.loads(payload)
    except ValueError as error:
        raise GateDriverError(f"{where} is not valid JSON: {error}") from error
    if not isinstance(manifest, dict) or not isinstance(manifest.get("phases"), dict):
        raise GateDriverError(f"{where} has no phases object")
    if not manifest["phases"]:
        raise GateDriverError(f"{where} declares no phase")
    for phase, cells in manifest["phases"].items():
        if not isinstance(cells, list) or not cells:
            raise GateDriverError(f"{where}: phase {phase!r} is not a non-empty list")
        for cell in cells:
            fault = _cell_fault(cell)
            if fault is not None:
                raise GateDriverError(f"{where}: phase {phase!r} cell {fault}")
    return manifest


def _cell_fault(cell: Any) -> str | None:
    if not isinstance(cell, dict):
        return "is not an object"
    for key, kind in (("id", str), ("cwd", str), ("argv", list), ("env", dict)):
        if not isinstance(cell.get(key), kind):
            return f"{cell.get('id')!r} has a malformed {key!r}"
    if "stdout_equals" not in cell:
        return f"{cell['id']!r} has no stdout_equals"
    if not cell["argv"] or not all(isinstance(item, str) for item in cell["argv"]):
        return f"{cell['id']!r} has a malformed argv"
    if not all(isinstance(x, str) for pair in cell["env"].items() for x in pair):
        return f"{cell['id']!r} has a malformed env"
    return None


class Replacements(dict):
    """Placeholder table. Tool placeholders resolve on first reference."""

    def __init__(self, base: dict[str, str], overrides: dict[str, str]) -> None:
        super().__init__(base)
        self.overrides = overrides
        self.tools: dict[str, str] = {}

    def __missing__(self, name: str) -> str:
        candidate = self.overrides.get(name)
        if candidate is None and name == "nightly_bin":
            candidate = str(pathlib.Path(self["nightly_cargo"]).parent)
        if candidate is None and name in WORKSPACE_TOOLS and "workspace" in self:
            beside = pathlib.Path(self["workspace"]) / WORKSPACE_TOOLS[name]
            if beside.is_file() and os.access(beside, os.X_OK):
                candidate = str(beside)
        if candidate is None and name in TOOL_EXECUTABLES:
            candidate = shutil.which(TOOL_EXECUTABLES[name])
        if candidate is None:
            raise UnresolvedPlaceholder(
                f"placeholder {{{name}}} is unresolved; pass --tool {name}=<path>"
            )
        # Resolve the *parent* directory but keep the final component as named.
        # `~/.cargo/bin/cargo` is a symlink to the rustup shim, which dispatches
        # on argv[0]; fully resolving it yields `.../bin/rustup` and every cargo
        # cell then fails with "unexpected argument". Absolutise without
        # collapsing the name the tool must be invoked under.
        path = pathlib.Path(candidate)
        if not path.is_absolute():
            path = pathlib.Path(shutil.which(str(path)) or path)
        resolved_path = path.parent.resolve(strict=True) / path.name
        if not os.access(resolved_path, os.X_OK):
            raise GateDriverError(f"tool {name} is not executable: {resolved_path}")
        resolved = str(resolved_path)
        self[name] = self.tools[name] = resolved
        return resolved


def expand(value: str, replacements: Replacements, where: str) -> str:
    """Substitute every {placeholder}. Unknown or unbalanced is fatal.

    Scanning continues past an unresolvable name so that a value naming three
    missing tools reports three, not the first.
    """

    parts, rest, unresolved = [], value, []
    while "{" in rest:
        prefix, _, tail = rest.partition("{")
        name, closed, rest = tail.partition("}")
        if not closed or not name or "}" in prefix:
            raise GateDriverError(f"{where}: unbalanced placeholder in {value!r}")
        try:
            parts += [prefix, replacements[name]]
        except UnresolvedPlaceholder as error:
            unresolved.append(str(error))
            parts.append(prefix)
    if "}" in rest:
        raise GateDriverError(f"{where}: unbalanced placeholder in {value!r}")
    if unresolved:
        raise UnresolvedPlaceholder(FAULT_SEPARATOR.join(unresolved))
    return "".join(parts) + rest


def expand_cell(cell: dict, phase: str, index: int, table: Replacements) -> dict:
    """Expand one cell, naming EVERY placeholder in it that will not resolve."""

    where = f"{phase}[{index}] {cell['id']}"
    unresolved: list[str] = []

    def resolve(value: str) -> str:
        try:
            return expand(value, table, where)
        except UnresolvedPlaceholder as error:
            unresolved.extend(str(error).split(FAULT_SEPARATOR))
            return value

    plan = {
        "id": cell["id"], "phase": phase, "index": index,
        "cwd": resolve(cell["cwd"]),
        "argv": [resolve(item) for item in cell["argv"]],
        "env": {n: resolve(v) for n, v in sorted(cell["env"].items())},
        "stdout_equals": cell["stdout_equals"],
        "stdout_sha256_line": bool(cell.get("stdout_sha256_line", False)),
    }  # fmt: skip
    if unresolved:
        raise UnresolvedPlaceholder(
            f"{where}: " + FAULT_SEPARATOR.join(dict.fromkeys(unresolved))
        )
    return plan


class _Sink:
    """Streaming hash plus a bounded head/tail excerpt. Nothing accumulates."""

    def __init__(self, limit: int) -> None:
        self.limit, self.total = limit, 0
        self.digest = hashlib.sha256()
        self.head, self.tail = bytearray(), bytearray()

    def feed(self, chunk: bytes) -> None:
        self.digest.update(chunk)
        self.total += len(chunk)
        if len(self.head) < EXCERPT_BYTES:
            self.head += chunk[: EXCERPT_BYTES - len(self.head)]
        self.tail += chunk
        if len(self.tail) > EXCERPT_BYTES:
            del self.tail[: len(self.tail) - EXCERPT_BYTES]

    def record(self) -> dict[str, Any]:
        excerpt = self.tail if self.total > EXCERPT_BYTES else b""
        return {
            "bytes": self.total, "sha256": self.digest.hexdigest(),
            "head": self.head.decode("utf-8", "replace"),
            "tail": bytes(excerpt).decode("utf-8", "replace"),
            "over_limit": self.total > self.limit,
        }  # fmt: skip


def _terminate_group(process: subprocess.Popen) -> None:
    """Bounded process-group cleanup: SIGTERM, grace, SIGKILL."""

    for number in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(os.getpgid(process.pid), number)
        except (ProcessLookupError, PermissionError):
            break
        try:
            process.wait(timeout=TERMINATE_GRACE_SECONDS)
            return
        except subprocess.TimeoutExpired:
            continue
    process.wait()


def run_cell(plan: dict, base_env: dict, timeout: float, limit: int) -> dict[str, Any]:
    """Run one cell under a wall-clock bound and an output bound."""

    started_at, started = _timestamp(), time.monotonic()
    sinks = {"stdout": _Sink(limit), "stderr": _Sink(limit)}
    try:
        process = subprocess.Popen(
            plan["argv"], cwd=plan["cwd"], env={**base_env, **plan["env"]},
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
        )  # fmt: skip
    except OSError as error:
        status = {"code": None, "signal": None, "timed_out": False, "error": str(error)}
        return _record(plan, timeout, started_at, started, status, sinks, [], ["spawn"])
    deadline, timed_out, flooded = started + timeout, False, False
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    while selector.get_map() and not flooded:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            timed_out = True
            break
        for key, _events in selector.select(timeout=min(remaining, 1.0)):
            chunk = os.read(key.fd, READ_CHUNK_BYTES)
            if not chunk:
                selector.unregister(key.fileobj)
                key.fileobj.close()
                continue
            sinks[key.data].feed(chunk)
            flooded = flooded or sinks[key.data].total > limit
    selector.close()
    if timed_out or flooded:
        _terminate_group(process)
    else:
        try:
            process.wait(timeout=max(0.0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            timed_out = True
            _terminate_group(process)
    for stream in (process.stdout, process.stderr):
        if stream is not None and not stream.closed:
            stream.close()
    code = process.returncode
    status = {
        "code": code if code is not None and code >= 0 else None,
        "signal": -code if code is not None and code < 0 else None,
        "timed_out": timed_out,
    }
    failures = [name for name, hit in (("timeout", timed_out), ("output_limit", flooded),
                ("exit_status", code != 0)) if hit]  # fmt: skip
    checks = _assertions(plan, sinks["stdout"])
    return _record(plan, timeout, started_at, started, status, sinks, checks, failures)


def _record(plan, timeout, started_at, started, status, sinks, assertions, failures):
    """One cell record. The only shape this driver ever writes for a cell."""

    failures = list(failures) + [
        f"assertion:{item['kind']}" for item in assertions if not item["ok"]
    ]
    return {
        **{key: plan[key] for key in ("id", "phase", "index", "cwd", "argv", "env")},
        "timeout_seconds": float(timeout), "started_at": started_at,
        "wall_ms": int((time.monotonic() - started) * 1000), "exit_status": status,
        "stdout": sinks["stdout"].record(), "stderr": sinks["stderr"].record(),
        "assertions": assertions, "failures": failures, "passed": not failures,
    }  # fmt: skip


def _assertions(plan: dict, stdout: _Sink) -> list[dict[str, Any]]:
    """Manifest assertions are recorded, never a reason to abort the run."""

    assertions = []
    if plan["stdout_equals"] is not None:
        wanted = hashlib.sha256(plan["stdout_equals"].encode("utf-8")).hexdigest()
        observed = stdout.digest.hexdigest()
        assertions.append({"kind": "stdout_equals", "expected_sha256": wanted,
                           "observed_sha256": observed, "ok": wanted == observed})  # fmt: skip
    if plan["stdout_sha256_line"]:
        text = stdout.head.decode("utf-8", "replace")
        value = text.strip()
        assertions.append({
            "kind": "stdout_sha256_line", "observed": value[:64],
            "ok": stdout.total <= EXCERPT_BYTES and text.endswith("\n")
            and len(value) == 64 and all(c in "0123456789abcdef" for c in value),
        })  # fmt: skip
    return assertions


def _tool_identity(argv: list[str] | None, cwd: Any = None) -> dict[str, Any]:
    if argv is None:
        return {"path": None, "version": None, "error": "not found"}
    try:
        done = subprocess.run(argv, cwd=cwd, stdin=subprocess.DEVNULL, check=False,
                              capture_output=True, timeout=VERSION_PROBE_TIMEOUT_SECONDS)  # fmt: skip
    except (OSError, subprocess.SubprocessError) as error:
        return {"path": argv[0], "argv": argv, "version": None, "error": str(error)}
    lines = (done.stdout or done.stderr).decode("utf-8", "replace").strip().splitlines()
    return {"path": argv[0], "argv": argv, "exit_code": done.returncode,
            "version": lines[0] if lines else None}  # fmt: skip


def tool_identities(resolved: dict[str, str], cwd: pathlib.Path) -> dict[str, Any]:
    """Identify every resolved tool placeholder plus the standard five.

    Probed from the workspace, so a rustup shim reports the toolchain that the
    cells themselves will use rather than the host default.
    """

    identities = {n: _tool_identity([p, "--version"], cwd) for n, p in resolved.items()}
    for name in ("cargo", "node", "rustc"):
        found = shutil.which(name)
        argv = [found, "--version"] if found else None
        identities.setdefault(name, _tool_identity(argv, cwd))
    identities["python"] = _tool_identity([sys.executable, "--version"], cwd)
    ruff = [sys.executable, "-m", "ruff", "--version"]
    identities["ruff"] = _tool_identity(ruff, cwd)
    return dict(sorted(identities.items()))


def validation_children(manifest: dict[str, Any]) -> list[str]:
    """Every `{validation}` child the manifest names, plus the documented floor.

    DERIVED, because the hand-written floor was narrower than the promise made
    by the function below. Twenty-one `gate_a` cells set
    `CARGO_TARGET_DIR={validation}/cargo-target/<name>` and no entry in
    `VALIDATION_CHILDREN` covered it, so the one child that every vendor build
    writes into was created by cargo under the ambient umask and never
    mode-checked -- while the driver reported that it had prepared "the
    documented validation tree".

    Only the FIRST path component after the placeholder is taken: a cell may
    select a named child of `cargo-target` (`docs/testing.md:395-397`) and it is
    `cargo-target` itself that must be owner-private, not each of its per-cell
    subdirectories, which cargo creates under the driver's own `umask 077`.
    """

    found: set[str] = set(VALIDATION_CHILDREN)
    for cells in manifest.get("phases", {}).values():
        for cell in cells:
            fields = [cell.get("cwd", "")]
            fields += list(cell.get("argv", []))
            fields += list(cell.get("env", {}).values())
            for value in fields:
                if not isinstance(value, str):
                    continue
                for piece in value.split(VALIDATION_PLACEHOLDER)[1:]:
                    child = piece.lstrip("/").split("/", 1)[0]
                    if child and not child.startswith("{"):
                        found.add(child)
    return sorted(found)


def prepare_validation_root(validation: pathlib.Path, children: list[str]) -> None:
    """Create the documented validation tree owner-private, or refuse.

    The driver used to `mkdir(exist_ok=True)` the root and leave the children to
    whichever cell reached them first. An operator who pre-created the tree
    under an ordinary umask then got FOUR red cells --
    `typescript_stage_prepare`, `typescript_stage_verify`, `typescript_tests`
    and `release_full_stack_e2e`, the last after five minutes of E2E -- every
    one of which reads as a product failure and every one of which was one mode
    bit on a directory this driver owns. Fail once, here, naming the directory.

    Enforced rather than merely created: an existing wider mode is exactly the
    case that produced those four, and silently `chmod`ing it would hide a
    validation root somebody else can already read.

    `children` is [`validation_children`]'s derivation, not a literal: the
    sentence above is about the tree the manifest actually uses, and it was
    false for `cargo-target` for as long as the list was written here by hand.
    """

    missing = [name for name in VALIDATION_CHILDREN if name not in children]
    if missing:
        raise GateDriverError(
            f"validation-child derivation lost the documented child(ren) {missing}; "
            "refusing to prepare a narrower tree than docs/testing.md requires"
        )
    for path in [validation, *(validation / name for name in children)]:
        path.mkdir(parents=True, mode=OWNER_ONLY_DIRECTORY, exist_ok=True)
        mode = stat.S_IMODE(path.lstat().st_mode)
        if mode != OWNER_ONLY_DIRECTORY:
            raise GateDriverError(
                f"validation directory {path} is mode {mode:04o}, "
                f"not {OWNER_ONLY_DIRECTORY:04o}"
            )


def release_executables(release: pathlib.Path) -> list[pathlib.Path]:
    """The release directory's own executables, sorted.

    Derived from the directory rather than from a list of the eight names, so a
    ninth binary cannot enter the candidate without every check below applying
    to it and without anyone noticing.
    """

    executables = sorted(
        path
        for path in release.iterdir()
        if stat.S_ISREG(path.lstat().st_mode) and path.lstat().st_mode & 0o111
    )
    if not executables:
        raise GateDriverError(f"release directory {release} holds no executable")
    return executables


def require_release_depinfo(release: pathlib.Path) -> None:
    """Every executable in the release directory ships cargo's depinfo beside it.

    `crates/e2e/tests/pool_concurrency.rs:237` proves the candidate is not stale
    by reading `<binary>.d` -- the depinfo cargo itself wrote -- and refuses when
    it is absent, because a mutation to `stateless.rs` making `Pool::commit`
    refuse EVERY turn was measured to pass the whole live wave against a daemon
    that predated it. A release directory assembled by copying only the eight
    executables therefore fails all nineteen pool tests, six minutes into
    `release_full_stack_e2e`, with nineteen identical panics that name a missing
    `.d` file. The Gate A release directory is `$PWD/target/release`
    (`tools/gate-a-candidate/candidate_envelope.py:2526`), which carries both.
    """

    missing = [
        str(p)
        for p in release_executables(release)
        if not p.with_suffix(".d").is_file()
    ]
    if missing:
        raise GateDriverError(
            "release directory is not a cargo build directory; no depinfo beside "
            + ", ".join(missing)
        )


def require_release_not_stale(release: pathlib.Path) -> None:
    """Every release executable is at least as new as the sources cargo built it from.

    THE PRESENCE OF DEPINFO SAYS THE DIRECTORY IS A CARGO BUILD. IT SAYS
    NOTHING ABOUT WHEN. No cell in `phase-manifest.json` builds `{release}` and
    none checked its age, so the whole gate's release lane was a function of an
    out-of-band `cargo build --release` an operator ran at some earlier commit,
    with nothing in the driver able to tell.

    MEASURED, 2026-08-07, receipt `/private/tmp/gate-full/receipt.json`. One
    stale `target/release` -- built before `d310481` added the agent tools and
    `--agent-version` -- produced THREE red cells in two phases, none of which
    named a stale binary:

      * `gate_d/mcp_process`, 9.0 s: `tools/list` returned nine tools where the
        source defines thirteen, which reads exactly like "the agent resource
        was never wired to MCP".
      * `gate_d/cli_process`, 62.6 s: `error: unexpected argument
        '--agent-version' found`, which reads exactly like "F7's fix is wrong".
      * `gate_a/release_full_stack_e2e`, 388.3 s: 14 failures, all of them
        `pool_concurrency.rs`'s OWN staleness guard, 6 minutes in.

    The last of those is the only thing in the gate that could see the cause,
    it covers five binaries rather than all eight, and it reports as a product
    failure inside the longest cell in phase A. Hoisting the same rule here
    turns all three into one refusal, before the first cell, naming every stale
    binary and the source that makes it stale.

    The rule is `crates/e2e/tests/pool_concurrency.rs:225-269` verbatim: the
    dependency set is READ FROM CARGO's `<binary>.d`, never guessed, because a
    hand-rolled "newer than anything under `crates/` and `bin/`" is wrong in
    both directions -- it marks `pmux-rmuxd` stale for an edit to
    `crates/service/src/stateless.rs`, which it does not link, and cannot be
    cleared by rebuilding, because a rebuild does not touch the mtime of a
    binary whose own inputs did not change. A source cargo listed but that no
    longer exists is skipped, not fatal: that is a source DELETED since the
    build, which the compile after it will answer for.
    """

    stale, empty = [], []
    for path in release_executables(release):
        built = path.lstat().st_mtime
        depinfo = path.with_suffix(".d")
        sources = [
            source
            for line in depinfo.read_text(
                encoding="utf-8", errors="replace"
            ).splitlines()
            if ": " in line
            for source in line.split(": ", 1)[1].split()
        ]
        if not sources:
            # "Nothing is stale" over an empty set says nothing at all.
            empty.append(str(depinfo))
            continue
        for source in sources:
            try:
                modified = os.stat(source).st_mtime
            except OSError:
                continue
            if modified > built:
                stale.append(f"{path} is older than {source}")
                break
    if empty:
        raise GateDriverError(
            "release depinfo lists no source, so staleness cannot be established: "
            + ", ".join(sorted(empty))
        )
    if stale:
        raise GateDriverError(
            f"{len(stale)} release binar{'y is' if len(stale) == 1 else 'ies are'} older "
            "than the source cargo says it is built from, so the gate would measure a "
            "candidate that no longer matches this tree; run `cargo build --locked "
            "--release --workspace` first:\n  " + "\n  ".join(stale)
        )


def release_identity(directory: pathlib.Path) -> dict[str, Any]:
    binaries = []
    for path in sorted(directory.iterdir()):
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode):
            continue
        content, size = _file_sha256(path)
        binaries.append({"name": path.name, "size": size, "sha256": content,
                         "mode": f"{stat.S_IMODE(metadata.st_mode):04o}"})  # fmt: skip
    return {"path": str(directory), "binaries": binaries}


def _require(mapping: Any, schema: dict[type, str], where: str) -> None:
    if not isinstance(mapping, dict):
        raise GateDriverError(f"{where} is not an object")
    for kind, names in schema.items():
        for key in names.split():
            if key not in mapping:
                raise GateDriverError(f"{where} is missing {key!r}")
            if not isinstance(mapping[key], kind):
                raise GateDriverError(f"{where} field {key!r} is not {kind.__name__}")


def validate_receipt(receipt: dict[str, Any]) -> dict[str, Any]:
    """Validate a receipt against this driver's own schema. Fatal on drift."""

    _require(receipt, RECEIPT_SCHEMA, "receipt")
    if receipt["schema_version"] != SCHEMA_VERSION:
        raise GateDriverError(f"receipt schema_version {receipt['schema_version']!r}")
    for cell in receipt["cells"]:
        where = f"cell {cell.get('id')!r}" if isinstance(cell, dict) else "cell"
        _require(cell, CELL_SCHEMA, where)
        for stream in ("stdout", "stderr"):
            _require(cell[stream], STREAM_SCHEMA, f"{where}.{stream}")
    return receipt


def parse_overrides(values: list[str]) -> dict[str, str]:
    overrides = {}
    for item in values:
        name, separator, path = item.partition("=")
        if not separator or not name or not path:
            raise GateDriverError(f"--tool expects NAME=PATH, got {item!r}")
        overrides[name] = path
    return overrides


def _plan(args: argparse.Namespace, manifest: dict, table: Replacements) -> tuple:
    """Expand every selected cell up front: a bad placeholder never runs.

    Every fault is collected and reported together. Expansion used to abort on
    the first one, and `gate_b` needs four placeholders: an operator who
    supplied `{cargo_fuzz}` learnt about `{nightly_cargo}` only on the next run,
    and about `{nightly_rustc}` only on the one after that. The phase is budgeted
    at four hours, so each of those discoveries costs a gate attempt.
    """

    declared = list(manifest["phases"])
    unknown = sorted(set(args.phases or ()) - set(declared))
    if unknown:
        raise GateDriverError(f"unknown phase {unknown}; manifest declares {declared}")
    phases = [p for p in declared if not args.phases or p in args.phases]
    timeouts = manifest.get("phase_timeouts_seconds", {})
    plans, faults = [], []
    for phase in phases:
        timeout = float(args.cell_timeout_seconds if args.cell_timeout_seconds is not None
                        else timeouts.get(phase, DEFAULT_CELL_TIMEOUT_SECONDS))  # fmt: skip
        for index, cell in enumerate(manifest["phases"][phase]):
            try:
                plans.append((expand_cell(cell, phase, index, table), timeout))
            except GateDriverError as error:
                faults.append(str(error))
    if faults:
        raise GateDriverError(
            f"{len(faults)} of {len(faults) + len(plans)} selected cells cannot be "
            "expanded, so nothing ran:\n  " + "\n  ".join(faults)
        )
    return phases, plans


def execute(args: argparse.Namespace) -> dict[str, Any]:
    """Run every planned cell in manifest order and build the receipt."""

    workspace = args.workspace.resolve(strict=True)
    release = args.release_dir.resolve(strict=True)
    require_release_depinfo(release)
    require_release_not_stale(release)
    validation = args.validation_root.resolve()
    manifest_path = args.manifest.resolve(strict=True)
    payload = manifest_path.read_bytes()
    manifest = load_manifest(payload, str(manifest_path))
    # The manifest is loaded FIRST so the validation tree can be derived from
    # it. Both still happen before any cell runs, which is the property that
    # matters: a refusal here costs nothing, and a refusal five minutes into
    # `release_full_stack_e2e` costs five minutes and reads as a product bug.
    prepare_validation_root(validation, validation_children(manifest))
    table = Replacements(
        {"workspace": str(workspace), "release": str(release), "validation": str(validation)},
        parse_overrides(args.tool),
    )  # fmt: skip
    phases, plans = _plan(args, manifest, table)
    limit = int(manifest.get("max_command_output_bytes", DEFAULT_MAX_OUTPUT_BYTES))
    base_env = {n: os.environ[n] for n in ENVIRONMENT_ALLOWLIST if n in os.environ}
    base_env.setdefault("PATH", os.defpath)
    started_at, started = _timestamp(), time.monotonic()
    before, cells = source_digest(workspace), []
    for position, (plan, timeout) in enumerate(plans, start=1):
        record = run_cell(plan, base_env, timeout, limit)
        cells.append(record)
        state = "ok" if record["passed"] else "FAILED " + ",".join(record["failures"])
        print(f"[{position}/{len(plans)}] {plan['phase']}/{plan['id']} {state} "
              f"{record['wall_ms']}ms", file=sys.stderr, flush=True)  # fmt: skip
        if args.stop_on_failure and not record["passed"]:
            break
    after = source_digest(workspace)
    failed = [cell["id"] for cell in cells if not cell["passed"]]
    return validate_receipt({
        "schema_version": SCHEMA_VERSION, "driver": "tools/gate-a/run_gate.py",
        "started_at": started_at, "completed_at": _timestamp(),
        "wall_ms": int((time.monotonic() - started) * 1000),
        "manifest": {"path": str(manifest_path), "sha256": hashlib.sha256(payload).hexdigest(),
                     "schema_version": manifest.get("schema_version"),
                     "max_command_output_bytes": limit},
        "phases": phases,
        "host": {"os": platform.system(), "arch": platform.machine(),
                 "kernel": platform.release(), "platform": platform.platform()},
        "tools": tool_identities(table.tools, workspace),
        "workspace": str(workspace), "validation_root": str(validation),
        "paths_are_absolute_because": ABSOLUTE_PATHS_ARE_THE_CONTRACT,
        "release": release_identity(release),
        "environment_base_keys": sorted(base_env),
        "source_digest_before": before, "source_digest_after": after,
        "source_unchanged": before["sha256"] == after["sha256"],
        "cells": cells,
        "summary": {"planned": len(plans), "executed": len(cells),
                    "passed": len(cells) - len(failed), "failed": len(failed),
                    "failed_ids": failed},
    })  # fmt: skip


def main(argv: list[str] | None = None) -> int:
    # docs/testing.md:124 requires every gate command to run under `umask 077`.
    # Children inherit it, so setting it once here covers all cells. Without it
    # `tsc` emits 0644 into the validation stage and `dist-stage.mjs verify`
    # rejects the tree with "client.d.ts mode must be 0600", failing the
    # TypeScript stage and both consumers of it.
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    for name in "manifest workspace release-dir validation-root receipt".split():
        parser.add_argument(f"--{name}", required=True, type=pathlib.Path)
    parser.add_argument("--phase", action="append", dest="phases", metavar="NAME")
    parser.add_argument("--tool", action="append", default=[], metavar="NAME=PATH")
    parser.add_argument("--continue-on-failure", action="store_true",
                        help="the default; accepted so it can be stated explicitly")  # fmt: skip
    parser.add_argument("--stop-on-failure", action="store_true",
                        help="opt out of recording every cell")  # fmt: skip
    parser.add_argument("--cell-timeout-seconds", type=float, default=None)
    args = parser.parse_args(argv)
    try:
        receipt = execute(args)
    except (GateDriverError, OSError) as error:
        print(f"gate-a driver error: {error}", file=sys.stderr)
        return 2
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    os.chmod(args.receipt, 0o600)
    total = receipt["summary"]
    verdict = "PASS" if total["failed"] == 0 else "FAIL"
    named = " failed: " + ", ".join(total["failed_ids"]) if total["failed_ids"] else ""
    print(f"receipt: {args.receipt}")
    print(f"{verdict} {total['passed']}/{total['planned']} cells passed, "
          f"{total['failed']} failed, {total['executed']} executed{named}")  # fmt: skip
    return 0 if total["failed"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
