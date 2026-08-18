"""Fail-closed evidence envelope for bounded, real-Claude pmux campaigns.

This module deliberately does not read Claude transcripts, inspect terminal
screens, inject PTY input, or decide whether a turn completed.  It launches the
frozen pmux applications through their public CLI/socket boundary and treats
pmux's exit status and public result as the only product verdict.
"""

from __future__ import annotations

import dataclasses
import datetime as dt
import errno
import fcntl
import hashlib
import json
import os
import platform
import re
import signal
import stat
import sys
import tempfile
import time
import types
import urllib.parse
import uuid
import builtins
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True


SCHEMA_VERSION = 1
SOURCE_SCHEMA = "pmux.phase0.source-identity.v1"
RESERVATION_SCHEMA = "pmux.phase0.attempt-reservation.v1"
ATTEMPT_SCHEMA = "pmux.phase0.attempt-evidence.v1"
CAMPAIGN_SCHEMA = "pmux.phase0.campaign-evidence.v1"
CAMPAIGN_CONTRACT_SCHEMA = "pmux.phase0.campaign-contract.v1"
ARTIFACT_SCHEMA = "pmux.phase0.artifact-manifest.v1"
MIN_GLOBAL_ATTEMPT_CEILING = 60
MAX_GLOBAL_ATTEMPT_CEILING = 100
# Every spelling of the global ordinal a ledger record may carry, newest first.
# ONE tuple, read by `_recognized_prefix_last` and by `summarize_attempt_ledger`:
# ordinals 5-29 spell it `global_attempt` and everything from 30 on spells it
# `global_attempt_ordinal`, so a counter that knows only the first stops at 29
# and reports the budget dozens of attempts cheaper than it is.
ORDINAL_SPELLINGS = (
    "global_attempt_ordinal",
    "global_attempt_number",
    "global_attempt",
    "attempt_ordinal",
)
# RESERVATIONS made outside this ledger: the four campaigns of 2026-07-28 that
# each reserved ordinal 31 in a copy of the ledger that was then discarded
# (`evidence/README.md`, "Four detached reservations"). They are not renumbered
# into the file -- forging hash-chained records to tidy the arithmetic would
# cost more integrity than four ordinals are worth -- so every count of the
# budget has to add them back.
#
# This is NOT "every real-Claude turn that never reserved an ordinal", and the
# comment here used to say it was ("and the only such number"). It is not:
# `evidence/turn-latency-2.1.220-macos-aarch64.json` is a committed receipt for
# 22 `pmux turn` plus 22 `pmux ask` turns against the operator's real Claude
# 2.1.220 (`driver.environment == "operator"`, `zero_latency: false`), measured
# 2026-08-06, seven days after this ledger's last record. Whether decision D4's
# ceiling was meant to cover instrument runs as well as campaigns is a question
# for the owner, so nothing here silently re-prices it -- but the claim that
# four was the only such number was false, and a constant that says so is how a
# budget stops being a budget.
DETACHED_GLOBAL_ATTEMPTS = 4
MAX_PROMPT_BYTES = 1024 * 1024
MAX_CAPTURE_BYTES = 16 * 1024 * 1024
MAX_ARTIFACT_TREE_BYTES = 256 * 1024 * 1024
MAX_ARTIFACT_ENTRIES = 16_384
MAX_ARTIFACT_DEPTH = 16
MAX_LEDGER_BYTES = 16 * 1024 * 1024
MAX_LEDGER_RECORDS = 10_000
MAX_TURN_TIMEOUT_SECONDS = 600
MAX_DAEMON_READY_TIMEOUT_SECONDS = 120
MAX_DAEMON_SHUTDOWN_TIMEOUT_SECONDS = 120
RMUX_SDK_VERSION = "0.9.0"
REQUIRED_RELEASE_BINARIES = (
    "pmux",
    "pmuxd",
    "pmux-mcp",
    "claude-p",
    "pmux-rmuxd",
    "pmux-launcher",
    "pmux-hook",
)
HEX_SHA256 = re.compile(r"^[0-9a-f]{64}$")
GIT_OBJECT_ID = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
SAFE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
SAFE_INTERNAL_NAME = re.compile(r"^\.[A-Za-z0-9][A-Za-z0-9._-]{0,191}$")
MODEL_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
EXACT_CLAUDE_VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
PLATFORM_COMPONENT = re.compile(r"^[a-z0-9_-]+$")
SECRET_PATTERN = re.compile(
    r"(?i)(api[_-]?key|authorization|bearer|password|secret|token)"
    r"(\s*[:=]\s*)([^\s,;]+)"
)
SENSITIVE_ENVIRONMENT_NAME = re.compile(
    r"(?i)(key|token|secret|password|credential|authorization|auth)"
)
APPROVED_EFFORTS = ("low", "medium")
DRAIN_CALIBRATION_SCHEMA = "pmux.phase0.drain-calibration.v1"
# `SessionActorConfig::default().poll_interval`, crates/service/src/v1/actor.rs:83.
# crates/protocol/src/v1.rs:1313-1319 states the classification rule this band
# implements: the candidate/last-activity difference straddles zero -- negative
# by the parse-and-analyze interval, positive by the interval between the
# confirming poll's stability measurement (a monotonic duration) and the
# completion timestamp read (a wall clock) -- and a difference within one actor
# poll interval of zero reads as "no late rows".
#
# CAVEAT, and it is not small: this is asserted from a constant the audited
# product self-reports, and `SessionActorConfig` is overridable. A campaign run
# with a different poll interval gets a band that is wrong in the direction that
# silently reclassifies samples. Nothing in this tooling can currently detect
# that. `tools/phase0/verify_calibration.py` carries the same constant, on
# purpose: it is a deliberately separate implementation.
ACTOR_POLL_INTERVAL_MS = 20
# Exactly the seven `PermissionArg` variants of bin/pmux/src/cli.rs:263-273 in
# their clap kebab-case wire spelling. The list is closed on purpose: a value
# this tool cannot name is a value the campaign cannot bind into a receipt.
PERMISSION_MODES = (
    "accept-edits",
    "auto",
    "bypass-permissions",
    "dangerously-skip-permissions",
    "default",
    "dont-ask",
    "plan",
)
# bin/claude-p/src/main.rs:133-140 exposes six of them; the facade has no
# `--dangerously-skip-permissions` and no `--agent`.
FACADE_PERMISSION_MODES = tuple(
    mode for mode in PERMISSION_MODES if mode != "dangerously-skip-permissions"
)
ENVIRONMENT_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,255}$")
# `--env KEY=VALUE` is delivered by putting VALUE in the pmux child's
# environment and passing pmux's name-only `--env-passthrough KEY`. The value
# never enters argv, which this envelope binds verbatim into a process receipt.
ENVIRONMENT_SET_DELIVERY = "env_passthrough_name_only"
AGENT_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
# One `ClaudeLaunchConfig::denied_tools` element (crates/protocol/src/v1.rs:1966,
# a `Vec<String>`), which the daemon emits as one `--disallowedTools` pair per
# element (crates/service/src/claude_launch.rs:202-204). The minified cell's
# value is the single pattern `*`, which empties builtins AND MCP.
#
# A comma is refused, and that is the whole reason this pattern exists: pmux
# declares `--denied-tool` with `value_delimiter = ','`
# (bin/pmux/src/cli.rs:192) while the facade declares `--disallowedTools`
# without one (bin/claude-p/src/main.rs:75). One comma would therefore mean two
# denied tools through one public entrypoint and one through the other, and the
# reservation would bind a launch the operator never asked for. Control
# characters are refused for the same reason the launcher refuses a NUL in argv.
DENIED_TOOL_VALUE = re.compile(r"^[^\x00-\x1f\x7f,]{1,256}$")
MAX_DENIED_TOOLS = 64
# The same protocol field under the two spellings its two public entrypoints
# use. Emitting the wrong one is a clap parse error *after* an ordinal is
# reserved, so the spelling is chosen from the entrypoint rather than assumed.
DENIED_TOOL_FLAG = {"pmux": "--denied-tool", "claude-p": "--disallowedTools"}
# Where each public entrypoint's launch surface is DECLARED, so the set of
# options a campaign could forward is read from the product rather than
# remembered. `test_phase0.py::LaunchSurfaceTests` parses these and holds
# `_launch_args`/`_forwarded_launch_args` to them in both directions: nothing
# emitted may be absent (or a retired hidden spelling), and nothing declared may
# be silently unforwarded.
#
# This exists because both halves had already failed. `--agent-file` was renamed
# to `--profile-file` and kept only as a hidden spelling that REFUSES by name,
# and this module went on emitting it -- so a campaign configured with an agent
# profile could not launch at all, through either entrypoint, and would have
# found out after reserving an ordinal. And `--cell` has never been forwarded,
# which is why no phase0 campaign has ever exercised a minified cell.
PUBLIC_ENTRYPOINT_LAUNCH_SURFACE = {
    "pmux": ("bin/pmux/src/cli.rs", "LaunchArgs"),
    "claude-p": ("bin/claude-p/src/main.rs", "Args"),
}
# Every `pmux start` option this module deliberately does not forward, and why.
# A reason per entry, because an option nobody wrote a reason for is an option
# nobody decided about. The test above requires this dict to be EXACTLY the
# difference, so a new `pmux start` option lands here or in the argv builder.
LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD = {
    "--cell": "phase0 cannot configure a minified cell. `--cell minified` is the "
    "only thing `require_tested_for_minified_cell` gates "
    "(crates/service/src/v1/registry.rs), and the graded prompt suite this "
    "envelope exists to run instructs the model to execute `shasum` -- which a "
    "cell launched with denied_tools ['*'] cannot do. Forwarding the flag would "
    "buy campaigns that spend an ordinal each to produce a guaranteed failure. "
    "Path B promotion evidence comes from "
    "tools/promotion/promote_claude_version.py, which drives `pmux ask` and "
    "therefore SessionCell::Minified with a toolless oracle.",
    "--agent": "the STORED SERVER agent, named by id and requiring "
    "--agent-version. This envelope forwards the CLIENT-SIDE profile "
    "(--profile/--profile-file); binding a server-side resource would make the "
    "launch a function of daemon state the receipt does not pin.",
    "--agent-version": "see --agent.",
    "--allowed-tool": "a campaign narrows the tool surface and never widens it; "
    "only the denial half is expressible here.",
    "--append-system-prompt": "SYSTEM_PROMPT_POLICY is `replace`, and `append` "
    "stays inexpressible until a campaign needs it.",
    "--append-system-prompt-file": "see --append-system-prompt.",
    "--config-isolation-root": "the campaign binds its own cwd and private "
    "runtime; a second isolation root would be a path the receipt does not pin.",
    "--env": "deliberately not emitted: the value would be written into the "
    "argv this envelope binds verbatim into a receipt. See the docstring of "
    "`_forwarded_launch_args`.",
    "--extra-arg": "the allowlist is --debug/--verbose "
    "(claude_launch.rs::SAFE_EXTRA_FLAGS) and neither is evidence.",
    "--hook-timeout-ms": "no campaign configures a lifecycle hook timeout; "
    "`--lifecycle` is forwarded and its default timeout is the product's.",
    "--idle-ttl-secs": "the campaign closes its own session; an idle TTL would "
    "let the daemon close it first.",
    "--mcp-config": "a minified cell has no MCP surface and a full cell's would "
    "be a resource the receipt does not pin.",
    "--mcp-json": "see --mcp-config.",
    "--plugin-dir": "see --mcp-config.",
    "--retention": "the campaign owns its own artifact retention.",
    "--settings": "settings files are a resource the receipt does not pin.",
    "--settings-json": "see --settings.",
    "--system-prompt-file": "the replacement is emitted as text under "
    "SYSTEM_PROMPT_DELIVERY, which `read_system_prompt` bounds; a path would be "
    "a second resource to pin.",
    "--unset": "the environment is bound by name through --env-passthrough.",
}
# `SystemPromptPolicy::Replace` (crates/protocol/src/v1.rs:2082) -- the FULL
# replacement the minified cell needs. `Append` exists in the protocol and is
# deliberately NOT expressible here: every value this tool can name is a value
# it must also bind into a receipt, so the surface stays closed until a campaign
# needs it.
SYSTEM_PROMPT_POLICY = "replace"
MAX_SYSTEM_PROMPT_BYTES = 64 * 1024
# Text only: tab and newline, nothing else below 0x20, and no DEL. The
# replacement becomes one argv element of an interactive-TUI launch, so an
# unreviewed control character would either be refused by the bounded launcher
# (NUL) or ride into a terminal as an escape sequence.
SYSTEM_PROMPT_TEXT = re.compile(r"^[^\x00-\x08\x0b-\x1f\x7f]+$")
# Unlike `--env`, the system prompt has NO name-only channel. `pmux
# --system-prompt` (bin/pmux/src/cli.rs:210-211) and the facade's flag both take
# the TEXT; the 0600 `--system-prompt-file` Claude finally reads is materialized
# daemon-side (crates/service/src/sensitive_launch.rs), far past argv. So the
# text does reach the launched argv, which this envelope binds verbatim into a
# process receipt whose `argv` is inside `receipt_sha256` -- redacting it after
# the fact would make a faithful receipt look forged. The exposure is therefore
# named in the contract instead of hidden, bounded by `read_system_prompt`
# refusing a credential-shaped document before anything launches.
SYSTEM_PROMPT_DELIVERY = "pmux_argv_replace"
# The exact launch-option name set a contract may carry, in its two generations.
# Both are closed: an unknown name is refused either way. See
# `_validate_launch_options` for why the older one is still admitted.
LEGACY_LAUNCH_OPTION_KEYS = frozenset(
    {
        "permission_mode",
        "environment_set_names",
        "environment_set_values_recorded",
        "environment_set_delivery",
        "environment_passthrough_names",
        "agent_name",
        "agent_file",
    }
)
LAUNCH_OPTION_KEYS = LEGACY_LAUNCH_OPTION_KEYS | {
    "denied_tools",
    "system_prompt_policy",
    "system_prompt_text_recorded",
    "system_prompt_delivery",
    "system_prompt_file",
}
# The TurnTimings fields this tool knows by name -- eight, and the count is
# load-bearing: the late-arrival field is DISCOVERED as the one name beyond
# this set, so every addition must be declared here or that discovery goes
# ambiguous and every gap could be computed from the wrong field (crates/protocol/src/v1.rs
# :1266-1289) plus `drain_ms`. Anything else in the published `timings` object is
# treated as the product's late-row-arrival observation; the name is read from
# the result rather than compiled in, so a field added upstream is picked up
# without editing this file.
KNOWN_TURN_TIMING_FIELDS = (
    "submitted_at_ms",
    "prompt_acknowledged_at_ms",
    "terminal_candidate_at_ms",
    "completed_at_ms",
    "drain_ms",
    # `stop_hook_at_ms` is NOT a late-arrival field -- it records when Claude's
    # Stop lifecycle hook arrived, for deciding whether a hook-based completion
    # fast path would be sound. It is listed here because the late-arrival field
    # is DISCOVERED as "the one name TurnTimings carries beyond this set". Once a
    # second unknown name existed, that discovery became ambiguous and the tool
    # could have silently computed every gap from the hook timestamp instead of
    # the transcript one -- a wrong number nobody would have questioned. Naming
    # it here keeps the discovery unambiguous rather than merely quiet.
    "stop_hook_at_ms",
    # Observational only, added with the turn_duration arrival instrumentation.
    # Named here for the SAME reason as stop_hook_at_ms: the late-arrival field is
    # DISCOVERED as "the one name beyond this set", so every additional name must
    # be declared or that discovery goes ambiguous and the tool could compute every
    # gap from the wrong field -- a wrong number nobody would question.
    "turn_duration_observed_at_ms",
    "post_turn_duration_row_observed_at_ms",
)
# `*_at_ms` fields are wall-clock epoch milliseconds, so the only bound the
# product itself guarantees is protocol v1's `safe_u64` fence.
MAX_TURN_TIMING_MS = 2**53 - 1
TOOLS_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = TOOLS_ROOT.parent
EVIDENCE_COMMON_ROOT = TOOLS_ROOT / "evidence_common"
LINUX_EVIDENCE_ROOT = TOOLS_ROOT / "linux-docker"
MAX_AUTHORITY_BYTES = 8 * 1024 * 1024


class EvidenceError(RuntimeError):
    """The envelope cannot safely acquire or bind evidence."""


class BudgetExhausted(EvidenceError):
    """A durable attempt cannot be reserved inside the approved ceiling."""


class CampaignInterrupted(EvidenceError):
    """A campaign signal interrupted an in-flight public command."""


def _exact_authority_bytes(
    path: Path, description: str
) -> tuple[bytes, tuple[int, ...]]:
    """Read one source authority through a retained no-follow descriptor."""

    try:
        before = path.lstat()
    except OSError as error:
        raise EvidenceError(f"{description} is unavailable") from error
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
        raise EvidenceError(f"{description} is not one exact authority file")
    descriptor = -1
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        opened = os.fstat(descriptor)
        if any(getattr(opened, field) != getattr(before, field) for field in fields):
            raise EvidenceError(f"{description} changed before read")
        payload = bytearray()
        while len(payload) < opened.st_size:
            chunk = os.read(descriptor, min(64 * 1024, opened.st_size - len(payload)))
            if not chunk:
                raise EvidenceError(f"{description} ended before its bound")
            payload.extend(chunk)
        if os.read(descriptor, 1):
            raise EvidenceError(f"{description} exceeded its bound")
        after = os.fstat(descriptor)
        if any(getattr(after, field) != getattr(opened, field) for field in fields):
            raise EvidenceError(f"{description} changed while read")
    except OSError as error:
        raise EvidenceError(f"{description} could not be read") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    try:
        final = path.lstat()
    except OSError as error:
        raise EvidenceError(f"{description} path disappeared after read") from error
    if any(getattr(final, field) != getattr(after, field) for field in fields):
        raise EvidenceError(f"{description} path changed after read")
    return bytes(payload), tuple(getattr(after, field) for field in fields)


def _load_exact_authority(
    path: Path,
    description: str,
    *,
    import_aliases: Mapping[str, types.ModuleType] | None = None,
) -> tuple[types.ModuleType, dict[str, Any], tuple[int, ...]]:
    """Load exact source without installing a shared import-name alias."""

    payload, witness = _exact_authority_bytes(path, description)
    digest = hashlib.sha256(payload).hexdigest()
    module_name = f"_pmux_phase0_{path.stem}_{os.urandom(8).hex()}"
    module = types.ModuleType(module_name)
    module.__file__ = str(path)
    module.__package__ = ""
    aliases = dict(import_aliases or {})
    original_import = builtins.__import__

    def exact_import(
        name: str,
        globals_value: Mapping[str, Any] | None = None,
        locals_value: Mapping[str, Any] | None = None,
        fromlist: Sequence[str] = (),
        level: int = 0,
    ) -> Any:
        if level == 0 and name in aliases:
            return aliases[name]
        return original_import(name, globals_value, locals_value, fromlist, level)

    module_builtins = dict(vars(builtins))
    module_builtins["__import__"] = exact_import
    module.__dict__["__builtins__"] = module_builtins
    sys.modules[module_name] = module
    try:
        exec(compile(payload, str(path), "exec", dont_inherit=True), module.__dict__)
    except Exception as error:
        raise EvidenceError(f"{description} could not load") from error
    finally:
        if sys.modules.get(module_name) is module:
            del sys.modules[module_name]
    try:
        relative = path.relative_to(REPOSITORY_ROOT).as_posix()
    except ValueError:
        relative = path.name
    identity = {"path": relative, "size": len(payload), "sha256": digest}
    return module, identity, witness


def _revalidate_exact_authority(
    path: Path,
    description: str,
    expected_identity: Mapping[str, Any],
    expected_witness: tuple[int, ...],
) -> dict[str, Any]:
    payload, witness = _exact_authority_bytes(path, description)
    try:
        relative = path.relative_to(REPOSITORY_ROOT).as_posix()
    except ValueError:
        relative = path.name
    identity = {
        "path": relative,
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }
    if witness != expected_witness or identity != dict(expected_identity):
        raise EvidenceError(f"{description} changed after module load")
    return identity


source_digest, SOURCE_DIGEST_AUTHORITY, _SOURCE_DIGEST_WITNESS = _load_exact_authority(
    LINUX_EVIDENCE_ROOT / "source_digest.py", "source-digest authority"
)
bounded_process = source_digest.bounded_process
BOUNDED_PROCESS_AUTHORITY = dict(source_digest._BOUNDED_PROCESS_IDENTITY)
managed_process, MANAGED_PROCESS_AUTHORITY, _MANAGED_PROCESS_WITNESS = (
    _load_exact_authority(
        EVIDENCE_COMMON_ROOT / "managed_process.py",
        "managed-process authority",
        import_aliases={"bounded_process": bounded_process},
    )
)


def evidence_authorities() -> dict[str, Any]:
    """Revalidate every executable evidence authority against its load witness."""

    bounded = source_digest._revalidate_bounded_process_authority()
    if bounded != BOUNDED_PROCESS_AUTHORITY:
        raise EvidenceError("bounded-process authority changed")
    return {
        "bounded_process": bounded,
        "managed_process": _revalidate_exact_authority(
            EVIDENCE_COMMON_ROOT / "managed_process.py",
            "managed-process authority",
            MANAGED_PROCESS_AUTHORITY,
            _MANAGED_PROCESS_WITNESS,
        ),
        "source_digest": _revalidate_exact_authority(
            LINUX_EVIDENCE_ROOT / "source_digest.py",
            "source-digest authority",
            SOURCE_DIGEST_AUTHORITY,
            _SOURCE_DIGEST_WITNESS,
        ),
    }


@dataclasses.dataclass(frozen=True)
class FileIdentity:
    path: str
    sha256: str
    size: int
    device: int
    inode: int
    uid: int
    link_count: int
    mode: int
    modified_ns: int
    changed_ns: int

    def public(self, *, include_path: bool = True) -> dict[str, Any]:
        value = dataclasses.asdict(self)
        if not include_path:
            value["path_sha256"] = sha256_text(self.path)
            del value["path"]
        return value


@dataclasses.dataclass(frozen=True)
class PromptIdentity:
    path: Path
    file: FileIdentity
    payload: bytes = dataclasses.field(repr=False)

    def public(self) -> dict[str, Any]:
        value = self.file.public(include_path=False)
        value["content_encoding"] = "caller_supplied_utf8_file"
        return value


@dataclasses.dataclass(frozen=True)
class DirectoryIdentity:
    path: str
    device: int
    inode: int
    uid: int
    mode: int

    def public(self, *, include_path: bool = False) -> dict[str, Any]:
        value = {
            "device": self.device,
            "inode": self.inode,
            "uid": self.uid,
            "mode": self.mode,
        }
        if include_path:
            value["path"] = self.path
        else:
            value["canonical_path_sha256"] = sha256_text(self.path)
        return value


@dataclasses.dataclass(frozen=True)
class LedgerPrefix:
    records: int
    sha256: str
    last_global_attempt: int


@dataclasses.dataclass(frozen=True)
class CampaignConfig:
    source_root: Path
    expected_source_digest: str
    release_bin_dir: Path
    expected_binary_hashes: Mapping[str, str]
    claude_bin: Path
    expected_claude_sha256: str
    cwd: Path
    prompt_paths: tuple[Path, ...]
    evidence_root: Path
    ledger_path: Path
    ledger_prefix: LedgerPrefix
    prior_campaign_anchors: Mapping[str, str]
    campaign_id: str
    global_attempt_ceiling: int
    max_attempts_this_run: int
    max_observed_tokens: int
    scenario: str
    resume_session_id: str | None
    model: str | None
    allowed_model_ids: tuple[str, ...]
    effort: str | None
    output_format: str
    compatibility: str
    tested_profile_path: Path | None
    terminal_rows: int
    terminal_cols: int
    terminal_profile: str
    input_transport: str
    lifecycle: str
    untested_transcript_drain_ms: int
    turn_timeout_seconds: int
    daemon_ready_timeout_seconds: int
    daemon_shutdown_timeout_seconds: int
    live: bool
    acknowledge_usage: bool
    acknowledge_untested: bool
    permission_mode: str | None = None
    # `--env KEY=VALUE`. Values are secrets by assumption: they reach the pmux
    # argv and nothing else. Only `environment_set_names` is ever bound into the
    # contract or written to an artifact.
    environment_set: Mapping[str, str] = dataclasses.field(default_factory=dict)
    environment_passthrough_names: tuple[str, ...] = ()
    agent_name: str | None = None
    agent_file: Path | None = None
    # `--denied-tool`, repeatable, retained in the exact order the entrypoint
    # will receive it: `denied_tools` is a `Vec<String>` the daemon walks in
    # order, so a sorted binding would describe a different launch.
    denied_tools: tuple[str, ...] = ()
    # `--system-prompt-file`. An owner-only document whose TEXT replaces
    # Claude's system prompt. Only the file identity is ever recorded; see
    # SYSTEM_PROMPT_DELIVERY for where the text does and does not travel.
    system_prompt_file: Path | None = None

    @property
    def environment_set_names(self) -> tuple[str, ...]:
        return tuple(sorted(self.environment_set))


@dataclasses.dataclass(frozen=True)
class SourceObservation:
    identity: Mapping[str, Any]
    revision_captures: Mapping[str, Mapping[str, Any]]


@dataclasses.dataclass(frozen=True)
class SocketIdentity:
    path: str
    device: int
    inode: int
    uid: int
    mode: int

    def public(self) -> dict[str, Any]:
        return {
            "path_sha256": sha256_text(self.path),
            "device": self.device,
            "inode": self.inode,
            "uid": self.uid,
            "mode": self.mode,
        }


@dataclasses.dataclass(frozen=True)
class CommandResult:
    argv_shape: tuple[str, ...]
    returncode: int | None
    timed_out: bool
    interrupted: bool
    elapsed_ms: int
    stdout: bytes
    stderr: bytes
    output_limit_exceeded: bool
    supervision_failure_reason: str | None
    cleanup_complete: bool
    output_complete: bool
    process_receipt: Mapping[str, Any]

    @property
    def supervised(self) -> bool:
        return self.supervision_failure_reason is None


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def pretty_json_bytes(value: Any) -> bytes:
    return (
        json.dumps(
            value,
            sort_keys=True,
            indent=2,
            ensure_ascii=False,
            allow_nan=False,
        )
        + "\n"
    ).encode("utf-8")


def _reject_json_constant(value: str) -> Any:
    raise EvidenceError(f"non-finite JSON number is forbidden: {value}")


def _unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise EvidenceError(f"duplicate JSON object key is forbidden: {key}")
        value[key] = item
    return value


def strict_json_loads(payload: bytes | str, *, label: str) -> Any:
    """Decode standards-compliant JSON without Python's permissive extensions."""

    try:
        return json.loads(
            payload,
            parse_constant=_reject_json_constant,
            object_pairs_hook=_unique_json_object,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"{label} is invalid JSON") from error


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_text(value: str) -> str:
    return sha256_bytes(value.encode("utf-8"))


def _validate_digest(value: str, label: str) -> str:
    normalized = value.lower()
    if HEX_SHA256.fullmatch(normalized) is None:
        raise EvidenceError(f"{label} must be a lowercase SHA-256 digest")
    return normalized


def _validate_uuid(value: str, label: str) -> str:
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError) as error:
        raise EvidenceError(f"{label} must be a canonical UUID") from error
    if str(parsed) != value.lower():
        raise EvidenceError(f"{label} must be a canonical lowercase UUID")
    return str(parsed)


def _assert_owner_only_directory(path: Path, *, create: bool) -> Path:
    absolute = path.absolute()
    if create:
        absolute.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        metadata = absolute.lstat()
    except FileNotFoundError as error:
        raise EvidenceError(f"directory does not exist: {absolute}") from error
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise EvidenceError(f"path must be a real directory, not a symlink: {absolute}")
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise EvidenceError(f"directory is not owned by the current user: {absolute}")
    if stat.S_IMODE(metadata.st_mode) & 0o077:
        raise EvidenceError(f"directory must be owner-only: {absolute}")
    return absolute


def _assert_private_parent(path: Path, *, create: bool) -> Path:
    return _assert_owner_only_directory(path.parent, create=create)


def capture_socket_identity(path: Path) -> SocketIdentity:
    absolute = path.absolute()
    try:
        metadata = absolute.lstat()
    except FileNotFoundError as error:
        raise EvidenceError("pmuxd socket is absent") from error
    if not stat.S_ISSOCK(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise EvidenceError("pmuxd endpoint is not a real Unix socket")
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise EvidenceError("pmuxd socket is not owned by the current user")
    mode = stat.S_IMODE(metadata.st_mode)
    if mode & 0o077:
        raise EvidenceError("pmuxd socket is not owner-only")
    return SocketIdentity(
        path=str(absolute),
        device=metadata.st_dev,
        inode=metadata.st_ino,
        uid=metadata.st_uid,
        mode=mode,
    )


def verify_socket_identity(path: Path, expected: SocketIdentity) -> None:
    current = capture_socket_identity(path)
    if current != expected:
        raise EvidenceError("pmuxd socket identity changed after readiness")


def _verify_open_path_identity(
    path: Path,
    descriptor: int,
    *,
    expected_size: int,
    parent_descriptor: int | None = None,
) -> None:
    """Fence a durable append against pathname replacement before authorization."""

    opened = os.fstat(descriptor)
    try:
        current = (
            os.stat(path.name, dir_fd=parent_descriptor, follow_symlinks=False)
            if parent_descriptor is not None
            else path.lstat()
        )
    except FileNotFoundError as error:
        raise EvidenceError(
            "attempt ledger pathname disappeared after append"
        ) from error
    if not stat.S_ISREG(opened.st_mode) or not stat.S_ISREG(current.st_mode):
        raise EvidenceError("attempt ledger pathname is no longer a regular file")
    if stat.S_ISLNK(current.st_mode):
        raise EvidenceError("attempt ledger pathname became a symlink")
    if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
        raise EvidenceError("attempt ledger pathname was replaced after append")
    if opened.st_size != expected_size or current.st_size != expected_size:
        raise EvidenceError("attempt ledger final size changed after append")
    if opened.st_nlink != 1 or current.st_nlink != 1:
        raise EvidenceError("attempt ledger must not have multiple hard links")
    if hasattr(os, "getuid") and (
        opened.st_uid != os.getuid() or current.st_uid != os.getuid()
    ):
        raise EvidenceError("attempt ledger is not owned by the current user")
    if stat.S_IMODE(opened.st_mode) & 0o077 or stat.S_IMODE(current.st_mode) & 0o077:
        raise EvidenceError("attempt ledger must be owner-only")


def _open_directory_nofollow(
    path: Path, *, require_owner_private: bool
) -> tuple[int, os.stat_result]:
    metadata = path.lstat()
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    opened = os.fstat(descriptor)
    try:
        if (
            not stat.S_ISDIR(opened.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or (opened.st_dev, opened.st_ino) != (metadata.st_dev, metadata.st_ino)
        ):
            raise EvidenceError("private directory changed while opening")
        if require_owner_private:
            if hasattr(os, "getuid") and opened.st_uid != os.getuid():
                raise EvidenceError(
                    "private directory is not owned by the current user"
                )
            if stat.S_IMODE(opened.st_mode) & 0o077:
                raise EvidenceError("private directory must be owner-only")
    except Exception:
        os.close(descriptor)
        raise
    return descriptor, opened


def _open_private_directory_nofollow(path: Path) -> tuple[int, os.stat_result]:
    return _open_directory_nofollow(path, require_owner_private=True)


def _verify_open_directory_path_identity(
    path: Path,
    descriptor: int,
    *,
    require_owner_private: bool = True,
) -> None:
    opened = os.fstat(descriptor)
    try:
        current = path.lstat()
    except FileNotFoundError as error:
        raise EvidenceError("private parent directory pathname disappeared") from error
    if (
        not stat.S_ISDIR(opened.st_mode)
        or not stat.S_ISDIR(current.st_mode)
        or stat.S_ISLNK(current.st_mode)
        or (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino)
    ):
        raise EvidenceError("private parent directory pathname was replaced")
    if require_owner_private:
        if hasattr(os, "getuid") and (
            opened.st_uid != os.getuid() or current.st_uid != os.getuid()
        ):
            raise EvidenceError("private parent directory is not owner-controlled")
        if (
            stat.S_IMODE(opened.st_mode) & 0o077
            or stat.S_IMODE(current.st_mode) & 0o077
        ):
            raise EvidenceError("private parent directory is no longer owner-only")


def _clear_directory_descriptor(descriptor: int, *, expected_device: int) -> None:
    """Remove one exact directory's children without following pathname links."""

    with os.scandir(descriptor) as iterator:
        names = sorted(entry.name for entry in iterator)
    directory_flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    for name in names:
        before = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        if before.st_dev != expected_device:
            raise EvidenceError("external runtime contains a foreign filesystem entry")
        if stat.S_ISDIR(before.st_mode) and not stat.S_ISLNK(before.st_mode):
            child = os.open(name, directory_flags, dir_fd=descriptor)
            try:
                opened = os.fstat(child)
                if not stat.S_ISDIR(opened.st_mode) or (
                    opened.st_dev,
                    opened.st_ino,
                ) != (before.st_dev, before.st_ino):
                    raise EvidenceError(
                        "external runtime directory changed while opening"
                    )
                _clear_directory_descriptor(child, expected_device=expected_device)
                current = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if not stat.S_ISDIR(current.st_mode) or (
                    current.st_dev,
                    current.st_ino,
                ) != (opened.st_dev, opened.st_ino):
                    raise EvidenceError(
                        "external runtime directory changed before removal"
                    )
            finally:
                os.close(child)
            os.rmdir(name, dir_fd=descriptor)
        else:
            current = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            if (current.st_dev, current.st_ino, current.st_mode) != (
                before.st_dev,
                before.st_ino,
                before.st_mode,
            ):
                raise EvidenceError("external runtime entry changed before removal")
            os.unlink(name, dir_fd=descriptor)
    os.fsync(descriptor)


def _descriptor_tree_members(
    descriptor: int,
    *,
    prefix: str = "",
    depth: int = 0,
    budget: list[int] | None = None,
) -> list[str]:
    """List one owned tree without following a path or symlink."""

    if depth > MAX_ARTIFACT_DEPTH:
        raise EvidenceError("runtime tree exceeds its depth bound")
    remaining = budget if budget is not None else [MAX_ARTIFACT_ENTRIES]
    with os.scandir(descriptor) as iterator:
        names = sorted(entry.name for entry in iterator)
    result: list[str] = []
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    for name in names:
        remaining[0] -= 1
        if remaining[0] < 0:
            raise EvidenceError("runtime tree exceeds its entry bound")
        relative = f"{prefix}/{name}" if prefix else name
        metadata = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        result.append(relative)
        if stat.S_ISDIR(metadata.st_mode) and not stat.S_ISLNK(metadata.st_mode):
            child = os.open(name, flags, dir_fd=descriptor)
            try:
                opened = os.fstat(child)
                current = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if (
                    not stat.S_ISDIR(opened.st_mode)
                    or (opened.st_dev, opened.st_ino)
                    != (metadata.st_dev, metadata.st_ino)
                    or (current.st_dev, current.st_ino)
                    != (opened.st_dev, opened.st_ino)
                ):
                    raise EvidenceError("runtime directory changed while listing")
                result.extend(
                    _descriptor_tree_members(
                        child,
                        prefix=relative,
                        depth=depth + 1,
                        budget=remaining,
                    )
                )
            finally:
                os.close(child)
    return result


def _open_regular_nofollow(path: Path) -> tuple[int, os.stat_result]:
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise EvidenceError(f"path must be a regular non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    opened = os.fstat(descriptor)
    if (opened.st_dev, opened.st_ino) != (metadata.st_dev, metadata.st_ino):
        os.close(descriptor)
        raise EvidenceError(f"file changed while opening: {path}")
    return descriptor, opened


def read_bounded_regular_file(path: Path, maximum: int) -> tuple[bytes, os.stat_result]:
    descriptor, opened = _open_regular_nofollow(path)
    try:
        chunks: list[bytes] = []
        total = 0
        while total <= maximum:
            chunk = os.read(descriptor, min(128 * 1024, maximum + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
        value = b"".join(chunks)
        after = os.fstat(descriptor)
        try:
            current = path.lstat()
        except FileNotFoundError as error:
            raise EvidenceError(
                f"file pathname disappeared after hashing: {path}"
            ) from error
    finally:
        os.close(descriptor)
    if len(value) > maximum:
        raise EvidenceError(f"file exceeds {maximum} bytes: {path}")
    receipt_fields = (
        "st_dev",
        "st_ino",
        "st_uid",
        "st_nlink",
        "st_mode",
        "st_size",
        "st_mtime_ns",
        "st_ctime_ns",
    )
    if tuple(getattr(opened, field) for field in receipt_fields) != tuple(
        getattr(after, field) for field in receipt_fields
    ):
        raise EvidenceError(f"file changed while hashing: {path}")
    if not stat.S_ISREG(current.st_mode) or stat.S_ISLNK(current.st_mode):
        raise EvidenceError(f"file pathname changed type after hashing: {path}")
    if tuple(getattr(after, field) for field in receipt_fields) != tuple(
        getattr(current, field) for field in receipt_fields
    ):
        raise EvidenceError(f"file pathname was replaced after hashing: {path}")
    return value, after


def identify_file(
    path: Path,
    *,
    executable: bool = False,
    maximum: int = 512 * 1024 * 1024,
) -> FileIdentity:
    absolute = path.absolute()
    value, metadata = read_bounded_regular_file(absolute, maximum)
    return _file_identity_from_read(absolute, value, metadata, executable=executable)


def identify_directory(
    path: Path, *, require_private: bool = False
) -> DirectoryIdentity:
    """Capture one canonical directory identity without trusting its spelling."""

    requested = path.absolute()
    try:
        canonical = requested.resolve(strict=True)
        metadata = canonical.lstat()
    except FileNotFoundError as error:
        raise EvidenceError(f"directory does not exist: {requested}") from error
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise EvidenceError(f"path must resolve to a real directory: {requested}")
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise EvidenceError(f"directory is not owned by the current user: {requested}")
    mode = stat.S_IMODE(metadata.st_mode)
    if require_private and mode & 0o077:
        raise EvidenceError(f"directory must be owner-only: {requested}")
    return DirectoryIdentity(
        path=str(canonical),
        device=metadata.st_dev,
        inode=metadata.st_ino,
        uid=metadata.st_uid,
        mode=mode,
    )


def verify_directory_identity(path: Path, expected: DirectoryIdentity) -> None:
    if identify_directory(path) != expected:
        raise EvidenceError("canonical directory identity changed after binding")


def _file_identity_from_read(
    path: Path,
    value: bytes,
    metadata: os.stat_result,
    *,
    executable: bool = False,
) -> FileIdentity:
    if executable and (stat.S_IMODE(metadata.st_mode) & 0o111) == 0:
        raise EvidenceError(f"file is not executable: {path}")
    return FileIdentity(
        path=str(path),
        sha256=sha256_bytes(value),
        size=len(value),
        device=metadata.st_dev,
        inode=metadata.st_ino,
        uid=metadata.st_uid,
        link_count=metadata.st_nlink,
        mode=stat.S_IMODE(metadata.st_mode),
        modified_ns=metadata.st_mtime_ns,
        changed_ns=metadata.st_ctime_ns,
    )


def identify_prompt(path: Path) -> PromptIdentity:
    absolute = path.absolute()
    value, metadata = read_bounded_regular_file(absolute, MAX_PROMPT_BYTES)
    try:
        value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise EvidenceError("prompt file must contain UTF-8") from error
    if not value:
        raise EvidenceError("prompt file must not be empty")
    return PromptIdentity(
        path=absolute,
        file=_file_identity_from_read(absolute, value, metadata),
        payload=value,
    )


def same_file_identity(first: FileIdentity, second: FileIdentity) -> bool:
    return first == second


def _revision_identity_sha256(value: Mapping[str, Any]) -> str:
    digest = hashlib.sha256()
    digest.update(b"pmux.phase0.workspace-revision-identity.v1\0")
    digest.update(canonical_json_bytes(dict(value)))
    return digest.hexdigest()


def _candidate_source_digest_authority(
    root: Path,
) -> tuple[types.ModuleType, dict[str, Any], tuple[int, ...]]:
    program = root / "tools" / "linux-docker" / "source_digest.py"
    module, identity, witness = _load_exact_authority(
        program, "candidate source-digest authority"
    )
    identity = {
        "path": "tools/linux-docker/source_digest.py",
        "size": identity["size"],
        "sha256": identity["sha256"],
    }
    if identity["sha256"] != SOURCE_DIGEST_AUTHORITY["sha256"]:
        raise EvidenceError("candidate source-digest authority is not the loaded one")
    bounded_identity = getattr(module, "_BOUNDED_PROCESS_IDENTITY", None)
    if bounded_identity != BOUNDED_PROCESS_AUTHORITY:
        raise EvidenceError("candidate source digest loaded another process authority")
    required = (
        "workspace_source_manifest",
        "workspace_revision_capture",
        "validate_workspace_revision_capture",
        "validate_workspace_revision_identity",
        "_revalidate_bounded_process_authority",
    )
    if any(not hasattr(module, name) for name in required):
        raise EvidenceError("candidate source-digest interface is incomplete")
    return module, identity, witness


def observe_source_identity(source_root: Path) -> SourceObservation:
    """Bind portable source bytes to stable Git facts and causal receipts."""

    root = source_root.resolve(strict=True)
    if not root.is_dir():
        raise EvidenceError("source root must be a directory")
    module, program_identity, program_witness = _candidate_source_digest_authority(root)
    try:
        capture_before = module.workspace_revision_capture(root)
        capture_before = module.validate_workspace_revision_capture(capture_before)
        manifest = module.workspace_source_manifest(root)
        capture_after = module.workspace_revision_capture(root)
        capture_after = module.validate_workspace_revision_capture(capture_after)
    except Exception as error:
        raise EvidenceError(
            f"canonical source/revision capture failed: {error}"
        ) from error
    digest_program = root / "tools" / "linux-docker" / "source_digest.py"
    payload, repeated_witness = _exact_authority_bytes(
        digest_program, "candidate source-digest authority"
    )
    if (
        repeated_witness != program_witness
        or sha256_bytes(payload) != program_identity["sha256"]
    ):
        raise EvidenceError("canonical source-digest authority changed during capture")
    try:
        current_bounded = module._revalidate_bounded_process_authority()
    except Exception as error:
        raise EvidenceError("candidate bounded-process authority changed") from error
    if current_bounded != BOUNDED_PROCESS_AUTHORITY:
        raise EvidenceError("candidate bounded-process authority changed")
    if capture_before["identity"] != capture_after["identity"]:
        raise EvidenceError("workspace revision changed across source capture")
    revision_identity = module.validate_workspace_revision_identity(
        capture_after["identity"]
    )
    if revision_identity.get("workspace") != str(root):
        raise EvidenceError("workspace revision capture names another source root")
    if (
        capture_before.get("bounded_process_implementation")
        != BOUNDED_PROCESS_AUTHORITY
        or capture_after.get("bounded_process_implementation")
        != BOUNDED_PROCESS_AUTHORITY
    ):
        raise EvidenceError("workspace revision used another process authority")
    try:
        schema_version = manifest["schema_version"]
        digest = manifest["workspace_source_sha256"]
        count = manifest["workspace_file_count"]
        algorithm = manifest["algorithm"]
    except (KeyError, TypeError) as error:
        raise EvidenceError(
            "canonical source digest returned invalid evidence"
        ) from error
    if not isinstance(digest, str) or HEX_SHA256.fullmatch(digest) is None:
        raise EvidenceError("canonical source digest returned an invalid SHA-256")
    if not isinstance(count, int) or isinstance(count, bool) or count < 1:
        raise EvidenceError("canonical source digest returned an invalid file count")
    if schema_version != 1 or not isinstance(algorithm, str) or not algorithm:
        raise EvidenceError(
            "canonical source digest returned an invalid algorithm identity"
        )
    authorities = evidence_authorities()
    identity = {
        "schema": SOURCE_SCHEMA,
        "algorithm": algorithm,
        "implementation": "tools/linux-docker/source_digest.py::workspace_source_manifest",
        "algorithm_sha256": program_identity["sha256"],
        "digest": digest,
        "file_count": count,
        "revision_identity": revision_identity,
        "revision_identity_sha256": _revision_identity_sha256(revision_identity),
        "phase0_evidence_authorities": authorities,
    }
    return SourceObservation(
        identity=identity,
        revision_captures={
            "before_source_manifest": capture_before,
            "after_source_manifest": capture_after,
        },
    )


def compute_source_identity(source_root: Path) -> dict[str, Any]:
    return dict(observe_source_identity(source_root).identity)


# The fields of a source identity that the frozen-candidate claim is actually
# made of: the content digest and the tool that produced it, plus the revision
# facts a reader would use to reconstruct the tree.
SOURCE_IDENTITY_CLAIM_FIELDS = (
    "digest",
    "file_count",
    "algorithm_sha256",
    "implementation",
    "phase0_evidence_authorities",
)
# `revision_identity` also carries `repository_control`: nine .git nodes with ten
# stat fields each, including `ctime_ns`. Any external `git status` -- an editor,
# an IDE, a workspace manager -- rewrites `.git/index` and moves `ctime_ns`, and
# a whole-dict comparison therefore failed a ~1.3 s window ~30% of the time and
# an ~11 s window essentially always. `revision_identity_sha256` is excluded for
# the same reason: it is a hash *of* `repository_control` among other things.
#
# This is a narrower fence, and it is a trade rather than a strict improvement
# (docs/instrument-fix-plan.md section 4.6). To slip through it, a mutation would
# have to leave the content digest, the commit, the porcelain status, the tracked
# binary diffs and the tool digests all identical -- in which case every claim
# the evidence makes about the tree is still true. The full observation,
# `repository_control` included, is still recorded on every observation record,
# so the forensic trail is unchanged.
#
# FOLLOW-UP, not addressed here: `tools/linux-docker/source_digest.py:1309`
# aborts a revision capture outright if `_repository_control_snapshot` moves
# *during* a single capture. That is a second, narrower window with the same
# cause and it is still open.
REVISION_IDENTITY_CLAIM_FIELDS = (
    "head",
    "head_ref",
    "branch",
    "detached",
    "status_porcelain_v1_z_sha256",
    "tracked_binary_diff_sha256",
    "source_digest_implementation",
)
_ABSENT = object()


def source_identity_claim(identity: Mapping[str, Any]) -> dict[str, Any]:
    """Project a source identity onto exactly the claim the evidence makes.

    Missing fields are a loud failure rather than a silent ``None == None``
    match: a projection that compares nothing would pass everything.
    """

    revision = identity.get("revision_identity")
    if not isinstance(revision, Mapping):
        raise EvidenceError("source identity carries no revision identity")
    missing = [name for name in SOURCE_IDENTITY_CLAIM_FIELDS if name not in identity]
    missing += [
        f"revision_identity.{name}"
        for name in REVISION_IDENTITY_CLAIM_FIELDS
        if name not in revision
    ]
    if missing:
        raise EvidenceError(
            "source identity is missing claim fields: " + ", ".join(sorted(missing))
        )
    claim = {name: identity[name] for name in SOURCE_IDENTITY_CLAIM_FIELDS}
    claim["revision_identity"] = {
        name: revision[name] for name in REVISION_IDENTITY_CLAIM_FIELDS
    }
    return claim


def source_identity_delta(
    frozen: Mapping[str, Any], current: Mapping[str, Any]
) -> list[str]:
    """Every field path that differs, whether or not it is part of the claim."""

    paths: list[str] = []
    for name in sorted(set(frozen) | set(current)):
        before = frozen.get(name, _ABSENT)
        after = current.get(name, _ABSENT)
        if before == after:
            continue
        if (
            name == "revision_identity"
            and isinstance(before, Mapping)
            and isinstance(after, Mapping)
        ):
            for inner in sorted(set(before) | set(after)):
                if before.get(inner, _ABSENT) != after.get(inner, _ABSENT):
                    paths.append(f"revision_identity.{inner}")
            continue
        paths.append(name)
    return paths


def launch_options_identity(config: CampaignConfig) -> dict[str, Any]:
    """Bind every launch option this envelope forwards to the pmux CLI.

    A launch option that changes Claude's behaviour but is invisible in the
    receipt would break reproducibility, so all of them are recorded. `--env`
    values are secrets by assumption: only the names appear here, exactly as
    ``environment_identity`` records ``values_recorded: false``. They are also
    delivered through pmux's name-only channel rather than argv, because this
    envelope binds the launched argv verbatim into a process receipt.

    The system prompt replacement is the one launch value with no such channel,
    so instead of pretending otherwise the binding states the delivery route and
    records only the document's identity. `denied_tools` is not sensitive -- a
    tool pattern is a policy, not a payload -- so it is bound in full, in order.
    """

    return {
        "permission_mode": config.permission_mode,
        "environment_set_names": list(config.environment_set_names),
        "environment_set_values_recorded": False,
        "environment_set_delivery": ENVIRONMENT_SET_DELIVERY,
        "environment_passthrough_names": sorted(config.environment_passthrough_names),
        "agent_name": config.agent_name,
        "denied_tools": list(config.denied_tools),
        "system_prompt_policy": (
            SYSTEM_PROMPT_POLICY if config.system_prompt_file is not None else None
        ),
        "system_prompt_text_recorded": False,
        "system_prompt_delivery": SYSTEM_PROMPT_DELIVERY,
    }


def environment_identity(environment: Mapping[str, str]) -> dict[str, Any]:
    names = sorted(environment)
    digest = hashlib.sha256()
    for name in names:
        encoded_name = name.encode("utf-8", errors="surrogateescape")
        encoded_value = environment[name].encode("utf-8", errors="surrogateescape")
        digest.update(len(encoded_name).to_bytes(8, "big"))
        digest.update(encoded_name)
        digest.update(len(encoded_value).to_bytes(8, "big"))
        digest.update(encoded_value)
    sensitive_names = sorted(
        name for name in names if SENSITIVE_ENVIRONMENT_NAME.search(name)
    )
    return {
        "entry_count": len(names),
        "names_sha256": sha256_bytes(canonical_json_bytes(names)),
        "values_bound_sha256": digest.hexdigest(),
        "sensitive_name_count": len(sensitive_names),
        "values_recorded": False,
    }


def sensitive_environment_values(environment: Mapping[str, str]) -> tuple[str, ...]:
    """Retain sensitive values only in memory for output redaction."""

    values = {
        value
        for name, value in environment.items()
        if value and SENSITIVE_ENVIRONMENT_NAME.search(name)
    }
    return tuple(sorted(values, key=lambda item: (-len(item), item)))


def _escaped_secret_variants(value: str) -> set[str]:
    variants = {value}
    frontier = {value}
    # Two encoding layers cover a value embedded in a JSON/string log which is
    # then itself JSON encoded, without retaining any variant in evidence.
    for _ in range(2):
        encoded: set[str] = set()
        for item in frontier:
            encoded.update(
                {
                    json.dumps(item, ensure_ascii=False)[1:-1],
                    json.dumps(item, ensure_ascii=True)[1:-1],
                    item.encode("unicode_escape").decode("ascii"),
                    urllib.parse.quote(item, safe=""),
                    urllib.parse.quote_plus(item, safe=""),
                }
            )
        encoded.discard("")
        frontier = encoded - variants
        variants.update(encoded)
    return variants


def current_platform_identity() -> dict[str, str]:
    system = platform.system().lower()
    os_name = {"darwin": "macos"}.get(system, system)
    machine = platform.machine().lower()
    arch = {"arm64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}.get(
        machine, machine
    )
    return {
        "os": os_name,
        "architecture": arch,
        "kernel_release": platform.release(),
        "python": platform.python_version(),
    }


def sys_platform() -> str:
    return platform.system().lower()


def validate_config(config: CampaignConfig, *, access_files: bool) -> None:
    _validate_digest(config.expected_source_digest, "expected source digest")
    _validate_digest(config.expected_claude_sha256, "expected Claude digest")
    missing_hashes = sorted(
        set(REQUIRED_RELEASE_BINARIES) - set(config.expected_binary_hashes)
    )
    extra_hashes = sorted(
        set(config.expected_binary_hashes) - set(REQUIRED_RELEASE_BINARIES)
    )
    if missing_hashes or extra_hashes:
        raise EvidenceError(
            f"binary hashes must name exactly {', '.join(REQUIRED_RELEASE_BINARIES)}"
        )
    for name, digest in config.expected_binary_hashes.items():
        _validate_digest(digest, f"expected {name} digest")
    _validate_uuid(config.campaign_id, "campaign ID")
    if config.resume_session_id is not None:
        _validate_uuid(config.resume_session_id, "resume session ID")
    if config.scenario not in {"one-shot", "persistent", "resume", "claude-p-one-shot"}:
        raise EvidenceError(
            "scenario must be one-shot, persistent, resume, or claude-p-one-shot"
        )
    if config.scenario == "resume" and config.resume_session_id is None:
        raise EvidenceError("resume scenario requires --resume-session-id")
    if config.scenario != "resume" and config.resume_session_id is not None:
        raise EvidenceError("--resume-session-id is valid only for resume scenario")
    if (
        config.scenario == "claude-p-one-shot"
        and config.compatibility != "require-tested"
    ):
        raise EvidenceError("claude-p-one-shot requires a tested compatibility profile")
    if config.scenario == "claude-p-one-shot" and config.output_format != "json":
        raise EvidenceError("claude-p-one-shot evidence currently requires JSON output")
    if config.scenario == "claude-p-one-shot" and (
        config.terminal_rows != 24
        or config.terminal_cols != 120
        or config.terminal_profile != "transparent"
        or config.input_transport != "auto"
        or config.lifecycle != "transcript"
    ):
        raise EvidenceError(
            "claude-p-one-shot requires the facade's fixed 24x120 transparent/auto/transcript cell"
        )
    if config.output_format not in {"json", "ndjson"}:
        raise EvidenceError("output format must be json or ndjson")
    if config.compatibility not in {"require-tested", "allow-untested"}:
        raise EvidenceError("compatibility must be require-tested or allow-untested")
    if config.terminal_profile != "transparent" or config.input_transport not in {
        "auto",
        "sdk",
    }:
        raise EvidenceError(
            "the v1 live envelope supports only transparent auto/sdk input"
        )
    if (
        type(config.terminal_rows) is not int
        or type(config.terminal_cols) is not int
        or not 1 <= config.terminal_rows <= 65_535
        or not 1 <= config.terminal_cols <= 65_535
    ):
        raise EvidenceError("terminal rows and columns must be in 1..65535")
    if config.lifecycle not in {"transcript", "hybrid"}:
        raise EvidenceError("lifecycle must be transcript or hybrid")
    if not isinstance(config.model, str) or MODEL_ID.fullmatch(config.model) is None:
        raise EvidenceError("the live campaign requires one explicit model selector")
    if not config.allowed_model_ids:
        raise EvidenceError("at least one exact allowed public model ID is required")
    if len(set(config.allowed_model_ids)) != len(config.allowed_model_ids):
        raise EvidenceError("allowed public model IDs must be unique")
    for model_id in config.allowed_model_ids:
        if not isinstance(model_id, str) or MODEL_ID.fullmatch(model_id) is None:
            raise EvidenceError("allowed public model IDs must be exact bounded IDs")
    if config.effort not in APPROVED_EFFORTS:
        raise EvidenceError("the approved campaign requires low or medium effort")
    if not config.prompt_paths:
        raise EvidenceError("at least one prompt file is required")
    if type(
        config.max_attempts_this_run
    ) is not int or config.max_attempts_this_run != len(config.prompt_paths):
        raise EvidenceError(
            "--max-attempts-this-run must equal the number of prompt files"
        )
    if not 1 <= config.max_attempts_this_run <= MAX_GLOBAL_ATTEMPT_CEILING:
        raise EvidenceError(
            "max attempts for this run is outside the bounded campaign range"
        )
    if (
        type(config.global_attempt_ceiling) is not int
        or not MIN_GLOBAL_ATTEMPT_CEILING
        <= config.global_attempt_ceiling
        <= MAX_GLOBAL_ATTEMPT_CEILING
    ):
        raise EvidenceError(
            "global attempt ceiling must be in the explicit range 60..100"
        )
    if (
        type(config.ledger_prefix.records) is not int
        or type(config.ledger_prefix.last_global_attempt) is not int
        or config.ledger_prefix.records < 0
        or config.ledger_prefix.last_global_attempt < 0
    ):
        raise EvidenceError("ledger prefix counts cannot be negative")
    _validate_digest(config.ledger_prefix.sha256, "ledger prefix digest")
    if not isinstance(config.prior_campaign_anchors, Mapping):
        raise EvidenceError("prior campaign anchors must be a mapping")
    for run_id, digest in config.prior_campaign_anchors.items():
        if not isinstance(run_id, str) or not isinstance(digest, str):
            raise EvidenceError("prior campaign anchors must map UUIDs to digests")
        _validate_uuid(run_id, "prior campaign run ID")
        _validate_digest(digest, "prior campaign manifest digest")
    if config.ledger_prefix.last_global_attempt > config.global_attempt_ceiling:
        raise EvidenceError("prefix last attempt already exceeds the global ceiling")
    if type(config.max_observed_tokens) is not int or config.max_observed_tokens <= 0:
        raise EvidenceError("max observed tokens must be greater than zero")
    timeout_bounds = (
        (
            config.turn_timeout_seconds,
            MAX_TURN_TIMEOUT_SECONDS,
            "turn timeout",
        ),
        (
            config.daemon_ready_timeout_seconds,
            MAX_DAEMON_READY_TIMEOUT_SECONDS,
            "daemon readiness timeout",
        ),
        (
            config.daemon_shutdown_timeout_seconds,
            MAX_DAEMON_SHUTDOWN_TIMEOUT_SECONDS,
            "daemon shutdown timeout",
        ),
    )
    for value, maximum, label in timeout_bounds:
        if type(value) is not int or not 1 <= value <= maximum:
            raise EvidenceError(f"{label} must be an exact integer in 1..{maximum}")
    if (
        type(config.untested_transcript_drain_ms) is not int
        or not 1 <= config.untested_transcript_drain_ms <= 60_000
    ):
        raise EvidenceError("untested transcript drain must be in 1..60000 ms")
    if config.permission_mode is not None:
        if config.permission_mode not in PERMISSION_MODES:
            raise EvidenceError(
                "--permission-mode must be one of the seven pmux PermissionArg values"
            )
        if (
            config.scenario == "claude-p-one-shot"
            and config.permission_mode not in FACADE_PERMISSION_MODES
        ):
            raise EvidenceError(
                "the claude-p facade has no --dangerously-skip-permissions "
                "(bin/claude-p/src/main.rs:133-140)"
            )
    if (
        len(config.environment_set) > 64
        or len(config.environment_passthrough_names) > 64
    ):
        raise EvidenceError("forwarded environment options exceed their reviewed bound")
    if len(set(config.environment_passthrough_names)) != len(
        config.environment_passthrough_names
    ):
        raise EvidenceError("--env-passthrough names must be unique")
    for name in (
        *config.environment_set_names,
        *config.environment_passthrough_names,
    ):
        if ENVIRONMENT_NAME.fullmatch(name) is None:
            raise EvidenceError(f"forwarded environment name {name!r} is not a name")
    for name, value in sorted(config.environment_set.items()):
        if not isinstance(value, str) or not value or len(value) > 4096:
            raise EvidenceError(f"--env {name} must carry a bounded non-empty value")
    if (config.agent_name is None) != (config.agent_file is None):
        raise EvidenceError("--agent and --agent-file are required together")
    if config.agent_name is not None:
        if AGENT_NAME.fullmatch(config.agent_name) is None:
            raise EvidenceError("--agent must be an exact bounded profile name")
        if config.scenario == "claude-p-one-shot":
            raise EvidenceError("the claude-p facade has no agent-profile surface")
    if not isinstance(config.denied_tools, tuple):
        raise EvidenceError("--denied-tool values must be an ordered tuple")
    if len(config.denied_tools) > MAX_DENIED_TOOLS:
        raise EvidenceError("--denied-tool exceeds its reviewed bound of 64 patterns")
    if len(set(config.denied_tools)) != len(config.denied_tools):
        raise EvidenceError("--denied-tool values must be unique")
    for tool in config.denied_tools:
        # A leading hyphen is refused separately from the character class: clap
        # does not set `allow_hyphen_values` on either spelling, so such a value
        # would be read as a flag by the entrypoint -- a parse failure AFTER the
        # ordinal was reserved, which is the expensive kind.
        if (
            not isinstance(tool, str)
            or DENIED_TOOL_VALUE.fullmatch(tool) is None
            or tool.startswith("-")
        ):
            raise EvidenceError(
                f"--denied-tool {tool!r} is not one bounded comma-free tool pattern"
            )
    if config.live and not config.acknowledge_usage:
        raise EvidenceError("--live requires --acknowledge-claude-usage")
    if (
        config.live
        and config.compatibility == "allow-untested"
        and not config.acknowledge_untested
    ):
        raise EvidenceError(
            "allow-untested live work requires --acknowledge-untested-compatibility"
        )
    if config.compatibility == "require-tested" and config.tested_profile_path is None:
        raise EvidenceError("require-tested campaigns require --tested-profile-file")
    if (
        config.compatibility == "allow-untested"
        and config.tested_profile_path is not None
    ):
        raise EvidenceError(
            "allow-untested campaigns must not supply --tested-profile-file"
        )
    if not access_files:
        return
    if not config.source_root.is_absolute() or not config.release_bin_dir.is_absolute():
        raise EvidenceError("source root and release binary directory must be absolute")
    if not config.claude_bin.is_absolute() or not config.cwd.is_absolute():
        raise EvidenceError("Claude executable and cwd must be absolute")
    if not config.evidence_root.is_absolute() or not config.ledger_path.is_absolute():
        raise EvidenceError("evidence root and ledger path must be absolute")
    if config.agent_file is not None and not config.agent_file.is_absolute():
        raise EvidenceError("--agent-file must be an absolute path")
    if config.system_prompt_file is not None and not (
        config.system_prompt_file.is_absolute()
    ):
        raise EvidenceError("--system-prompt-file must be an absolute path")
    _assert_owner_only_directory(config.evidence_root, create=True)
    _assert_private_parent(config.ledger_path, create=True)
    identify_directory(config.cwd)
    if config.agent_file is not None:
        read_bounded_regular_file(config.agent_file.absolute(), MAX_PROMPT_BYTES)
    if config.system_prompt_file is not None:
        # Read it here for the same reason the agent profile is read here: a
        # document that cannot be admitted must fail before the campaign starts,
        # not after the first ordinal is durably spent.
        read_system_prompt(config.system_prompt_file)
    if config.tested_profile_path is not None:
        profile, _identity = read_profile(config.tested_profile_path)
        platform_identity = current_platform_identity()
        if (
            profile["os"] != platform_identity["os"]
            or profile["arch"] != platform_identity["architecture"]
            or profile["terminal_profile"] != config.terminal_profile
            or profile["input_transport"] != "sdk"
        ):
            raise EvidenceError(
                "tested profile does not identify the exact requested host and resolved cell"
            )


def validate_tested_profile(value: Any) -> dict[str, Any]:
    """Validate one exact, resolved compatibility cell before daemon launch."""

    expected = {
        "claude_version",
        "os",
        "arch",
        "terminal_profile",
        "input_transport",
        "transcript_drain_ms",
    }
    if not isinstance(value, dict) or set(value) != expected:
        raise EvidenceError("tested profile has an unexpected schema or field set")
    version = value.get("claude_version")
    os_name = value.get("os")
    architecture = value.get("arch")
    drain = value.get("transcript_drain_ms")
    if (
        not isinstance(version, str)
        or len(version) > 128
        or EXACT_CLAUDE_VERSION.fullmatch(version) is None
    ):
        raise EvidenceError("tested profile version must be exact major.minor.patch")
    for label, component in (("os", os_name), ("arch", architecture)):
        if (
            not isinstance(component, str)
            or len(component) > 64
            or PLATFORM_COMPONENT.fullmatch(component) is None
        ):
            raise EvidenceError(f"tested profile {label} is not a normalized token")
    if value.get("terminal_profile") != "transparent":
        raise EvidenceError("tested profile must use the transparent terminal cell")
    if value.get("input_transport") != "sdk":
        raise EvidenceError("tested profile must name the resolved sdk input cell")
    if type(drain) is not int or not 1 <= drain <= 60_000:
        raise EvidenceError("tested profile transcript drain is outside 1..60000 ms")
    return dict(value)


def normalize_claude_version_output(payload: bytes) -> str:
    """Apply the production service's exact first-semver normalization rule."""

    try:
        output = payload.decode("utf-8")
    except UnicodeDecodeError as error:
        raise EvidenceError("Claude version stdout is not UTF-8") from error
    for raw_token in output.split():
        start = 0
        end = len(raw_token)
        while start < end:
            character = raw_token[start]
            if (character.isascii() and character.isalnum()) or character == ".":
                break
            start += 1
        while end > start:
            character = raw_token[end - 1]
            if (character.isascii() and character.isalnum()) or character == ".":
                break
            end -= 1
        token = raw_token[start:end]
        if EXACT_CLAUDE_VERSION.fullmatch(token) is not None:
            return token
    raise EvidenceError("Claude version stdout has no exact major.minor.patch token")


def read_profile(path: Path) -> tuple[dict[str, Any], FileIdentity]:
    absolute = path.absolute()
    payload, metadata = read_bounded_regular_file(absolute, 1024 * 1024)
    try:
        value = strict_json_loads(payload, label="tested profile file")
    except EvidenceError as error:
        raise EvidenceError(
            "tested profile file must contain one JSON object"
        ) from error
    if not isinstance(value, dict):
        raise EvidenceError("tested profile file must contain one JSON object")
    profile = validate_tested_profile(value)
    if payload != canonical_json_bytes(profile) + b"\n":
        raise EvidenceError("tested profile file must use canonical JSON plus newline")
    return profile, _file_identity_from_read(absolute, payload, metadata)


def read_system_prompt(path: Path) -> tuple[str, FileIdentity]:
    """Read the one document whose text REPLACES Claude's system prompt.

    Owner-only is required, not preferred. The daemon materializes the
    replacement into a 0600 file of its own
    (`crates/service/src/sensitive_launch.rs`); sourcing it from a
    world-readable file would make that pointless, and this envelope already
    holds `--agent-file`, the ledger and every artifact to the same rule.

    The text is returned so the caller can put it in the redaction set and hand
    it to the entrypoint, and is never part of the returned identity: what gets
    bound is the digest, exactly as prompts are bound by digest and delivered on
    stdin.
    """

    absolute = path.absolute()
    payload, metadata = read_bounded_regular_file(absolute, MAX_SYSTEM_PROMPT_BYTES)
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise EvidenceError("system prompt file is not owned by the current user")
    if stat.S_IMODE(metadata.st_mode) & 0o077:
        raise EvidenceError("system prompt file must be owner-only (mode 0600)")
    if metadata.st_nlink != 1:
        raise EvidenceError("system prompt file must not have multiple hard links")
    if not payload:
        raise EvidenceError("system prompt file must not be empty")
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError as error:
        raise EvidenceError("system prompt file must contain UTF-8") from error
    if SYSTEM_PROMPT_TEXT.fullmatch(text) is None:
        raise EvidenceError(
            "system prompt file must be text: tab and newline are the only "
            "control characters a replacement may carry"
        )
    # Refused for the same reason a hyphen-leading `--denied-tool` value is, and
    # it costs more here: neither entrypoint sets `allow_hyphen_values` on
    # `--system-prompt` (bin/pmux/src/cli.rs:210-211, bin/claude-p/src/main.rs:87-88),
    # so the text becomes the argv element after the flag and clap reads it as a
    # flag instead -- measured against the frozen release binary, `pmux run
    # --system-prompt "-leading"` dies with "unexpected argument '-l' found".
    # Reservation precedes launch (MF-2), so admitting such a document burns an
    # irreplaceable ordinal on an argv parse failure. The caller reflows the
    # first line; nothing about the instruction has to change.
    if text.startswith("-"):
        raise EvidenceError(
            "system prompt file must not begin with '-': it travels as the argv "
            "element after --system-prompt, which neither entrypoint parses with "
            "allow_hyphen_values, so it would fail to parse after the ordinal "
            "was already reserved"
        )
    # The one guard that earns the argv exposure named in SYSTEM_PROMPT_DELIVERY.
    # A system prompt can contain anything, and this one becomes an argv element
    # that lands in `ps` and inside a receipt digest that must stay faithful. So
    # a document this tool would have had to redact is refused outright instead:
    # the caller can restate the instruction without an assignment that reads as
    # a credential.
    if redact_text(text) != text:
        raise EvidenceError(
            "system prompt file reads as carrying a credential (api key, token, "
            "password, bearer or secret assignment). It would travel in argv and "
            "into a receipt that cannot be redacted after the fact; rewrite it"
        )
    return text, _file_identity_from_read(absolute, payload, metadata)


def dry_run_manifest(config: CampaignConfig) -> dict[str, Any]:
    validate_config(config, access_files=False)
    return {
        "schema_version": SCHEMA_VERSION,
        "mode": "dry_run",
        "live_commands_executed": False,
        "writes_performed": False,
        "campaign_id": config.campaign_id,
        "scenario": config.scenario,
        "planned_attempts": len(config.prompt_paths),
        "global_attempt_ceiling": config.global_attempt_ceiling,
        "max_observed_tokens": config.max_observed_tokens,
        "source": {
            "root": str(config.source_root),
            "expected_digest": config.expected_source_digest,
        },
        "release_binary_directory": str(config.release_bin_dir),
        "expected_binary_sha256": dict(sorted(config.expected_binary_hashes.items())),
        "claude": {
            "path": str(config.claude_bin),
            "expected_sha256": config.expected_claude_sha256,
            "model": config.model,
            "allowed_public_model_ids": sorted(config.allowed_model_ids),
            "effort": config.effort,
        },
        "cell": {
            "output_format": config.output_format,
            "resume_session_id": config.resume_session_id,
            "terminal_rows": config.terminal_rows,
            "terminal_cols": config.terminal_cols,
            "terminal_profile": config.terminal_profile,
            "input_transport": config.input_transport,
            "lifecycle": config.lifecycle,
            "compatibility": config.compatibility,
            "launch_options": {
                **launch_options_identity(config),
                "agent_file": (
                    str(config.agent_file) if config.agent_file is not None else None
                ),
                # The PATH, never the text. A dry run performs no path access,
                # so this is the operator's own spelling echoed back; the live
                # run replaces it with the document's bound identity.
                "system_prompt_file": (
                    str(config.system_prompt_file)
                    if config.system_prompt_file is not None
                    else None
                ),
            },
        },
        "inputs": [
            {"prompt_file_index": index, "content_in_argv": False}
            for index, _ in enumerate(config.prompt_paths, start=1)
        ],
        "ledger": {
            "path": str(config.ledger_path),
            "prefix_records": config.ledger_prefix.records,
            "prefix_sha256": config.ledger_prefix.sha256,
            "prefix_last_global_attempt": config.ledger_prefix.last_global_attempt,
            "reservation_before_possible_claude_launch": True,
            "reservations_are_never_reused": True,
            "prior_campaign_anchors": dict(
                sorted(config.prior_campaign_anchors.items())
            ),
        },
        "authority": {
            "pmux_exit_and_public_result": "authoritative",
            "transcript_parsing": False,
            "terminal_classification": False,
            "direct_rmux_or_pty_input": False,
        },
        "required_live_guards_present": {
            "live": config.live,
            "acknowledge_claude_usage": config.acknowledge_usage,
            "acknowledge_untested_compatibility": config.acknowledge_untested,
        },
    }


def _private_create_at(name: str, parent_descriptor: int, mode: int) -> int:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    return os.open(name, flags, mode, dir_fd=parent_descriptor)


def _open_or_create_private_append_file_at(
    name: str, parent_descriptor: int, mode: int
) -> int:
    """Open one append file without racing concurrent nonexclusive creates.

    Darwin can transiently return ``ENOENT`` when several threads issue
    concurrent ``openat(O_CREAT)`` calls for the same missing member. Separate
    the existing-file open from one exclusive creation; contenders that lose
    the exclusive create then open the now-existing member without ``O_CREAT``.
    """

    flags = os.O_RDWR | os.O_APPEND | getattr(os, "O_CLOEXEC", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        return os.open(name, flags, dir_fd=parent_descriptor)
    except FileNotFoundError:
        pass
    try:
        return os.open(
            name,
            flags | os.O_CREAT | os.O_EXCL,
            mode,
            dir_fd=parent_descriptor,
        )
    except FileExistsError:
        return os.open(name, flags, dir_fd=parent_descriptor)


def _write_private_atomic_at(
    parent_descriptor: int, name: str, data: bytes
) -> os.stat_result:
    if not name or "/" in name or name in {".", ".."}:
        raise EvidenceError("evidence artifact name is unsafe")
    try:
        os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
    except FileNotFoundError:
        pass
    else:
        raise EvidenceError(f"refusing to overwrite evidence artifact: {name}")
    temporary_name = f".{name}.{uuid.uuid4().hex}.tmp"
    descriptor = _private_create_at(temporary_name, parent_descriptor, 0o600)
    published = False
    try:
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise EvidenceError(f"short write while creating {name}")
            view = view[written:]
        os.fchmod(descriptor, 0o600)
        os.fsync(descriptor)
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_nlink != 1
            or stat.S_IMODE(opened.st_mode) != 0o600
            or (hasattr(os, "getuid") and opened.st_uid != os.getuid())
        ):
            raise EvidenceError("temporary evidence file failed its identity fence")
        os.link(
            temporary_name,
            name,
            src_dir_fd=parent_descriptor,
            dst_dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        target = os.stat(
            name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            not stat.S_ISREG(target.st_mode)
            or stat.S_ISLNK(target.st_mode)
            or (target.st_dev, target.st_ino) != (opened.st_dev, opened.st_ino)
            or target.st_nlink != 2
            or stat.S_IMODE(target.st_mode) != 0o600
            or (hasattr(os, "getuid") and target.st_uid != os.getuid())
        ):
            raise EvidenceError("published evidence file failed its identity fence")
        os.unlink(temporary_name, dir_fd=parent_descriptor)
        published = True
        target = os.stat(
            name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if target.st_nlink != 1 or (target.st_dev, target.st_ino) != (
            opened.st_dev,
            opened.st_ino,
        ):
            raise EvidenceError("published evidence file gained another hard link")
        os.fsync(parent_descriptor)
        target = os.stat(
            name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            target.st_nlink != 1
            or (target.st_dev, target.st_ino) != (opened.st_dev, opened.st_ino)
            or target.st_size != len(data)
            or stat.S_IMODE(target.st_mode) != 0o600
        ):
            raise EvidenceError("durable evidence file identity changed")
        return target
    except Exception:
        try:
            os.unlink(temporary_name, dir_fd=parent_descriptor)
        except FileNotFoundError:
            pass
        if published:
            # The destination may already be durable. Never remove it on an
            # ambiguous post-publication failure.
            pass
        raise
    finally:
        os.close(descriptor)


def write_private_atomic(path: Path, data: bytes) -> None:
    absolute = path.absolute()
    parent = _assert_private_parent(absolute, create=False)
    if absolute.parent != parent or not absolute.name:
        raise EvidenceError("evidence artifact is not an exact parent member")
    parent_descriptor, _ = _open_private_directory_nofollow(parent)
    try:
        _verify_open_directory_path_identity(parent, parent_descriptor)
        _write_private_atomic_at(parent_descriptor, absolute.name, data)
        _verify_open_directory_path_identity(parent, parent_descriptor)
    finally:
        os.close(parent_descriptor)


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _ledger_lines(payload: bytes) -> list[bytes]:
    if len(payload) > MAX_LEDGER_BYTES:
        raise EvidenceError("attempt ledger exceeds the bounded size")
    if payload and not payload.endswith(b"\n"):
        raise EvidenceError("attempt ledger has a torn final record")
    lines = payload.splitlines(keepends=True)
    if len(lines) > MAX_LEDGER_RECORDS:
        raise EvidenceError("attempt ledger has too many records")
    for index, line in enumerate(lines, start=1):
        if line in {b"\n", b"\r\n"}:
            raise EvidenceError(f"attempt ledger record {index} is empty")
        try:
            value = strict_json_loads(line, label=f"attempt ledger record {index}")
        except EvidenceError as error:
            raise EvidenceError(
                f"attempt ledger record {index} is invalid JSON"
            ) from error
        if not isinstance(value, dict):
            raise EvidenceError(f"attempt ledger record {index} is not an object")
    return lines


def _recognized_ordinals(
    lines: Sequence[bytes], *, label: str = "immutable ledger prefix record"
) -> list[int]:
    recognized: list[int] = []
    for line in lines:
        record = strict_json_loads(line, label=label)
        for key in ORDINAL_SPELLINGS:
            value = record.get(key)
            if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
                recognized.append(value)
                break
    for previous, current in zip(recognized, recognized[1:], strict=False):
        if current <= previous:
            raise EvidenceError(
                "recognized immutable prefix attempts must be strictly increasing"
            )
    return recognized


def _recognized_prefix_last(lines: Sequence[bytes]) -> int | None:
    recognized = _recognized_ordinals(lines)
    return recognized[-1] if recognized else None


def global_attempts_consumed_through(
    last_global_attempt: int, *, detached: int = DETACHED_GLOBAL_ATTEMPTS
) -> int:
    """How many global attempts stand consumed once `last_global_attempt` is reserved.

    ONE derivation, three callers, because the ledger's last ordinal and the
    global consumed count are not the same number and the difference is exactly
    `DETACHED_GLOBAL_ATTEMPTS`.

    `summarize_attempt_ledger` always added the detached reservations back.
    `reserve_attempt` did not: it compared the bare next ordinal against the
    ceiling under a refusal that says "global real-Claude attempt ceiling is
    exhausted", so the message named the global count while the predicate tested
    the file's own numbering. Against ordinal 81 and a ceiling of 100 that guard
    would still hand out 19 ordinals (82..100) while `phase0.py budget` reported
    15 remaining -- four attempts past a ceiling `evidence/README.md` calls "a
    total across all campaigns", spent believing the tool agreed.

    A second copy of `+ detached` at each site is what let the two drift, so
    there is no second copy: the planning guard, the reservation guard and the
    budget report all ask this.
    """

    if detached < 0:
        raise EvidenceError("detached reservations cannot be negative")
    return last_global_attempt + detached


def summarize_attempt_ledger(
    path: Path, *, detached: int = DETACHED_GLOBAL_ATTEMPTS
) -> dict[str, int | list[int]]:
    """Derive the global real-Claude attempt budget from the ledger itself.

    Every number here is COMPUTED from the file on the call. The budget was
    published as prose once -- `evidence/README.md` said "47 of the authorized
    100 global attempts are consumed; 53 remain" while the file had already
    reached ordinal 81 -- and prose cannot be appended to by a reservation. The
    ceiling comes from the records' own `global_attempt_ceiling`, not from
    `MAX_GLOBAL_ATTEMPT_CEILING`, so a campaign authorized under a lower ceiling
    is reported against the ceiling it was actually reserved against.

    The ordinal spellings are `ORDINAL_SPELLINGS`, shared with
    `_recognized_prefix_last`, because a second copy of that tuple is what makes
    a scan stop at ordinal 29 and read the budget fifty-two attempts cheap.
    """

    payload, _ = read_bounded_regular_file(path, MAX_LEDGER_BYTES)
    lines = _ledger_lines(payload)
    if not lines:
        raise EvidenceError("attempt ledger is empty; it states no budget")
    ordinals = _recognized_ordinals(lines, label="attempt ledger record")
    if len(ordinals) != len(lines):
        raise EvidenceError(
            "an attempt ledger record spells its ordinal in none of "
            f"{', '.join(ORDINAL_SPELLINGS)}; the budget cannot be counted from it"
        )
    first, last = ordinals[0], ordinals[-1]
    if ordinals != list(range(first, last + 1)):
        raise EvidenceError(
            "attempt ledger ordinals are not contiguous; a gap is either a lost "
            "record or a reservation this file never saw"
        )
    ceilings = sorted(
        {
            value
            for line in lines
            for value in (
                strict_json_loads(line, label="attempt ledger record").get(
                    "global_attempt_ceiling"
                ),
            )
            if isinstance(value, int) and not isinstance(value, bool)
        }
    )
    for ceiling in ceilings:
        if not MIN_GLOBAL_ATTEMPT_CEILING <= ceiling <= MAX_GLOBAL_ATTEMPT_CEILING:
            raise EvidenceError(
                f"attempt ledger records a global ceiling of {ceiling}, outside "
                f"the explicit range {MIN_GLOBAL_ATTEMPT_CEILING}..{MAX_GLOBAL_ATTEMPT_CEILING}"
            )
    ceiling = ceilings[-1] if ceilings else MAX_GLOBAL_ATTEMPT_CEILING
    consumed = global_attempts_consumed_through(last, detached=detached)
    if consumed > ceiling:
        raise EvidenceError(
            f"attempt ledger has already consumed {consumed} of a ceiling of {ceiling}"
        )
    return {
        "records": len(lines),
        "first_ordinal": first,
        "last_ordinal": last,
        "predating_the_file": first - 1,
        "detached": detached,
        "consumed": consumed,
        "ceiling": ceiling,
        "ceilings_recorded": ceilings,
        "remaining": ceiling - consumed,
    }


# ---- The launch surface, read from the product ------------------------------
#
# A clap `#[arg(...)]` declaration spells its long option one of two ways:
# explicitly, `long = "denied-tool"`, or implicitly, `long` plus the field name
# with underscores turned into hyphens. Both are matched here, and `hide = true`
# is kept SEPARATE rather than dropped: a hidden option is a retired spelling
# the product refuses by name, so emitting one is worse than emitting an unknown
# flag -- it parses, and then the launch is refused after an ordinal is spent.
_CLAP_LONG_NAMED = re.compile(r'\blong\s*=\s*"(?P<name>[A-Za-z0-9_-]+)"')
_CLAP_FIELD_AFTER_ATTRS = re.compile(
    r"\s*(?:#\[[^\n]*\]\s*)*(?:pub\s+)?([a-z_][a-z0-9_]*)\s*:"
)


def _balanced(source: str, opening: int, open_char: str, close_char: str) -> int:
    """Index just past the `close_char` that balances `source[opening]`.

    Written rather than regexed because clap attributes nest brackets --
    `conflicts_with_all = ["a", "b"]` -- and a `[^\\]]*` attribute pattern
    silently stopped at the first inner `]`. It dropped the four system-prompt
    options from the derived surface, which is precisely the way a derivation
    fails safe-looking: fewer options declared means fewer omissions to explain.
    """

    depth = 0
    for index in range(opening, len(source)):
        if source[index] == open_char:
            depth += 1
        elif source[index] == close_char:
            depth -= 1
            if depth == 0:
                return index + 1
    raise EvidenceError("unbalanced Rust source while reading a clap declaration")


def clap_long_options(source: str, struct: str) -> tuple[set[str], set[str]]:
    """`(accepted, retired)` long options declared by one clap struct.

    Reads the product's own declaration instead of a list kept here. The two
    sets are returned apart because a caller must never emit a member of the
    second one.
    """

    opening = re.search(rf"\bstruct {re.escape(struct)}\s*\{{", source)
    if opening is None:
        raise EvidenceError(f"no clap struct named {struct!r} in this source")
    body = source[opening.end() - 1 : _balanced(source, opening.end() - 1, "{", "}")]
    # Doc comments quote attributes -- `LaunchArgs` explains `#[arg(skip)]` in
    # prose above the field it applies to -- so a scanner that reads comments
    # finds a declaration with no field after it. Whole comment lines go first.
    body = "\n".join(
        line for line in body.splitlines() if not line.lstrip().startswith("//")
    )
    accepted: set[str] = set()
    retired: set[str] = set()
    cursor = 0
    while True:
        start = body.find("#[arg(", cursor)
        if start < 0:
            break
        end = _balanced(body, start + len("#[arg"), "(", ")")
        attrs = body[start + len("#[arg(") : end - 1]
        cursor = body.find("]", end) + 1
        field_match = _CLAP_FIELD_AFTER_ATTRS.match(body, cursor)
        if field_match is None:
            raise EvidenceError(
                f"a #[arg(...)] in {struct!r} is not followed by a field name"
            )
        field = field_match.group(1)
        if re.search(r"\bskip\b", attrs):
            continue
        named = _CLAP_LONG_NAMED.search(attrs)
        if named is not None:
            option = f"--{named.group('name')}"
        elif re.search(r"(^|[,(\s])long([,)\s]|$)", attrs):
            option = "--" + field.replace("_", "-")
        else:
            continue
        (retired if re.search(r"hide\s*=\s*true", attrs) else accepted).add(option)
    if not accepted:
        raise EvidenceError(f"clap struct {struct!r} declared no long options")
    return accepted, retired


# ---- Real Claude turns this ledger never saw --------------------------------
#
# The ledger is the authority for RESERVED attempts and is not a census of real
# Claude turns: `pmux ask`, `pmux turn` and the `PMUX_POOL_REAL_CLAUDE` lanes
# all reach a real model and reserve nothing. `evidence/README.md` says so in
# prose; this is the same statement with a predicate, so `phase0.py budget`
# reports the shortfall instead of leaving a reader to find the paragraph.
#
# Keyed on a WITNESS each receipt already carries, with NO default: a receipt
# this table cannot classify stops the count rather than being silently read as
# zero, which is the failure mode a budget cannot have.
UNCOUNTED_TURN_RECEIPTS: tuple[tuple[str, str], ...] = (
    ("pmux-claude-version-promotion-v1", "algorithm"),
    ("pmux-turn-latency-v1", "algorithm"),
    # 24 real Sonnet 5 turns, driven through `pmux ask`, which reserves no
    # ordinal. Keyed on `schema` rather than `algorithm` because this receipt is
    # a measurement of pmux, not the output of a named algorithm -- the table
    # already takes the field name per row for exactly that reason.
    ("pmux.screen-veto-cost.v1", "schema"),
    # The live adversarial suite: real Sonnet 5 turns through `pmux ask`, again
    # reserving no ordinal. Its reader COUNTS THE RECORDS rather than reading a
    # stated total, so a receipt that adds a turn without adding a record is
    # counted at the number of turns it can name.
    ("pmux.live-adversarial-suite.v1", "schema"),
)


def _promotion_turns(receipt: Mapping[str, Any]) -> int:
    declared = receipt.get("real_claude_turns") or {}
    count = declared.get("count")
    if not isinstance(count, int) or isinstance(count, bool) or count < 0:
        raise EvidenceError(
            "a promotion receipt states no `real_claude_turns.count`, so the "
            "turns it spent cannot be counted"
        )
    return count


def _turn_latency_turns(receipt: Mapping[str, Any]) -> int:
    driver = receipt.get("driver") or {}
    if driver.get("environment") != "operator" or driver.get("zero_latency"):
        return 0
    total = 0
    for path in ("path_a", "path_b"):
        samples = (receipt.get(path) or {}).get("samples")
        if samples is None:
            continue
        if not isinstance(samples, list):
            raise EvidenceError(f"{path}.samples is not a list of turns")
        total += len(samples)
    return total


def _screen_veto_cost_turns(receipt: Mapping[str, Any]) -> int:
    total = (receipt.get("turns") or {}).get("total")
    if not isinstance(total, int) or isinstance(total, bool) or total < 0:
        raise EvidenceError(
            "a screen-veto-cost receipt states no `turns.total`, so the real "
            "turns it spent cannot be counted"
        )
    return total


def _live_adversarial_turns(receipt: Mapping[str, Any]) -> int:
    """One per record, counted, not read off a stated total.

    `_screen_veto_cost_turns` above takes `turns.total` because that receipt's
    per-turn array is a sample of what it summarises. This one's array IS the
    run: every real turn the suite spent has a record, including the ones whose
    results were discarded, so counting the records cannot disagree with the
    turns spent the way a hand-maintained total can.
    """

    records = (receipt.get("turns") or {}).get("records")
    if not isinstance(records, list):
        raise EvidenceError(
            "a live-adversarial-suite receipt carries no `turns.records` list, so "
            "the real turns it spent cannot be counted"
        )
    return len(records)


_UNCOUNTED_TURN_READERS = {
    "pmux-claude-version-promotion-v1": _promotion_turns,
    "pmux-turn-latency-v1": _turn_latency_turns,
    "pmux.screen-veto-cost.v1": _screen_veto_cost_turns,
    "pmux.live-adversarial-suite.v1": _live_adversarial_turns,
}

# Committed artifacts that are the receipt for something other than a model
# turn, and that SAY SO in a `schema` field. Named by that field rather than
# sniffed for a distinguishing key, because "this document happens to hold
# `failing_conditions`" is a statement about a schema nobody wrote down, and the
# next such artifact to land will not hold it either. Adding a name here is the
# same decision the tuple above records: it says the file spends no turns.
NO_TURN_RECEIPT_SCHEMAS: tuple[str, ...] = (
    "pmux.mutation-survivor-register.v1",
    "pmux.path-b-defect-register.v1",
    # A filtered `cargo mutants` run: it compiles and tests mutants of committed
    # source and reaches no model at all.
    "pmux.mutation-filtered-run.v1",
    # The census of what one full-scope campaign enumerated: `cargo mutants
    # --list` over committed source, which parses files and calls nothing.
    "pmux.mutation-enumeration.v1",
)


def real_claude_turns_outside_the_ledger(evidence_dir: Path) -> dict[str, Any]:
    """Every committed receipt for real model turns that reserved no ordinal.

    Returns the per-receipt counts and their total. It does NOT change
    `consumed` or `remaining`: whether decision D4's ceiling was meant to cover
    instrument runs as well as campaigns is the owner's question, and a tool
    that re-priced it silently would be answering it.
    """

    receipts: dict[str, int] = {}
    unclassified: list[str] = []
    for path in sorted(evidence_dir.glob("*.json")):
        payload, _ = read_bounded_regular_file(path, MAX_ARTIFACT_TREE_BYTES)
        document = strict_json_loads(payload.decode("utf-8"), label=str(path))
        if not isinstance(document, dict):
            unclassified.append(path.name)
            continue
        if document.get("schema") in NO_TURN_RECEIPT_SCHEMAS:
            continue
        witness = next(
            (
                value
                for value, field in UNCOUNTED_TURN_RECEIPTS
                if document.get(field) == value
            ),
            None,
        )
        if witness is None:
            # A receipt with no turn witness is a corpus or verifier artifact:
            # it reads files that already exist and calls no model. Named here
            # so that "not a turn receipt" is a decision rather than a default.
            if "recommended_transcript_drain_ms" in document or (
                "failing_conditions" in document
            ):
                continue
            unclassified.append(path.name)
            continue
        receipts[path.name] = _UNCOUNTED_TURN_READERS[witness](document)
    if unclassified:
        raise EvidenceError(
            "the evidence directory holds receipt(s) this budget cannot "
            f"classify as spending turns or not: {unclassified}. Classify them "
            "in UNCOUNTED_TURN_RECEIPTS rather than letting a real turn go "
            "uncounted"
        )
    return {
        "receipts": receipts,
        "total": sum(receipts.values()),
        "note": "real Claude turns behind committed receipts that reserved no "
        "ledger ordinal. NOT added to `consumed`: decision D4 -- whether the "
        "ceiling covers instrument runs as well as campaigns -- is the owner's, "
        "and this figure exists so the question is asked against a number. It "
        "is also a LOWER bound: the PMUX_POOL_REAL_CLAUDE e2e lanes reach a real "
        "model, reserve nothing and leave no receipt, so nothing can count them.",
    }


def _validate_prefix_last(lines: Sequence[bytes], prefix: LedgerPrefix) -> None:
    recognized = _recognized_prefix_last(lines)
    if recognized is not None and recognized != prefix.last_global_attempt:
        raise EvidenceError(
            "explicit prefix last global attempt disagrees with recognized immutable records"
        )


def _validate_reservation_record(record: Mapping[str, Any]) -> None:
    required = {
        "schema",
        "reserved_at",
        "status",
        "campaign_id",
        "run_id",
        "attempt_id",
        "campaign_contract",
        "campaign_contract_sha256",
        "global_attempt_ordinal",
        "global_attempt_ceiling",
        "prior_observed_tokens",
        "ledger_prefix_records",
        "ledger_prefix_sha256",
        "previous_ledger_sha256",
        "previous_reservation_sha256",
        "artifact_directory",
        "scenario",
        "scenario_role",
        "session_id",
        "generation_id",
        "turn_id",
        "prompt_suite_index",
        "source",
        "binaries",
        "public_entrypoint",
        "exercised_binaries",
        "claude",
        "rmux",
        "platform",
        "cell",
        "prompt",
        "environment",
        "reservation_sha256",
    }
    if set(record) != required or record.get("schema") != RESERVATION_SCHEMA:
        raise EvidenceError("post-prefix ledger record has an unknown schema")
    if (
        not isinstance(record.get("reserved_at"), str)
        or not record["reserved_at"]
        or record.get("status") != "reserved_before_possible_claude_launch"
    ):
        raise EvidenceError("reservation status/timestamp is invalid")
    claimed = record.get("reservation_sha256")
    if not isinstance(claimed, str) or HEX_SHA256.fullmatch(claimed) is None:
        raise EvidenceError("reservation record has an invalid digest")
    body = dict(record)
    del body["reservation_sha256"]
    if sha256_bytes(canonical_json_bytes(body)) != claimed:
        raise EvidenceError("reservation record digest does not match its content")
    campaign_id = record.get("campaign_id")
    contract = record.get("campaign_contract")
    contract_digest = record.get("campaign_contract_sha256")
    if not isinstance(campaign_id, str) or not isinstance(contract, dict):
        raise EvidenceError("reservation record has no campaign contract")
    _validate_campaign_contract(contract, expected_campaign_id=campaign_id)
    if (
        not isinstance(contract_digest, str)
        or HEX_SHA256.fullmatch(contract_digest) is None
        or campaign_contract_sha256(contract) != contract_digest
    ):
        raise EvidenceError("reservation campaign contract digest is invalid")
    for key in ("attempt_id", "session_id"):
        value = record.get(key)
        if not isinstance(value, str):
            raise EvidenceError(f"reservation {key} is invalid")
        _validate_uuid(value, f"reservation {key}")
    run_id = record.get("run_id")
    if not isinstance(run_id, str):
        raise EvidenceError("reservation run identity is invalid")
    _validate_uuid(run_id, "reservation run ID")
    turn_id = record.get("turn_id")
    if turn_id is not None:
        if not isinstance(turn_id, str):
            raise EvidenceError("reservation turn identity is invalid")
        _validate_uuid(turn_id, "reservation turn ID")
    artifact_directory = record.get("artifact_directory")
    if (
        not isinstance(artifact_directory, str)
        or not Path(artifact_directory).is_absolute()
    ):
        raise EvidenceError("reservation artifact directory is invalid")
    if record.get("global_attempt_ceiling") != contract.get("global_attempt_ceiling"):
        raise EvidenceError("reservation global ceiling differs from its contract")
    ordinal = record.get("global_attempt_ordinal")
    prefix_records = record.get("ledger_prefix_records")
    prefix_digest = record.get("ledger_prefix_sha256")
    previous_ledger = record.get("previous_ledger_sha256")
    previous_reservation = record.get("previous_reservation_sha256")
    if (
        not isinstance(ordinal, int)
        or isinstance(ordinal, bool)
        or ordinal <= 0
        or not isinstance(prefix_records, int)
        or isinstance(prefix_records, bool)
        or prefix_records < 0
        or not isinstance(prefix_digest, str)
        or HEX_SHA256.fullmatch(prefix_digest) is None
        or not isinstance(previous_ledger, str)
        or HEX_SHA256.fullmatch(previous_ledger) is None
        or not (
            previous_reservation is None
            or (
                isinstance(previous_reservation, str)
                and HEX_SHA256.fullmatch(previous_reservation) is not None
            )
        )
    ):
        raise EvidenceError("reservation ledger/ordinal binding is invalid")
    prior_usage = record.get("prior_observed_tokens")
    if (
        not isinstance(prior_usage, int)
        or isinstance(prior_usage, bool)
        or prior_usage < 0
    ):
        raise EvidenceError("reservation prior observed-token accounting is invalid")
    generation_id = record.get("generation_id")
    if generation_id is not None:
        if not isinstance(generation_id, str):
            raise EvidenceError("reservation generation identity is invalid")
        _validate_uuid(generation_id, "reservation generation ID")
    suite_index = record.get("prompt_suite_index")
    prompt_suite = contract.get("prompt_suite")
    if (
        not isinstance(suite_index, int)
        or isinstance(suite_index, bool)
        or not isinstance(prompt_suite, list)
        or not 1 <= suite_index <= len(prompt_suite)
        or record.get("prompt") != prompt_suite[suite_index - 1]
    ):
        raise EvidenceError("reservation prompt differs from its campaign suite")
    candidate = contract["candidate"]
    expected_entrypoint = (
        "claude-p" if contract["scenario"] == "claude-p-one-shot" else "pmux"
    )
    expected_exercised = {"pmuxd", "pmux-rmuxd", "pmux-launcher"}
    expected_exercised.add(expected_entrypoint)
    if contract["cell"]["lifecycle"] == "hybrid":
        expected_exercised.add("pmux-hook")
    scenario_role = record.get("scenario_role")
    if (
        record.get("source") != candidate["source"]
        or record.get("binaries") != candidate["binaries"]
        or record.get("claude") != candidate["claude"]
        or record.get("rmux") != candidate["rmux"]
        or record.get("platform") != contract["platform"]
        or record.get("environment") != contract["environment"]
        or record.get("scenario") != contract["scenario"]
        or record.get("public_entrypoint") != expected_entrypoint
        or record.get("exercised_binaries") != sorted(expected_exercised)
        or not isinstance(scenario_role, str)
        or SAFE_NAME.fullmatch(scenario_role) is None
        or record.get("cell")
        != {
            **contract["cell"],
            "model": contract["authorization"]["launch_model_selector"],
            "effort": contract["authorization"]["selected_effort"],
            "auth_policy": contract["authorization"]["auth_policy"],
        }
    ):
        raise EvidenceError("reservation differs from its immutable campaign contract")


def exercised_binaries(config: CampaignConfig) -> tuple[str, ...]:
    names = {"pmuxd", "pmux-rmuxd", "pmux-launcher"}
    names.add("claude-p" if config.scenario == "claude-p-one-shot" else "pmux")
    if config.lifecycle == "hybrid":
        names.add("pmux-hook")
    return tuple(sorted(names))


def build_campaign_contract(
    config: CampaignConfig,
    *,
    source_identity: Mapping[str, Any],
    binary_identities: Mapping[str, FileIdentity],
    claude_identity: FileIdentity,
    claude_version_identity: Mapping[str, Any],
    cwd_identity: DirectoryIdentity,
    prompt_identities: Sequence[PromptIdentity],
    environment: Mapping[str, str],
    tested_profile_identity: FileIdentity | None,
    tested_profile_value: Mapping[str, Any] | None = None,
    agent_file_identity: FileIdentity | None = None,
    system_prompt_file_identity: FileIdentity | None = None,
) -> dict[str, Any]:
    """Build the one run-independent authorization object for a campaign ID."""

    contract = {
        "schema": CAMPAIGN_CONTRACT_SCHEMA,
        "campaign_id": config.campaign_id,
        "global_attempt_ceiling": config.global_attempt_ceiling,
        "max_observed_tokens": config.max_observed_tokens,
        "authorization": {
            "launch_model_selector": config.model,
            "allowed_public_model_ids": sorted(config.allowed_model_ids),
            "approved_efforts": list(APPROVED_EFFORTS),
            "selected_effort": config.effort,
            "auth_policy": "subscription",
        },
        "candidate": {
            "source": dict(source_identity),
            "binaries": {
                name: identity.public()
                for name, identity in sorted(binary_identities.items())
            },
            "claude": {
                "binary": claude_identity.public(),
                "version_output": dict(claude_version_identity),
            },
            "rmux": {
                "sdk_version": RMUX_SDK_VERSION,
                "sidecar_binary_sha256": binary_identities["pmux-rmuxd"].sha256,
            },
        },
        "platform": current_platform_identity(),
        "cwd": cwd_identity.public(),
        "prompt_suite": [
            {"suite_index": index, **identity.public()}
            for index, identity in enumerate(prompt_identities, start=1)
        ],
        "environment": environment_identity(environment),
        "scenario": config.scenario,
        "cell": {
            "output_format": config.output_format,
            "resume_session_id": config.resume_session_id,
            "terminal_rows": config.terminal_rows,
            "terminal_cols": config.terminal_cols,
            "terminal_profile": config.terminal_profile,
            "input_transport": config.input_transport,
            "lifecycle": config.lifecycle,
            "compatibility": config.compatibility,
            "untested_transcript_drain_ms": config.untested_transcript_drain_ms,
            "turn_timeout_seconds": config.turn_timeout_seconds,
            "daemon_ready_timeout_seconds": config.daemon_ready_timeout_seconds,
            "daemon_shutdown_timeout_seconds": config.daemon_shutdown_timeout_seconds,
            "tested_profile_file": (
                tested_profile_identity.public(include_path=False)
                if tested_profile_identity is not None
                else None
            ),
            "tested_profile": (
                dict(tested_profile_value) if tested_profile_value is not None else None
            ),
            "launch_options": launch_options_binding(
                config, agent_file_identity, system_prompt_file_identity
            ),
        },
    }
    _validate_campaign_contract(contract, expected_campaign_id=config.campaign_id)
    return contract


def launch_options_binding(
    config: CampaignConfig,
    agent_file_identity: FileIdentity | None,
    system_prompt_file_identity: FileIdentity | None = None,
) -> dict[str, Any]:
    return {
        **launch_options_identity(config),
        "agent_file": (
            agent_file_identity.public(include_path=False)
            if agent_file_identity is not None
            else None
        ),
        "system_prompt_file": (
            system_prompt_file_identity.public(include_path=False)
            if system_prompt_file_identity is not None
            else None
        ),
    }


def campaign_contract_sha256(contract: Mapping[str, Any]) -> str:
    return sha256_bytes(canonical_json_bytes(dict(contract)))


def _exact_nonnegative_integer(
    value: Any,
    label: str,
    *,
    minimum: int = 0,
    maximum: int | None = None,
) -> int:
    if type(value) is not int or value < minimum:
        raise EvidenceError(f"{label} is not an exact bounded integer")
    if maximum is not None and value > maximum:
        raise EvidenceError(f"{label} is not an exact bounded integer")
    return value


def _validate_public_file_identity(
    value: Any,
    label: str,
    *,
    include_path: bool,
    require_executable: bool = False,
) -> dict[str, Any]:
    path_field = "path" if include_path else "path_sha256"
    expected = {
        path_field,
        "sha256",
        "size",
        "device",
        "inode",
        "uid",
        "link_count",
        "mode",
        "modified_ns",
        "changed_ns",
    }
    if not isinstance(value, dict) or set(value) != expected:
        raise EvidenceError(f"{label} has an unexpected file-identity schema")
    if HEX_SHA256.fullmatch(str(value.get("sha256"))) is None:
        raise EvidenceError(f"{label} has an invalid content digest")
    if include_path:
        path = value.get("path")
        if (
            not isinstance(path, str)
            or not path
            or len(path.encode("utf-8", errors="surrogateescape")) > 16_384
            or "\x00" in path
            or not Path(path).is_absolute()
        ):
            raise EvidenceError(f"{label} has an invalid absolute path")
    elif HEX_SHA256.fullmatch(str(value.get("path_sha256"))) is None:
        raise EvidenceError(f"{label} has an invalid path digest")
    _exact_nonnegative_integer(
        value.get("size"), label + " size", maximum=512 * 1024 * 1024
    )
    _exact_nonnegative_integer(value.get("device"), label + " device")
    _exact_nonnegative_integer(value.get("inode"), label + " inode")
    _exact_nonnegative_integer(value.get("uid"), label + " owner")
    _exact_nonnegative_integer(
        value.get("link_count"), label + " link count", minimum=1
    )
    mode = _exact_nonnegative_integer(
        value.get("mode"), label + " mode", maximum=0o7777
    )
    _exact_nonnegative_integer(value.get("modified_ns"), label + " modification time")
    _exact_nonnegative_integer(value.get("changed_ns"), label + " change time")
    if require_executable and mode & 0o111 == 0:
        raise EvidenceError(f"{label} is not executable")
    return dict(value)


def _validate_source_identity(value: Any) -> dict[str, Any]:
    expected = {
        "schema",
        "algorithm",
        "implementation",
        "algorithm_sha256",
        "digest",
        "file_count",
        "revision_identity",
        "revision_identity_sha256",
        "phase0_evidence_authorities",
    }
    if (
        not isinstance(value, dict)
        or set(value) != expected
        or value.get("schema") != SOURCE_SCHEMA
        or value.get("implementation")
        != "tools/linux-docker/source_digest.py::workspace_source_manifest"
    ):
        raise EvidenceError("campaign contract source identity is invalid")
    algorithm = value.get("algorithm")
    if (
        not isinstance(algorithm, str)
        or not algorithm
        or len(algorithm.encode("utf-8")) > 512
        or HEX_SHA256.fullmatch(str(value.get("algorithm_sha256"))) is None
        or HEX_SHA256.fullmatch(str(value.get("digest"))) is None
        or HEX_SHA256.fullmatch(str(value.get("revision_identity_sha256"))) is None
    ):
        raise EvidenceError("campaign contract source identity is invalid")
    _exact_nonnegative_integer(
        value.get("file_count"),
        "campaign contract source file count",
        minimum=1,
        maximum=10_000_000,
    )
    try:
        revision_identity = source_digest.validate_workspace_revision_identity(
            value.get("revision_identity")
        )
    except Exception as error:
        raise EvidenceError("campaign contract revision identity is invalid") from error
    if value["revision_identity_sha256"] != _revision_identity_sha256(
        revision_identity
    ):
        raise EvidenceError("campaign contract revision identity digest disagrees")
    if value["algorithm_sha256"] != SOURCE_DIGEST_AUTHORITY["sha256"]:
        raise EvidenceError("campaign contract source authority is not the loaded one")
    if value.get("phase0_evidence_authorities") != evidence_authorities():
        raise EvidenceError("campaign contract process/source authorities disagree")
    normalized = dict(value)
    normalized["revision_identity"] = revision_identity
    return normalized


def _validate_claude_version_identity(value: Any) -> dict[str, Any]:
    expected = {
        "stdout_sha256",
        "stderr_sha256",
        "combined_sha256",
        "stdout_bytes",
        "stderr_bytes",
        "normalized_version",
        "stdout_text",
    }
    if not isinstance(value, dict) or set(value) != expected:
        raise EvidenceError("campaign contract Claude version identity is invalid")
    for field in ("stdout_sha256", "stderr_sha256", "combined_sha256"):
        if HEX_SHA256.fullmatch(str(value.get(field))) is None:
            raise EvidenceError("campaign contract Claude version identity is invalid")
    stdout_bytes = _exact_nonnegative_integer(
        value.get("stdout_bytes"),
        "Claude version stdout size",
        maximum=MAX_CAPTURE_BYTES,
    )
    stderr_bytes = _exact_nonnegative_integer(
        value.get("stderr_bytes"),
        "Claude version stderr size",
        maximum=MAX_CAPTURE_BYTES,
    )
    if stdout_bytes + stderr_bytes > MAX_CAPTURE_BYTES:
        raise EvidenceError("campaign contract Claude version output is oversized")
    normalized = value.get("normalized_version")
    stdout_text = value.get("stdout_text")
    if (
        not isinstance(normalized, str)
        or EXACT_CLAUDE_VERSION.fullmatch(normalized) is None
        or not isinstance(stdout_text, str)
        or len(stdout_text.encode("utf-8")) > MAX_CAPTURE_BYTES
    ):
        raise EvidenceError("campaign contract Claude version identity is invalid")
    stdout_payload = stdout_text.encode("utf-8")
    if (
        len(stdout_payload) != stdout_bytes
        or sha256_bytes(stdout_payload) != value["stdout_sha256"]
    ):
        raise EvidenceError("campaign contract Claude version stdout binding disagrees")
    try:
        text_version = normalize_claude_version_output(stdout_payload)
    except EvidenceError as error:
        raise EvidenceError(
            "campaign contract Claude version stdout has no normalized version"
        ) from error
    if text_version != normalized:
        raise EvidenceError("campaign contract Claude version identities disagree")
    return dict(value)


def _validate_launch_options(value: Any, *, scenario: str) -> dict[str, Any]:
    """Validate the forwarded pmux launch options bound into the contract.

    Two exact shapes are accepted, never a mixture and never an unknown name.
    Contracts written before the minified-cell launch surface existed carry the
    original seven, and `inspect_ledger` re-validates EVERY post-prefix
    reservation on every append: requiring the five newer names would make an
    audit run with a short `--ledger-prefix-records` fail on evidence that is
    not wrong, only older, and would do so after the reader had already trusted
    it. Anything the current tool writes carries all twelve.
    """

    if not isinstance(value, dict) or set(value) not in (
        LEGACY_LAUNCH_OPTION_KEYS,
        LAUNCH_OPTION_KEYS,
    ):
        raise EvidenceError("campaign contract launch options are invalid")
    if value.get("environment_set_values_recorded") is not False:
        raise EvidenceError("campaign contract recorded --env values")
    if value.get("environment_set_delivery") != ENVIRONMENT_SET_DELIVERY:
        raise EvidenceError("campaign contract --env delivery is not name-only")
    permission_mode = value.get("permission_mode")
    allowed_modes = (
        FACADE_PERMISSION_MODES if scenario == "claude-p-one-shot" else PERMISSION_MODES
    )
    if permission_mode is not None and permission_mode not in allowed_modes:
        raise EvidenceError("campaign contract permission mode is invalid")
    for field in ("environment_set_names", "environment_passthrough_names"):
        names = value.get(field)
        if (
            not isinstance(names, list)
            or names != sorted(set(names))
            or len(names) > 64
            or any(
                not isinstance(name, str) or ENVIRONMENT_NAME.fullmatch(name) is None
                for name in names
            )
        ):
            raise EvidenceError(f"campaign contract {field} are invalid")
    agent_name = value.get("agent_name")
    agent_file = value.get("agent_file")
    if agent_name is not None and (
        scenario == "claude-p-one-shot"
        or not isinstance(agent_name, str)
        or AGENT_NAME.fullmatch(agent_name) is None
    ):
        raise EvidenceError("campaign contract agent name is invalid")
    if (agent_name is None) != (agent_file is None):
        raise EvidenceError("campaign contract agent name and file must agree")
    if agent_file is not None:
        _validate_public_file_identity(
            agent_file, "campaign contract agent file", include_path=False
        )
    if set(value) == LEGACY_LAUNCH_OPTION_KEYS:
        return dict(value)
    denied_tools = value.get("denied_tools")
    if (
        not isinstance(denied_tools, list)
        or len(denied_tools) > MAX_DENIED_TOOLS
        or len(set(denied_tools)) != len(denied_tools)
        or any(
            not isinstance(tool, str)
            or DENIED_TOOL_VALUE.fullmatch(tool) is None
            or tool.startswith("-")
            for tool in denied_tools
        )
    ):
        raise EvidenceError("campaign contract denied tools are invalid")
    if value.get("system_prompt_text_recorded") is not False:
        raise EvidenceError("campaign contract recorded a system prompt replacement")
    if value.get("system_prompt_delivery") != SYSTEM_PROMPT_DELIVERY:
        raise EvidenceError("campaign contract system prompt delivery is unrecognized")
    policy = value.get("system_prompt_policy")
    system_prompt_file = value.get("system_prompt_file")
    if policy is not None and policy != SYSTEM_PROMPT_POLICY:
        raise EvidenceError("campaign contract system prompt policy is invalid")
    if (policy is None) != (system_prompt_file is None):
        raise EvidenceError("campaign contract system prompt policy and file disagree")
    if system_prompt_file is not None:
        identity = _validate_public_file_identity(
            system_prompt_file,
            "campaign contract system prompt file",
            include_path=False,
        )
        # The same two properties `read_system_prompt` enforced at admission,
        # re-asserted from the record alone so an auditor never has to trust
        # that the writer checked them.
        if identity["mode"] & 0o077 or identity["link_count"] != 1:
            raise EvidenceError(
                "campaign contract system prompt file was not owner-only and singly linked"
            )
        if not 1 <= identity["size"] <= MAX_SYSTEM_PROMPT_BYTES:
            raise EvidenceError("campaign contract system prompt file size is invalid")
    return dict(value)


def _validate_campaign_contract(
    contract: Mapping[str, Any], *, expected_campaign_id: str | None = None
) -> None:
    required = {
        "schema",
        "campaign_id",
        "global_attempt_ceiling",
        "max_observed_tokens",
        "authorization",
        "candidate",
        "platform",
        "cwd",
        "prompt_suite",
        "environment",
        "scenario",
        "cell",
    }
    if set(contract) != required or contract.get("schema") != CAMPAIGN_CONTRACT_SCHEMA:
        raise EvidenceError("campaign contract has an unexpected schema or field set")
    campaign_id = contract.get("campaign_id")
    if not isinstance(campaign_id, str):
        raise EvidenceError("campaign contract has no campaign identity")
    canonical_campaign = _validate_uuid(campaign_id, "campaign contract ID")
    if expected_campaign_id is not None and canonical_campaign != expected_campaign_id:
        raise EvidenceError("campaign contract belongs to a different campaign")
    ceiling = contract.get("global_attempt_ceiling")
    if (
        not isinstance(ceiling, int)
        or isinstance(ceiling, bool)
        or not MIN_GLOBAL_ATTEMPT_CEILING <= ceiling <= MAX_GLOBAL_ATTEMPT_CEILING
    ):
        raise EvidenceError("campaign contract has an invalid global ceiling")
    budget = contract.get("max_observed_tokens")
    if not isinstance(budget, int) or isinstance(budget, bool) or budget <= 0:
        raise EvidenceError("campaign contract has an invalid observed-token budget")
    authorization = contract.get("authorization")
    if not isinstance(authorization, dict) or set(authorization) != {
        "launch_model_selector",
        "allowed_public_model_ids",
        "approved_efforts",
        "selected_effort",
        "auth_policy",
    }:
        raise EvidenceError("campaign contract authorization is invalid")
    model = authorization.get("launch_model_selector")
    allowed = authorization.get("allowed_public_model_ids")
    if not isinstance(model, str) or MODEL_ID.fullmatch(model) is None:
        raise EvidenceError("campaign contract launch model is invalid")
    if (
        not isinstance(allowed, list)
        or not allowed
        or allowed != sorted(set(allowed))
        or any(
            not isinstance(item, str) or MODEL_ID.fullmatch(item) is None
            for item in allowed
        )
    ):
        raise EvidenceError("campaign contract allowed model IDs are invalid")
    if authorization.get("approved_efforts") != list(APPROVED_EFFORTS):
        raise EvidenceError("campaign contract effort authorization is invalid")
    if authorization.get("selected_effort") not in APPROVED_EFFORTS:
        raise EvidenceError("campaign contract selected effort is invalid")
    if authorization.get("auth_policy") != "subscription":
        raise EvidenceError("campaign contract auth policy is invalid")
    candidate = contract.get("candidate")
    platform_value = contract.get("platform")
    cwd = contract.get("cwd")
    prompt_suite = contract.get("prompt_suite")
    environment_value = contract.get("environment")
    cell = contract.get("cell")
    if not isinstance(candidate, dict) or set(candidate) != {
        "source",
        "binaries",
        "claude",
        "rmux",
    }:
        raise EvidenceError("campaign contract candidate is invalid")
    if not isinstance(platform_value, dict) or not isinstance(cwd, dict):
        raise EvidenceError("campaign contract platform/cwd identity is invalid")
    if set(platform_value) != {"os", "architecture", "kernel_release", "python"}:
        raise EvidenceError("campaign contract platform identity is invalid")
    platform_os = platform_value.get("os")
    platform_arch = platform_value.get("architecture")
    if (
        not isinstance(platform_os, str)
        or not isinstance(platform_arch, str)
        or len(platform_os) > 64
        or len(platform_arch) > 64
        or PLATFORM_COMPONENT.fullmatch(platform_os) is None
        or PLATFORM_COMPONENT.fullmatch(platform_arch) is None
        or any(
            not isinstance(platform_value.get(field), str)
            or not platform_value[field]
            or len(platform_value[field].encode("utf-8")) > 512
            for field in ("kernel_release", "python")
        )
    ):
        raise EvidenceError("campaign contract platform identity is invalid")
    if set(cwd) != {"canonical_path_sha256", "device", "inode", "uid", "mode"}:
        raise EvidenceError("campaign contract cwd identity is invalid")
    if HEX_SHA256.fullmatch(str(cwd.get("canonical_path_sha256"))) is None:
        raise EvidenceError("campaign contract cwd fields are invalid")
    for field in ("device", "inode", "uid"):
        _exact_nonnegative_integer(cwd.get(field), f"campaign contract cwd {field}")
    _exact_nonnegative_integer(
        cwd.get("mode"), "campaign contract cwd mode", maximum=0o7777
    )
    if (
        not isinstance(prompt_suite, list)
        or not prompt_suite
        or any(not isinstance(item, dict) for item in prompt_suite)
        or [item.get("suite_index") for item in prompt_suite]
        != list(range(1, len(prompt_suite) + 1))
    ):
        raise EvidenceError("campaign contract prompt suite is invalid")
    for item in prompt_suite:
        if set(item) != {
            "suite_index",
            "sha256",
            "size",
            "device",
            "inode",
            "uid",
            "link_count",
            "mode",
            "modified_ns",
            "changed_ns",
            "path_sha256",
            "content_encoding",
        }:
            raise EvidenceError("campaign contract prompt identity is invalid")
        if item.get("content_encoding") != "caller_supplied_utf8_file":
            raise EvidenceError("campaign contract prompt identity is invalid")
        prompt_file = dict(item)
        del prompt_file["suite_index"]
        del prompt_file["content_encoding"]
        _validate_public_file_identity(
            prompt_file,
            "campaign contract prompt identity",
            include_path=False,
        )
        if (
            prompt_file["size"] < 1
            or prompt_file["size"] > MAX_PROMPT_BYTES
            or prompt_file["link_count"] != 1
        ):
            raise EvidenceError("campaign contract prompt identity is invalid")
    if (
        not isinstance(environment_value, dict)
        or set(environment_value)
        != {
            "entry_count",
            "names_sha256",
            "values_bound_sha256",
            "sensitive_name_count",
            "values_recorded",
        }
        or environment_value.get("values_recorded") is not False
        or HEX_SHA256.fullmatch(str(environment_value.get("names_sha256"))) is None
        or HEX_SHA256.fullmatch(str(environment_value.get("values_bound_sha256")))
        is None
    ):
        raise EvidenceError("campaign contract environment binding is invalid")
    entry_count = _exact_nonnegative_integer(
        environment_value.get("entry_count"),
        "campaign contract environment entry count",
        maximum=1_000_000,
    )
    sensitive_count = _exact_nonnegative_integer(
        environment_value.get("sensitive_name_count"),
        "campaign contract sensitive environment count",
        maximum=entry_count,
    )
    if sensitive_count > entry_count:
        raise EvidenceError("campaign contract environment counts are invalid")
    if (
        not isinstance(cell, dict)
        or set(cell)
        != {
            "output_format",
            "resume_session_id",
            "terminal_rows",
            "terminal_cols",
            "terminal_profile",
            "input_transport",
            "lifecycle",
            "compatibility",
            "untested_transcript_drain_ms",
            "turn_timeout_seconds",
            "daemon_ready_timeout_seconds",
            "daemon_shutdown_timeout_seconds",
            "tested_profile_file",
            "tested_profile",
            "launch_options",
        }
        or contract.get("scenario")
        not in {
            "one-shot",
            "persistent",
            "resume",
            "claude-p-one-shot",
        }
    ):
        raise EvidenceError("campaign contract scenario/cell is invalid")
    scenario = contract["scenario"]
    if cell.get("output_format") not in {"json", "ndjson"}:
        raise EvidenceError("campaign contract output format is invalid")
    resume_session_id = cell.get("resume_session_id")
    if scenario == "resume":
        if not isinstance(resume_session_id, str):
            raise EvidenceError("campaign contract resume identity is invalid")
        _validate_uuid(resume_session_id, "campaign contract resume session ID")
    elif resume_session_id is not None:
        raise EvidenceError("campaign contract has an unexpected resume identity")
    for field in ("terminal_rows", "terminal_cols"):
        _exact_nonnegative_integer(
            cell.get(field), f"campaign contract {field}", minimum=1, maximum=65_535
        )
    if (
        cell.get("terminal_profile") != "transparent"
        or cell.get("input_transport") not in {"auto", "sdk"}
        or cell.get("lifecycle") not in {"transcript", "hybrid"}
        or cell.get("compatibility") not in {"require-tested", "allow-untested"}
    ):
        raise EvidenceError("campaign contract execution cell is invalid")
    for field, maximum in (
        ("untested_transcript_drain_ms", 60_000),
        ("turn_timeout_seconds", MAX_TURN_TIMEOUT_SECONDS),
        ("daemon_ready_timeout_seconds", MAX_DAEMON_READY_TIMEOUT_SECONDS),
        ("daemon_shutdown_timeout_seconds", MAX_DAEMON_SHUTDOWN_TIMEOUT_SECONDS),
    ):
        _exact_nonnegative_integer(
            cell.get(field), f"campaign contract {field}", minimum=1, maximum=maximum
        )
    tested_profile_file = cell.get("tested_profile_file")
    tested_profile = cell.get("tested_profile")
    if cell["compatibility"] == "require-tested":
        _validate_public_file_identity(
            tested_profile_file,
            "campaign contract tested profile",
            include_path=False,
        )
        if tested_profile_file["link_count"] != 1:
            raise EvidenceError("campaign contract tested profile is multiply linked")
        validated_profile = validate_tested_profile(tested_profile)
        canonical_profile_payload = canonical_json_bytes(validated_profile) + b"\n"
        if tested_profile_file["sha256"] != sha256_bytes(
            canonical_profile_payload
        ) or tested_profile_file["size"] != len(canonical_profile_payload):
            raise EvidenceError("campaign contract tested profile file disagrees")
        if (
            validated_profile["os"] != platform_os
            or validated_profile["arch"] != platform_arch
            or validated_profile["terminal_profile"] != cell["terminal_profile"]
            or validated_profile["input_transport"] != "sdk"
        ):
            raise EvidenceError("campaign contract tested profile cell disagrees")
    elif tested_profile_file is not None or tested_profile is not None:
        raise EvidenceError("campaign contract untested cell includes a tested profile")
    _validate_launch_options(cell.get("launch_options"), scenario=scenario)
    if scenario == "claude-p-one-shot" and (
        cell["output_format"] != "json"
        or cell["terminal_rows"] != 24
        or cell["terminal_cols"] != 120
        or cell["terminal_profile"] != "transparent"
        or cell["input_transport"] != "auto"
        or cell["lifecycle"] != "transcript"
        or cell["compatibility"] != "require-tested"
    ):
        raise EvidenceError("campaign contract facade cell is invalid")
    source = candidate.get("source")
    binaries = candidate.get("binaries")
    claude = candidate.get("claude")
    rmux = candidate.get("rmux")
    if (
        not isinstance(binaries, dict)
        or set(binaries) != set(REQUIRED_RELEASE_BINARIES)
        or not isinstance(claude, dict)
        or set(claude) != {"binary", "version_output"}
        or not isinstance(rmux, dict)
        or set(rmux) != {"sdk_version", "sidecar_binary_sha256"}
        or rmux.get("sdk_version") != RMUX_SDK_VERSION
        or HEX_SHA256.fullmatch(str(rmux.get("sidecar_binary_sha256"))) is None
    ):
        raise EvidenceError("campaign contract candidate contents are invalid")
    _validate_source_identity(source)
    release_parents: set[str] = set()
    for name in REQUIRED_RELEASE_BINARIES:
        binary = _validate_public_file_identity(
            binaries[name],
            f"campaign contract {name} binary",
            include_path=True,
            require_executable=True,
        )
        binary_path = Path(binary["path"])
        if binary_path.name != name or binary["link_count"] != 1:
            raise EvidenceError("campaign contract release binary layout is invalid")
        release_parents.add(str(binary_path.parent))
    if len(release_parents) != 1:
        raise EvidenceError("campaign contract release binaries are not co-located")
    claude_binary = _validate_public_file_identity(
        claude["binary"],
        "campaign contract Claude binary",
        include_path=True,
        require_executable=True,
    )
    if claude_binary["link_count"] != 1:
        raise EvidenceError("campaign contract Claude binary is multiply linked")
    _validate_claude_version_identity(claude["version_output"])
    if (
        tested_profile is not None
        and tested_profile["claude_version"]
        != claude["version_output"]["normalized_version"]
    ):
        raise EvidenceError("campaign contract tested profile version disagrees")
    if rmux["sidecar_binary_sha256"] != binaries["pmux-rmuxd"]["sha256"]:
        raise EvidenceError("campaign contract rmux sidecar identity disagrees")
    # Also prove the object remains canonical-JSON encodable without NaN.
    canonical_json_bytes(dict(contract))


def inspect_ledger(
    path: Path, prefix: LedgerPrefix, campaign_id: str
) -> dict[str, Any]:
    _validate_digest(prefix.sha256, "ledger prefix digest")
    _validate_uuid(campaign_id, "campaign ID")
    if not path.exists():
        payload = b""
    else:
        payload, metadata = read_bounded_regular_file(path, MAX_LEDGER_BYTES)
        if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
            raise EvidenceError("attempt ledger is not owned by the current user")
        if stat.S_IMODE(metadata.st_mode) & 0o077:
            raise EvidenceError("attempt ledger must be owner-only")
        if metadata.st_nlink != 1:
            raise EvidenceError("attempt ledger must not have multiple hard links")
    lines = _ledger_lines(payload)
    if prefix.records > len(lines):
        raise EvidenceError("attempt ledger is shorter than the immutable prefix")
    prefix_payload = b"".join(lines[: prefix.records])
    if sha256_bytes(prefix_payload) != prefix.sha256:
        raise EvidenceError("attempt ledger immutable prefix digest changed")
    _validate_prefix_last(lines[: prefix.records], prefix)
    tail: list[dict[str, Any]] = []
    expected_ordinal = prefix.last_global_attempt + 1
    previous_full_digest = sha256_bytes(prefix_payload)
    previous_reservation_digest: str | None = None
    contract_by_campaign: dict[str, str] = {}
    reconstructed = prefix_payload
    for index, line in enumerate(lines[prefix.records :], start=prefix.records + 1):
        record = strict_json_loads(line, label=f"attempt ledger record {index}")
        _validate_reservation_record(record)
        existing_campaign = record.get("campaign_id")
        if not isinstance(existing_campaign, str):
            raise EvidenceError(f"ledger record {index} has no campaign identity")
        _validate_uuid(existing_campaign, f"ledger record {index} campaign ID")
        contract_digest = record["campaign_contract_sha256"]
        previous_contract = contract_by_campaign.setdefault(
            existing_campaign, contract_digest
        )
        if previous_contract != contract_digest:
            raise EvidenceError("one campaign contains mixed immutable contracts")
        if record.get("global_attempt_ordinal") != expected_ordinal:
            raise EvidenceError(
                f"ledger record {index} breaks global attempt continuity"
            )
        if (
            record.get("ledger_prefix_records") != prefix.records
            or record.get("ledger_prefix_sha256") != prefix.sha256
        ):
            raise EvidenceError(
                f"ledger record {index} is not bound to the immutable prefix"
            )
        if record.get("previous_ledger_sha256") != previous_full_digest:
            raise EvidenceError(f"ledger record {index} breaks the append hash chain")
        if record.get("previous_reservation_sha256") != previous_reservation_digest:
            raise EvidenceError(
                f"ledger record {index} breaks the reservation hash chain"
            )
        tail.append(record)
        reconstructed += line
        previous_full_digest = sha256_bytes(reconstructed)
        previous_reservation_digest = record["reservation_sha256"]
        expected_ordinal += 1
    return {
        "record_count": len(lines),
        "tail_count": len(tail),
        "next_global_attempt": expected_ordinal,
        "full_sha256": sha256_bytes(payload),
        "previous_reservation_sha256": previous_reservation_digest,
        "campaign_contract_sha256": contract_by_campaign.get(campaign_id),
        "reservations": tail,
        "campaign_reservations": [
            record for record in tail if record.get("campaign_id") == campaign_id
        ],
    }


def _validate_attempt_outcome(
    outcome: Any,
    reservation: Mapping[str, Any],
    *,
    expected_prior_tokens: int,
    require_success: bool,
) -> int:
    required = {
        "schema",
        "campaign_id",
        "run_id",
        "attempt_id",
        "global_attempt_ordinal",
        "reservation_sha256",
        "campaign_contract_sha256",
        "status",
        "pmux_product_verdict_source",
        "product_semantics_reimplemented",
        "observed_tokens_from_public_result",
        "prior_observed_tokens",
        "cumulative_observed_tokens",
        "public_result_binding",
        "error",
        "commands",
        # Whether the frozen candidate still matched AFTER the public command.
        # Recorded as its own fact rather than folded into `status`, because
        # "did pmux produce a correct result" and "did the harness environment
        # hold still" are independent. This check used to run inside the try
        # that owns the verdict, so a moved `.git/index` timestamp rewrote a
        # completed turn into a product failure.
        #
        # The key is REQUIRED on every outcome, but its value may be None: a
        # failed attempt whose public command never ran (pmux_start_failed,
        # public_handle_mismatch, public_command_not_acquired,
        # socket_identity_changed_before_pmux_start) had no post-command
        # environment to check, and None records that the check never ran
        # rather than pretending it passed. Outcomes written before this key
        # existed omit it entirely and remain rejected -- a deliberate break
        # with pre-existing artifacts, kept because loosening the exact-key
        # fence for superseded evidence would weaken it for all future
        # evidence.
        "post_command_source_check",
    }
    if (
        not isinstance(outcome, dict)
        or set(outcome) != required
        or outcome.get("schema") != ATTEMPT_SCHEMA
    ):
        raise EvidenceError("attempt outcome schema is invalid")
    source_check = outcome.get("post_command_source_check")
    if source_check is not None and (
        not isinstance(source_check, dict)
        or set(source_check) != {"status", "error"}
        or source_check.get("status") not in {"ok", "failed"}
        or not (
            source_check.get("error") is None
            or isinstance(source_check.get("error"), str)
        )
        or (source_check.get("status") == "ok")
        is not (source_check.get("error") is None)
    ):
        raise EvidenceError("attempt post-command source check is invalid")
    if (
        outcome.get("pmux_product_verdict_source") != "public_exit_and_result"
        or outcome.get("product_semantics_reimplemented") is not False
        or not isinstance(outcome.get("commands"), dict)
        or not (outcome.get("error") is None or isinstance(outcome.get("error"), str))
    ):
        raise EvidenceError("attempt outcome authority fields are invalid")
    for key in (
        "campaign_id",
        "run_id",
        "attempt_id",
        "global_attempt_ordinal",
        "reservation_sha256",
        "campaign_contract_sha256",
    ):
        expected = (
            reservation.get("global_attempt_ordinal")
            if key == "global_attempt_ordinal"
            else reservation.get(key)
        )
        if outcome.get(key) != expected:
            raise EvidenceError(f"attempt outcome {key} differs from its reservation")
    status = outcome.get("status")
    if status not in {"pmux_exit_zero", "failed"}:
        raise EvidenceError("attempt outcome has an unknown status")
    if status == "pmux_exit_zero" and source_check is None:
        # A successful attempt by definition ran the public command, so the
        # post-command check always ran; only a pre-command failure may be
        # None.
        raise EvidenceError(
            "successful attempt is missing its post-command source check"
        )
    prior = outcome.get("prior_observed_tokens")
    cumulative = outcome.get("cumulative_observed_tokens")
    if prior != expected_prior_tokens or prior != reservation.get(
        "prior_observed_tokens"
    ):
        raise EvidenceError("attempt prior observed-token accounting disagrees")
    if (
        not isinstance(cumulative, int)
        or isinstance(cumulative, bool)
        or cumulative < prior
    ):
        raise EvidenceError("attempt cumulative observed-token accounting is invalid")
    if status == "failed":
        if require_success:
            raise EvidenceError(
                "a prior campaign attempt failed; later usage cannot be authorized"
            )
        if outcome.get("observed_tokens_from_public_result") is not None:
            raise EvidenceError(
                "failed attempt unexpectedly claims authoritative usage"
            )
        if outcome.get("public_result_binding") is not None:
            raise EvidenceError("failed attempt unexpectedly claims a public result")
        if cumulative != prior:
            raise EvidenceError("failed attempt changed cumulative observed usage")
        return cumulative
    usage = outcome.get("observed_tokens_from_public_result")
    binding = outcome.get("public_result_binding")
    if not isinstance(usage, int) or isinstance(usage, bool) or usage < 0:
        raise EvidenceError("successful attempt has invalid observed usage")
    if cumulative != prior + usage:
        raise EvidenceError("attempt cumulative usage is not its exact durable sum")
    if not isinstance(binding, dict):
        raise EvidenceError("successful attempt has no public result binding")
    if set(binding) != {
        "session_id",
        "generation_id",
        "turn_id",
        "model",
        "claude_version",
        "compatibility",
        "timings",
        "drain_calibration",
    }:
        raise EvidenceError("successful attempt public result binding is invalid")
    timings = turn_timings_binding(binding.get("timings"))
    if binding.get("drain_calibration") != drain_calibration_from_timings(timings):
        raise EvidenceError(
            "attempt drain calibration is not the exact derivation of its timings"
        )
    commands = outcome["commands"]
    if commands.get("public_result_binding") != binding:
        raise EvidenceError(
            "attempt command evidence changed its public result binding"
        )
    expected_session = reservation.get("session_id")
    expected_generation = reservation.get("generation_id")
    expected_turn = reservation.get("turn_id")
    if binding.get("session_id") != expected_session:
        raise EvidenceError("attempt public session differs from its reservation")
    if (
        expected_generation is not None
        and binding.get("generation_id") != expected_generation
    ):
        raise EvidenceError("attempt public generation differs from its reservation")
    if expected_turn is not None and binding.get("turn_id") != expected_turn:
        raise EvidenceError("attempt public turn differs from its reservation")
    if reservation.get("scenario_role") in {
        "fresh_persistent_start_and_turn",
        "resume_start_and_turn",
    }:
        start_binding = commands.get("start_public_binding")
        if (
            not isinstance(start_binding, dict)
            or start_binding.get("session_id") != expected_session
            or start_binding.get("generation_id") != binding.get("generation_id")
        ):
            raise EvidenceError("attempt turn differs from its public start handle")
    contract = reservation["campaign_contract"]
    expected_claude_version = contract["candidate"]["claude"]["version_output"][
        "normalized_version"
    ]
    if (
        binding.get("claude_version") != expected_claude_version
        or binding.get("compatibility", {}).get("claude_version")
        != expected_claude_version
    ):
        raise EvidenceError(
            "attempt public Claude version differs from its frozen version probe"
        )
    tested_profile = contract["cell"]["tested_profile"]
    expected_compatibility = (
        {**tested_profile, "tested": True}
        if tested_profile is not None
        else {
            "claude_version": expected_claude_version,
            "os": contract["platform"]["os"],
            "arch": contract["platform"]["architecture"],
            "terminal_profile": contract["cell"]["terminal_profile"],
            "input_transport": "sdk",
            "tested": False,
            "transcript_drain_ms": contract["cell"]["untested_transcript_drain_ms"],
        }
    )
    if binding.get("compatibility") != expected_compatibility:
        raise EvidenceError(
            "attempt public compatibility differs from its campaign contract"
        )
    allowed_models = contract["authorization"]["allowed_public_model_ids"]
    if binding.get("model") not in allowed_models:
        raise EvidenceError("attempt public model is outside its campaign contract")
    return cumulative


def _reconcile_prior_usage_locked(
    config: CampaignConfig,
    tail_records: Sequence[Mapping[str, Any]],
    *,
    campaign_contract_digest: str,
    current_run_id: str | None,
    current_run_attempt_anchors: Mapping[str, str],
) -> int:
    """Reconstruct prior usage from published evidence while holding the ledger lock."""

    root = _assert_owner_only_directory(config.evidence_root, create=False)
    prior = 0
    prior_run_ids: set[str] = set()
    current_run_attempt_ids: set[str] = set()
    audited_attempt_manifest_by_id: dict[str, str] = {}
    campaign_contract: Mapping[str, Any] | None = None
    for record in tail_records:
        if record.get("campaign_id") != config.campaign_id:
            continue
        if record.get("campaign_contract_sha256") != campaign_contract_digest:
            raise EvidenceError("campaign restart attempted to change its contract")
        if campaign_contract is None:
            campaign_contract = record["campaign_contract"]
        elif campaign_contract != record["campaign_contract"]:
            raise EvidenceError("campaign restart found mixed campaign contracts")
        if record.get("prior_observed_tokens") != prior:
            raise EvidenceError("reservation prior usage breaks durable continuity")
        artifact = Path(record.get("artifact_directory", "")).absolute()
        if (
            artifact.parent != root
            or artifact.name != f"attempt-{record.get('attempt_id')}"
        ):
            raise EvidenceError(
                "prior reservation artifact path is outside evidence root"
            )
        if not artifact.exists():
            raise EvidenceError(
                "a prior reservation has no outcome artifact; usage is unknown"
            )
        audited = audit_artifact_directory(artifact)
        attempt_id = record.get("attempt_id")
        if not isinstance(attempt_id, str):
            raise EvidenceError("prior reservation has no canonical attempt ID")
        audited_manifest = audited.get("manifest_sha256")
        if not isinstance(audited_manifest, str):
            raise EvidenceError("prior attempt has no artifact manifest digest")
        audited_attempt_manifest_by_id[attempt_id] = audited_manifest
        expected_binding = {
            "campaign_id": config.campaign_id,
            "run_id": record.get("run_id"),
            "attempt_id": record.get("attempt_id"),
            "reservation_sha256": record.get("reservation_sha256"),
            "campaign_contract_sha256": campaign_contract_digest,
            "source_digest": record["campaign_contract"]["candidate"]["source"][
                "digest"
            ],
        }
        if audited.get("binding") != expected_binding:
            raise EvidenceError("prior attempt artifact binding is invalid")
        if (
            _read_artifact_json(
                artifact,
                "reservation.json",
                expected_manifest_sha256=audited_manifest,
            )
            != record
        ):
            raise EvidenceError("prior artifact reservation differs from the ledger")
        outcome = _read_artifact_json(
            artifact,
            "outcome.json",
            expected_manifest_sha256=audited_manifest,
        )
        if not isinstance(outcome, dict) or audited.get("status") != outcome.get(
            "status"
        ):
            raise EvidenceError("prior attempt manifest status is invalid")
        prior = _validate_attempt_outcome(
            outcome,
            record,
            expected_prior_tokens=prior,
            require_success=True,
        )
        run_id = record.get("run_id")
        if isinstance(run_id, str):
            if run_id == current_run_id:
                current_run_attempt_ids.add(attempt_id)
                if current_run_attempt_anchors.get(attempt_id) != audited_manifest:
                    raise EvidenceError(
                        "current run attempt differs from its retained manifest anchor"
                    )
            else:
                prior_run_ids.add(run_id)
    if set(current_run_attempt_anchors) != current_run_attempt_ids:
        raise EvidenceError("current run attempt anchors are missing or extraneous")
    if set(config.prior_campaign_anchors) != prior_run_ids:
        raise EvidenceError("prior campaign anchors are missing or extraneous")
    for run_id in sorted(prior_run_ids):
        if campaign_contract is None:
            raise EvidenceError("prior run has no bound campaign contract")
        campaign_path = root / f"campaign-run-{run_id}"
        if not campaign_path.exists():
            raise EvidenceError(
                "a prior campaign run has no final cleanup/acquisition artifact"
            )
        audited = audit_artifact_directory(campaign_path)
        if audited.get("manifest_sha256") != config.prior_campaign_anchors[run_id]:
            raise EvidenceError(
                "prior campaign differs from its externally retained manifest anchor"
            )
        summary = _read_artifact_json(
            campaign_path,
            "campaign.json",
            expected_manifest_sha256=audited["manifest_sha256"],
        )
        cleanup = summary.get("cleanup") if isinstance(summary, dict) else None
        expected_binding = {
            "campaign_id": config.campaign_id,
            "run_id": run_id,
            "campaign_contract_sha256": campaign_contract_digest,
            "source_digest": campaign_contract["candidate"]["source"]["digest"],
            "ledger_prefix_sha256": config.ledger_prefix.sha256,
        }
        if audited.get("binding") != expected_binding:
            raise EvidenceError("prior campaign artifact binding is invalid")
        if (
            not isinstance(summary, dict)
            or summary.get("schema") != CAMPAIGN_SCHEMA
            or summary.get("campaign_contract_sha256") != campaign_contract_digest
            or summary.get("campaign_contract") != campaign_contract
            or summary.get("source") != campaign_contract["candidate"]["source"]
            or audited.get("status") != summary.get("status")
            or summary.get("status") != "acquired"
            or not isinstance(cleanup, dict)
            or cleanup.get("status") != "verified"
            or cleanup.get("external_runtime_removed") is not True
        ):
            raise EvidenceError(
                "prior campaign run did not prove acquisition and cleanup"
            )
        summary_attempts = summary.get("attempts")
        if not isinstance(summary_attempts, list):
            raise EvidenceError("prior campaign has no attempt manifest chain")
        run_attempt_ids = {
            record["attempt_id"]
            for record in tail_records
            if record.get("campaign_id") == config.campaign_id
            and record.get("run_id") == run_id
        }
        summary_attempt_ids = {
            item.get("attempt_id")
            for item in summary_attempts
            if isinstance(item, dict)
        }
        if (
            len(summary_attempt_ids) != len(summary_attempts)
            or summary_attempt_ids != run_attempt_ids
        ):
            raise EvidenceError(
                "prior campaign attempt chain differs from its reservations"
            )
        for item in summary_attempts:
            if not isinstance(item, dict) or set(item) != {
                "global_attempt_ordinal",
                "attempt_id",
                "status",
                "campaign_contract_sha256",
                "cumulative_observed_tokens",
                "evidence_directory",
                "reservation_sha256",
                "artifact_manifest_sha256",
                # Same two fields the live summary now carries; a resumed
                # campaign must accept the shape its predecessor wrote.
                "error",
                "post_command_source_check",
            }:
                raise EvidenceError("prior campaign attempt chain is invalid")
            if item.get(
                "artifact_manifest_sha256"
            ) != audited_attempt_manifest_by_id.get(item["attempt_id"]):
                raise EvidenceError(
                    "prior attempt differs from its campaign manifest chain"
                )
    return prior


def reserve_attempt(
    config: CampaignConfig,
    *,
    attempt_id: str,
    session_id: str,
    generation_id: str | None,
    turn_id: str | None,
    prompt_suite_index: int,
    scenario_role: str,
    campaign_contract: Mapping[str, Any],
    source_identity: Mapping[str, Any],
    binary_identities: Mapping[str, FileIdentity],
    claude_identity: FileIdentity,
    claude_version_identity: Mapping[str, Any],
    prompt_identity: PromptIdentity,
    environment: Mapping[str, str],
    artifact_directory: Path,
    run_id: str,
    tested_profile_identity: FileIdentity | None = None,
    tested_profile_value: Mapping[str, Any] | None = None,
    agent_file_identity: FileIdentity | None = None,
    system_prompt_file_identity: FileIdentity | None = None,
    current_run_attempt_anchors: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    _validate_uuid(attempt_id, "attempt ID")
    _validate_uuid(session_id, "session ID")
    if turn_id is not None:
        _validate_uuid(turn_id, "turn ID")
    if generation_id is not None:
        _validate_uuid(generation_id, "generation ID")
    _validate_uuid(run_id, "run ID")
    _validate_campaign_contract(
        campaign_contract, expected_campaign_id=config.campaign_id
    )
    contract_digest = campaign_contract_sha256(campaign_contract)
    if campaign_contract.get("global_attempt_ceiling") != config.global_attempt_ceiling:
        raise EvidenceError("campaign contract changed the global attempt ceiling")
    if campaign_contract.get("max_observed_tokens") != config.max_observed_tokens:
        raise EvidenceError("campaign contract changed the observed-token budget")
    expected_authorization = {
        "launch_model_selector": config.model,
        "allowed_public_model_ids": sorted(config.allowed_model_ids),
        "approved_efforts": list(APPROVED_EFFORTS),
        "selected_effort": config.effort,
        "auth_policy": "subscription",
    }
    expected_cell = {
        "output_format": config.output_format,
        "resume_session_id": config.resume_session_id,
        "terminal_rows": config.terminal_rows,
        "terminal_cols": config.terminal_cols,
        "terminal_profile": config.terminal_profile,
        "input_transport": config.input_transport,
        "lifecycle": config.lifecycle,
        "compatibility": config.compatibility,
        "untested_transcript_drain_ms": config.untested_transcript_drain_ms,
        "turn_timeout_seconds": config.turn_timeout_seconds,
        "daemon_ready_timeout_seconds": config.daemon_ready_timeout_seconds,
        "daemon_shutdown_timeout_seconds": config.daemon_shutdown_timeout_seconds,
        "tested_profile_file": (
            tested_profile_identity.public(include_path=False)
            if tested_profile_identity is not None
            else None
        ),
        "tested_profile": (
            dict(tested_profile_value) if tested_profile_value is not None else None
        ),
        "launch_options": launch_options_binding(
            config, agent_file_identity, system_prompt_file_identity
        ),
    }
    if (
        campaign_contract.get("authorization") != expected_authorization
        or campaign_contract.get("scenario") != config.scenario
        or campaign_contract.get("cell") != expected_cell
    ):
        raise EvidenceError("campaign behavior differs from its immutable contract")
    expected_candidate = {
        "source": dict(source_identity),
        "binaries": {
            name: identity.public()
            for name, identity in sorted(binary_identities.items())
        },
        "claude": {
            "binary": claude_identity.public(),
            "version_output": dict(claude_version_identity),
        },
        "rmux": {
            "sdk_version": RMUX_SDK_VERSION,
            "sidecar_binary_sha256": binary_identities["pmux-rmuxd"].sha256,
        },
    }
    if source_identity.get("digest") != config.expected_source_digest:
        raise EvidenceError("reservation source differs from the frozen digest")
    if {name: identity.sha256 for name, identity in binary_identities.items()} != dict(
        config.expected_binary_hashes
    ):
        raise EvidenceError("reservation binaries differ from the frozen digests")
    if claude_identity.sha256 != config.expected_claude_sha256:
        raise EvidenceError("reservation Claude binary differs from its frozen digest")
    if campaign_contract.get("candidate") != expected_candidate:
        raise EvidenceError("reservation candidate differs from its campaign contract")
    if campaign_contract.get("environment") != environment_identity(environment):
        raise EvidenceError(
            "reservation environment differs from its campaign contract"
        )
    if campaign_contract.get("platform") != current_platform_identity():
        raise EvidenceError("reservation platform differs from its campaign contract")
    prompt_suite = campaign_contract.get("prompt_suite")
    if (
        not isinstance(prompt_suite_index, int)
        or isinstance(prompt_suite_index, bool)
        or not isinstance(prompt_suite, list)
        or not 1 <= prompt_suite_index <= len(prompt_suite)
        or prompt_suite[prompt_suite_index - 1]
        != {"suite_index": prompt_suite_index, **prompt_identity.public()}
    ):
        raise EvidenceError("attempt prompt is not the bound campaign-suite member")
    evidence_root = _assert_owner_only_directory(config.evidence_root, create=False)
    exact_artifact = artifact_directory.absolute()
    if (
        exact_artifact.parent != evidence_root
        or exact_artifact.name != f"attempt-{attempt_id}"
    ):
        raise EvidenceError("attempt artifact path is outside the exact evidence root")
    ledger_path = config.ledger_path.absolute()
    parent = _assert_private_parent(ledger_path, create=True)
    if ledger_path.parent != parent or not ledger_path.name:
        raise EvidenceError("attempt ledger path is not an exact parent member")
    parent_descriptor, _ = _open_private_directory_nofollow(parent)
    try:
        _verify_open_directory_path_identity(parent, parent_descriptor)
        descriptor = _open_or_create_private_append_file_at(
            ledger_path.name, parent_descriptor, 0o600
        )
    except Exception:
        os.close(parent_descriptor)
        raise
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise EvidenceError(
                "attempt ledger must be one regular non-hardlinked file"
            )
        if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
            raise EvidenceError("attempt ledger is not owned by the current user")
        if stat.S_IMODE(metadata.st_mode) & 0o077:
            raise EvidenceError("attempt ledger must be owner-only")
        os.lseek(descriptor, 0, os.SEEK_SET)
        chunks: list[bytes] = []
        size = 0
        while size <= MAX_LEDGER_BYTES:
            chunk = os.read(descriptor, min(128 * 1024, MAX_LEDGER_BYTES + 1 - size))
            if not chunk:
                break
            chunks.append(chunk)
            size += len(chunk)
        payload = b"".join(chunks)
        if len(payload) > MAX_LEDGER_BYTES:
            raise EvidenceError("attempt ledger exceeds the bounded size")
        lines = _ledger_lines(payload)
        if config.ledger_prefix.records > len(lines):
            raise EvidenceError("attempt ledger is shorter than the immutable prefix")
        prefix_payload = b"".join(lines[: config.ledger_prefix.records])
        if sha256_bytes(prefix_payload) != config.ledger_prefix.sha256:
            raise EvidenceError("attempt ledger immutable prefix digest changed")
        _validate_prefix_last(
            lines[: config.ledger_prefix.records], config.ledger_prefix
        )

        tail_records: list[dict[str, Any]] = []
        expected_ordinal = config.ledger_prefix.last_global_attempt + 1
        previous_full_digest = sha256_bytes(prefix_payload)
        previous_reservation_digest: str | None = None
        reconstructed = prefix_payload
        for index, line in enumerate(
            lines[config.ledger_prefix.records :],
            start=config.ledger_prefix.records + 1,
        ):
            existing = strict_json_loads(line, label=f"attempt ledger record {index}")
            _validate_reservation_record(existing)
            existing_campaign = existing.get("campaign_id")
            if not isinstance(existing_campaign, str):
                raise EvidenceError(f"ledger record {index} has no campaign identity")
            _validate_uuid(existing_campaign, f"ledger record {index} campaign ID")
            if existing.get("global_attempt_ordinal") != expected_ordinal:
                raise EvidenceError(
                    f"ledger record {index} breaks global attempt continuity"
                )
            if (
                existing.get("ledger_prefix_records") != config.ledger_prefix.records
                or existing.get("ledger_prefix_sha256") != config.ledger_prefix.sha256
            ):
                raise EvidenceError(
                    f"ledger record {index} is not bound to the immutable prefix"
                )
            if existing.get("previous_ledger_sha256") != previous_full_digest:
                raise EvidenceError(
                    f"ledger record {index} breaks the append hash chain"
                )
            if (
                existing.get("previous_reservation_sha256")
                != previous_reservation_digest
            ):
                raise EvidenceError(
                    f"ledger record {index} breaks the reservation hash chain"
                )
            tail_records.append(existing)
            reconstructed += line
            previous_full_digest = sha256_bytes(reconstructed)
            previous_reservation_digest = existing["reservation_sha256"]
            expected_ordinal += 1

        existing_contracts = {
            record["campaign_contract_sha256"]
            for record in tail_records
            if record.get("campaign_id") == config.campaign_id
        }
        if existing_contracts and existing_contracts != {contract_digest}:
            raise EvidenceError("campaign restart attempted to change its contract")
        prior_observed_tokens = _reconcile_prior_usage_locked(
            config,
            tail_records,
            campaign_contract_digest=contract_digest,
            current_run_id=run_id,
            current_run_attempt_anchors=current_run_attempt_anchors or {},
        )
        if prior_observed_tokens >= config.max_observed_tokens:
            raise BudgetExhausted("observed public usage reached the campaign guard")

        ordinal = config.ledger_prefix.last_global_attempt + len(tail_records) + 1
        # The GLOBAL count, not the file's own numbering: the refusal below says
        # "global real-Claude attempt ceiling", and the detached reservations are
        # part of that total (`global_attempts_consumed_through`).
        if global_attempts_consumed_through(ordinal) > config.global_attempt_ceiling:
            raise BudgetExhausted("global real-Claude attempt ceiling is exhausted")
        if any(record.get("attempt_id") == attempt_id for record in tail_records):
            raise EvidenceError("attempt ID is already reserved")

        body: dict[str, Any] = {
            "schema": RESERVATION_SCHEMA,
            "reserved_at": utc_now(),
            "status": "reserved_before_possible_claude_launch",
            "campaign_id": config.campaign_id,
            "run_id": run_id,
            "attempt_id": attempt_id,
            "campaign_contract": dict(campaign_contract),
            "campaign_contract_sha256": contract_digest,
            "global_attempt_ordinal": ordinal,
            "global_attempt_ceiling": config.global_attempt_ceiling,
            "prior_observed_tokens": prior_observed_tokens,
            "ledger_prefix_records": config.ledger_prefix.records,
            "ledger_prefix_sha256": config.ledger_prefix.sha256,
            "previous_ledger_sha256": sha256_bytes(payload),
            "previous_reservation_sha256": previous_reservation_digest,
            "artifact_directory": str(exact_artifact),
            "scenario": config.scenario,
            "scenario_role": scenario_role,
            "session_id": session_id,
            "generation_id": generation_id,
            "turn_id": turn_id,
            "prompt_suite_index": prompt_suite_index,
            "source": dict(source_identity),
            "binaries": {
                name: identity.public()
                for name, identity in sorted(binary_identities.items())
            },
            "public_entrypoint": (
                "claude-p" if config.scenario == "claude-p-one-shot" else "pmux"
            ),
            "exercised_binaries": list(exercised_binaries(config)),
            "claude": {
                "binary": claude_identity.public(),
                "version_output": dict(claude_version_identity),
            },
            "rmux": {
                "sdk_version": RMUX_SDK_VERSION,
                "sidecar_binary_sha256": binary_identities["pmux-rmuxd"].sha256,
            },
            "platform": current_platform_identity(),
            "cell": {
                **campaign_contract["cell"],
                "model": config.model,
                "effort": config.effort,
                "auth_policy": "subscription",
            },
            "prompt": {
                "suite_index": prompt_suite_index,
                **prompt_identity.public(),
            },
            "environment": environment_identity(environment),
        }
        body["reservation_sha256"] = sha256_bytes(canonical_json_bytes(body))
        encoded = canonical_json_bytes(body) + b"\n"
        if len(payload) + len(encoded) > MAX_LEDGER_BYTES:
            raise EvidenceError(
                "attempt reservation would exceed the ledger size bound"
            )
        os.lseek(descriptor, 0, os.SEEK_END)
        view = memoryview(encoded)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise EvidenceError("attempt reservation append was short")
            view = view[written:]
        os.fsync(descriptor)
        os.lseek(descriptor, 0, os.SEEK_SET)
        durable = b""
        while len(durable) < len(payload) + len(encoded):
            chunk = os.read(descriptor, len(payload) + len(encoded) - len(durable))
            if not chunk:
                break
            durable += chunk
        if durable != payload + encoded:
            raise EvidenceError("attempt reservation durability verification failed")
        _verify_open_path_identity(
            ledger_path,
            descriptor,
            expected_size=len(payload) + len(encoded),
            parent_descriptor=parent_descriptor,
        )
        os.fsync(parent_descriptor)
        _verify_open_path_identity(
            ledger_path,
            descriptor,
            expected_size=len(payload) + len(encoded),
            parent_descriptor=parent_descriptor,
        )
        _verify_open_directory_path_identity(parent, parent_descriptor)
        return body
    finally:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)
            os.close(parent_descriptor)


def _rename_directory_noreplace_at(
    parent_descriptor: int, source_name: str, destination_name: str
) -> None:
    """Atomically publish one sibling directory through its retained parent."""

    import ctypes

    source_bytes = os.fsencode(source_name)
    destination_bytes = os.fsencode(destination_name)
    library = ctypes.CDLL(None, use_errno=True)
    system = sys_platform()
    if system == "linux" and hasattr(library, "renameat2"):
        operation = library.renameat2
        operation.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        operation.restype = ctypes.c_int
        result = operation(
            parent_descriptor,
            source_bytes,
            parent_descriptor,
            destination_bytes,
            1,
        )
    elif system == "darwin" and hasattr(library, "renameatx_np"):
        operation = library.renameatx_np
        operation.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        operation.restype = ctypes.c_int
        result = operation(
            parent_descriptor,
            source_bytes,
            parent_descriptor,
            destination_bytes,
            0x00000004,
        )
    else:
        raise EvidenceError("host has no supported atomic no-replace rename primitive")
    if result == 0:
        return
    error_number = ctypes.get_errno()
    if error_number in {errno.EEXIST, errno.ENOTEMPTY}:
        raise EvidenceError(f"artifact publication collision: {destination_name}")
    raise EvidenceError(
        f"atomic artifact publication failed: {os.strerror(error_number)}"
    )


def _rename_directory_noreplace(source: Path, destination: Path) -> None:
    """Compatibility wrapper for one exact sibling publication."""

    source = source.absolute()
    destination = destination.absolute()
    if source.parent != destination.parent:
        raise EvidenceError("artifact publication must remain in one parent")
    parent = _assert_owner_only_directory(source.parent, create=False)
    parent_descriptor, _ = _open_private_directory_nofollow(parent)
    try:
        _verify_open_directory_path_identity(parent, parent_descriptor)
        _rename_directory_noreplace_at(parent_descriptor, source.name, destination.name)
        os.fsync(parent_descriptor)
        _verify_open_directory_path_identity(parent, parent_descriptor)
    finally:
        os.close(parent_descriptor)


def _artifact_directory_key(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_uid,
        metadata.st_gid,
        metadata.st_mode,
        metadata.st_nlink,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def _artifact_file_key(metadata: os.stat_result) -> tuple[int, ...]:
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


def _validate_private_artifact_metadata(
    metadata: os.stat_result, *, expect_directory: bool, label: str
) -> None:
    expected = (
        stat.S_ISDIR(metadata.st_mode)
        if expect_directory
        else stat.S_ISREG(metadata.st_mode)
    )
    if not expected or stat.S_ISLNK(metadata.st_mode):
        kind = "directory" if expect_directory else "regular file"
        raise EvidenceError(
            f"artifact entry must be a real non-symlink {kind}: {label}"
        )
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise EvidenceError(f"artifact entry is not owned by the current user: {label}")
    if stat.S_IMODE(metadata.st_mode) & 0o077:
        raise EvidenceError(f"artifact entry is not owner-only: {label}")
    if not expect_directory and metadata.st_nlink != 1:
        raise EvidenceError(f"artifact file has multiple hard links: {label}")


def _open_private_child_directory_at(
    parent_descriptor: int,
    name: str,
    *,
    create: bool,
    expected_device: int,
    allow_internal_name: bool = False,
) -> tuple[int, os.stat_result]:
    if SAFE_NAME.fullmatch(name) is None and not (
        allow_internal_name and SAFE_INTERNAL_NAME.fullmatch(name) is not None
    ):
        raise EvidenceError(f"artifact directory name is unsafe: {name}")
    try:
        before = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
    except FileNotFoundError:
        if not create:
            raise
        os.mkdir(name, mode=0o700, dir_fd=parent_descriptor)
        os.fsync(parent_descriptor)
        before = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
    _validate_private_artifact_metadata(before, expect_directory=True, label=name)
    if before.st_dev != expected_device:
        raise EvidenceError("artifact directory crosses the retained filesystem")
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    descriptor = os.open(name, flags, dir_fd=parent_descriptor)
    try:
        opened = os.fstat(descriptor)
        current = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
        _validate_private_artifact_metadata(opened, expect_directory=True, label=name)
        if _artifact_directory_key(before) != _artifact_directory_key(
            opened
        ) or _artifact_directory_key(opened) != _artifact_directory_key(current):
            raise EvidenceError("artifact directory changed while opening")
        return descriptor, opened
    except Exception:
        os.close(descriptor)
        raise


def _read_private_artifact_file_at(
    parent_descriptor: int, name: str, maximum: int, *, label: str
) -> tuple[bytes, os.stat_result]:
    if SAFE_NAME.fullmatch(name) is None:
        raise EvidenceError(f"artifact file name is unsafe: {name}")
    before = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
    _validate_private_artifact_metadata(before, expect_directory=False, label=label)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(name, flags, dir_fd=parent_descriptor)
    try:
        opened = os.fstat(descriptor)
        _validate_private_artifact_metadata(opened, expect_directory=False, label=label)
        if _artifact_file_key(before) != _artifact_file_key(opened):
            raise EvidenceError(f"artifact file changed while opening: {label}")
        chunks: list[bytes] = []
        total = 0
        while total <= maximum:
            chunk = os.read(descriptor, min(128 * 1024, maximum + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
        payload = b"".join(chunks)
        if len(payload) > maximum:
            raise EvidenceError(f"artifact file exceeds its bound: {label}")
        after = os.fstat(descriptor)
        current = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
        if _artifact_file_key(opened) != _artifact_file_key(
            after
        ) or _artifact_file_key(after) != _artifact_file_key(current):
            raise EvidenceError(f"artifact file changed while reading: {label}")
        return payload, after
    finally:
        os.close(descriptor)


def _artifact_entries_from_descriptor(
    root_descriptor: int,
) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    entry_count = 0
    total_bytes = 0
    root_device = os.fstat(root_descriptor).st_dev

    def walk(descriptor: int, prefix: tuple[str, ...], depth: int) -> None:
        nonlocal entry_count, total_bytes
        if depth > MAX_ARTIFACT_DEPTH:
            raise EvidenceError("artifact tree exceeds its depth bound")
        before = os.fstat(descriptor)
        _validate_private_artifact_metadata(
            before,
            expect_directory=True,
            label="/".join(prefix) or "artifact root",
        )
        if before.st_dev != root_device:
            raise EvidenceError("artifact tree crosses the retained filesystem")
        with os.scandir(descriptor) as iterator:
            names = sorted(entry.name for entry in iterator)
        if len(names) > MAX_ARTIFACT_ENTRIES - entry_count:
            raise EvidenceError("artifact tree exceeds its entry-count bound")
        for name in names:
            if SAFE_NAME.fullmatch(name) is None:
                raise EvidenceError(f"artifact tree contains an unsafe name: {name}")
            entry_count += 1
            relative_parts = (*prefix, name)
            relative = "/".join(relative_parts)
            metadata = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            if stat.S_ISDIR(metadata.st_mode) and not stat.S_ISLNK(metadata.st_mode):
                child, _ = _open_private_child_directory_at(
                    descriptor,
                    name,
                    create=False,
                    expected_device=root_device,
                )
                try:
                    walk(child, relative_parts, depth + 1)
                    current = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                    if _artifact_directory_key(os.fstat(child)) != (
                        _artifact_directory_key(current)
                    ):
                        raise EvidenceError(
                            "artifact directory changed during traversal"
                        )
                finally:
                    os.close(child)
                continue
            payload, current = _read_private_artifact_file_at(
                descriptor, name, MAX_CAPTURE_BYTES, label=relative
            )
            total_bytes += len(payload)
            if total_bytes > MAX_ARTIFACT_TREE_BYTES:
                raise EvidenceError("artifact tree exceeds its cumulative byte bound")
            if relative == "artifact-manifest.json":
                continue
            entries.append(
                {
                    "path": relative,
                    "size": len(payload),
                    "sha256": sha256_bytes(payload),
                    "mode": stat.S_IMODE(current.st_mode),
                }
            )
        with os.scandir(descriptor) as iterator:
            after_names = sorted(entry.name for entry in iterator)
        after = os.fstat(descriptor)
        if names != after_names or _artifact_directory_key(before) != (
            _artifact_directory_key(after)
        ):
            raise EvidenceError("artifact directory changed during traversal")

    walk(root_descriptor, (), 0)
    return sorted(entries, key=lambda item: item["path"])


def _artifact_manifest_from_descriptor(
    root_descriptor: int,
) -> tuple[dict[str, Any], bytes]:
    payload, _ = _read_private_artifact_file_at(
        root_descriptor,
        "artifact-manifest.json",
        MAX_CAPTURE_BYTES,
        label="artifact-manifest.json",
    )
    manifest = strict_json_loads(payload, label="artifact manifest")
    if (
        not isinstance(manifest, dict)
        or set(manifest) != {"schema", "published_at", "status", "binding", "files"}
        or manifest.get("schema") != ARTIFACT_SCHEMA
        or not isinstance(manifest.get("published_at"), str)
        or not isinstance(manifest.get("status"), str)
        or not isinstance(manifest.get("binding"), dict)
    ):
        raise EvidenceError("artifact manifest schema is invalid")
    expected = manifest.get("files")
    if not isinstance(expected, list):
        raise EvidenceError("artifact manifest file list is invalid")
    paths: list[str] = []
    for item in expected:
        if not isinstance(item, dict) or set(item) != {
            "path",
            "size",
            "sha256",
            "mode",
        }:
            raise EvidenceError("artifact manifest file entry is invalid")
        relative = item.get("path")
        digest = item.get("sha256")
        parts = relative.split("/") if isinstance(relative, str) else []
        if (
            not parts
            or any(SAFE_NAME.fullmatch(part) is None for part in parts)
            or type(item.get("size")) is not int
            or not 0 <= item["size"] <= MAX_CAPTURE_BYTES
            or not isinstance(digest, str)
            or HEX_SHA256.fullmatch(digest) is None
            or type(item.get("mode")) is not int
            or not 0 <= item["mode"] <= 0o7777
            or item["mode"] & 0o077
        ):
            raise EvidenceError("artifact manifest file entry is invalid")
        paths.append(relative)
    if paths != sorted(paths) or len(paths) != len(set(paths)):
        raise EvidenceError("artifact manifest file list is not uniquely ordered")
    return manifest, payload


def _audit_artifact_descriptor(
    root_descriptor: int, *, display_path: str
) -> dict[str, Any]:
    manifest, payload = _artifact_manifest_from_descriptor(root_descriptor)
    actual = _artifact_entries_from_descriptor(root_descriptor)
    if actual != manifest["files"]:
        raise EvidenceError(
            f"artifact content does not match its manifest: {display_path}"
        )
    repeated, repeated_payload = _artifact_manifest_from_descriptor(root_descriptor)
    if repeated != manifest or repeated_payload != payload:
        raise EvidenceError("artifact manifest changed during audit")
    return {
        "path": display_path,
        "status": manifest["status"],
        "binding": manifest["binding"],
        "file_count": len(actual),
        "files": actual,
        "manifest_sha256": sha256_bytes(payload),
    }


def _open_artifact_directory_path(
    path: Path,
) -> tuple[Path, int, int, os.stat_result]:
    root = path.absolute()
    parent = _assert_private_parent(root, create=False)
    if root.parent != parent or SAFE_NAME.fullmatch(root.name) is None:
        raise EvidenceError("artifact path is not one safe parent member")
    parent_descriptor, parent_metadata = _open_private_directory_nofollow(parent)
    try:
        _verify_open_directory_path_identity(parent, parent_descriptor)
        root_descriptor, root_metadata = _open_private_child_directory_at(
            parent_descriptor,
            root.name,
            create=False,
            expected_device=parent_metadata.st_dev,
        )
        return root, parent_descriptor, root_descriptor, root_metadata
    except Exception:
        os.close(parent_descriptor)
        raise


class AtomicArtifactDirectory:
    def __init__(self, root: Path, final_name: str) -> None:
        if SAFE_NAME.fullmatch(final_name) is None:
            raise EvidenceError("artifact directory name is unsafe")
        self.root = _assert_owner_only_directory(root, create=True)
        self.root_identity = identify_directory(self.root, require_private=True)
        self.final = self.root / final_name
        self.final_name = final_name
        self.staging_name = f".{final_name}.{uuid.uuid4().hex}.staging"
        self.staging = self.root / self.staging_name
        self.root_descriptor = -1
        self.staging_descriptor = -1
        self._closed = False
        self._published = False
        self.publication_receipt: dict[str, Any] | None = None
        self.root_descriptor, root_metadata = _open_private_directory_nofollow(
            self.root
        )
        try:
            _verify_open_directory_path_identity(self.root, self.root_descriptor)
            os.stat(
                self.final_name,
                dir_fd=self.root_descriptor,
                follow_symlinks=False,
            )
        except FileNotFoundError:
            pass
        else:
            self.close()
            raise EvidenceError(f"artifact directory already exists: {self.final}")
        try:
            self.staging_descriptor, staging_metadata = (
                _open_private_child_directory_at(
                    self.root_descriptor,
                    self.staging_name,
                    create=True,
                    expected_device=root_metadata.st_dev,
                    allow_internal_name=True,
                )
            )
            self.staging_identity = DirectoryIdentity(
                path=str(self.staging.resolve(strict=True)),
                device=staging_metadata.st_dev,
                inode=staging_metadata.st_ino,
                uid=staging_metadata.st_uid,
                mode=stat.S_IMODE(staging_metadata.st_mode),
            )
            _verify_open_directory_path_identity(self.root, self.root_descriptor)
        except Exception:
            self.close()
            raise

    def close(self) -> None:
        if self._closed:
            return
        for descriptor_name in ("staging_descriptor", "root_descriptor"):
            descriptor = getattr(self, descriptor_name, -1)
            if descriptor >= 0:
                try:
                    os.close(descriptor)
                finally:
                    setattr(self, descriptor_name, -1)
        self._closed = True

    def __del__(self) -> None:  # pragma: no cover - last-resort descriptor hygiene
        try:
            self.close()
        except Exception:
            pass

    def _verify_open(self) -> None:
        if self._closed or self.root_descriptor < 0 or self.staging_descriptor < 0:
            raise EvidenceError("artifact writer is closed")
        _verify_open_directory_path_identity(self.root, self.root_descriptor)
        verify_directory_identity(self.staging, self.staging_identity)
        current = os.stat(
            self.staging_name,
            dir_fd=self.root_descriptor,
            follow_symlinks=False,
        )
        if _artifact_directory_key(current) != _artifact_directory_key(
            os.fstat(self.staging_descriptor)
        ):
            raise EvidenceError("artifact staging directory pathname was replaced")

    def write(self, relative: str, data: bytes) -> Path:
        if self._published:
            raise EvidenceError("artifact directory is already published")
        parts = self._relative_parts(relative)
        self._verify_open()
        descriptors = [os.dup(self.staging_descriptor)]
        directory_links: list[tuple[int, int, str]] = []
        try:
            expected_device = os.fstat(self.staging_descriptor).st_dev
            for part in parts[:-1]:
                child, _ = _open_private_child_directory_at(
                    descriptors[-1],
                    part,
                    create=True,
                    expected_device=expected_device,
                )
                directory_links.append((descriptors[-1], child, part))
                descriptors.append(child)
            _write_private_atomic_at(descriptors[-1], parts[-1], data)
            for parent_descriptor, child_descriptor, name in reversed(directory_links):
                current = os.stat(
                    name,
                    dir_fd=parent_descriptor,
                    follow_symlinks=False,
                )
                if _artifact_directory_key(os.fstat(child_descriptor)) != (
                    _artifact_directory_key(current)
                ):
                    raise EvidenceError(
                        "artifact directory changed during descriptor-anchored write"
                    )
        finally:
            for descriptor in reversed(descriptors):
                os.close(descriptor)
        self._verify_open()
        return self.staging.joinpath(*parts)

    def write_json(self, relative: str, value: Any) -> Path:
        return self.write(relative, pretty_json_bytes(value))

    def _relative_parts(self, relative: str) -> tuple[str, ...]:
        value = Path(relative)
        parts = tuple(value.parts)
        if (
            value.is_absolute()
            or not parts
            or len(parts) > MAX_ARTIFACT_DEPTH
            or any(SAFE_NAME.fullmatch(part) is None for part in parts)
        ):
            raise EvidenceError("artifact path must be a safe relative path")
        return parts

    def publish(self, *, status: str, binding: Mapping[str, Any]) -> Path:
        if self._published:
            raise EvidenceError("artifact directory is already published")
        try:
            self._verify_open()
            files = _artifact_entries_from_descriptor(self.staging_descriptor)
            manifest = {
                "schema": ARTIFACT_SCHEMA,
                "published_at": utc_now(),
                "status": status,
                "binding": dict(binding),
                "files": files,
            }
            _write_private_atomic_at(
                self.staging_descriptor,
                "artifact-manifest.json",
                pretty_json_bytes(manifest),
            )
            os.fsync(self.staging_descriptor)
            self._verify_open()
            try:
                os.stat(
                    self.final_name,
                    dir_fd=self.root_descriptor,
                    follow_symlinks=False,
                )
            except FileNotFoundError:
                pass
            else:
                raise EvidenceError(
                    f"artifact directory appeared before publication: {self.final}"
                )
            expected = os.fstat(self.staging_descriptor)
            _rename_directory_noreplace_at(
                self.root_descriptor, self.staging_name, self.final_name
            )
            os.fsync(self.root_descriptor)
            _verify_open_directory_path_identity(self.root, self.root_descriptor)
            published_metadata = os.fstat(self.staging_descriptor)
            final_metadata = os.stat(
                self.final_name,
                dir_fd=self.root_descriptor,
                follow_symlinks=False,
            )
            if (published_metadata.st_dev, published_metadata.st_ino) != (
                expected.st_dev,
                expected.st_ino,
            ) or _artifact_directory_key(final_metadata) != _artifact_directory_key(
                published_metadata
            ):
                raise EvidenceError(
                    "published artifact directory failed its identity fence"
                )
            try:
                os.stat(
                    self.staging_name,
                    dir_fd=self.root_descriptor,
                    follow_symlinks=False,
                )
            except FileNotFoundError:
                pass
            else:
                raise EvidenceError("artifact staging path remained after publication")
            self.publication_receipt = _audit_artifact_descriptor(
                self.staging_descriptor, display_path=str(self.final)
            )
            repeated = os.stat(
                self.final_name,
                dir_fd=self.root_descriptor,
                follow_symlinks=False,
            )
            if _artifact_directory_key(repeated) != _artifact_directory_key(
                published_metadata
            ):
                raise EvidenceError("published artifact directory was replaced")
            self._published = True
            return self.final
        finally:
            self.close()


def redact_text(
    value: str,
    prompts: Iterable[bytes] = (),
    sensitive_values: Iterable[str] = (),
) -> str:
    redacted = value
    for prompt in prompts:
        text = prompt.decode("utf-8", errors="ignore")
        if text:
            redacted = redacted.replace(text, "<redacted-prompt>")
    variants = {
        variant
        for sensitive in sensitive_values
        for variant in _escaped_secret_variants(sensitive)
    }
    for variant in sorted(variants, key=len, reverse=True):
        redacted = redacted.replace(variant, "<redacted-environment-value>")
    redacted = SECRET_PATTERN.sub(
        lambda match: f"{match.group(1)}{match.group(2)}<redacted>", redacted
    )
    return redacted


def run_command(
    argv: Sequence[str],
    *,
    cwd: Path,
    environment: Mapping[str, str],
    timeout_seconds: int,
    argv_shape: Sequence[str],
    stdin_payload: bytes | None = None,
) -> CommandResult:
    """Run one finite native command through the shared exact supervisor."""

    if (
        not argv
        or not isinstance(argv[0], str)
        or not Path(argv[0]).is_absolute()
        or type(timeout_seconds) is not int
        or not 1 <= timeout_seconds <= 86_400
    ):
        raise EvidenceError("finite command launch contract is invalid")
    if stdin_payload is not None and len(stdin_payload) > MAX_PROMPT_BYTES:
        raise EvidenceError("stdin prompt exceeds the envelope bound")
    canonical_cwd = cwd.resolve(strict=True)
    if canonical_cwd != cwd:
        raise EvidenceError("finite command cwd must already be canonical")
    authorities_before = evidence_authorities()
    started = time.monotonic()
    try:
        executable = bounded_process.bind_executable(Path(argv[0]))
        bounded_result = bounded_process.run(
            executable,
            list(argv),
            cwd=canonical_cwd,
            environment=dict(environment),
            timeout_seconds=timeout_seconds,
            drain_timeout_seconds=min(5, timeout_seconds),
            maximum_output_bytes=MAX_CAPTURE_BYTES,
            description="Phase-0 finite public command",
            stdin_bytes=stdin_payload,
        )
        receipt = bounded_process.validate_execution_receipt(bounded_result.receipt)
        bounded_process.verify_receipt_context(
            receipt,
            cwd=canonical_cwd,
            environment=environment,
            stdin_bytes=stdin_payload,
        )
        if (
            receipt["exit_code"] != bounded_result.exit_code
            or receipt["stdout_size"] != len(bounded_result.stdout)
            or receipt["stdout_sha256"] != sha256_bytes(bounded_result.stdout)
            or receipt["stderr_size"] != len(bounded_result.stderr)
            or receipt["stderr_sha256"] != sha256_bytes(bounded_result.stderr)
        ):
            raise EvidenceError("finite command disagrees with its process receipt")
        result = CommandResult(
            argv_shape=tuple(argv_shape),
            returncode=bounded_result.exit_code,
            timed_out=False,
            interrupted=False,
            elapsed_ms=int((time.monotonic() - started) * 1000),
            stdout=bounded_result.stdout,
            stderr=bounded_result.stderr,
            output_limit_exceeded=False,
            supervision_failure_reason=None,
            cleanup_complete=True,
            output_complete=True,
            process_receipt=receipt,
        )
    except bounded_process.BoundedProcessFailure as error:
        failure = error.result
        receipt = bounded_process.validate_failure_receipt(failure.receipt)
        bounded_process.verify_receipt_context(
            receipt,
            cwd=canonical_cwd,
            environment=environment,
            stdin_bytes=stdin_payload,
        )
        if (
            receipt["failure_reason"] != failure.reason
            or receipt["exit_code"] != failure.exit_code
            or receipt["stdout_size"] != len(failure.stdout)
            or receipt["stdout_sha256"] != sha256_bytes(failure.stdout)
            or receipt["stderr_size"] != len(failure.stderr)
            or receipt["stderr_sha256"] != sha256_bytes(failure.stderr)
        ):
            raise EvidenceError("failed command disagrees with its process receipt")
        interrupted = isinstance(error.__cause__, CampaignInterrupted) or (
            failure.reason == "supervisor_interrupted"
        )
        result = CommandResult(
            argv_shape=tuple(argv_shape),
            returncode=failure.exit_code,
            timed_out=failure.reason == "timeout",
            interrupted=interrupted,
            elapsed_ms=int((time.monotonic() - started) * 1000),
            stdout=failure.stdout,
            stderr=failure.stderr,
            output_limit_exceeded=failure.reason == "output_limit",
            supervision_failure_reason=failure.reason,
            cleanup_complete=failure.cleanup_complete,
            output_complete=failure.output_complete,
            process_receipt=receipt,
        )
    except bounded_process.BoundedProcessError as error:
        raise EvidenceError(f"finite command supervision failed: {error}") from error
    if evidence_authorities() != authorities_before:
        raise EvidenceError("process/source authority changed across finite command")
    return result


def parse_public_output(payload: bytes, output_format: str) -> tuple[Any, int]:
    if output_format == "json":
        return strict_json_loads(payload, label="pmux public output"), 1
    try:
        lines = payload.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise EvidenceError("pmux public output is not UTF-8") from error
    records = [
        strict_json_loads(line, label=f"pmux NDJSON record {index}")
        for index, line in enumerate(lines, start=1)
        if line.strip()
    ]
    if not records:
        raise EvidenceError("pmux NDJSON output is empty")
    return records, len(records)


def public_result(value: Any, output_format: str) -> Mapping[str, Any]:
    if output_format == "json":
        if not isinstance(value, dict):
            raise EvidenceError("pmux JSON result is not an object")
        return value
    if not isinstance(value, list):
        raise EvidenceError("pmux NDJSON acquisition is not a record list")
    result_indexes = [
        index
        for index, record in enumerate(value)
        if isinstance(record, dict) and record.get("type") == "result"
    ]
    if result_indexes != [len(value) - 1]:
        raise EvidenceError(
            "pmux NDJSON must end with exactly one public result commit record"
        )
    result = value[-1].get("data")
    if not isinstance(result, dict):
        raise EvidenceError("pmux NDJSON result commit data is not an object")
    return result


def observed_tokens_from_public_result(result: Mapping[str, Any]) -> int:
    usage = result.get("usage")
    combined = usage.get("combined") if isinstance(usage, dict) else None
    if not isinstance(combined, dict):
        raise EvidenceError(
            "pmux public result has no combined usage for the usage guard"
        )
    total = 0
    for field in (
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ):
        value = combined.get(field)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise EvidenceError(f"pmux public result has invalid {field}")
        total += value
    return total


def turn_timings_binding(value: Any) -> dict[str, Any]:
    """Capture the whole published `timings` object verbatim.

    Field names are not compiled in beyond the eight this tool already knows
    (``KNOWN_TURN_TIMING_FIELDS``): every key is carried through so a timing the
    product starts publishing lands in the receipt without a code change here.
    Only the value *shape* is checked — exact non-negative bounded integers.
    """

    if not isinstance(value, dict) or not value:
        raise EvidenceError("pmux public result has no turn timings")
    if len(value) > 32:
        raise EvidenceError("pmux turn timings exceed the reviewed field bound")
    captured: dict[str, Any] = {}
    for name, item in value.items():
        if not isinstance(name, str) or SAFE_NAME.fullmatch(name) is None:
            raise EvidenceError("pmux turn timings have an unusable field name")
        if item is None:
            captured[name] = None
            continue
        captured[name] = _exact_nonnegative_integer(
            item, f"pmux turn timing {name}", maximum=MAX_TURN_TIMING_MS
        )
    for name in ("submitted_at_ms", "completed_at_ms"):
        if not isinstance(captured.get(name), int):
            raise EvidenceError(f"pmux turn timings are missing {name}")
    return captured


def drain_calibration_from_timings(timings: Mapping[str, Any]) -> dict[str, Any]:
    """Derive the one number a drain campaign exists to produce.

    ``late_arrival_gap_ms`` is *how much later than the terminal candidate the
    last transcript row actually arrived*, which is not the same as the drain
    pmux waited. The field carrying it is discovered rather than named: any
    ``timings`` key outside ``KNOWN_TURN_TIMING_FIELDS`` is the product's
    late-arrival observation. An absolute ``*_at_ms`` timestamp is differenced
    against the terminal candidate; any other ``*_ms`` field is already a gap.
    Two unknown fields is ambiguous and fails loudly rather than guessing.

    The difference is kept **signed**. It straddles zero by a few milliseconds
    when no row arrives late — the candidate stamp and the last-activity stamp
    are taken from the same read — so clamping at zero would erase exactly the
    boundary between "the candidate row was the last row" and "one more row
    landed a millisecond later".
    """

    extra = sorted(name for name in timings if name not in KNOWN_TURN_TIMING_FIELDS)
    if len(extra) > 1:
        raise EvidenceError(
            "pmux turn timings publish more than one unrecognized field; "
            "the late-arrival source for drain calibration is ambiguous"
        )
    candidate = timings.get("terminal_candidate_at_ms")
    completed = timings.get("completed_at_ms")
    calibration: dict[str, Any] = {
        "terminal_candidate_at_ms": candidate,
        "completed_at_ms": completed,
        "drain_ms": timings.get("drain_ms"),
        "late_arrival_field": extra[0] if extra else None,
        "late_arrival_basis": None,
        "late_arrival_gap_ms": None,
        "uncomputable_reason": None,
    }
    if not extra:
        calibration["uncomputable_reason"] = "no_late_arrival_field_published"
        return calibration
    field = extra[0]
    observed = timings[field]
    if observed is None:
        calibration["uncomputable_reason"] = "late_arrival_field_absent"
        return calibration
    if not isinstance(candidate, int) or not isinstance(completed, int):
        calibration["uncomputable_reason"] = "no_terminal_candidate_timestamp"
        return calibration
    if field.endswith("_at_ms"):
        if observed > completed:
            raise EvidenceError(
                "pmux late-arrival timestamp is later than the committed turn"
            )
        calibration["late_arrival_basis"] = "absolute_timestamp"
        calibration["late_arrival_gap_ms"] = observed - candidate
        return calibration
    if not field.endswith("_ms"):
        raise EvidenceError(
            "pmux late-arrival timing field is neither a timestamp nor a duration"
        )
    if observed > completed - candidate:
        raise EvidenceError(
            "pmux late-arrival duration exceeds the observed drain window"
        )
    calibration["late_arrival_basis"] = "duration"
    calibration["late_arrival_gap_ms"] = observed
    return calibration


def _nearest_rank(samples: Sequence[int], percentile: int) -> int:
    """Exact integer nearest-rank percentile; no float, no interpolation."""

    index = -(-percentile * len(samples) // 100) - 1
    return samples[min(max(index, 0), len(samples) - 1)]


def summarize_drain_calibration(
    calibrations: Sequence[Mapping[str, Any]],
    *,
    configured_transcript_drain_ms: int | None,
    noise_band_ms: int = ACTOR_POLL_INTERVAL_MS,
) -> dict[str, Any]:
    """Summarize the late-arrival distribution the campaign measured.

    The count that matters is ``no_late_row_attempts`` — attempts whose gap is
    at or below the noise band, i.e. nothing arrived after the terminal
    candidate. A drain justified only by those is calibrated against *absence of
    evidence*, which is much weaker than a measured worst case;
    ``interpretation`` says so in the output so a low number is not read as
    permission to cut the drain.

    ``gap <= 0`` was the wrong rule, and this function used to print the right
    one in ``interpretation`` while contradicting it in the count.
    crates/protocol/src/v1.rs:1313-1319 states it: the difference straddles zero
    by a few milliseconds, positive by the interval between the confirming
    poll's stability measurement and the completion timestamp read, and a
    difference within one actor poll interval of zero reads as "no late rows".
    One +1 ms clock artifact used to flip ``late_row_attempts`` to 1, suppress
    the absence-of-evidence reading, and republish the artifact as a measured
    1 ms lower bound with 1,999 ms of apparent headroom -- an invitation to cut
    ``transcript_drain_ms`` on noise. Gaps inside the band get their own
    ``within_noise_band_attempts`` bucket rather than being hidden, so "we saw
    nothing" and "we saw only noise" stay distinguishable.
    """

    gaps = sorted(
        item["late_arrival_gap_ms"]
        for item in calibrations
        if isinstance(item.get("late_arrival_gap_ms"), int)
    )
    fields = sorted(
        {
            item["late_arrival_field"]
            for item in calibrations
            if item.get("late_arrival_field") is not None
        }
    )
    no_late_row = sum(1 for gap in gaps if gap <= noise_band_ms)
    within_band = sum(1 for gap in gaps if 0 < gap <= noise_band_ms)
    late_row = len(gaps) - no_late_row
    maximum = gaps[-1] if gaps else None
    if not gaps:
        interpretation = (
            "no attempt produced a computable late-arrival gap; this run cannot "
            "calibrate transcript_drain_ms and no reduction is defensible from it"
        )
    elif late_row == 0:
        interpretation = (
            f"every attempt observed a gap at or below the {noise_band_ms} ms "
            f"noise band (one actor poll interval), {within_band} of them inside "
            "the band rather than at or below zero: no transcript row arrived "
            "after the terminal candidate in any turn. This measures the "
            "ABSENCE of late rows, not a worst case, and is much weaker evidence "
            "than a measured gap. There is no measured worst case to publish "
            "headroom against. Do not read it as permission to cut "
            "transcript_drain_ms; re-run with structured prompts before proposing "
            "any reduction"
        )
    else:
        interpretation = (
            f"{late_row} of {len(gaps)} attempts observed a transcript row after "
            f"the terminal candidate; the {maximum} ms maximum is the only measured "
            f"lower bound on a defensible transcript_drain_ms. A gap within one "
            f"actor poll interval of zero ({noise_band_ms} ms) is measurement "
            "noise, not a late row, and prompts that answer in a single flushed "
            "block bias this distribution low"
        )
    # Headroom is only meaningful against a MEASURED worst case. When the
    # largest gap is inside the noise band there is no measured late row at all,
    # and publishing `configured - 1` as headroom turns a clock artifact into an
    # apparent 1,999 ms of proven margin. Leave it null and let the
    # absence-of-evidence interpretation carry the result instead.
    headroom_ms: int | None = None
    if (
        configured_transcript_drain_ms is not None
        and maximum is not None
        and maximum > noise_band_ms
    ):
        headroom_ms = configured_transcript_drain_ms - maximum
    return {
        "schema": DRAIN_CALIBRATION_SCHEMA,
        "attempts_considered": len(calibrations),
        "attempts_without_computable_gap": len(calibrations) - len(gaps),
        "late_arrival_fields": fields,
        "percentile_rule": "nearest_rank_exact_integer",
        "gap_ms": (
            {
                "count": len(gaps),
                "min": gaps[0],
                "median": _nearest_rank(gaps, 50),
                "p95": _nearest_rank(gaps, 95),
                "max": maximum,
            }
            if gaps
            else None
        ),
        "noise_band_ms": noise_band_ms,
        "no_late_row_attempts": no_late_row,
        "within_noise_band_attempts": within_band,
        "late_row_attempts": late_row,
        "configured_transcript_drain_ms": configured_transcript_drain_ms,
        "headroom_ms": headroom_ms,
        "interpretation": interpretation,
    }


def extract_public_handle(value: Any) -> tuple[str, str]:
    if not isinstance(value, dict):
        raise EvidenceError("pmux start output is not an object")
    session_id = value.get("session_id")
    generation_id = value.get("generation_id")
    if not isinstance(session_id, str) or not isinstance(generation_id, str):
        raise EvidenceError("pmux start output is missing its public handle")
    return _validate_uuid(session_id, "public session ID"), _validate_uuid(
        generation_id, "public generation ID"
    )


def _compatibility_binding(
    value: Any,
    config: CampaignConfig,
    tested_profile: Mapping[str, Any] | None = None,
    expected_claude_version: str | None = None,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise EvidenceError("pmux public result has no compatibility report")
    required = {
        "claude_version",
        "os",
        "arch",
        "terminal_profile",
        "input_transport",
        "tested",
        "transcript_drain_ms",
    }
    if set(value) != required:
        raise EvidenceError("pmux compatibility report has an unexpected field set")
    claude_version = value["claude_version"]
    os_name = value["os"]
    arch = value["arch"]
    tested = value["tested"]
    drain = value["transcript_drain_ms"]
    if (
        not isinstance(claude_version, str)
        or len(claude_version) > 128
        or EXACT_CLAUDE_VERSION.fullmatch(claude_version) is None
    ):
        raise EvidenceError("pmux compatibility report has an invalid Claude version")
    if (
        not isinstance(os_name, str)
        or not isinstance(arch, str)
        or len(os_name) > 64
        or len(arch) > 64
        or PLATFORM_COMPONENT.fullmatch(os_name) is None
        or PLATFORM_COMPONENT.fullmatch(arch) is None
    ):
        raise EvidenceError("pmux compatibility report has an invalid platform")
    platform_identity = current_platform_identity()
    if os_name != platform_identity["os"] or arch != platform_identity["architecture"]:
        raise EvidenceError(
            "pmux compatibility report does not match the evidence host"
        )
    if value["terminal_profile"] != config.terminal_profile:
        raise EvidenceError("pmux compatibility report changed the terminal profile")
    if value["input_transport"] != "sdk":
        raise EvidenceError("pmux compatibility report did not resolve SDK input")
    if not isinstance(tested, bool):
        raise EvidenceError("pmux compatibility report has an invalid tested flag")
    if config.compatibility == "require-tested" and not tested:
        raise EvidenceError("pmux accepted an untested cell under require-tested")
    if config.compatibility == "allow-untested" and tested:
        raise EvidenceError("pmux reported a tested cell without an admitted profile")
    if (
        expected_claude_version is not None
        and claude_version != expected_claude_version
    ):
        raise EvidenceError(
            "pmux compatibility version differs from the frozen Claude probe"
        )
    if (
        not isinstance(drain, int)
        or isinstance(drain, bool)
        or not 1 <= drain <= 60_000
    ):
        raise EvidenceError("pmux compatibility report has an invalid transcript drain")
    if not tested and drain != config.untested_transcript_drain_ms:
        raise EvidenceError(
            "pmux untested compatibility drain differs from daemon policy"
        )
    if tested:
        if tested_profile is None:
            raise EvidenceError(
                "pmux reported a tested cell without bound profile evidence"
            )
        expected_profile = validate_tested_profile(dict(tested_profile))
        expected_report = {
            **expected_profile,
            "tested": True,
        }
        if dict(value) != expected_report:
            raise EvidenceError(
                "pmux tested compatibility report differs from the bound profile"
            )
    elif tested_profile is not None:
        raise EvidenceError("an admitted tested profile produced an untested result")
    return dict(value)


def public_result_binding(
    value: Mapping[str, Any],
    config: CampaignConfig,
    *,
    expected_session_id: str,
    expected_generation_id: str | None,
    expected_turn_id: str | None,
    tested_profile: Mapping[str, Any] | None = None,
    expected_claude_version: str | None = None,
) -> dict[str, Any]:
    session_id = value.get("session_id")
    generation_id = value.get("generation_id")
    turn_id = value.get("turn_id")
    claude_version = value.get("claude_version")
    model = value.get("model")
    if not all(isinstance(item, str) for item in (session_id, generation_id, turn_id)):
        raise EvidenceError(
            "pmux public result is missing its exact session/turn identity"
        )
    canonical_session = _validate_uuid(session_id, "public result session ID")
    canonical_generation = _validate_uuid(generation_id, "public result generation ID")
    canonical_turn = _validate_uuid(turn_id, "public result turn ID")
    if canonical_session != expected_session_id:
        raise EvidenceError("pmux public result returned a different session ID")
    if (
        expected_generation_id is not None
        and canonical_generation != expected_generation_id
    ):
        raise EvidenceError("pmux public result returned a different generation ID")
    if expected_turn_id is not None and canonical_turn != expected_turn_id:
        raise EvidenceError("pmux public result returned a different turn ID")
    if not isinstance(model, str) or model not in config.allowed_model_ids:
        raise EvidenceError("pmux public result returned an unauthorized model ID")
    compatibility = _compatibility_binding(
        value.get("compatibility"),
        config,
        tested_profile,
        expected_claude_version,
    )
    if (
        not isinstance(claude_version, str)
        or claude_version != compatibility["claude_version"]
    ):
        raise EvidenceError("pmux result and compatibility Claude versions disagree")
    timings = turn_timings_binding(value.get("timings"))
    return {
        "session_id": canonical_session,
        "generation_id": canonical_generation,
        "turn_id": canonical_turn,
        "model": model,
        "claude_version": claude_version,
        "compatibility": compatibility,
        "timings": timings,
        "drain_calibration": drain_calibration_from_timings(timings),
    }


def public_start_binding(
    value: Any,
    config: CampaignConfig,
    *,
    expected_session_id: str,
    tested_profile: Mapping[str, Any] | None = None,
    expected_claude_version: str | None = None,
) -> dict[str, Any]:
    session_id, generation_id = extract_public_handle(value)
    if session_id != expected_session_id:
        raise EvidenceError("pmux start returned a different public session ID")
    assert isinstance(value, dict)
    return {
        "session_id": session_id,
        "generation_id": generation_id,
        "compatibility": _compatibility_binding(
            value.get("compatibility"),
            config,
            tested_profile,
            expected_claude_version,
        ),
    }


def public_ping_binding(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {
        "server_version",
        "protocol_version",
    }:
        raise EvidenceError("pmux ping returned an unexpected DTO")
    if not isinstance(value["server_version"], str) or not value["server_version"]:
        raise EvidenceError("pmux ping returned an invalid server version")
    if (
        not isinstance(value["protocol_version"], int)
        or isinstance(value["protocol_version"], bool)
        or value["protocol_version"] != 1
    ):
        raise EvidenceError("pmux ping returned an unsupported protocol version")
    return dict(value)


def public_close_binding(
    value: Any, *, expected_session_id: str, expected_generation_id: str
) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {
        "session_id",
        "generation_id",
        "already_closed",
        "process_reaped",
    }:
        raise EvidenceError("pmux close returned an unexpected DTO")
    session_id = value["session_id"]
    generation_id = value["generation_id"]
    if not isinstance(session_id, str) or not isinstance(generation_id, str):
        raise EvidenceError("pmux close returned an invalid public handle")
    session_id = _validate_uuid(session_id, "close session ID")
    generation_id = _validate_uuid(generation_id, "close generation ID")
    if session_id != expected_session_id or generation_id != expected_generation_id:
        raise EvidenceError("pmux close returned a different public handle")
    if not isinstance(value["already_closed"], bool) or not isinstance(
        value["process_reaped"], bool
    ):
        raise EvidenceError("pmux close returned invalid boolean proof fields")
    if value["process_reaped"] is not True:
        raise EvidenceError("pmux close did not prove process reaping")
    return dict(value)


def _command_evidence(
    result: CommandResult,
    prompt_payloads: Iterable[bytes],
    sensitive_values: Iterable[str] = (),
) -> dict[str, Any]:
    return {
        "argv_shape": list(result.argv_shape),
        "returncode": result.returncode,
        "timed_out": result.timed_out,
        "interrupted": result.interrupted,
        "elapsed_ms": result.elapsed_ms,
        "stdout_bytes": len(result.stdout),
        "stdout_sha256": sha256_bytes(result.stdout),
        "stderr_bytes": len(result.stderr),
        "stderr_sha256": sha256_bytes(result.stderr),
        "stderr_redacted": redact_text(
            result.stderr.decode("utf-8", errors="replace"),
            prompt_payloads,
            sensitive_values,
        ),
        "output_limit_exceeded": result.output_limit_exceeded,
        "supervision_failure_reason": result.supervision_failure_reason,
        "cleanup_complete": result.cleanup_complete,
        "output_complete": result.output_complete,
        "process_receipt": dict(result.process_receipt),
    }


def _command_completed_cleanly(result: CommandResult) -> bool:
    return (
        result.supervised
        and result.returncode == 0
        and not result.timed_out
        and not result.interrupted
        and not result.output_limit_exceeded
        and result.cleanup_complete
        and result.output_complete
    )


def _managed_result_evidence(
    result: Any,
    prompt_payloads: Iterable[bytes],
    sensitive_values: Iterable[str],
) -> dict[str, Any]:
    receipt = dict(result.receipt)
    failure_reason = (
        result.reason if isinstance(result, bounded_process.FailureResult) else None
    )
    return {
        "exit_code": result.exit_code,
        "failure_reason": failure_reason,
        "stdout_bytes": len(result.stdout),
        "stdout_sha256": sha256_bytes(result.stdout),
        "stderr_bytes": len(result.stderr),
        "stderr_sha256": sha256_bytes(result.stderr),
        "stderr_redacted": redact_text(
            result.stderr.decode("utf-8", errors="replace"),
            prompt_payloads,
            sensitive_values,
        ),
        "cleanup_complete": (
            result.cleanup_complete
            if isinstance(result, bounded_process.FailureResult)
            else True
        ),
        "output_complete": (
            result.output_complete
            if isinstance(result, bounded_process.FailureResult)
            else True
        ),
        "process_receipt": receipt,
    }


class CampaignRunner:
    def __init__(self, config: CampaignConfig, environment: Mapping[str, str]) -> None:
        self.config = config
        self.environment = dict(environment)
        # Every --env value is a secret by assumption, so it joins the redaction
        # set even when its name does not look sensitive.
        self.redaction_values = tuple(
            sorted(
                {
                    *sensitive_environment_values(self.environment),
                    *(value for value in config.environment_set.values() if value),
                },
                key=lambda item: (-len(item), item),
            )
        )
        self.binary_identities: dict[str, FileIdentity] = {}
        self.claude_identity: FileIdentity | None = None
        self.claude_version_identity: dict[str, Any] = {}
        self.cwd_identity: DirectoryIdentity | None = None
        self.campaign_contract: dict[str, Any] = {}
        self.campaign_contract_digest = ""
        self.tested_profile_identity: FileIdentity | None = None
        self.tested_profile_value: dict[str, Any] | None = None
        self.agent_file_identity: FileIdentity | None = None
        self.system_prompt_file_identity: FileIdentity | None = None
        self.system_prompt_text: str | None = None
        self.drain_calibrations: list[dict[str, Any]] = []
        self.source_identity: dict[str, Any] = {}
        self.source_observation_count = 0
        self.pending_source_observations: list[dict[str, Any]] = []
        self.prompts: list[PromptIdentity] = []
        self.prompt_payloads: list[bytes] = []
        self.daemon: Any | None = None
        self.daemon_terminal_result: Any | None = None
        self.campaign_artifacts: AtomicArtifactDirectory | None = None
        self.socket_path: Path | None = None
        self.socket_identity: SocketIdentity | None = None
        self.runtime_parent: Path | None = None
        self.observed_tokens = 0
        self.published_attempts: list[dict[str, Any]] = []
        self.session_id: str | None = None
        self.generation_id: str | None = None
        self.run_id = str(uuid.uuid4())
        self.external_runtime_root: Path | None = None
        self.external_runtime_descriptor: int | None = None
        self.external_runtime_parent: Path | None = None
        self.external_runtime_parent_descriptor: int | None = None
        self.external_runtime_child_identities: dict[str, tuple[int, ...]] = {}

    def _redact(self, value: str) -> str:
        return redact_text(value, self.prompt_payloads, self.redaction_values)

    def _redacted_bytes(self, value: bytes) -> bytes:
        return self._redact(value.decode("utf-8", errors="replace")).encode("utf-8")

    def _cwd(self) -> Path:
        if self.cwd_identity is None:
            raise EvidenceError("campaign cwd identity was not bound")
        return Path(self.cwd_identity.path)

    def _record_source_observation(
        self,
        label: str,
        observation: SourceObservation,
        *,
        writer: AtomicArtifactDirectory | None = None,
        notes: Sequence[str] = (),
        changed_fields: Sequence[str] = (),
    ) -> dict[str, Any]:
        if SAFE_NAME.fullmatch(label) is None:
            raise EvidenceError("source observation label is invalid")
        self.source_observation_count += 1
        record = {
            "schema": "pmux.phase0.source-observation.v1",
            "sequence": self.source_observation_count,
            "label": label,
            "source_identity": dict(observation.identity),
            "revision_captures": {
                name: dict(capture)
                for name, capture in observation.revision_captures.items()
            },
            "changed_fields": list(changed_fields),
            "notes": list(notes),
        }
        target = writer or self.campaign_artifacts
        if target is None:
            self.pending_source_observations.append(record)
        else:
            target.write_json(
                f"source-observation-{self.source_observation_count:04d}-{label}.json",
                record,
            )
        return record

    def _flush_pending_source_observations(self) -> None:
        if self.campaign_artifacts is None:
            raise EvidenceError("campaign artifact writer is absent")
        pending = self.pending_source_observations
        self.pending_source_observations = []
        for record in pending:
            self.campaign_artifacts.write_json(
                f"source-observation-{record['sequence']:04d}-{record['label']}.json",
                record,
            )

    def _create_external_runtime(self) -> None:
        parent = Path("/tmp").resolve(strict=True)
        parent_descriptor, _ = _open_directory_nofollow(
            parent, require_owner_private=False
        )
        _verify_open_directory_path_identity(
            parent,
            parent_descriptor,
            require_owner_private=False,
        )
        root = Path(
            tempfile.mkdtemp(
                prefix=f"pmux-p0-{self.run_id[:8]}-",
                dir=parent,
            )
        )
        root.chmod(0o700)
        try:
            root_descriptor, _ = _open_private_directory_nofollow(root)
        except Exception:
            os.close(parent_descriptor)
            raise
        self.external_runtime_root = root
        self.external_runtime_descriptor = root_descriptor
        self.external_runtime_parent = parent
        self.external_runtime_parent_descriptor = parent_descriptor
        try:
            _verify_open_directory_path_identity(root, root_descriptor)
            _verify_open_directory_path_identity(
                parent,
                parent_descriptor,
                require_owner_private=False,
            )
            os.mkdir("daemon", mode=0o700, dir_fd=root_descriptor)
            os.mkdir("private-runtime", mode=0o700, dir_fd=root_descriptor)
            for name in ("daemon", "private-runtime"):
                child, metadata = _open_private_child_directory_at(
                    root_descriptor,
                    name,
                    create=False,
                    expected_device=os.fstat(root_descriptor).st_dev,
                )
                try:
                    self.external_runtime_child_identities[name] = (
                        metadata.st_dev,
                        metadata.st_ino,
                        metadata.st_uid,
                        stat.S_IMODE(metadata.st_mode),
                    )
                finally:
                    os.close(child)
            os.fsync(root_descriptor)
        except Exception:
            try:
                self._remove_external_runtime()
            except Exception:
                pass
            raise
        self.socket_path = root / "daemon" / "pmux.sock"
        self.runtime_parent = root / "private-runtime"

    def _open_external_runtime_child(self, name: str) -> int:
        if self.external_runtime_descriptor is None:
            raise EvidenceError("external runtime root descriptor is absent")
        expected = self.external_runtime_child_identities.get(name)
        if expected is None:
            raise EvidenceError("external runtime child identity is absent")
        child, metadata = _open_private_child_directory_at(
            self.external_runtime_descriptor,
            name,
            create=False,
            expected_device=os.fstat(self.external_runtime_descriptor).st_dev,
        )
        if (
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_uid,
            stat.S_IMODE(metadata.st_mode),
        ) != expected:
            os.close(child)
            raise EvidenceError("external runtime child directory was replaced")
        return child

    def run(self) -> dict[str, Any]:
        validate_config(self.config, access_files=True)
        self._bind_candidate()
        ledger_before = inspect_ledger(
            self.config.ledger_path,
            self.config.ledger_prefix,
            self.config.campaign_id,
        )
        if (
            global_attempts_consumed_through(
                ledger_before["next_global_attempt"]
                + self.config.max_attempts_this_run
                - 1
            )
            > self.config.global_attempt_ceiling
        ):
            raise BudgetExhausted(
                "the planned run cannot fit inside the remaining global attempt ceiling"
            )
        campaign_name = f"campaign-run-{self.run_id}"
        self.campaign_artifacts = AtomicArtifactDirectory(
            self.config.evidence_root, campaign_name
        )
        self._flush_pending_source_observations()
        self._create_external_runtime()
        status = "failed"
        error: str | None = None
        cleanup: dict[str, Any] = {"status": "not_attempted"}
        interrupted_signal: int | None = None
        previous_handlers: dict[int, Any] = {}

        def request_stop(signum: int, _frame: Any) -> None:
            nonlocal interrupted_signal
            interrupted_signal = signum
            raise CampaignInterrupted(
                f"campaign interrupted by {signal.Signals(signum).name}"
            )

        for signum in (signal.SIGINT, signal.SIGTERM):
            previous_handlers[signum] = signal.getsignal(signum)
            signal.signal(signum, request_stop)
        try:
            self._start_daemon()
            self._execute_scenario()
            self._verify_candidate_unchanged()
            status = "acquired"
        except Exception as caught:
            error = self._redact(str(caught))
        finally:
            for signum in previous_handlers:
                signal.signal(signum, signal.SIG_IGN)
            try:
                cleanup = self._stop_daemon_and_audit()
            except Exception as caught:
                cleanup = {
                    "status": "inconclusive",
                    "error": self._redact(str(caught)),
                }
            for signum, handler in previous_handlers.items():
                signal.signal(signum, handler)

        try:
            self._capture_daemon_logs()
        except Exception as caught:
            cleanup["log_capture_error"] = self._redact(str(caught))
        try:
            cleanup["external_runtime_removed"] = self._remove_external_runtime()
        except Exception as caught:
            cleanup["external_runtime_removed"] = False
            cleanup["external_runtime_error"] = self._redact(str(caught))
        if cleanup.get("status") != "verified":
            status = "failed"
        if not cleanup.get("external_runtime_removed"):
            status = "failed"
        if cleanup.get("log_capture_error"):
            status = "failed"
        if interrupted_signal is not None:
            status = "failed"
        summary = {
            "schema": CAMPAIGN_SCHEMA,
            "campaign_id": self.config.campaign_id,
            "run_id": self.run_id,
            "status": status,
            "error": error,
            "campaign_contract": self.campaign_contract,
            "campaign_contract_sha256": self.campaign_contract_digest,
            "source": self.source_identity,
            "binaries": {
                name: identity.public()
                for name, identity in sorted(self.binary_identities.items())
            },
            "claude": {
                "binary": self.claude_identity.public()
                if self.claude_identity
                else None,
                "version_output": self.claude_version_identity,
            },
            "rmux": {
                "sdk_version": RMUX_SDK_VERSION,
                "sidecar_binary_sha256": (
                    self.binary_identities["pmux-rmuxd"].sha256
                    if "pmux-rmuxd" in self.binary_identities
                    else ""
                ),
            },
            "platform": current_platform_identity(),
            "scenario": self.config.scenario,
            "public_entrypoint": (
                "claude-p" if self.config.scenario == "claude-p-one-shot" else "pmux"
            ),
            "exercised_binaries": list(exercised_binaries(self.config)),
            "attempts": self.published_attempts,
            "attempt_count": len(self.published_attempts),
            "drain_calibration": summarize_drain_calibration(
                self.drain_calibrations,
                configured_transcript_drain_ms=self._configured_transcript_drain_ms(),
            ),
            "observed_tokens": self.observed_tokens,
            "max_observed_tokens": self.config.max_observed_tokens,
            "cleanup": cleanup,
            "authority": {
                "pmux_exit_and_public_result": "authoritative",
                "transcript_parsed_by_envelope": False,
                "terminal_interpreted_by_envelope": False,
                "direct_input_by_envelope": False,
            },
        }
        assert self.campaign_artifacts is not None
        self.campaign_artifacts.write_json("campaign.json", summary)
        final = self.campaign_artifacts.publish(
            status=status,
            binding={
                "campaign_id": self.config.campaign_id,
                "run_id": self.run_id,
                "campaign_contract_sha256": self.campaign_contract_digest,
                "source_digest": self.source_identity.get("digest"),
                "ledger_prefix_sha256": self.config.ledger_prefix.sha256,
            },
        )
        if self.campaign_artifacts.publication_receipt is None:
            raise EvidenceError("campaign publication returned no manifest receipt")
        summary["evidence_directory"] = str(final)
        summary["campaign_manifest_sha256"] = (
            self.campaign_artifacts.publication_receipt["manifest_sha256"]
        )
        return summary

    def _configured_transcript_drain_ms(self) -> int:
        if self.tested_profile_value is not None:
            return int(self.tested_profile_value["transcript_drain_ms"])
        return self.config.untested_transcript_drain_ms

    def _bind_candidate(self) -> None:
        source_observation = observe_source_identity(self.config.source_root)
        source = dict(source_observation.identity)
        if source["digest"] != self.config.expected_source_digest:
            raise EvidenceError(
                "current source digest does not match the frozen candidate"
            )
        self.source_identity = source
        self._record_source_observation("candidate_bind", source_observation)
        release = self.config.release_bin_dir.resolve(strict=True)
        if not release.is_dir():
            raise EvidenceError("release binary directory is not a directory")
        for name in REQUIRED_RELEASE_BINARIES:
            identity = identify_file(release / name, executable=True)
            if Path(identity.path).parent != release:
                raise EvidenceError(
                    f"{name} resolved outside the exact release directory"
                )
            if identity.sha256 != self.config.expected_binary_hashes[name]:
                raise EvidenceError(f"{name} digest does not match the frozen manifest")
            self.binary_identities[name] = identity
        claude = identify_file(self.config.claude_bin, executable=True)
        if claude.sha256 != self.config.expected_claude_sha256:
            raise EvidenceError(
                "Claude executable digest does not match the frozen identity"
            )
        self.claude_identity = claude
        self.cwd_identity = identify_directory(self.config.cwd)
        self.prompts = [identify_prompt(path) for path in self.config.prompt_paths]
        self.prompt_payloads = [prompt.payload for prompt in self.prompts]
        self.prompt_payloads.extend(
            str(prompt.path).encode("utf-8") for prompt in self.prompts
        )
        if self.config.tested_profile_path is not None:
            (
                self.tested_profile_value,
                self.tested_profile_identity,
            ) = read_profile(self.config.tested_profile_path)
        if self.config.agent_file is not None:
            self.agent_file_identity = identify_file(
                self.config.agent_file, maximum=MAX_PROMPT_BYTES
            )
        if self.config.system_prompt_file is not None:
            (
                self.system_prompt_text,
                self.system_prompt_file_identity,
            ) = read_system_prompt(self.config.system_prompt_file)
            # The replacement joins the redaction set for the same reason every
            # `--env` value does: whatever else it is, it must not survive in
            # captured output. This happens before the first command below, so
            # nothing is captured un-redacted.
            self.redaction_values = tuple(
                sorted(
                    {*self.redaction_values, self.system_prompt_text},
                    key=lambda item: (-len(item), item),
                )
            )
        version = run_command(
            [self.claude_identity.path, "--version"],
            cwd=self._cwd(),
            environment=self.environment,
            timeout_seconds=15,
            argv_shape=["<frozen-claude>", "--version"],
        )
        if not _command_completed_cleanly(version):
            raise EvidenceError("Claude --version did not complete cleanly")
        combined = version.stdout + version.stderr
        try:
            version_stdout_text = version.stdout.decode("utf-8")
        except UnicodeDecodeError as error:
            raise EvidenceError("Claude version stdout is not UTF-8") from error
        if self._redact(version_stdout_text) != version_stdout_text:
            raise EvidenceError(
                "Claude version stdout unexpectedly contains a sensitive value"
            )
        self.claude_version_identity = {
            "stdout_sha256": sha256_bytes(version.stdout),
            "stderr_sha256": sha256_bytes(version.stderr),
            "combined_sha256": sha256_bytes(combined),
            "stdout_bytes": len(version.stdout),
            "stderr_bytes": len(version.stderr),
            "normalized_version": normalize_claude_version_output(version.stdout),
            "stdout_text": version_stdout_text,
        }
        _validate_claude_version_identity(self.claude_version_identity)
        if (
            self.tested_profile_value is not None
            and self.tested_profile_value["claude_version"]
            != self.claude_version_identity["normalized_version"]
        ):
            raise EvidenceError(
                "tested profile Claude version differs from the frozen Claude probe"
            )
        self.campaign_contract = build_campaign_contract(
            self.config,
            source_identity=self.source_identity,
            binary_identities=self.binary_identities,
            claude_identity=self.claude_identity,
            claude_version_identity=self.claude_version_identity,
            cwd_identity=self.cwd_identity,
            prompt_identities=self.prompts,
            environment=self.environment,
            tested_profile_identity=self.tested_profile_identity,
            tested_profile_value=self.tested_profile_value,
            agent_file_identity=self.agent_file_identity,
            system_prompt_file_identity=self.system_prompt_file_identity,
        )
        self.campaign_contract_digest = campaign_contract_sha256(self.campaign_contract)

    def _verify_candidate_unchanged(
        self,
        prompt_index: int | None = None,
        *,
        label: str = "candidate_verify",
        writer: AtomicArtifactDirectory | None = None,
    ) -> dict[str, Any]:
        if self.cwd_identity is None:
            raise EvidenceError("campaign cwd identity was never bound")
        verify_directory_identity(self.config.cwd, self.cwd_identity)
        observation = observe_source_identity(self.config.source_root)
        current_source = dict(observation.identity)
        # Gate exactly the claim, record everything else. See
        # SOURCE_IDENTITY_CLAIM_FIELDS for why the whole-dict comparison this
        # replaces destroyed a paid, successful Claude turn.
        changed = source_identity_delta(self.source_identity, current_source)
        claim_changed = source_identity_claim(current_source) != source_identity_claim(
            self.source_identity
        )
        notes: list[str] = []
        if changed and not claim_changed:
            notes.append(
                "source identity moved outside the frozen-candidate claim "
                f"({', '.join(changed)}); every claim field is unchanged, so this "
                "is recorded as an observation and does not fail the campaign"
            )
        record = self._record_source_observation(
            label,
            observation,
            writer=writer,
            notes=notes,
            changed_fields=changed,
        )
        if claim_changed:
            raise EvidenceError(
                "frozen source changed during the campaign: " + ", ".join(changed)
            )
        for name, expected in self.binary_identities.items():
            current = identify_file(Path(expected.path), executable=True)
            if not same_file_identity(expected, current):
                raise EvidenceError(
                    f"frozen binary changed during the campaign: {name}"
                )
        assert self.claude_identity is not None
        if not same_file_identity(
            self.claude_identity,
            identify_file(Path(self.claude_identity.path), executable=True),
        ):
            raise EvidenceError("Claude executable changed during the campaign")
        if prompt_index is not None:
            current_prompt = identify_prompt(self.prompts[prompt_index].path)
            if current_prompt.file != self.prompts[prompt_index].file:
                raise EvidenceError("prompt input changed before its reserved attempt")
        if self.tested_profile_identity is not None:
            current_profile = identify_file(
                Path(self.tested_profile_identity.path), maximum=1024 * 1024
            )
            if current_profile != self.tested_profile_identity:
                raise EvidenceError(
                    "tested compatibility profile changed during the campaign"
                )
        if self.agent_file_identity is not None:
            current_agent = identify_file(
                Path(self.agent_file_identity.path), maximum=MAX_PROMPT_BYTES
            )
            if current_agent != self.agent_file_identity:
                raise EvidenceError("agent profile changed during the campaign")
        if self.system_prompt_file_identity is not None:
            # Re-read through the full admission path, not just `identify_file`:
            # a replacement that lost mode 0600 between attempts is exactly as
            # disqualifying as one whose bytes changed.
            current_text, current_system_prompt = read_system_prompt(
                Path(self.system_prompt_file_identity.path)
            )
            if (
                current_system_prompt != self.system_prompt_file_identity
                or current_text != self.system_prompt_text
            ):
                raise EvidenceError(
                    "system prompt replacement changed during the campaign"
                )
        current_contract = build_campaign_contract(
            self.config,
            source_identity=self.source_identity,
            binary_identities=self.binary_identities,
            claude_identity=self.claude_identity,
            claude_version_identity=self.claude_version_identity,
            cwd_identity=self.cwd_identity,
            prompt_identities=self.prompts,
            environment=self.environment,
            tested_profile_identity=self.tested_profile_identity,
            tested_profile_value=self.tested_profile_value,
            agent_file_identity=self.agent_file_identity,
            system_prompt_file_identity=self.system_prompt_file_identity,
        )
        if (
            current_contract != self.campaign_contract
            or campaign_contract_sha256(current_contract)
            != self.campaign_contract_digest
        ):
            raise EvidenceError("immutable campaign contract changed after binding")
        return record

    def _start_daemon(self) -> None:
        self._verify_candidate_unchanged(label="before_daemon_launch")
        assert self.campaign_artifacts is not None
        assert self.socket_path is not None
        assert self.runtime_parent is not None
        command = [
            self.binary_identities["pmuxd"].path,
            "serve",
            "--socket",
            str(self.socket_path),
            "--runtime-parent",
            str(self.runtime_parent),
            "--untested-transcript-drain-ms",
            str(self.config.untested_transcript_drain_ms),
        ]
        profile_identity: dict[str, Any] | None = None
        if (
            self.tested_profile_value is not None
            and self.tested_profile_identity is not None
        ):
            command.extend(
                [
                    "--tested-claude-profile",
                    canonical_json_bytes(self.tested_profile_value).decode("utf-8"),
                ]
            )
            profile_identity = self.tested_profile_identity.public(include_path=False)
        self.campaign_artifacts.write_json(
            "daemon-launch.json",
            {
                "argv_shape": [
                    "<frozen-pmuxd>",
                    "serve",
                    "--socket",
                    "<private-campaign-socket>",
                    "--runtime-parent",
                    "<private-campaign-runtime>",
                    "--untested-transcript-drain-ms",
                    str(self.config.untested_transcript_drain_ms),
                    *(
                        ["--tested-claude-profile", "<canonical-profile-json>"]
                        if profile_identity
                        else []
                    ),
                ],
                "tested_profile_file": profile_identity,
                "process_authorities": evidence_authorities(),
            },
        )
        daemon_timeout = (
            self.config.daemon_ready_timeout_seconds
            + self.config.max_attempts_this_run
            * (self.config.turn_timeout_seconds + 30)
            + self.config.daemon_shutdown_timeout_seconds
            + 300
        )
        if not 1 <= daemon_timeout <= 86_400:
            raise EvidenceError("managed daemon lifetime exceeds its reviewed bound")
        try:
            executable = bounded_process.bind_executable(
                Path(self.binary_identities["pmuxd"].path)
            )
            if executable.sha256 != self.binary_identities["pmuxd"].sha256:
                raise EvidenceError("managed daemon binding differs from candidate")
            self.daemon = managed_process.start_managed(
                executable,
                command,
                cwd=self._cwd(),
                environment=self.environment,
                timeout_seconds=daemon_timeout,
                graceful_stop_timeout_seconds=(
                    self.config.daemon_shutdown_timeout_seconds
                ),
                drain_timeout_seconds=min(
                    5, self.config.daemon_shutdown_timeout_seconds
                ),
                maximum_output_bytes=MAX_CAPTURE_BYTES,
                description="Phase-0 managed pmuxd",
            )
        except bounded_process.BoundedProcessFailure as error:
            self.daemon_terminal_result = error.result
            self.campaign_artifacts.write_json(
                "daemon-launch-failure.json",
                _managed_result_evidence(
                    error.result, self.prompt_payloads, self.redaction_values
                ),
            )
            raise EvidenceError("pmuxd failed its managed launch boundary") from error
        except bounded_process.BoundedProcessError as error:
            raise EvidenceError(f"pmuxd managed launch failed: {error}") from error
        assert self.daemon is not None
        self.campaign_artifacts.write_json(
            "daemon-managed-identity.json",
            dataclasses.asdict(self.daemon.identity),
        )
        deadline = time.monotonic() + self.config.daemon_ready_timeout_seconds
        last_error = "daemon socket did not appear"
        while time.monotonic() < deadline:
            assert self.daemon is not None
            try:
                health = self.daemon.health()
            except bounded_process.BoundedProcessFailure as error:
                self.daemon_terminal_result = error.result
                raise EvidenceError("pmuxd exited before readiness") from error
            if not health.running:
                raise EvidenceError("pmuxd stopped before readiness")
            if self.socket_path.exists():
                ready_identity = capture_socket_identity(self.socket_path)
                ping = run_command(
                    [
                        self.binary_identities["pmux"].path,
                        "--socket",
                        str(self.socket_path),
                        "--output",
                        "json",
                        "ping",
                    ],
                    cwd=self._cwd(),
                    environment=self.environment,
                    timeout_seconds=5,
                    argv_shape=[
                        "<frozen-pmux>",
                        "--socket",
                        "<private-campaign-socket>",
                        "--output",
                        "json",
                        "ping",
                    ],
                )
                if ping.interrupted:
                    raise CampaignInterrupted(
                        "campaign interrupted during pmuxd readiness"
                    )
                if _command_completed_cleanly(ping):
                    ping_value, _ = parse_public_output(ping.stdout, "json")
                    ping_binding = public_ping_binding(ping_value)
                    verify_socket_identity(self.socket_path, ready_identity)
                    self.socket_identity = ready_identity
                    self.campaign_artifacts.write_json(
                        "daemon-ready.json",
                        {
                            "command": _command_evidence(
                                ping, self.prompt_payloads, self.redaction_values
                            ),
                            "public_binding": ping_binding,
                            "socket_identity": ready_identity.public(),
                        },
                    )
                    return
                last_error = self._redact(ping.stderr.decode("utf-8", errors="replace"))
            time.sleep(0.05)
        raise EvidenceError(f"pmuxd readiness failed: {last_error}")

    def _verify_socket_unchanged(self) -> None:
        if self.socket_path is None or self.socket_identity is None:
            raise EvidenceError("pmuxd socket identity was never bound at readiness")
        verify_socket_identity(self.socket_path, self.socket_identity)

    def _socket_absent_at_bound_runtime(self) -> bool:
        descriptor = self._open_external_runtime_child("daemon")
        try:
            try:
                os.stat("pmux.sock", dir_fd=descriptor, follow_symlinks=False)
            except FileNotFoundError:
                return True
            return False
        finally:
            os.close(descriptor)

    def _private_runtime_members(self) -> list[str]:
        descriptor = self._open_external_runtime_child("private-runtime")
        try:
            return _descriptor_tree_members(descriptor)
        finally:
            os.close(descriptor)

    def _launch_args(self, session_id: str, *, resume: bool) -> list[str]:
        args = ["--resume", session_id] if resume else ["--session-id", session_id]
        args.extend(
            ["--claude", self.claude_identity.path if self.claude_identity else ""]
        )
        args.extend(["--cwd", str(self._cwd())])
        if self.config.model is not None:
            args.extend(["--model", self.config.model])
        if self.config.effort is not None:
            args.extend(["--effort", self.config.effort])
        args.extend(
            [
                "--auth",
                "subscription",
                "--terminal-profile",
                self.config.terminal_profile,
                "--input-transport",
                self.config.input_transport,
                "--lifecycle",
                self.config.lifecycle,
                "--rows",
                str(self.config.terminal_rows),
                "--cols",
                str(self.config.terminal_cols),
                "--compatibility",
                self.config.compatibility,
            ]
        )
        args.extend(self._forwarded_launch_args("pmux"))
        return args

    def _forwarded_launch_args(self, entrypoint: str) -> list[str]:
        """Forward the launch surface pmux gained, in one bound order.

        `--env KEY=VALUE` is deliberately **not** emitted. `pmux` itself warns
        that the value is visible in argv (`bin/pmux/src/cli.rs:216-219`), and
        this envelope binds the launched argv verbatim into an evidence-grade
        process receipt, so an argv-borne value would be written to disk. The
        value is instead placed in the pmux child's environment and forwarded
        through pmux's own name-only channel, `--env-passthrough KEY`, which
        lands in the same launch `set` term. The contract records the split.

        The system prompt replacement has no such channel and IS emitted as
        text; `SYSTEM_PROMPT_DELIVERY` records why, and `read_system_prompt`
        bounds what may be admitted to that route. `entrypoint` selects the
        denied-tool spelling: the two public entrypoints spell one protocol
        field two ways, and guessing costs an already-reserved ordinal.

        The client-side profile is `--profile NAME --profile-file PATH`. It was
        `--agent NAME --agent-file PATH` and this builder went on emitting that
        after the product renamed it: `--agent` now names a STORED SERVER agent
        by id and requires `--agent-version`, and `--agent-file` survives only as
        a hidden spelling that refuses by name (`bin/pmux/src/cli.rs`). So a
        campaign configured with a profile could not launch through EITHER
        entrypoint -- `claude-p` declares neither option at all -- and would have
        discovered it one ordinal after reserving one.
        `LaunchSurfaceTests` now reads both structs and holds this function to
        them.
        """

        if entrypoint not in DENIED_TOOL_FLAG:
            raise EvidenceError(f"unknown public entrypoint {entrypoint!r}")
        args: list[str] = []
        if self.config.agent_name is not None and self.config.agent_file is not None:
            if entrypoint != "pmux":
                raise EvidenceError(
                    f"the {entrypoint} entrypoint declares no client-side profile "
                    "option, so a campaign configured with --agent/--agent-file "
                    "cannot run through it"
                )
            args.extend(
                [
                    "--profile",
                    self.config.agent_name,
                    "--profile-file",
                    str(self.config.agent_file),
                ]
            )
        if self.config.permission_mode is not None:
            args.extend(["--permission-mode", self.config.permission_mode])
        for tool in self.config.denied_tools:
            args.extend([DENIED_TOOL_FLAG[entrypoint], tool])
        if self.system_prompt_text is not None:
            args.extend(["--system-prompt", self.system_prompt_text])
        for name in sorted(
            {
                *self.config.environment_set_names,
                *self.config.environment_passthrough_names,
            }
        ):
            args.extend(["--env-passthrough", name])
        return args

    def _launch_environment(self) -> dict[str, str]:
        """The pmux child's environment, carrying every `--env` value.

        Only the commands that also carry launch options receive it; the daemon,
        the Claude version probe, and the contract binding keep the unmodified
        campaign environment.
        """

        return {**self.environment, **self.config.environment_set}

    def _execute_scenario(self) -> None:
        if self.config.scenario == "claude-p-one-shot":
            for index in range(len(self.prompts)):
                self._execute_claude_p_one_shot(index)
            return
        if self.config.scenario == "one-shot":
            for index in range(len(self.prompts)):
                self._execute_one_shot(index)
            return
        self._execute_persistent(resume=self.config.scenario == "resume")

    def _ensure_usage_budget_for_next_attempt(self) -> None:
        if self.observed_tokens >= self.config.max_observed_tokens:
            raise BudgetExhausted("observed public usage reached the campaign guard")

    def _reserve(
        self,
        index: int,
        session_id: str,
        generation_id: str | None,
        turn_id: str | None,
        role: str,
    ) -> tuple[dict[str, Any], AtomicArtifactDirectory]:
        self._ensure_usage_budget_for_next_attempt()
        source_before = self._verify_candidate_unchanged(
            index, label=f"attempt_{index + 1}_before_reservation"
        )
        attempt_id = str(uuid.uuid4())
        final_name = f"attempt-{attempt_id}"
        final_path = self.config.evidence_root / final_name
        reservation = reserve_attempt(
            self.config,
            attempt_id=attempt_id,
            session_id=session_id,
            generation_id=generation_id,
            turn_id=turn_id,
            prompt_suite_index=index + 1,
            scenario_role=role,
            campaign_contract=self.campaign_contract,
            source_identity=self.source_identity,
            binary_identities=self.binary_identities,
            claude_identity=self.claude_identity,
            claude_version_identity=self.claude_version_identity,
            prompt_identity=self.prompts[index],
            environment=self.environment,
            artifact_directory=final_path,
            run_id=self.run_id,
            tested_profile_identity=self.tested_profile_identity,
            tested_profile_value=self.tested_profile_value,
            agent_file_identity=self.agent_file_identity,
            system_prompt_file_identity=self.system_prompt_file_identity,
            current_run_attempt_anchors={
                item["attempt_id"]: item["artifact_manifest_sha256"]
                for item in self.published_attempts
            },
        )
        self.observed_tokens = reservation["prior_observed_tokens"]
        writer = AtomicArtifactDirectory(self.config.evidence_root, final_name)
        writer.write_json("reservation.json", reservation)
        writer.write_json("source-observation-before-command.json", source_before)
        return reservation, writer

    def _execute_one_shot(self, index: int) -> None:
        session_id = str(uuid.uuid4())
        turn_id = str(uuid.uuid4())
        reservation, writer = self._reserve(
            index, session_id, None, turn_id, "fresh_one_shot"
        )
        assert self.socket_path is not None
        command = [
            self.binary_identities["pmux"].path,
            "--socket",
            str(self.socket_path),
            "--output",
            self.config.output_format,
            "oneshot",
            *self._launch_args(session_id, resume=False),
            "--prompt-file",
            "-",
            "--turn-id",
            turn_id,
            "--timeout-secs",
            str(self.config.turn_timeout_seconds),
        ]
        shape = [
            "<frozen-pmux>",
            "--socket",
            "<private-campaign-socket>",
            "--output",
            self.config.output_format,
            "oneshot",
            "<frozen-launch-options>",
            "--prompt-file",
            "<bound-prompt-on-stdin>",
            "--turn-id",
            turn_id,
            "--timeout-secs",
            str(self.config.turn_timeout_seconds),
        ]
        self._execute_reserved_command(
            index,
            reservation,
            writer,
            command,
            shape,
            "run",
            stdin_payload=self.prompts[index].payload,
            expected_session_id=session_id,
            expected_generation_id=None,
            expected_turn_id=turn_id,
        )

    def _execute_claude_p_one_shot(self, index: int) -> None:
        session_id = str(uuid.uuid4())
        reservation, writer = self._reserve(
            index, session_id, None, None, "claude_p_fresh_one_shot"
        )
        assert self.socket_path is not None
        command = [
            self.binary_identities["claude-p"].path,
            "-p",
            "--socket",
            str(self.socket_path),
            "--claude-bin",
            self.claude_identity.path if self.claude_identity else "",
            "--cwd",
            str(self._cwd()),
            "--session-id",
            session_id,
            "--output-format",
            "json",
            "--timeout-seconds",
            str(self.config.turn_timeout_seconds),
        ]
        if self.config.model is not None:
            command.extend(["--model", self.config.model])
        if self.config.effort is not None:
            command.extend(["--effort", self.config.effort])
        command.extend(self._forwarded_launch_args("claude-p"))
        shape = [
            "<frozen-claude-p>",
            "-p",
            "--socket",
            "<private-campaign-socket>",
            "--claude-bin",
            "<frozen-claude>",
            "--cwd",
            "<bound-cwd>",
            "--session-id",
            session_id,
            "--output-format",
            "json",
            "--timeout-seconds",
            str(self.config.turn_timeout_seconds),
            "<optional-model-effort>",
            "<frozen-launch-options>",
            "<prompt-on-stdin>",
        ]
        self._execute_reserved_command(
            index,
            reservation,
            writer,
            command,
            shape,
            "claude-p",
            stdin_payload=self.prompts[index].payload,
            expected_session_id=session_id,
            expected_generation_id=None,
            expected_turn_id=None,
        )

    def _execute_persistent(self, *, resume: bool) -> None:
        session_id = self.config.resume_session_id if resume else str(uuid.uuid4())
        assert session_id is not None
        first_turn_id = str(uuid.uuid4())
        role = "resume_start_and_turn" if resume else "fresh_persistent_start_and_turn"
        reservation, writer = self._reserve(0, session_id, None, first_turn_id, role)
        assert self.socket_path is not None
        try:
            self._verify_socket_unchanged()
        except EvidenceError as caught:
            self._publish_failed_attempt(
                reservation,
                writer,
                "socket_identity_changed_before_pmux_start",
                {"socket_error": self._redact(str(caught))},
            )
            raise
        start_command = [
            self.binary_identities["pmux"].path,
            "--socket",
            str(self.socket_path),
            "--output",
            "json",
            "start",
            *self._launch_args(session_id, resume=resume),
        ]
        start_result = run_command(
            start_command,
            cwd=self._cwd(),
            environment=self._launch_environment(),
            timeout_seconds=self.config.turn_timeout_seconds,
            argv_shape=[
                "<frozen-pmux>",
                "--socket",
                "<private-campaign-socket>",
                "--output",
                "json",
                "start",
                "<frozen-launch-options>",
            ],
        )
        writer.write(
            "pmux-start.stdout.json", self._redacted_bytes(start_result.stdout)
        )
        writer.write(
            "pmux-start.stderr.redacted.txt",
            redact_text(
                start_result.stderr.decode("utf-8", errors="replace"),
                self.prompt_payloads,
                self.redaction_values,
            ).encode("utf-8"),
        )
        if not _command_completed_cleanly(start_result):
            self._publish_failed_attempt(
                reservation,
                writer,
                "pmux_start_failed",
                {
                    "start": _command_evidence(
                        start_result,
                        self.prompt_payloads,
                        self.redaction_values,
                    )
                },
            )
            raise EvidenceError(
                "pmux start failed; the reserved attempt remains consumed"
            )
        try:
            start_value, _ = parse_public_output(start_result.stdout, "json")
            start_binding = public_start_binding(
                start_value,
                self.config,
                expected_session_id=session_id,
                tested_profile=self.tested_profile_value,
                expected_claude_version=self.claude_version_identity[
                    "normalized_version"
                ],
            )
        except EvidenceError:
            self._publish_failed_attempt(
                reservation,
                writer,
                "public_handle_mismatch",
                {
                    "start": _command_evidence(
                        start_result,
                        self.prompt_payloads,
                        self.redaction_values,
                    )
                },
            )
            raise
        self.session_id = start_binding["session_id"]
        self.generation_id = start_binding["generation_id"]
        self._verify_candidate_unchanged(
            0,
            label="attempt_1_after_pmux_start",
            writer=writer,
        )
        first_command, first_shape = self._turn_command(0, first_turn_id)
        self._execute_reserved_command(
            0,
            reservation,
            writer,
            first_command,
            first_shape,
            "turn",
            extra={
                "start": _command_evidence(
                    start_result, self.prompt_payloads, self.redaction_values
                ),
                "start_public_binding": start_binding,
            },
            stdin_payload=self.prompts[0].payload,
            expected_session_id=session_id,
            expected_generation_id=self.generation_id,
            expected_turn_id=first_turn_id,
        )
        for index in range(1, len(self.prompts)):
            turn_id = str(uuid.uuid4())
            reservation, writer = self._reserve(
                index,
                session_id,
                self.generation_id,
                turn_id,
                "warm_turn",
            )
            command, shape = self._turn_command(index, turn_id)
            self._execute_reserved_command(
                index,
                reservation,
                writer,
                command,
                shape,
                "turn",
                stdin_payload=self.prompts[index].payload,
                expected_session_id=session_id,
                expected_generation_id=self.generation_id,
                expected_turn_id=turn_id,
            )
        self._close_persistent_session()

    def _turn_command(self, index: int, turn_id: str) -> tuple[list[str], list[str]]:
        assert self.socket_path is not None
        assert self.session_id is not None
        assert self.generation_id is not None
        command = [
            self.binary_identities["pmux"].path,
            "--socket",
            str(self.socket_path),
            "--output",
            self.config.output_format,
            "turn",
            self.session_id,
            "--generation",
            self.generation_id,
            "--prompt-file",
            "-",
            "--turn-id",
            turn_id,
            "--timeout-secs",
            str(self.config.turn_timeout_seconds),
        ]
        shape = [
            "<frozen-pmux>",
            "--socket",
            "<private-campaign-socket>",
            "--output",
            self.config.output_format,
            "turn",
            self.session_id,
            "--generation",
            self.generation_id,
            "--prompt-file",
            "<bound-prompt-on-stdin>",
            "--turn-id",
            turn_id,
            "--timeout-secs",
            str(self.config.turn_timeout_seconds),
        ]
        return command, shape

    def _execute_reserved_command(
        self,
        index: int,
        reservation: Mapping[str, Any],
        writer: AtomicArtifactDirectory,
        command: Sequence[str],
        shape: Sequence[str],
        label: str,
        *,
        extra: Mapping[str, Any] | None = None,
        stdin_payload: bytes | None = None,
        expected_session_id: str,
        expected_generation_id: str | None,
        expected_turn_id: str | None,
    ) -> None:
        try:
            self._verify_socket_unchanged()
            result = run_command(
                command,
                cwd=self._cwd(),
                environment=self._launch_environment(),
                timeout_seconds=self.config.turn_timeout_seconds + 30,
                argv_shape=shape,
                stdin_payload=stdin_payload,
            )
        except Exception as caught:
            self._publish_failed_attempt(
                reservation,
                writer,
                "public_command_not_acquired",
                {"envelope_error": self._redact(str(caught))},
            )
            raise
        suffix = "json" if self.config.output_format == "json" else "ndjson"
        writer.write(
            f"pmux-{label}.stdout.{suffix}", self._redacted_bytes(result.stdout)
        )
        writer.write(
            f"pmux-{label}.stderr.redacted.txt",
            redact_text(
                result.stderr.decode("utf-8", errors="replace"),
                self.prompt_payloads,
                self.redaction_values,
            ).encode("utf-8"),
        )
        command_evidence = _command_evidence(
            result, self.prompt_payloads, self.redaction_values
        )
        evidence = dict(extra or {})
        evidence[label] = command_evidence
        status = "pmux_exit_zero"
        usage: int | None = None
        result_binding: dict[str, Any] | None = None
        error: str | None = None
        try:
            if not result.supervised:
                raise EvidenceError(
                    f"pmux {label} supervision failed: "
                    f"{result.supervision_failure_reason}"
                )
            if result.returncode != 0:
                raise EvidenceError(f"pmux {label} exited with {result.returncode}")
            if result.timed_out:
                raise EvidenceError(f"pmux {label} exceeded the envelope timeout")
            if result.interrupted:
                raise EvidenceError(f"pmux {label} was interrupted")
            if result.output_limit_exceeded:
                raise EvidenceError(f"pmux {label} exceeded the evidence output bound")
            if not result.cleanup_complete or not result.output_complete:
                raise EvidenceError(
                    f"pmux {label} process/output cleanup was incomplete"
                )
            parsed, records = parse_public_output(
                result.stdout, self.config.output_format
            )
            public = public_result(parsed, self.config.output_format)
            result_binding = public_result_binding(
                public,
                self.config,
                expected_session_id=expected_session_id,
                expected_generation_id=expected_generation_id,
                expected_turn_id=expected_turn_id,
                tested_profile=self.tested_profile_value,
                expected_claude_version=self.claude_version_identity[
                    "normalized_version"
                ],
            )
            acquired_usage = observed_tokens_from_public_result(public)
            evidence["public_output_record_count"] = records
            evidence["public_result_binding"] = result_binding
            self._verify_socket_unchanged()
            usage = acquired_usage
            self.observed_tokens = reservation["prior_observed_tokens"] + usage
            self.drain_calibrations.append(dict(result_binding["drain_calibration"]))
        except Exception as caught:
            status = "failed"
            error = self._redact(str(caught))
            usage = None
            result_binding = None
            self.observed_tokens = reservation["prior_observed_tokens"]
        # "Did pmux produce a correct result" and "did the harness environment
        # hold still" are independent facts, and the second must never overwrite
        # the first. This check used to sit inside the try above, so a moved
        # `.git/index` timestamp rewrote a completed turn into status="failed",
        # nulled its public_result_binding, discarded its usage and dropped its
        # drain sample -- which is what happened to ordinal 32. It is recorded
        # as its own fact here, and it still stops the campaign after the
        # attempt is published, because a genuinely changed source invalidates
        # the frozen candidate for every *later* attempt, not this one.
        post_command_source_check: dict[str, Any] = {"status": "ok", "error": None}
        try:
            self._verify_candidate_unchanged(
                index,
                label=f"attempt_{index + 1}_after_public_command",
                writer=writer,
            )
        except Exception as caught:
            post_command_source_check = {
                "status": "failed",
                "error": self._redact(str(caught)),
            }
        outcome = {
            "schema": ATTEMPT_SCHEMA,
            "campaign_id": self.config.campaign_id,
            "run_id": self.run_id,
            "attempt_id": reservation["attempt_id"],
            "global_attempt_ordinal": reservation["global_attempt_ordinal"],
            "reservation_sha256": reservation["reservation_sha256"],
            "campaign_contract_sha256": self.campaign_contract_digest,
            "status": status,
            "pmux_product_verdict_source": "public_exit_and_result",
            "product_semantics_reimplemented": False,
            "observed_tokens_from_public_result": usage,
            "prior_observed_tokens": reservation["prior_observed_tokens"],
            "cumulative_observed_tokens": self.observed_tokens,
            "public_result_binding": result_binding,
            "error": error,
            "post_command_source_check": post_command_source_check,
            "commands": evidence,
        }
        writer.write_json("outcome.json", outcome)
        final = writer.publish(
            status=status,
            binding={
                "campaign_id": self.config.campaign_id,
                "run_id": self.run_id,
                "attempt_id": reservation["attempt_id"],
                "reservation_sha256": reservation["reservation_sha256"],
                "campaign_contract_sha256": self.campaign_contract_digest,
                "source_digest": self.source_identity["digest"],
            },
        )
        if writer.publication_receipt is None:
            raise EvidenceError("attempt publication returned no manifest receipt")
        self.published_attempts.append(
            {
                "global_attempt_ordinal": reservation["global_attempt_ordinal"],
                "attempt_id": reservation["attempt_id"],
                "status": status,
                "error": error,
                "post_command_source_check": post_command_source_check,
                "campaign_contract_sha256": self.campaign_contract_digest,
                "cumulative_observed_tokens": self.observed_tokens,
                "evidence_directory": str(final),
                "reservation_sha256": reservation["reservation_sha256"],
                "artifact_manifest_sha256": writer.publication_receipt[
                    "manifest_sha256"
                ],
            }
        )
        if status != "pmux_exit_zero":
            raise EvidenceError(
                "pmux did not produce an authoritative successful public result; "
                "campaign stopped and the reservation remains consumed"
            )
        if post_command_source_check["status"] != "ok":
            raise EvidenceError(
                "the harness could not confirm the frozen source after a "
                "successful pmux command; the attempt above stands as a completed "
                "turn and its result binding is published, but no further attempt "
                "may run against an unfrozen candidate: "
                f"{post_command_source_check['error']}"
            )

    def _publish_failed_attempt(
        self,
        reservation: Mapping[str, Any],
        writer: AtomicArtifactDirectory,
        reason: str,
        commands: Mapping[str, Any],
    ) -> None:
        outcome = {
            "schema": ATTEMPT_SCHEMA,
            "campaign_id": self.config.campaign_id,
            "run_id": self.run_id,
            "attempt_id": reservation["attempt_id"],
            "global_attempt_ordinal": reservation["global_attempt_ordinal"],
            "reservation_sha256": reservation["reservation_sha256"],
            "campaign_contract_sha256": self.campaign_contract_digest,
            "status": "failed",
            "observed_tokens_from_public_result": None,
            "prior_observed_tokens": reservation["prior_observed_tokens"],
            "cumulative_observed_tokens": reservation["prior_observed_tokens"],
            "public_result_binding": None,
            "error": reason,
            "commands": dict(commands),
            "pmux_product_verdict_source": "public_exit_and_result",
            "product_semantics_reimplemented": False,
            # None, not a {"status": ...} object: every failure that reaches
            # this publisher happened BEFORE the public command ran (or before
            # its handle was acquired), so there is no post-command environment
            # to report on. The summary entry appended below already records
            # None; the outcome must say the same thing, and
            # _validate_attempt_outcome accepts None exactly here.
            "post_command_source_check": None,
        }
        writer.write_json("outcome.json", outcome)
        final = writer.publish(
            status="failed",
            binding={
                "campaign_id": self.config.campaign_id,
                "run_id": self.run_id,
                "attempt_id": reservation["attempt_id"],
                "reservation_sha256": reservation["reservation_sha256"],
                "campaign_contract_sha256": self.campaign_contract_digest,
                "source_digest": self.source_identity["digest"],
            },
        )
        if writer.publication_receipt is None:
            raise EvidenceError("attempt publication returned no manifest receipt")
        self.published_attempts.append(
            {
                "global_attempt_ordinal": reservation["global_attempt_ordinal"],
                "attempt_id": reservation["attempt_id"],
                "status": "failed",
                "error": reason,
                "post_command_source_check": None,
                "campaign_contract_sha256": self.campaign_contract_digest,
                "cumulative_observed_tokens": reservation["prior_observed_tokens"],
                "evidence_directory": str(final),
                "reservation_sha256": reservation["reservation_sha256"],
                "artifact_manifest_sha256": writer.publication_receipt[
                    "manifest_sha256"
                ],
            }
        )

    def _close_persistent_session(self) -> None:
        assert self.socket_path is not None
        if self.session_id is None or self.generation_id is None:
            return
        self._verify_candidate_unchanged()
        self._verify_socket_unchanged()
        result = run_command(
            [
                self.binary_identities["pmux"].path,
                "--socket",
                str(self.socket_path),
                "--output",
                "json",
                "close",
                self.session_id,
                "--generation",
                self.generation_id,
            ],
            cwd=self._cwd(),
            environment=self.environment,
            timeout_seconds=self.config.daemon_shutdown_timeout_seconds,
            argv_shape=[
                "<frozen-pmux>",
                "--socket",
                "<private-campaign-socket>",
                "--output",
                "json",
                "close",
                self.session_id,
                "--generation",
                self.generation_id,
            ],
        )
        assert self.campaign_artifacts is not None
        self.campaign_artifacts.write(
            "pmux-close.stdout.json", self._redacted_bytes(result.stdout)
        )
        self.campaign_artifacts.write(
            "pmux-close.stderr.redacted.txt",
            redact_text(
                result.stderr.decode("utf-8", errors="replace"),
                self.prompt_payloads,
                self.redaction_values,
            ).encode("utf-8"),
        )
        close_evidence = _command_evidence(
            result, self.prompt_payloads, self.redaction_values
        )
        if not _command_completed_cleanly(result):
            self.campaign_artifacts.write_json("pmux-close.json", close_evidence)
            raise EvidenceError("pmux close failed; campaign cannot be promoted")
        close_value, _ = parse_public_output(result.stdout, "json")
        close_evidence["public_binding"] = public_close_binding(
            close_value,
            expected_session_id=self.session_id,
            expected_generation_id=self.generation_id,
        )
        self.campaign_artifacts.write_json("pmux-close.json", close_evidence)
        self._verify_socket_unchanged()

    def _stop_daemon_and_audit(self) -> dict[str, Any]:
        daemon = self.daemon
        if daemon is None:
            return {
                "status": "verified",
                "daemon_started": False,
                "remaining_exact_processes": [],
                "socket_removed": True,
                "runtime_children": [],
            }
        socket_identity_error: str | None = None
        try:
            self._verify_socket_unchanged()
        except EvidenceError as caught:
            socket_identity_error = self._redact(str(caught))
        terminal_error: str | None = None
        try:
            terminal = daemon.finalize(signal_number=signal.SIGTERM)
            managed_process.validate_managed_execution_receipt(terminal.receipt)
        except bounded_process.BoundedProcessFailure as error:
            terminal = error.result
            managed_process.validate_managed_failure_receipt(terminal.receipt)
            terminal_error = self._redact(str(error))
        except bounded_process.BoundedProcessError as error:
            terminal = daemon.terminal_result
            terminal_error = self._redact(str(error))
        self.daemon_terminal_result = terminal
        if terminal is None:
            return {
                "status": "inconclusive",
                "daemon_started": True,
                "daemon_returncode": None,
                "graceful_shutdown": False,
                "forced_shutdown": False,
                "observed_exact_process_count": 0,
                "remaining_exact_processes": [],
                "targeted_rescue_signals": [],
                "socket_removed": False,
                "socket_identity_before_shutdown": (
                    self.socket_identity.public()
                    if self.socket_identity is not None
                    else None
                ),
                "socket_identity_error": socket_identity_error,
                "runtime_children": [],
                "process_scan_errors": [terminal_error or "missing terminal receipt"],
                "managed_process": None,
                "scope": "shared exact managed-process supervisor",
                "limitation": (
                    "ordinary non-hostile child attribution; deliberate marker-close "
                    "before a wholly unobserved escape is outside the evidence claim"
                ),
            }
        receipt = dict(terminal.receipt)
        ledger = list(terminal.process_ledger)
        remaining = [row for row in ledger if row.get("reaped") is not True]
        try:
            socket_removed = self._socket_absent_at_bound_runtime()
            runtime_children = self._private_runtime_members()
        except EvidenceError as error:
            socket_removed = False
            runtime_children = ["<runtime-audit-inconclusive>"]
            socket_identity_error = socket_identity_error or self._redact(str(error))
        success = isinstance(terminal, bounded_process.RunResult)
        cleanup_complete = (
            True if success else bool(getattr(terminal, "cleanup_complete", False))
        )
        status = "verified"
        if (
            not success
            or terminal.exit_code != 0
            or remaining
            or not socket_removed
            or runtime_children
            or socket_identity_error is not None
        ):
            status = "failed" if cleanup_complete else "inconclusive"
        evidence = {
            "status": status,
            "daemon_started": True,
            "daemon_returncode": terminal.exit_code,
            "graceful_shutdown": success,
            "forced_shutdown": receipt.get("failure_reason") == "graceful_stop_timeout",
            "observed_exact_process_count": len(ledger),
            "remaining_exact_processes": remaining,
            "targeted_rescue_signals": [],
            "socket_removed": socket_removed,
            "socket_identity_before_shutdown": (
                self.socket_identity.public()
                if self.socket_identity is not None
                else None
            ),
            "socket_identity_error": socket_identity_error,
            "runtime_children": runtime_children,
            "process_scan_errors": [] if terminal_error is None else [terminal_error],
            "managed_process": _managed_result_evidence(
                terminal, self.prompt_payloads, self.redaction_values
            ),
            "scope": "shared exact managed-process supervisor and process ledger",
            "limitation": (
                "ordinary non-hostile child attribution; deliberate marker-close before "
                "a wholly unobserved escape is outside the evidence claim"
            ),
        }
        assert self.campaign_artifacts is not None
        self.campaign_artifacts.write_json("daemon-stop.json", evidence)
        return evidence

    def _capture_daemon_logs(self) -> None:
        assert self.campaign_artifacts is not None
        terminal = self.daemon_terminal_result
        if terminal is not None:
            self.campaign_artifacts.write(
                "pmuxd.stdout.redacted.txt",
                self._redact(terminal.stdout.decode("utf-8", errors="replace")).encode(
                    "utf-8"
                ),
            )
            self.campaign_artifacts.write(
                "pmuxd.stderr.redacted.txt",
                self._redact(terminal.stderr.decode("utf-8", errors="replace")).encode(
                    "utf-8"
                ),
            )
        log = (
            self.socket_path.parent / "logs" / "pmuxd.log" if self.socket_path else None
        )
        if log is not None and log.exists():
            payload, _ = read_bounded_regular_file(log, MAX_CAPTURE_BYTES)
            self.campaign_artifacts.write(
                "pmuxd.log.redacted.txt",
                self._redact(payload.decode("utf-8", errors="replace")).encode("utf-8"),
            )

    def _remove_external_runtime(self) -> bool:
        root = self.external_runtime_root
        root_descriptor = self.external_runtime_descriptor
        parent = self.external_runtime_parent
        parent_descriptor = self.external_runtime_parent_descriptor
        if (
            root is None
            and root_descriptor is None
            and parent is None
            and parent_descriptor is None
        ):
            return True
        if (
            root is None
            or root_descriptor is None
            or parent is None
            or parent_descriptor is None
        ):
            raise EvidenceError("external runtime descriptor state is incomplete")
        try:
            if root.parent != parent or not root.name.startswith(
                f"pmux-p0-{self.run_id[:8]}-"
            ):
                raise EvidenceError(
                    "refusing to remove an unexpected external runtime path"
                )
            _verify_open_directory_path_identity(
                parent,
                parent_descriptor,
                require_owner_private=False,
            )
            _verify_open_directory_path_identity(root, root_descriptor)
            root_metadata = os.fstat(root_descriptor)
            _clear_directory_descriptor(
                root_descriptor,
                expected_device=root_metadata.st_dev,
            )
            _verify_open_directory_path_identity(root, root_descriptor)
            os.rmdir(root.name, dir_fd=parent_descriptor)
            os.fsync(parent_descriptor)
            if os.path.lexists(root):
                raise EvidenceError("external runtime pathname remains after removal")
            return True
        finally:
            os.close(root_descriptor)
            os.close(parent_descriptor)
            self.external_runtime_descriptor = None
            self.external_runtime_parent_descriptor = None


def _validate_private_artifact_entry(
    path: Path, *, expect_directory: bool
) -> os.stat_result:
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise EvidenceError(f"artifact entry disappeared: {path}") from error
    _validate_private_artifact_metadata(
        metadata, expect_directory=expect_directory, label=str(path)
    )
    return metadata


def _verify_artifact_directory_member(
    root: Path,
    parent_descriptor: int,
    root_descriptor: int,
    expected: os.stat_result,
) -> None:
    _verify_open_directory_path_identity(root.parent, parent_descriptor)
    opened = os.fstat(root_descriptor)
    try:
        current = os.stat(root.name, dir_fd=parent_descriptor, follow_symlinks=False)
    except FileNotFoundError as error:
        raise EvidenceError("artifact directory pathname disappeared") from error
    _validate_private_artifact_metadata(opened, expect_directory=True, label=str(root))
    _validate_private_artifact_metadata(current, expect_directory=True, label=str(root))
    if _artifact_directory_key(expected) != _artifact_directory_key(
        opened
    ) or _artifact_directory_key(opened) != _artifact_directory_key(current):
        raise EvidenceError("artifact directory pathname was replaced")


def _read_artifact_json(
    root: Path, relative: str, *, expected_manifest_sha256: str
) -> Any:
    _validate_digest(expected_manifest_sha256, "artifact manifest digest")
    parts = tuple(Path(relative).parts)
    if (
        Path(relative).is_absolute()
        or not parts
        or len(parts) > MAX_ARTIFACT_DEPTH
        or any(SAFE_NAME.fullmatch(part) is None for part in parts)
        or relative == "artifact-manifest.json"
    ):
        raise EvidenceError("artifact JSON path must be one safe manifest member")
    absolute, parent_descriptor, root_descriptor, root_metadata = (
        _open_artifact_directory_path(root)
    )
    try:
        first_audit = _audit_artifact_descriptor(
            root_descriptor, display_path=str(absolute)
        )
        if first_audit["manifest_sha256"] != expected_manifest_sha256:
            raise EvidenceError("artifact differs from its retained manifest anchor")
        manifest, _ = _artifact_manifest_from_descriptor(root_descriptor)
        expected_entries = {item["path"]: item for item in manifest["files"]}
        expected_entry = expected_entries.get(relative)
        if expected_entry is None:
            raise EvidenceError("artifact JSON member is absent from its manifest")
        descriptor = os.dup(root_descriptor)
        try:
            expected_device = os.fstat(root_descriptor).st_dev
            for part in parts[:-1]:
                child, _ = _open_private_child_directory_at(
                    descriptor,
                    part,
                    create=False,
                    expected_device=expected_device,
                )
                os.close(descriptor)
                descriptor = child
            payload, metadata = _read_private_artifact_file_at(
                descriptor,
                parts[-1],
                MAX_CAPTURE_BYTES,
                label=relative,
            )
        finally:
            os.close(descriptor)
        actual_entry = {
            "path": relative,
            "size": len(payload),
            "sha256": sha256_bytes(payload),
            "mode": stat.S_IMODE(metadata.st_mode),
        }
        if actual_entry != expected_entry:
            raise EvidenceError("artifact JSON member differs from its manifest")
        repeated = _audit_artifact_descriptor(
            root_descriptor, display_path=str(absolute)
        )
        if repeated != first_audit:
            raise EvidenceError("artifact tree changed while reading JSON evidence")
        _verify_artifact_directory_member(
            absolute,
            parent_descriptor,
            root_descriptor,
            root_metadata,
        )
        return strict_json_loads(payload, label=f"artifact {relative}")
    finally:
        os.close(root_descriptor)
        os.close(parent_descriptor)


def audit_artifact_directory(path: Path) -> dict[str, Any]:
    root, parent_descriptor, root_descriptor, root_metadata = (
        _open_artifact_directory_path(path)
    )
    try:
        audited = _audit_artifact_descriptor(root_descriptor, display_path=str(root))
        _verify_artifact_directory_member(
            root,
            parent_descriptor,
            root_descriptor,
            root_metadata,
        )
        return audited
    finally:
        os.close(root_descriptor)
        os.close(parent_descriptor)


def _evidence_root_snapshot(root: Path) -> dict[str, Any]:
    """Capture one complete descriptor-anchored evidence-root witness."""

    absolute = _assert_owner_only_directory(root, create=False)
    descriptor, opened = _open_private_directory_nofollow(absolute)
    try:
        _verify_open_directory_path_identity(absolute, descriptor)
        with os.scandir(descriptor) as iterator:
            names = sorted(entry.name for entry in iterator)
        if len(names) > MAX_ARTIFACT_ENTRIES:
            raise EvidenceError("evidence root exceeds its entry-count bound")
        entries: list[dict[str, Any]] = []
        for name in names:
            internal = SAFE_INTERNAL_NAME.fullmatch(name) is not None
            if SAFE_NAME.fullmatch(name) is None and not internal:
                raise EvidenceError(f"evidence root contains an unsafe name: {name}")
            child, metadata = _open_private_child_directory_at(
                descriptor,
                name,
                create=False,
                expected_device=opened.st_dev,
                allow_internal_name=internal,
            )
            try:
                current = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if _artifact_directory_key(os.fstat(child)) != (
                    _artifact_directory_key(current)
                ):
                    raise EvidenceError(
                        "evidence artifact changed during root snapshot"
                    )
                entries.append(
                    {
                        "name": name,
                        "directory_key": list(_artifact_directory_key(current)),
                    }
                )
            finally:
                os.close(child)
        with os.scandir(descriptor) as iterator:
            repeated_names = sorted(entry.name for entry in iterator)
        repeated = os.fstat(descriptor)
        if names != repeated_names or _artifact_directory_key(opened) != (
            _artifact_directory_key(repeated)
        ):
            raise EvidenceError("evidence root changed during its complete snapshot")
        _verify_open_directory_path_identity(absolute, descriptor)
        return {
            "root_key": list(_artifact_directory_key(repeated)),
            "entries": entries,
        }
    finally:
        os.close(descriptor)


def audit_campaign(
    *,
    ledger_path: Path,
    prefix: LedgerPrefix,
    campaign_id: str,
    evidence_root: Path,
    expected_campaign_anchors: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    supplied_campaign_anchors = dict(expected_campaign_anchors or {})
    for run_id, digest in supplied_campaign_anchors.items():
        if not isinstance(run_id, str) or not isinstance(digest, str):
            raise EvidenceError("campaign anchors must map UUIDs to digests")
        _validate_uuid(run_id, "campaign anchor run ID")
        _validate_digest(digest, "campaign artifact manifest digest")
    root = _assert_owner_only_directory(evidence_root, create=False)
    root_snapshot = _evidence_root_snapshot(root)
    ledger = inspect_ledger(ledger_path, prefix, campaign_id)
    all_reservations = ledger["reservations"]
    reservations = ledger["campaign_reservations"]
    reservation_by_attempt: dict[str, Mapping[str, Any]] = {}
    for reservation in all_reservations:
        attempt_id = reservation["attempt_id"]
        if attempt_id in reservation_by_attempt:
            raise EvidenceError("ledger contains a duplicate attempt identity")
        reservation_by_attempt[attempt_id] = reservation

    staging: list[str] = []
    unknown_entries: list[str] = []
    attempt_paths: dict[str, Path] = {}
    campaign_paths: dict[str, Path] = {}
    for name in [item["name"] for item in root_snapshot["entries"]]:
        entry = root / name
        metadata = entry.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            raise EvidenceError(f"evidence root contains a symlink: {entry.name}")
        if not stat.S_ISDIR(metadata.st_mode):
            raise EvidenceError(
                f"evidence root contains a special/non-directory entry: {entry.name}"
            )
        _validate_private_artifact_entry(entry, expect_directory=True)
        if entry.name.startswith(".") and entry.name.endswith(".staging"):
            staging.append(entry.name)
            continue
        if entry.name.startswith("attempt-"):
            identifier = entry.name.removeprefix("attempt-")
            try:
                _validate_uuid(identifier, "attempt artifact ID")
            except EvidenceError:
                audit_artifact_directory(entry)
                unknown_entries.append(entry.name)
                continue
            if identifier in attempt_paths:
                raise EvidenceError("multiple attempt artifacts have one identity")
            attempt_paths[identifier] = entry
            continue
        if entry.name.startswith("campaign-run-"):
            identifier = entry.name.removeprefix("campaign-run-")
            try:
                _validate_uuid(identifier, "campaign artifact run ID")
            except EvidenceError:
                audit_artifact_directory(entry)
                unknown_entries.append(entry.name)
                continue
            if identifier in campaign_paths:
                raise EvidenceError("multiple campaign artifacts have one run identity")
            campaign_paths[identifier] = entry
            continue
        audit_artifact_directory(entry)
        unknown_entries.append(entry.name)

    audited_attempts_by_id: dict[str, dict[str, Any]] = {}
    outcomes_by_id: dict[str, Mapping[str, Any]] = {}
    orphan_attempt_ids: list[str] = []
    for attempt_id, artifact in sorted(attempt_paths.items()):
        audited = audit_artifact_directory(artifact)
        artifact_reservation = _read_artifact_json(
            artifact,
            "reservation.json",
            expected_manifest_sha256=audited["manifest_sha256"],
        )
        if not isinstance(artifact_reservation, dict):
            raise EvidenceError("attempt artifact reservation is not an object")
        _validate_reservation_record(artifact_reservation)
        reservation = reservation_by_attempt.get(attempt_id)
        if reservation is None:
            reservation = artifact_reservation
            orphan_attempt_ids.append(attempt_id)
        elif artifact_reservation != reservation:
            raise EvidenceError(
                "attempt artifact reservation differs from the durable ledger"
            )
        expected_path = Path(reservation["artifact_directory"]).absolute()
        if expected_path != artifact:
            raise EvidenceError(
                "attempt artifact pathname differs from its reservation"
            )
        contract = reservation["campaign_contract"]
        expected_binding = {
            "campaign_id": reservation["campaign_id"],
            "run_id": reservation["run_id"],
            "attempt_id": attempt_id,
            "reservation_sha256": reservation["reservation_sha256"],
            "campaign_contract_sha256": reservation["campaign_contract_sha256"],
            "source_digest": contract["candidate"]["source"]["digest"],
        }
        if audited.get("binding") != expected_binding:
            raise EvidenceError("attempt artifact binding differs from its reservation")
        outcome = _read_artifact_json(
            artifact,
            "outcome.json",
            expected_manifest_sha256=audited["manifest_sha256"],
        )
        _validate_attempt_outcome(
            outcome,
            reservation,
            expected_prior_tokens=reservation["prior_observed_tokens"],
            require_success=False,
        )
        if audited.get("status") != outcome.get("status"):
            raise EvidenceError("attempt manifest and outcome statuses disagree")
        audited.update(
            {
                "attempt_id": attempt_id,
                "campaign_id": reservation["campaign_id"],
                "run_id": reservation["run_id"],
                "source_digest": expected_binding["source_digest"],
                "campaign_contract_sha256": reservation["campaign_contract_sha256"],
                "outcome_status": outcome["status"],
                "cumulative_observed_tokens": outcome["cumulative_observed_tokens"],
            }
        )
        audited_attempts_by_id[attempt_id] = audited
        outcomes_by_id[attempt_id] = outcome

    missing_all: list[str] = []
    usage_unknown_campaigns: set[str] = set()
    prior_by_campaign: dict[str, int] = {}
    for reservation in all_reservations:
        record_root = Path(reservation["artifact_directory"]).absolute().parent
        if record_root != root:
            if reservation["campaign_id"] == campaign_id:
                raise EvidenceError("campaign reservation points outside evidence root")
            continue
        attempt_id = reservation["attempt_id"]
        expected_prior = prior_by_campaign.setdefault(reservation["campaign_id"], 0)
        if reservation["prior_observed_tokens"] != expected_prior:
            raise EvidenceError("campaign durable usage continuity is invalid")
        outcome = outcomes_by_id.get(attempt_id)
        if outcome is None:
            missing_all.append(attempt_id)
            usage_unknown_campaigns.add(reservation["campaign_id"])
            continue
        cumulative = _validate_attempt_outcome(
            outcome,
            reservation,
            expected_prior_tokens=expected_prior,
            require_success=False,
        )
        prior_by_campaign[reservation["campaign_id"]] = cumulative

    campaign_artifacts_all: dict[str, dict[str, Any]] = {}
    orphan_campaign_run_ids: list[str] = []
    campaign_calibrations: list[dict[str, Any]] = []
    campaign_drain_values: set[int] = set()
    summary_required = {
        "schema",
        "campaign_id",
        "run_id",
        "status",
        "error",
        "campaign_contract",
        "campaign_contract_sha256",
        "source",
        "binaries",
        "claude",
        "rmux",
        "platform",
        "scenario",
        "public_entrypoint",
        "exercised_binaries",
        "attempts",
        "attempt_count",
        "observed_tokens",
        "max_observed_tokens",
        "cleanup",
        "authority",
        "drain_calibration",
    }
    for run_id, path in sorted(campaign_paths.items()):
        audited = audit_artifact_directory(path)
        summary = _read_artifact_json(
            path,
            "campaign.json",
            expected_manifest_sha256=audited["manifest_sha256"],
        )
        if (
            not isinstance(summary, dict)
            or set(summary) != summary_required
            or summary.get("schema") != CAMPAIGN_SCHEMA
        ):
            raise EvidenceError("campaign summary schema is invalid")
        if summary.get("run_id") != run_id or not isinstance(
            summary.get("campaign_id"), str
        ):
            raise EvidenceError("campaign summary identity is invalid")
        _validate_uuid(summary["campaign_id"], "campaign summary ID")
        contract = summary.get("campaign_contract")
        if not isinstance(contract, dict):
            raise EvidenceError("campaign summary has no immutable contract")
        _validate_campaign_contract(
            contract, expected_campaign_id=summary["campaign_id"]
        )
        contract_digest = campaign_contract_sha256(contract)
        if summary.get("campaign_contract_sha256") != contract_digest:
            raise EvidenceError("campaign summary contract digest is invalid")
        source_digest = contract["candidate"]["source"]["digest"]
        expected_binding = {
            "campaign_id": summary["campaign_id"],
            "run_id": run_id,
            "campaign_contract_sha256": contract_digest,
            "source_digest": source_digest,
            "ledger_prefix_sha256": prefix.sha256,
        }
        if audited.get("binding") != expected_binding:
            raise EvidenceError("campaign artifact binding differs from its summary")
        if audited.get("status") != summary.get("status") or summary.get(
            "status"
        ) not in {"acquired", "failed"}:
            raise EvidenceError("campaign manifest and summary statuses disagree")
        if (
            summary.get("source") != contract["candidate"]["source"]
            or summary.get("binaries") != contract["candidate"]["binaries"]
            or summary.get("claude") != contract["candidate"]["claude"]
            or summary.get("rmux") != contract["candidate"]["rmux"]
            or summary.get("platform") != contract["platform"]
            or summary.get("scenario") != contract["scenario"]
            or summary.get("max_observed_tokens") != contract["max_observed_tokens"]
        ):
            raise EvidenceError("campaign summary differs from its immutable contract")
        expected_entrypoint = (
            "claude-p" if contract["scenario"] == "claude-p-one-shot" else "pmux"
        )
        expected_exercised = {"pmuxd", "pmux-rmuxd", "pmux-launcher"}
        expected_exercised.add(expected_entrypoint)
        if contract["cell"]["lifecycle"] == "hybrid":
            expected_exercised.add("pmux-hook")
        if (
            summary.get("public_entrypoint") != expected_entrypoint
            or summary.get("exercised_binaries") != sorted(expected_exercised)
            or summary.get("authority")
            != {
                "pmux_exit_and_public_result": "authoritative",
                "transcript_parsed_by_envelope": False,
                "terminal_interpreted_by_envelope": False,
                "direct_input_by_envelope": False,
            }
            or not (
                summary.get("error") is None or isinstance(summary.get("error"), str)
            )
        ):
            raise EvidenceError("campaign summary authority fields are invalid")
        run_reservations = [
            item for item in all_reservations if item.get("run_id") == run_id
        ]
        if not run_reservations:
            orphan_campaign_run_ids.append(run_id)
        elif any(
            item["campaign_id"] != summary["campaign_id"]
            or item["campaign_contract_sha256"] != contract_digest
            for item in run_reservations
        ):
            raise EvidenceError("campaign run reservations use a different contract")
        summary_attempts = summary.get("attempts")
        if (
            not isinstance(summary_attempts, list)
            or summary.get("attempt_count") != len(summary_attempts)
            or len(summary_attempts) != len(run_reservations)
        ):
            raise EvidenceError("campaign summary attempt accounting is invalid")
        observed_calibrations: list[dict[str, Any]] = []
        expected_by_id = {item["attempt_id"]: item for item in run_reservations}
        if {
            item.get("attempt_id")
            for item in summary_attempts
            if isinstance(item, dict)
        } != set(expected_by_id):
            raise EvidenceError("campaign summary attempts differ from reservations")
        for item in summary_attempts:
            if not isinstance(item, dict) or set(item) != {
                "global_attempt_ordinal",
                "attempt_id",
                "status",
                "campaign_contract_sha256",
                "cumulative_observed_tokens",
                "evidence_directory",
                "reservation_sha256",
                "artifact_manifest_sha256",
                # An attempt that spent an ordinal must be able to say WHY it
                # failed in the summary a human actually reads, and must record
                # the post-command source check as a fact separate from the
                # product verdict.
                "error",
                "post_command_source_check",
            }:
                raise EvidenceError("campaign summary attempt entry is invalid")
            reservation = expected_by_id[item["attempt_id"]]
            attempt = audited_attempts_by_id.get(item["attempt_id"])
            outcome = outcomes_by_id.get(item["attempt_id"])
            if attempt is None or outcome is None:
                raise EvidenceError(
                    "campaign summary references missing attempt evidence"
                )
            expected_item = {
                "global_attempt_ordinal": reservation["global_attempt_ordinal"],
                "attempt_id": reservation["attempt_id"],
                "status": outcome["status"],
                "campaign_contract_sha256": contract_digest,
                "cumulative_observed_tokens": outcome["cumulative_observed_tokens"],
                "evidence_directory": reservation["artifact_directory"],
                "reservation_sha256": reservation["reservation_sha256"],
                "artifact_manifest_sha256": attempt["manifest_sha256"],
                # Cross-checked against the attempt's own outcome, so the
                # summary cannot disagree with the artifact it summarizes about
                # why an ordinal was spent or whether the source held still.
                "error": outcome["error"],
                "post_command_source_check": outcome["post_command_source_check"],
            }
            if item != expected_item:
                raise EvidenceError("campaign summary attempt evidence is invalid")
            if outcome["status"] == "pmux_exit_zero":
                observed_calibrations.append(
                    dict(outcome["public_result_binding"]["drain_calibration"])
                )
        tested_profile = contract["cell"]["tested_profile"]
        configured_drain = (
            tested_profile["transcript_drain_ms"]
            if tested_profile is not None
            else contract["cell"]["untested_transcript_drain_ms"]
        )
        expected_calibration = summarize_drain_calibration(
            observed_calibrations,
            configured_transcript_drain_ms=configured_drain,
        )
        if summary["campaign_id"] == campaign_id:
            campaign_calibrations.extend(observed_calibrations)
            campaign_drain_values.add(configured_drain)
        if summary.get("drain_calibration") != expected_calibration:
            raise EvidenceError(
                "campaign drain calibration is not the exact summary of its attempts"
            )
        expected_observed = (
            summary_attempts[-1]["cumulative_observed_tokens"]
            if summary_attempts
            else 0
        )
        if summary.get("observed_tokens") != expected_observed:
            raise EvidenceError("campaign summary observed usage is invalid")
        cleanup = summary.get("cleanup")
        if not isinstance(cleanup, dict) or cleanup.get("status") not in {
            "verified",
            "failed",
            "inconclusive",
            "not_attempted",
        }:
            raise EvidenceError("campaign cleanup proof is invalid")
        if run_reservations:
            ready = _read_artifact_json(
                path,
                "daemon-ready.json",
                expected_manifest_sha256=audited["manifest_sha256"],
            )
            if not isinstance(ready, dict) or set(ready) != {
                "command",
                "public_binding",
                "socket_identity",
            }:
                raise EvidenceError("campaign daemon readiness evidence is invalid")
            public_ping_binding(ready["public_binding"])
        acquired = (
            bool(run_reservations)
            and len(run_reservations) == len(contract["prompt_suite"])
            and all(
                outcomes_by_id[item["attempt_id"]]["status"] == "pmux_exit_zero"
                for item in run_reservations
            )
            and cleanup.get("status") == "verified"
            and cleanup.get("external_runtime_removed") is True
        )
        if (summary["status"] == "acquired") != acquired:
            raise EvidenceError("campaign acquisition status contradicts its evidence")
        if acquired and contract["scenario"] in {"persistent", "resume"}:
            final_reservation = run_reservations[-1]
            final_outcome = outcomes_by_id[final_reservation["attempt_id"]]
            result_binding = final_outcome["public_result_binding"]
            close_evidence = _read_artifact_json(
                path,
                "pmux-close.json",
                expected_manifest_sha256=audited["manifest_sha256"],
            )
            if not isinstance(close_evidence, dict) or not isinstance(
                close_evidence.get("public_binding"), dict
            ):
                raise EvidenceError("campaign close evidence has no public proof")
            close_binding = public_close_binding(
                close_evidence["public_binding"],
                expected_session_id=result_binding["session_id"],
                expected_generation_id=result_binding["generation_id"],
            )
            close_stdout = _read_artifact_json(
                path,
                "pmux-close.stdout.json",
                expected_manifest_sha256=audited["manifest_sha256"],
            )
            if close_stdout != close_binding:
                raise EvidenceError("campaign close stdout differs from its proof")
        audited.update(
            {
                "run_id": run_id,
                "campaign_id": summary["campaign_id"],
                "source_digest": source_digest,
                "campaign_contract_sha256": contract_digest,
                "campaign_status": summary["status"],
                "cleanup_status": cleanup.get("status"),
                "external_runtime_removed": cleanup.get("external_runtime_removed"),
            }
        )
        campaign_artifacts_all[run_id] = audited

    expected_run_ids = {item["run_id"] for item in reservations}
    published_target_runs = {
        run_id
        for run_id, item in campaign_artifacts_all.items()
        if item["campaign_id"] == campaign_id
    }
    missing_run_ids = sorted(expected_run_ids - published_target_runs)
    missing = sorted(
        item["attempt_id"]
        for item in reservations
        if item["attempt_id"] not in outcomes_by_id
    )
    attempts = [
        audited_attempts_by_id[item["attempt_id"]]
        for item in reservations
        if item["attempt_id"] in audited_attempts_by_id
    ]
    campaign_artifacts = [
        campaign_artifacts_all[run_id] for run_id in sorted(published_target_runs)
    ]
    attempt_failures = sorted(
        item["attempt_id"]
        for item in reservations
        if item["attempt_id"] in outcomes_by_id
        and outcomes_by_id[item["attempt_id"]]["status"] != "pmux_exit_zero"
    )
    run_failures = sorted(
        run_id
        for run_id in expected_run_ids
        if run_id not in campaign_artifacts_all
        or campaign_artifacts_all[run_id]["campaign_status"] != "acquired"
        or campaign_artifacts_all[run_id]["cleanup_status"] != "verified"
        or campaign_artifacts_all[run_id]["external_runtime_removed"] is not True
    )
    contract_digests = sorted(
        {item["campaign_contract_sha256"] for item in reservations}
    )
    source_digests = sorted(
        {
            item["campaign_contract"]["candidate"]["source"]["digest"]
            for item in reservations
        }
    )
    residue = bool(
        staging or unknown_entries or orphan_attempt_ids or orphan_campaign_run_ids
    )
    supplied_run_ids = set(supplied_campaign_anchors)
    missing_anchor_run_ids = sorted(expected_run_ids - supplied_run_ids)
    extra_anchor_run_ids = sorted(supplied_run_ids - expected_run_ids)
    mismatched_anchor_run_ids = sorted(
        run_id
        for run_id in expected_run_ids & supplied_run_ids & set(campaign_artifacts_all)
        if campaign_artifacts_all[run_id]["manifest_sha256"]
        != supplied_campaign_anchors[run_id]
    )
    campaign_anchors_verified = not (
        missing_anchor_run_ids
        or extra_anchor_run_ids
        or mismatched_anchor_run_ids
        or missing_run_ids
    )
    accounting_complete = bool(reservations) and not missing and not missing_run_ids
    promotion_reasons: list[str] = []
    if not accounting_complete:
        promotion_reasons.append("evidence accounting is incomplete")
    if residue:
        promotion_reasons.append("evidence root contains unaccounted residue")
    if campaign_id in usage_unknown_campaigns:
        promotion_reasons.append("campaign observed-token usage is unknown")
    if attempt_failures:
        promotion_reasons.append("one or more product attempts failed")
    if run_failures:
        promotion_reasons.append(
            "one or more campaign runs failed acquisition or cleanup"
        )
    if len(contract_digests) != 1:
        promotion_reasons.append(
            "campaign evidence is not bound to exactly one immutable contract"
        )
    if len(source_digests) != 1:
        promotion_reasons.append(
            "campaign evidence is not bound to exactly one source digest"
        )
    if not campaign_anchors_verified:
        promotion_reasons.append(
            "externally retained campaign manifest anchors are incomplete or mismatched"
        )
    promotion_eligible = not promotion_reasons
    verdict = "complete" if accounting_complete and not residue else "incomplete"
    if _evidence_root_snapshot(root) != root_snapshot:
        raise EvidenceError("evidence root changed during campaign audit")
    return {
        "schema_version": SCHEMA_VERSION,
        "campaign_id": campaign_id,
        "verdict": verdict,
        "accounting_verdict": verdict,
        "promotion_verdict": "eligible" if promotion_eligible else "ineligible",
        "promotion_eligible": promotion_eligible,
        "promotion_blockers": promotion_reasons,
        "ledger": {
            key: value
            for key, value in ledger.items()
            if key not in {"reservations", "campaign_reservations"}
        },
        "reservation_count": len(reservations),
        "campaign_contract_sha256": contract_digests[0]
        if len(contract_digests) == 1
        else None,
        "durable_observed_tokens": (
            None
            if campaign_id in usage_unknown_campaigns
            else prior_by_campaign.get(campaign_id)
        ),
        "drain_calibration": summarize_drain_calibration(
            campaign_calibrations,
            configured_transcript_drain_ms=(
                next(iter(campaign_drain_values))
                if len(campaign_drain_values) == 1
                else None
            ),
        ),
        "attempt_artifacts": attempts,
        "missing_attempt_artifacts": missing,
        "missing_reservation_artifacts_all_campaigns": sorted(missing_all),
        "campaign_artifacts": campaign_artifacts,
        "missing_campaign_run_ids": missing_run_ids,
        "staging_residue": sorted(staging),
        "unknown_artifact_entries": sorted(unknown_entries),
        "orphan_attempt_artifact_ids": sorted(orphan_attempt_ids),
        "orphan_campaign_run_ids": sorted(orphan_campaign_run_ids),
        "failed_attempt_ids": attempt_failures,
        "failed_campaign_run_ids": run_failures,
        "source_digests": source_digests,
        "campaign_anchors": {
            "verified": campaign_anchors_verified,
            "required_run_ids": sorted(expected_run_ids),
            "supplied": dict(sorted(supplied_campaign_anchors.items())),
            "missing_run_ids": missing_anchor_run_ids,
            "extra_run_ids": extra_anchor_run_ids,
            "mismatched_run_ids": mismatched_anchor_run_ids,
        },
    }


def safe_remove_unpublished_staging(path: Path, evidence_root: Path) -> None:
    """Remove one exact unpublished staging directory after explicit review.

    This helper is intentionally not exposed by the live command. It exists for
    self-tests and future operator tooling that has already matched an exact
    path. It refuses anything outside the evidence root or without the staging
    marker.
    """

    root = _assert_owner_only_directory(evidence_root, create=False)
    target = path.absolute()
    if (
        target.parent != root
        or SAFE_INTERNAL_NAME.fullmatch(target.name) is None
        or not target.name.endswith(".staging")
    ):
        raise EvidenceError("refusing to remove a non-exact staging directory")
    root_descriptor, root_metadata = _open_private_directory_nofollow(root)
    target_descriptor = -1
    try:
        _verify_open_directory_path_identity(root, root_descriptor)
        target_descriptor, target_metadata = _open_private_child_directory_at(
            root_descriptor,
            target.name,
            create=False,
            expected_device=root_metadata.st_dev,
            allow_internal_name=True,
        )
        _clear_directory_descriptor(
            target_descriptor, expected_device=target_metadata.st_dev
        )
        current = os.stat(target.name, dir_fd=root_descriptor, follow_symlinks=False)
        if _artifact_directory_key(os.fstat(target_descriptor)) != (
            _artifact_directory_key(current)
        ):
            raise EvidenceError("artifact staging directory changed before removal")
        _verify_open_directory_path_identity(root, root_descriptor)
        os.rmdir(target.name, dir_fd=root_descriptor)
        os.fsync(root_descriptor)
        try:
            os.stat(target.name, dir_fd=root_descriptor, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise EvidenceError("artifact staging directory remains after removal")
        _verify_open_directory_path_identity(root, root_descriptor)
    finally:
        if target_descriptor >= 0:
            os.close(target_descriptor)
        os.close(root_descriptor)
