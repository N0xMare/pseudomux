from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import dataclasses
import hashlib
import io
import json
import os
import re
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import unittest
import uuid
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
PHASE0 = ROOT / "tools" / "phase0"
sys.path.insert(0, str(PHASE0))

import phase0  # noqa: E402
import phase0_lib  # noqa: E402
from phase0 import main  # noqa: E402
from phase0_lib import (  # noqa: E402
    ARTIFACT_SCHEMA,
    DENIED_TOOL_FLAG,
    LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD,
    PUBLIC_ENTRYPOINT_LAUNCH_SURFACE,
    DRAIN_CALIBRATION_SCHEMA,
    FACADE_PERMISSION_MODES,
    KNOWN_TURN_TIMING_FIELDS,
    LEGACY_LAUNCH_OPTION_KEYS,
    MAX_DENIED_TOOLS,
    MAX_SYSTEM_PROMPT_BYTES,
    PERMISSION_MODES,
    REQUIRED_RELEASE_BINARIES,
    RESERVATION_SCHEMA,
    SYSTEM_PROMPT_DELIVERY,
    AtomicArtifactDirectory,
    BudgetExhausted,
    CampaignConfig,
    # Deliberately retained placeholder: `CampaignInterrupted` is raised at
    # phase0_lib.py:5248 and :5708 but no test asserts either path yet.
    # Tracked in docs/current-state.md. Remove the import only together with that entry.
    CampaignInterrupted,  # noqa: F401
    CampaignRunner,
    EvidenceError,
    FileIdentity,
    LedgerPrefix,
    audit_artifact_directory,
    audit_campaign,
    build_campaign_contract,
    campaign_contract_sha256,
    capture_socket_identity,
    clap_long_options,
    canonical_json_bytes,
    compute_source_identity,
    current_platform_identity,
    drain_calibration_from_timings,
    dry_run_manifest,
    environment_identity,
    extract_public_handle,
    identify_file,
    identify_directory,
    identify_prompt,
    inspect_ledger,
    observed_tokens_from_public_result,
    parse_public_output,
    public_result,
    public_result_binding,
    public_close_binding,
    public_ping_binding,
    read_profile,
    real_claude_turns_outside_the_ledger,
    read_system_prompt,
    redact_text,
    reserve_attempt,
    run_command,
    sha256_bytes,
    strict_json_loads,
    summarize_drain_calibration,
    turn_timings_binding,
    validate_config,
    verify_socket_identity,
    verify_directory_identity,
    _verify_open_path_identity,
)


EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
CAMPAIGN_ID = "11111111-1111-4111-8111-111111111111"
ATTEMPT_ID = "22222222-2222-4222-8222-222222222222"
SESSION_ID = "33333333-3333-4333-8333-333333333333"
TURN_ID = "44444444-4444-4444-8444-444444444444"
RUN_ID = "55555555-5555-4555-8555-555555555555"
GENERATION_ID = "66666666-6666-4666-8666-666666666666"
PROCESS_FIXTURE_SOURCE = PHASE0 / "tests" / "fixtures" / "process_fixture.c"
# One published TurnTimings shape. `last_transcript_row_at_ms` stands in for the
# late-row field the product publishes: this tool discovers it rather than
# naming it, so the fixture name deliberately differs from any real one.
TURN_TIMINGS = {
    "submitted_at_ms": 1_000,
    "prompt_acknowledged_at_ms": 1_700,
    "terminal_candidate_at_ms": 9_000,
    "completed_at_ms": 11_400,
    "drain_ms": 2_400,
    "last_transcript_row_at_ms": 9_000,
}


def compile_process_fixture(destination: Path) -> Path:
    compiler = shutil.which("cc")
    if compiler is None:
        raise unittest.SkipTest("a C compiler is required for native process fixtures")
    destination.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            compiler,
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            str(PROCESS_FIXTURE_SOURCE),
            "-o",
            str(destination),
        ],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    destination.chmod(0o700)
    return destination


def make_private_directory(path: Path) -> Path:
    path.mkdir(parents=True, mode=0o700)
    path.chmod(0o700)
    return path


def write_file(path: Path, payload: bytes, mode: int = 0o600) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(payload)
    path.chmod(mode)
    return path


def fake_identity(path: Path, payload: bytes = b"binary") -> FileIdentity:
    write_file(path, payload, 0o700)
    return identify_file(path, executable=True)


def make_fixture_source_repo(source: Path) -> None:
    """Create a private single-commit repo that observe_source_identity accepts.

    The tree carries the live copies of the two authority files the capture
    revalidates (source_digest.py and bounded_process.py) plus a minimal
    workspace marker, and its own `.git`, so capturing it never touches the
    live checkout's `.git`.
    """

    destination = source / "tools" / "linux-docker" / "source_digest.py"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "tools" / "linux-docker" / "source_digest.py", destination)
    common = source / "tools" / "evidence_common" / "bounded_process.py"
    common.parent.mkdir(parents=True)
    shutil.copy2(ROOT / "tools" / "evidence_common" / "bounded_process.py", common)
    write_file(source / "Cargo.toml", b"[workspace]\n")
    subprocess.run(["git", "init", "-q", str(source)], check=True)
    subprocess.run(["git", "-C", str(source), "add", "."], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(source),
            "-c",
            "user.name=pmux-test",
            "-c",
            "user.email=pmux-test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
        check=True,
    )


_SOURCE_IDENTITY_TEMPLATE: dict[str, object] | None = None


def fake_source_identity(digest: str = "a" * 64) -> dict[str, object]:
    global _SOURCE_IDENTITY_TEMPLATE
    if _SOURCE_IDENTITY_TEMPLATE is None:
        # Computed from a hermetic fixture repo, NOT from the live checkout.
        # `compute_source_identity(ROOT)` reads the live `.git`'s own stat
        # identity, so any concurrent git command (an editor, a workspace
        # poller) aborted the capture and made whichever test first needed the
        # template -- in practice
        # test_audit_reports_reserved_crash_without_artifact_as_incomplete --
        # flake in a git-polled workspace. The template is used only for its
        # shape; every caller substitutes the digest, so a fixture-derived
        # identity asserts exactly as much as a live-derived one did.
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            make_fixture_source_repo(source)
            _SOURCE_IDENTITY_TEMPLATE = compute_source_identity(source)
    value = json.loads(json.dumps(_SOURCE_IDENTITY_TEMPLATE))
    value["digest"] = digest
    return value


def fake_version_identity(version: str = "9.9.9") -> dict[str, object]:
    stdout = f"fake Claude {version}\n".encode()
    stderr = b""
    return {
        "stdout_sha256": sha256_bytes(stdout),
        "stderr_sha256": sha256_bytes(stderr),
        "combined_sha256": sha256_bytes(stdout + stderr),
        "stdout_bytes": len(stdout),
        "stderr_bytes": len(stderr),
        "normalized_version": version,
        "stdout_text": stdout.decode(),
    }


def replace_artifact_tree_with_reissued_manifest(path: Path) -> str:
    """Replace one valid artifact with a separately issued, self-consistent tree."""

    replacement = path.parent / f".replacement-{uuid.uuid4().hex}"
    displaced = path.parent / f".displaced-{uuid.uuid4().hex}"
    shutil.copytree(path, replacement)
    manifest_path = replacement / "artifact-manifest.json"
    manifest = strict_json_loads(
        manifest_path.read_bytes(), label="test replacement artifact manifest"
    )
    manifest["published_at"] = f"{manifest['published_at']}-reissued"
    write_file(manifest_path, phase0_lib.pretty_json_bytes(manifest))
    path.rename(displaced)
    replacement.rename(path)
    shutil.rmtree(displaced)
    return audit_artifact_directory(path)["manifest_sha256"]


def base_config(root: Path, *, live: bool = False) -> CampaignConfig:
    source = root / "source"
    release = root / "release"
    evidence = make_private_directory(root / "evidence")
    ledger_parent = make_private_directory(root / "ledger")
    prompt = write_file(root / "prompt.txt", b"bounded prompt\n")
    claude = write_file(root / "claude", b"#!/bin/sh\nexit 0\n", 0o700)
    hashes = {name: "0" * 64 for name in REQUIRED_RELEASE_BINARIES}
    return CampaignConfig(
        source_root=source,
        expected_source_digest="1" * 64,
        release_bin_dir=release,
        expected_binary_hashes=hashes,
        claude_bin=claude,
        expected_claude_sha256=identify_file(claude).sha256,
        cwd=root,
        prompt_paths=(prompt,),
        evidence_root=evidence,
        ledger_path=ledger_parent / "attempts.ndjson",
        ledger_prefix=LedgerPrefix(0, EMPTY_SHA256, 0),
        prior_campaign_anchors={},
        campaign_id=CAMPAIGN_ID,
        global_attempt_ceiling=60,
        max_attempts_this_run=1,
        max_observed_tokens=10_000,
        scenario="one-shot",
        resume_session_id=None,
        model="sonnet",
        allowed_model_ids=("claude-sonnet-5-test",),
        effort="low",
        output_format="json",
        compatibility="allow-untested",
        tested_profile_path=None,
        terminal_rows=24,
        terminal_cols=120,
        terminal_profile="transparent",
        input_transport="sdk",
        lifecycle="transcript",
        untested_transcript_drain_ms=2_000,
        turn_timeout_seconds=5,
        daemon_ready_timeout_seconds=5,
        daemon_shutdown_timeout_seconds=5,
        live=live,
        acknowledge_usage=live,
        acknowledge_untested=live,
    )


class LedgerBudgetTests(unittest.TestCase):
    """The budget is counted from the ledger, never written down beside it.

    `evidence/README.md` published "47 of the authorized 100 global attempts are
    consumed; 53 remain" against a file that had already reached ordinal 81 --
    85 consumed, 15 left -- and the one-liner it printed to check that number
    disagreed with it too. Prose cannot be appended to by a reservation, so the
    figure is gone and `summarize_attempt_ledger` computes it instead.
    """

    def ledger(self, directory: Path, records: list[dict[str, object]]) -> Path:
        path = Path(directory) / "ledger.ndjson"
        path.write_text(
            "".join(f"{json.dumps(record)}\n" for record in records), encoding="utf-8"
        )
        return path

    def test_the_budget_is_derived_across_both_ordinal_spellings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = self.ledger(
                Path(directory),
                [
                    {"global_attempt": 5, "kind": "approved_prior_baseline"},
                    {"global_attempt": 6},
                    {"global_attempt_ordinal": 7, "global_attempt_ceiling": 100},
                    {"global_attempt_ordinal": 8, "global_attempt_ceiling": 100},
                ],
            )
            summary = phase0_lib.summarize_attempt_ledger(path, detached=4)
        self.assertEqual(
            summary,
            {
                "records": 4,
                "first_ordinal": 5,
                "last_ordinal": 8,
                "predating_the_file": 4,
                "detached": 4,
                "consumed": 12,
                "ceiling": 100,
                "ceilings_recorded": [100],
                "remaining": 88,
            },
        )

    def test_a_scan_that_knows_only_the_legacy_spelling_is_impossible_here(
        self,
    ) -> None:
        # The tuple is shared with `_recognized_prefix_last`; a second copy is
        # what made a hand-written recount stop at ordinal 29.
        self.assertIn("global_attempt", phase0_lib.ORDINAL_SPELLINGS)
        self.assertIn("global_attempt_ordinal", phase0_lib.ORDINAL_SPELLINGS)
        with tempfile.TemporaryDirectory() as directory:
            path = self.ledger(
                Path(directory),
                [{"global_attempt": 5}, {"global_attempt_ordinal": 6}],
            )
            summary = phase0_lib.summarize_attempt_ledger(path, detached=0)
        self.assertEqual(summary["last_ordinal"], 6)

    def test_the_recorded_ceiling_wins_over_the_constant(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = self.ledger(
                Path(directory),
                [{"global_attempt_ordinal": 1, "global_attempt_ceiling": 60}],
            )
            summary = phase0_lib.summarize_attempt_ledger(path, detached=0)
        self.assertEqual(summary["ceiling"], 60)
        self.assertEqual(summary["remaining"], 59)

    def test_an_uncountable_ledger_refuses_rather_than_reports(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cases = {
                "not contiguous": [
                    {"global_attempt": 5},
                    {"global_attempt_ordinal": 7},
                ],
                "spells its ordinal in none of": [
                    {"global_attempt": 5},
                    {"attempt": 6},
                ],
                "outside the explicit range": [
                    {"global_attempt_ordinal": 5, "global_attempt_ceiling": 101},
                ],
                "already consumed": [
                    {"global_attempt_ordinal": 99, "global_attempt_ceiling": 100},
                ],
                "states no budget": [],
            }
            for expected, records in cases.items():
                with self.subTest(expected=expected):
                    path = root / f"{abs(hash(expected))}.ndjson"
                    path.write_text(
                        "".join(f"{json.dumps(r)}\n" for r in records), encoding="utf-8"
                    )
                    with self.assertRaisesRegex(EvidenceError, re.escape(expected)):
                        phase0_lib.summarize_attempt_ledger(path, detached=4)

    def test_every_subcommand_the_parser_offers_is_listed_in_the_readme(self) -> None:
        """Derived from argparse, because a hand-kept list loses its newest row.

        `budget` was added to close a stale figure in `evidence/README.md`; a
        command table beside it that still named four commands would be the same
        defect one directory over.
        """

        actions = [
            action
            for action in phase0._parser()._actions
            if isinstance(action, argparse._SubParsersAction)
        ]
        self.assertEqual(len(actions), 1)
        offered = set(actions[0].choices)
        readme = (PHASE0 / "README.md").read_text(encoding="utf-8")
        listed = set(re.findall(r"^phase0\.py (\S+)", readme, flags=re.MULTILINE))
        self.assertEqual(listed, offered)


class LaunchSurfaceTests(unittest.TestCase):
    """What a campaign can launch, held to what the product accepts.

    Both halves of this had already failed silently. `--agent-file` was renamed
    to `--profile-file` and kept only as a HIDDEN spelling that refuses by name,
    and the argv builder went on emitting it -- through `claude-p`, which
    declares no profile option at all -- so a campaign configured with a profile
    could not launch, and would have found out one ordinal after reserving one.
    And `--cell` has never been forwarded, which is why no phase0 campaign has
    ever exercised `SessionCell::Minified`, the only thing
    `require_tested_for_minified_cell` gates.

    The option sets are read from the clap declarations rather than listed here.
    """

    def _emitted(self, entrypoint: str, *, profile: bool = True) -> set[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = dataclasses.replace(
                base_config(root),
                agent_name="reviewer" if profile else None,
                agent_file=(
                    write_file(root / "profile.json", b"{}\n") if profile else None
                ),
                permission_mode="dont-ask",
                denied_tools=("*",),
                environment_set={"PHASE0_SET": "v"},
                environment_passthrough_names=("PHASE0_PASSTHROUGH",),
            )
            runner = CampaignRunner(config, {})
            runner.system_prompt_text = "a bounded replacement"
            runner.claude_identity = identify_file(config.claude_bin)
            runner._cwd = lambda: root  # type: ignore[method-assign]
            argv = list(runner._forwarded_launch_args(entrypoint))
            if entrypoint == "pmux":
                argv += runner._launch_args("a-session", resume=False)
                argv += runner._launch_args("a-session", resume=True)
            return {token for token in argv if token.startswith("--")}

    def _declared(self, entrypoint: str) -> tuple[set[str], set[str]]:
        path, struct = PUBLIC_ENTRYPOINT_LAUNCH_SURFACE[entrypoint]
        return clap_long_options((ROOT / path).read_text(encoding="utf-8"), struct)

    def test_every_option_phase0_emits_is_one_the_entrypoint_still_accepts(
        self,
    ) -> None:
        for entrypoint in sorted(PUBLIC_ENTRYPOINT_LAUNCH_SURFACE):
            with self.subTest(entrypoint=entrypoint):
                accepted, retired = self._declared(entrypoint)
                emitted = self._emitted(entrypoint, profile=entrypoint == "pmux")
                self.assertEqual(
                    emitted & retired,
                    set(),
                    f"{entrypoint}: a retired spelling parses and is then refused "
                    "by name, so emitting one costs an ordinal to discover",
                )
                self.assertLessEqual(
                    emitted,
                    accepted,
                    f"{entrypoint}: an option this envelope emits is not declared",
                )

    def test_a_profile_campaign_is_refused_by_the_entrypoint_that_has_no_profile(
        self,
    ) -> None:
        """`claude-p` declares no `--profile`/`--profile-file`, so a campaign
        configured with one is refused at argv-build time rather than by clap
        after an ordinal has been reserved."""

        accepted, _ = self._declared("claude-p")
        self.assertNotIn("--profile", accepted)
        self.assertNotIn("--profile-file", accepted)
        with self.assertRaisesRegex(EvidenceError, "no client-side profile option"):
            self._emitted("claude-p", profile=True)
        self.assertIn("--profile", self._emitted("pmux", profile=True))

    def test_every_pmux_start_option_is_forwarded_or_has_a_written_reason(
        self,
    ) -> None:
        accepted, _ = self._declared("pmux")
        emitted = self._emitted("pmux")
        self.assertEqual(
            sorted(accepted - emitted),
            sorted(LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD),
            "an option `pmux start` accepts is neither forwarded nor given a "
            "reason for not being forwarded",
        )
        # A reason is either written here or is a cross-reference to one that
        # is. "see --other." with no --other in this table is how a reason
        # becomes a pointer at nothing.
        for option, reason in LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD.items():
            with self.subTest(option=option):
                referred = re.fullmatch(r"see (--[a-z-]+)\.", reason.strip())
                if referred is not None:
                    self.assertIn(
                        referred.group(1),
                        LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD,
                        f"{option} defers to an option this table does not hold",
                    )
                    continue
                self.assertGreater(
                    len(reason), 40, f"{option} is neither explained nor deferred"
                )

    def test_the_cell_flag_is_the_one_that_makes_a_campaign_path_b(self) -> None:
        """`--cell` is absent from the campaign envelope on purpose, and the
        reason names where minified-cell evidence comes from instead."""

        self.assertIn("--cell", LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD)
        reason = LAUNCH_OPTIONS_PHASE0_DOES_NOT_FORWARD["--cell"]
        self.assertIn("promote_claude_version.py", reason)
        self.assertTrue(
            (ROOT / "tools" / "promotion" / "promote_claude_version.py").is_file(),
            "the reason names a tool that does not exist",
        )
        # And the shipped graded prompts are what makes forwarding it wrong:
        # they instruct the model to run a shell command a toolless cell has no
        # way to run. Read from the suite rather than asserted about it.
        suite = sorted((PHASE0 / "prompts").glob("*.txt"))
        self.assertTrue(suite, "the graded prompt suite is missing")
        tool_requiring = [
            path.name for path in suite if "shasum" in path.read_text(encoding="utf-8")
        ]
        self.assertGreater(
            len(tool_requiring),
            len(suite) // 2,
            "most graded prompts require a shell command; if that stopped being "
            "true, the reason --cell is not forwarded needs rewriting",
        )

    def test_clap_long_options_reads_nested_brackets_and_ignores_comments(
        self,
    ) -> None:
        """The two ways this parser has to fail, both closed.

        A `conflicts_with_all = ["a", "b"]` attribute ends its first `]` inside
        itself, and `LaunchArgs` quotes `#[arg(skip)]` in a doc comment above
        the field it applies to.
        """

        source = (
            "struct Sample {\n"
            "    /// Quotes #[arg(skip)] in prose.\n"
            '    #[arg(long, conflicts_with_all = ["x", "y"])]\n'
            "    pub system_prompt: Option<String>,\n"
            '    #[arg(long = "denied-tool")]\n'
            "    pub denied_tools: Vec<String>,\n"
            "    #[arg(long, hide = true)]\n"
            "    pub agent_file: Option<PathBuf>,\n"
            "    #[arg(skip)]\n"
            "    pub from_environment: BTreeSet<String>,\n"
            "}\n"
        )
        accepted, retired = clap_long_options(source, "Sample")
        self.assertEqual(accepted, {"--system-prompt", "--denied-tool"})
        self.assertEqual(retired, {"--agent-file"})


class UncountedRealTurnTests(unittest.TestCase):
    """The ledger under-reports real exposure, and now says so with a number."""

    def test_the_budget_counts_real_turns_behind_committed_receipts(self) -> None:
        census = real_claude_turns_outside_the_ledger(ROOT / "evidence")
        self.assertEqual(
            census["receipts"]["turn-latency-2.1.220-macos-aarch64.json"],
            44,
            "the operator turn-latency receipt is 22 `pmux turn` plus 22 "
            "`pmux ask` real round trips that reserved nothing",
        )
        self.assertEqual(
            census["receipts"]["turn-latency-double-macos-aarch64.json"],
            0,
            "the double receipt called no model",
        )
        self.assertEqual(census["total"], sum(census["receipts"].values()))
        self.assertIn("PMUX_POOL_REAL_CLAUDE", census["note"])

    def test_an_unclassifiable_receipt_stops_the_count_rather_than_reading_zero(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_file(root / "mystery.json", b'{"turns": 900}\n')
            with self.assertRaisesRegex(EvidenceError, "cannot\n?\\s*classify"):
                real_claude_turns_outside_the_ledger(root)

    def test_the_budget_report_carries_the_shortfall_beside_consumed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = write_file(
                root / "model-attempt-ledger.ndjson",
                json.dumps(
                    {"global_attempt": 5, "kind": "approved_prior_baseline"}
                ).encode()
                + b"\n",
            )
            shutil.copy2(
                ROOT / "evidence" / "turn-latency-2.1.220-macos-aarch64.json", root
            )
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                status = main(["budget", "--ledger", str(ledger), "--detached", "0"])
            self.assertEqual(status, 0)
            report = json.loads(stdout.getvalue())
            self.assertEqual(report["consumed"], 5)
            self.assertEqual(report["real_turns_outside_the_ledger"]["total"], 44)
            # Reported BESIDE the budget, never folded into it: decision D4 is
            # the owner's and a tool that re-priced it silently would answer it.
            self.assertEqual(report["remaining"], report["ceiling"] - 5)


class ConfigAndDryRunTests(unittest.TestCase):
    def test_canonical_json_is_stable_and_rejects_nan(self) -> None:
        self.assertEqual(canonical_json_bytes({"b": 2, "a": 1}), b'{"a":1,"b":2}')
        with self.assertRaises(ValueError):
            canonical_json_bytes({"invalid": float("nan")})

    def test_dry_run_performs_no_path_access_and_contains_no_prompt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            config.prompt_paths[0].unlink()
            manifest = dry_run_manifest(config)
            rendered = json.dumps(manifest)
            self.assertEqual(manifest["mode"], "dry_run")
            self.assertFalse(manifest["writes_performed"])
            self.assertNotIn("bounded prompt", rendered)
            self.assertFalse(manifest["authority"]["transcript_parsing"])
            self.assertFalse(manifest["authority"]["direct_rmux_or_pty_input"])

    def test_live_requires_both_usage_acknowledgements_for_untested(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = dataclasses.replace(
                base_config(Path(directory)),
                live=True,
                acknowledge_usage=False,
                acknowledge_untested=False,
            )
            with self.assertRaisesRegex(EvidenceError, "acknowledge-claude-usage"):
                validate_config(config, access_files=False)
            config = dataclasses.replace(config, acknowledge_usage=True)
            with self.assertRaisesRegex(EvidenceError, "untested-compatibility"):
                validate_config(config, access_files=False)

    def test_global_ceiling_is_explicitly_restricted_to_60_through_100(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            for invalid in (0, 59, 101, 1_000):
                with self.subTest(invalid=invalid):
                    with self.assertRaisesRegex(EvidenceError, "60..100"):
                        validate_config(
                            dataclasses.replace(config, global_attempt_ceiling=invalid),
                            access_files=False,
                        )
            for valid in (60, 80, 100):
                validate_config(
                    dataclasses.replace(config, global_attempt_ceiling=valid),
                    access_files=False,
                )

    def test_campaign_timeouts_have_reviewed_exact_integer_maxima(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            fields = (
                ("turn_timeout_seconds", phase0_lib.MAX_TURN_TIMEOUT_SECONDS),
                (
                    "daemon_ready_timeout_seconds",
                    phase0_lib.MAX_DAEMON_READY_TIMEOUT_SECONDS,
                ),
                (
                    "daemon_shutdown_timeout_seconds",
                    phase0_lib.MAX_DAEMON_SHUTDOWN_TIMEOUT_SECONDS,
                ),
            )
            for field, maximum in fields:
                validate_config(
                    dataclasses.replace(config, **{field: maximum}),
                    access_files=False,
                )
                for invalid in (0, maximum + 1, True):
                    with (
                        self.subTest(field=field, invalid=invalid),
                        self.assertRaisesRegex(EvidenceError, "exact integer"),
                    ):
                        validate_config(
                            dataclasses.replace(config, **{field: invalid}),
                            access_files=False,
                        )

    def test_all_campaign_numeric_guards_reject_boolean_substitution(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            for field in (
                "terminal_rows",
                "terminal_cols",
                "max_attempts_this_run",
                "global_attempt_ceiling",
                "max_observed_tokens",
                "untested_transcript_drain_ms",
            ):
                with self.subTest(field=field), self.assertRaises(EvidenceError):
                    validate_config(
                        dataclasses.replace(config, **{field: True}),
                        access_files=False,
                    )
            for prefix in (
                LedgerPrefix(True, EMPTY_SHA256, 0),
                LedgerPrefix(0, EMPTY_SHA256, True),
            ):
                with self.subTest(prefix=prefix), self.assertRaises(EvidenceError):
                    validate_config(
                        dataclasses.replace(config, ledger_prefix=prefix),
                        access_files=False,
                    )

    def test_planned_attempt_count_must_equal_prompt_file_count(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = dataclasses.replace(
                base_config(Path(directory)), max_attempts_this_run=2
            )
            with self.assertRaisesRegex(EvidenceError, "must equal"):
                validate_config(config, access_files=False)

    def test_resume_identity_is_required_only_for_resume(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            with self.assertRaisesRegex(EvidenceError, "requires"):
                validate_config(
                    dataclasses.replace(config, scenario="resume"), access_files=False
                )
            with self.assertRaisesRegex(EvidenceError, "only"):
                validate_config(
                    dataclasses.replace(config, resume_session_id=SESSION_ID),
                    access_files=False,
                )

    def test_require_tested_needs_profile_but_allow_untested_does_not(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            validate_config(config, access_files=False)
            with self.assertRaisesRegex(EvidenceError, "tested-profile-file"):
                validate_config(
                    dataclasses.replace(config, compatibility="require-tested"),
                    access_files=False,
                )

    def test_tested_profile_is_exact_resolved_and_bound_to_the_host_cell(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host = current_platform_identity()
            value = {
                "claude_version": "9.9.9",
                "os": host["os"],
                "arch": host["architecture"],
                "terminal_profile": "transparent",
                "input_transport": "sdk",
                "transcript_drain_ms": 2_000,
            }
            profile = write_file(
                root / "profile.json", canonical_json_bytes(value) + b"\n"
            )
            config = dataclasses.replace(
                base_config(root),
                compatibility="require-tested",
                tested_profile_path=profile,
            )
            validate_config(config, access_files=True)
            loaded, _identity = read_profile(profile)
            self.assertEqual(loaded, value)

            with self.assertRaisesRegex(EvidenceError, "must not supply"):
                validate_config(
                    dataclasses.replace(config, compatibility="allow-untested"),
                    access_files=False,
                )

            invalid_values = (
                {**value, "extra": True},
                {**value, "claude_version": "9.9"},
                {**value, "os": "MacOS"},
                {**value, "terminal_profile": "rmux_standard"},
                {**value, "input_transport": "auto"},
                {**value, "transcript_drain_ms": True},
                {**value, "transcript_drain_ms": 60_001},
            )
            for index, invalid in enumerate(invalid_values):
                candidate = write_file(
                    root / f"invalid-{index}.json",
                    canonical_json_bytes(invalid) + b"\n",
                )
                with self.subTest(invalid=invalid), self.assertRaises(EvidenceError):
                    read_profile(candidate)

            noncanonical = write_file(
                root / "noncanonical.json",
                json.dumps(value, indent=2).encode("utf-8") + b"\n",
            )
            with self.assertRaisesRegex(EvidenceError, "canonical JSON"):
                read_profile(noncanonical)

            wrong_host = write_file(
                root / "wrong-host.json",
                canonical_json_bytes({**value, "arch": "wrong_arch"}) + b"\n",
            )
            with self.assertRaisesRegex(EvidenceError, "exact requested host"):
                validate_config(
                    dataclasses.replace(config, tested_profile_path=wrong_host),
                    access_files=True,
                )

    def test_model_allowlist_and_effort_are_explicit_and_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            invalid = (
                dataclasses.replace(config, model=None),
                dataclasses.replace(config, allowed_model_ids=()),
                dataclasses.replace(
                    config,
                    allowed_model_ids=(
                        "claude-sonnet-5-test",
                        "claude-sonnet-5-test",
                    ),
                ),
                dataclasses.replace(config, effort=None),
                dataclasses.replace(config, effort="high"),
            )
            for candidate in invalid:
                with (
                    self.subTest(candidate=candidate),
                    self.assertRaises(EvidenceError),
                ):
                    validate_config(candidate, access_files=False)

    def test_claude_p_scenario_is_confined_to_its_fixed_native_cell(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = write_file(root / "profile.json", b"{}\n")
            config = dataclasses.replace(
                base_config(root),
                scenario="claude-p-one-shot",
                compatibility="require-tested",
                tested_profile_path=profile,
                input_transport="auto",
            )
            validate_config(config, access_files=False)
            with self.assertRaisesRegex(EvidenceError, "fixed"):
                validate_config(
                    dataclasses.replace(config, input_transport="sdk"),
                    access_files=False,
                )

    def test_permission_modes_are_exactly_the_pmux_cli_value_enum(self) -> None:
        """A value-enum drift fence: the campaign cannot offer a mode pmux lacks."""

        source = (ROOT / "bin" / "pmux" / "src" / "cli.rs").read_text()
        body = source.split("pub enum PermissionArg {", 1)[1].split("\n}", 1)[0]
        variants = [
            line.strip().rstrip(",")
            for line in body.splitlines()
            if line.strip() and not line.strip().startswith("//")
        ]
        kebab = tuple(
            sorted(
                re.sub(r"(?<!^)(?=[A-Z])", "-", variant).lower() for variant in variants
            )
        )
        self.assertEqual(kebab, PERMISSION_MODES)
        self.assertIn("dangerously-skip-permissions", PERMISSION_MODES)
        facade = (ROOT / "bin" / "claude-p" / "src" / "main.rs").read_text()
        facade_body = facade.split("enum FacadePermissionMode {", 1)[1].split("\n}", 1)[
            0
        ]
        facade_variants = tuple(
            sorted(
                re.sub(r"(?<!^)(?=[A-Z])", "-", line.strip().rstrip(",")).lower()
                for line in facade_body.splitlines()
                if line.strip() and not line.strip().startswith("//")
            )
        )
        self.assertEqual(facade_variants, FACADE_PERMISSION_MODES)

    def test_forwarded_launch_options_are_bounded_and_never_carry_values(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            agent_file = write_file(root / "agent.json", b'{"model":"sonnet"}\n')
            config = dataclasses.replace(
                base_config(root),
                permission_mode="dangerously-skip-permissions",
                environment_set={"PHASE0_FORWARDED": "s3cr3t"},
                environment_passthrough_names=("PHASE0_PASSTHROUGH",),
                agent_name="reviewer",
                agent_file=agent_file,
            )
            validate_config(config, access_files=False)
            manifest = dry_run_manifest(config)
            options = manifest["cell"]["launch_options"]
            self.assertEqual(options["environment_set_names"], ["PHASE0_FORWARDED"])
            self.assertFalse(options["environment_set_values_recorded"])
            self.assertNotIn(b"s3cr3t", canonical_json_bytes(manifest))
            invalid = (
                dataclasses.replace(config, permission_mode="nonexistent-mode"),
                dataclasses.replace(config, agent_file=None),
                dataclasses.replace(config, agent_name=None),
                dataclasses.replace(config, environment_set={"1BAD": "x"}),
                dataclasses.replace(config, environment_set={"OK": ""}),
                dataclasses.replace(config, environment_passthrough_names=("OK", "OK")),
            )
            for candidate in invalid:
                with (
                    self.subTest(candidate=candidate.permission_mode),
                    self.assertRaises(EvidenceError),
                ):
                    validate_config(candidate, access_files=False)

    def test_denied_tool_spellings_match_both_public_entrypoints(self) -> None:
        """A flag-name drift fence, in the shape of the permission-mode one.

        The two entrypoints spell one protocol field two ways. Emitting the
        wrong spelling is a clap parse failure AFTER the ordinal is reserved, so
        the mapping is asserted against the sources rather than remembered.
        """

        pmux = (ROOT / "bin" / "pmux" / "src" / "cli.rs").read_text()
        self.assertIn("#[arg(long = \"denied-tool\", value_delimiter = ',')]", pmux)
        facade = (ROOT / "bin" / "claude-p" / "src" / "main.rs").read_text()
        self.assertIn('#[arg(long = "disallowedTools")]', facade)
        self.assertEqual(
            DENIED_TOOL_FLAG,
            {"pmux": "--denied-tool", "claude-p": "--disallowedTools"},
        )
        # The comma rule exists because exactly one of the two splits on it.
        self.assertNotIn("value_delimiter", facade)

    def test_minified_launch_options_are_bounded_and_bind_no_prompt_text(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            system_prompt = write_file(
                root / "system-prompt.txt",
                b"You are a minified cell. Marker PATHB-CELL-9D41.\n",
            )
            config = dataclasses.replace(
                base_config(root),
                permission_mode="dont-ask",
                denied_tools=("*", "Bash(rm:*)"),
                system_prompt_file=system_prompt,
            )
            validate_config(config, access_files=True)
            manifest = dry_run_manifest(config)
            options = manifest["cell"]["launch_options"]
            # Order is preserved: denied_tools is a Vec the daemon walks in
            # order, so a sorted binding would describe a different launch.
            self.assertEqual(options["denied_tools"], ["*", "Bash(rm:*)"])
            self.assertEqual(options["system_prompt_policy"], "replace")
            self.assertFalse(options["system_prompt_text_recorded"])
            self.assertEqual(options["system_prompt_delivery"], SYSTEM_PROMPT_DELIVERY)
            self.assertEqual(options["system_prompt_file"], str(system_prompt))
            self.assertNotIn(b"minified cell", canonical_json_bytes(manifest))
            # A campaign that asks for neither still says so explicitly.
            plain = dry_run_manifest(
                dataclasses.replace(config, denied_tools=(), system_prompt_file=None)
            )["cell"]["launch_options"]
            self.assertEqual(plain["denied_tools"], [])
            self.assertIsNone(plain["system_prompt_policy"])
            self.assertIsNone(plain["system_prompt_file"])
            invalid = (
                # One comma would mean two denied tools through pmux and one
                # through the facade.
                ("Bash,Read",),
                # clap sets no allow_hyphen_values on either spelling.
                ("--debug",),
                ("Bash", "Bash"),
                ("",),
                ("Bash\nRead",),
                tuple(f"tool-{index}" for index in range(MAX_DENIED_TOOLS + 1)),
            )
            for denied in invalid:
                with (
                    self.subTest(denied=denied[:2]),
                    self.assertRaises(EvidenceError),
                ):
                    validate_config(
                        dataclasses.replace(config, denied_tools=denied),
                        access_files=False,
                    )
            with self.assertRaisesRegex(EvidenceError, "absolute path"):
                validate_config(
                    dataclasses.replace(
                        config, system_prompt_file=Path("system-prompt.txt")
                    ),
                    access_files=True,
                )

    def test_system_prompt_replacement_must_be_owner_only_text_without_secrets(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            good = write_file(root / "ok.txt", b"Reply with the marker.\n")
            text, identity = read_system_prompt(good)
            self.assertEqual(text, "Reply with the marker.\n")
            self.assertEqual(identity.sha256, identify_file(good).sha256)
            self.assertEqual(identity.link_count, 1)

            world_readable = write_file(root / "loose.txt", b"hello\n", 0o644)
            with self.assertRaisesRegex(EvidenceError, "owner-only"):
                read_system_prompt(world_readable)

            with self.assertRaisesRegex(EvidenceError, "must not be empty"):
                read_system_prompt(write_file(root / "empty.txt", b""))
            with self.assertRaisesRegex(EvidenceError, "UTF-8"):
                read_system_prompt(write_file(root / "binary.txt", b"\xff\xfe"))
            with self.assertRaisesRegex(EvidenceError, "control characters"):
                read_system_prompt(write_file(root / "escape.txt", b"clear \x1b[2J\n"))
            with self.assertRaisesRegex(EvidenceError, "exceeds"):
                read_system_prompt(
                    write_file(root / "huge.txt", b"x" * (MAX_SYSTEM_PROMPT_BYTES + 1))
                )
            # The guard that earns the argv route: this document would have been
            # redacted out of captured output, and argv cannot be redacted after
            # the fact without making a faithful receipt look forged.
            with self.assertRaisesRegex(EvidenceError, "credential"):
                read_system_prompt(
                    write_file(root / "leaky.txt", b"Use api_key: sk-live-abc123\n")
                )
            # Refused here, not discovered by clap after the ordinal was spent.
            # Measured against the frozen release binary: `pmux run
            # --system-prompt "-leading hyphen text"` dies with "unexpected
            # argument '-l' found", and reservation precedes launch (MF-2).
            for opener in (b"-leading hyphen text\n", b"--verbose is not a prompt\n"):
                with (
                    self.subTest(opener=opener),
                    self.assertRaisesRegex(EvidenceError, "must not begin with"),
                ):
                    read_system_prompt(write_file(root / "hyphen.txt", opener))
            # A hyphen anywhere else is ordinary prose and stays admissible;
            # clap only reads the FIRST character of the value as a flag.
            mid, _ = read_system_prompt(
                write_file(root / "mid.txt", b"Reply with the marker -- exactly.\n")
            )
            self.assertEqual(mid, "Reply with the marker -- exactly.\n")
            linked = write_file(root / "linked.txt", b"hello\n")
            os.link(linked, root / "linked-again.txt")
            with self.assertRaisesRegex(EvidenceError, "multiple hard links"):
                read_system_prompt(linked)

    def test_launch_options_admit_the_legacy_seven_and_refuse_a_mixture(self) -> None:
        """Older reservations must stay readable; unknown names must not appear.

        `inspect_ledger` re-validates every post-prefix reservation on every
        append, so requiring the newer names would fail an audit on evidence
        that is merely older -- while still admitting an unknown one would give
        up the closed set.
        """

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = dataclasses.replace(
                base_config(root), permission_mode="dont-ask", denied_tools=("*",)
            )
            current = dry_run_manifest(config)["cell"]["launch_options"]
            current.pop("agent_file")
            legacy = {
                key: value
                for key, value in current.items()
                if key in LEGACY_LAUNCH_OPTION_KEYS
            }
            legacy["agent_file"] = None
            self.assertEqual(set(legacy), set(LEGACY_LAUNCH_OPTION_KEYS))
            phase0_lib._validate_launch_options(legacy, scenario="one-shot")

            full = {**current, "agent_file": None, "system_prompt_file": None}
            phase0_lib._validate_launch_options(full, scenario="one-shot")
            invalid = (
                {**full, "unreviewed_option": True},
                {key: value for key, value in full.items() if key != "denied_tools"},
                {**full, "denied_tools": ["Bash,Read"]},
                {**full, "denied_tools": ["*", "*"]},
                {**full, "system_prompt_text_recorded": True},
                {**full, "system_prompt_delivery": "somewhere_else"},
                {**full, "system_prompt_policy": "append"},
                # A policy with no bound document, and a document with no policy.
                {**full, "system_prompt_policy": "replace"},
            )
            for candidate in invalid:
                with (
                    self.subTest(keys=sorted(candidate)),
                    self.assertRaises(EvidenceError),
                ):
                    phase0_lib._validate_launch_options(candidate, scenario="one-shot")

    def test_the_facade_rejects_the_bypass_mode_and_agent_profiles(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = write_file(root / "profile.json", b"{}\n")
            facade = dataclasses.replace(
                base_config(root),
                scenario="claude-p-one-shot",
                compatibility="require-tested",
                tested_profile_path=profile,
                input_transport="auto",
            )
            validate_config(
                dataclasses.replace(facade, permission_mode="plan"), access_files=False
            )
            with self.assertRaisesRegex(EvidenceError, "claude-p facade"):
                validate_config(
                    dataclasses.replace(
                        facade, permission_mode="dangerously-skip-permissions"
                    ),
                    access_files=False,
                )
            with self.assertRaisesRegex(EvidenceError, "agent-profile surface"):
                validate_config(
                    dataclasses.replace(
                        facade,
                        agent_name="reviewer",
                        agent_file=root / "agent.json",
                    ),
                    access_files=False,
                )

    def test_cli_probe_is_dry_run_by_default(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = base_config(root)
            arguments = [
                "probe",
                "--source-root",
                str(config.source_root),
                "--expected-source-digest",
                config.expected_source_digest,
                "--release-bin-dir",
                str(config.release_bin_dir),
            ]
            for name, digest in config.expected_binary_hashes.items():
                arguments.extend(["--binary-sha256", f"{name}={digest}"])
            arguments.extend(
                [
                    "--claude-bin",
                    str(config.claude_bin),
                    "--claude-sha256",
                    config.expected_claude_sha256,
                    "--cwd",
                    str(config.cwd),
                    "--prompt-file",
                    str(config.prompt_paths[0]),
                    "--evidence-root",
                    str(config.evidence_root),
                    "--ledger",
                    str(config.ledger_path),
                    "--ledger-prefix-records",
                    "0",
                    "--ledger-prefix-sha256",
                    EMPTY_SHA256,
                    "--prefix-last-global-attempt",
                    "0",
                    "--campaign-id",
                    CAMPAIGN_ID,
                    "--global-attempt-ceiling",
                    "60",
                    "--max-attempts-this-run",
                    "1",
                    "--max-observed-tokens",
                    "10000",
                    "--model",
                    "sonnet",
                    "--allowed-model-id",
                    "claude-sonnet-5-test",
                    "--effort",
                    "low",
                    "--compatibility",
                    "allow-untested",
                ]
            )
            before = config.evidence_root.stat().st_mtime_ns
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(arguments), 0)
            self.assertEqual(config.evidence_root.stat().st_mtime_ns, before)
            self.assertFalse(config.ledger_path.exists())

    def test_live_probe_prints_the_external_campaign_anchor_pair(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = dataclasses.replace(base_config(Path(directory)), live=True)
            result = {
                "campaign_id": CAMPAIGN_ID,
                "run_id": RUN_ID,
                "status": "acquired",
                "attempt_count": 1,
                "observed_tokens": 10,
                "cleanup": {"status": "verified"},
                "evidence_directory": "/private/evidence/campaign-run",
                "campaign_manifest_sha256": "a" * 64,
                "drain_calibration": summarize_drain_calibration(
                    [], configured_transcript_drain_ms=2_000
                ),
            }
            runner = mock.Mock()
            runner.run.return_value = result
            output = io.StringIO()
            with (
                mock.patch.object(phase0, "_config", return_value=config),
                mock.patch.object(phase0, "CampaignRunner", return_value=runner),
                contextlib.redirect_stdout(output),
            ):
                status = phase0._run_probe(object())
            self.assertEqual(status, 0)
            rendered = json.loads(output.getvalue())
            self.assertEqual(rendered["run_id"], RUN_ID)
            self.assertEqual(rendered["campaign_manifest_sha256"], "a" * 64)
            self.assertIn(
                "cannot calibrate", rendered["drain_calibration"]["interpretation"]
            )


class IdentityAndRedactionTests(unittest.TestCase):
    def test_prompt_identity_binds_bytes_without_publishing_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            prompt = write_file(
                Path(directory) / "secret-name.txt", "λ prompt".encode()
            )
            identity = identify_prompt(prompt)
            public = identity.public()
            self.assertEqual(public["sha256"], sha256_bytes("λ prompt".encode()))
            self.assertNotIn("path", public)
            self.assertNotIn("secret-name", json.dumps(public))

    def test_prompt_payload_is_the_same_read_bound_by_its_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            prompt = write_file(Path(directory) / "prompt.txt", b"first bytes")
            identity = identify_prompt(prompt)
            write_file(prompt, b"replacement bytes")
            self.assertEqual(identity.payload, b"first bytes")
            self.assertEqual(identity.file.sha256, sha256_bytes(identity.payload))
            self.assertNotEqual(
                identity.file.sha256, identify_prompt(prompt).file.sha256
            )

    def test_profile_value_and_identity_come_from_one_retained_read(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            host = current_platform_identity()
            first = {
                "claude_version": "9.9.9",
                "os": host["os"],
                "arch": host["architecture"],
                "terminal_profile": "transparent",
                "input_transport": "sdk",
                "transcript_drain_ms": 2_000,
            }
            replacement = {**first, "transcript_drain_ms": 2_001}
            first_payload = canonical_json_bytes(first) + b"\n"
            profile = write_file(Path(directory) / "profile.json", first_payload)
            value, identity = read_profile(profile)
            write_file(profile, canonical_json_bytes(replacement) + b"\n")
            self.assertEqual(value, first)
            self.assertEqual(identity.sha256, sha256_bytes(first_payload))
            self.assertNotEqual(identity.sha256, read_profile(profile)[1].sha256)

    def test_prompt_identity_rejects_empty_invalid_utf8_oversized_and_symlink(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            empty = write_file(root / "empty", b"")
            invalid = write_file(root / "invalid", b"\xff")
            large = write_file(root / "large", b"x" * (1024 * 1024 + 1))
            target = write_file(root / "target", b"prompt")
            link = root / "link"
            link.symlink_to(target)
            for path in (empty, invalid, large, link):
                with self.subTest(path=path.name), self.assertRaises(EvidenceError):
                    identify_prompt(path)

    def test_environment_identity_binds_values_without_recording_them(self) -> None:
        identity = environment_identity(
            {"PATH": "/bin", "ANTHROPIC_API_KEY": "super-secret-value"}
        )
        rendered = json.dumps(identity)
        self.assertNotIn("super-secret-value", rendered)
        self.assertNotIn("ANTHROPIC_API_KEY", rendered)
        self.assertEqual(identity["sensitive_name_count"], 1)
        self.assertFalse(identity["values_recorded"])

    def test_redaction_removes_prompt_and_common_secret_assignment(self) -> None:
        text = "prompt body API_KEY=abc123 Authorization:BearerValue"
        redacted = redact_text(text, [b"prompt body"])
        self.assertNotIn("prompt body", redacted)
        self.assertNotIn("abc123", redacted)
        self.assertNotIn("BearerValue", redacted)

    def test_redaction_removes_bare_json_unicode_and_url_escaped_env_values(
        self,
    ) -> None:
        secret = 'sëcret "value" /+'
        variants = {
            secret,
            json.dumps(secret, ensure_ascii=False)[1:-1],
            json.dumps(secret, ensure_ascii=True)[1:-1],
            phase0_lib.urllib.parse.quote(secret, safe=""),
            phase0_lib.urllib.parse.quote_plus(secret, safe=""),
        }
        redacted = redact_text("\n".join(sorted(variants)), sensitive_values=(secret,))
        for variant in variants:
            self.assertNotIn(variant, redacted)
        self.assertIn("<redacted-environment-value>", redacted)

    def test_canonical_cwd_identity_detects_path_replacement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cwd = root / "cwd"
            cwd.mkdir()
            identity = identify_directory(cwd)
            cwd.rename(root / "displaced")
            cwd.mkdir()
            with self.assertRaisesRegex(EvidenceError, "identity changed"):
                verify_directory_identity(cwd, identity)

    def test_file_identity_rejects_symlink_and_detects_content_change(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = write_file(root / "binary", b"one", 0o700)
            first = identify_file(binary, executable=True)
            write_file(binary, b"two", 0o700)
            second = identify_file(binary, executable=True)
            self.assertNotEqual(first, second)
            link = root / "link"
            link.symlink_to(binary)
            with self.assertRaises(EvidenceError):
                identify_file(link)

    def test_file_identity_rejects_path_replacement_during_the_bound_read(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = write_file(root / "binary", b"a" * (256 * 1024), 0o700)
            displaced = root / "displaced"
            original_read = phase0_lib.os.read
            replaced = False

            def read_then_replace(descriptor: int, count: int) -> bytes:
                nonlocal replaced
                value = original_read(descriptor, count)
                if value and not replaced:
                    replaced = True
                    binary.rename(displaced)
                    write_file(binary, b"replacement", 0o700)
                return value

            with (
                mock.patch.object(
                    phase0_lib.os,
                    "read",
                    side_effect=read_then_replace,
                ),
                self.assertRaisesRegex(
                    EvidenceError, "changed while hashing|pathname was replaced"
                ),
            ):
                identify_file(binary, executable=True)

            self.assertEqual(displaced.read_bytes(), b"a" * (256 * 1024))
            self.assertEqual(binary.read_bytes(), b"replacement")

    def test_socket_identity_replacement_is_detected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "pmux.sock"
            first = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            second = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                first.bind(str(path))
                os.chmod(path, 0o600)
                identity = capture_socket_identity(path)
                path.unlink()
                second.bind(str(path))
                os.chmod(path, 0o600)
                with self.assertRaisesRegex(EvidenceError, "identity changed"):
                    verify_socket_identity(path, identity)
            finally:
                first.close()
                second.close()

    def test_external_runtime_replacement_is_preserved_and_cleanup_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            runner = CampaignRunner(base_config(parent), {})
            root = make_private_directory(parent / f"pmux-p0-{runner.run_id[:8]}-owned")
            parent_descriptor, _ = phase0_lib._open_directory_nofollow(
                parent, require_owner_private=False
            )
            root_descriptor, _ = phase0_lib._open_private_directory_nofollow(root)
            runner.external_runtime_root = root
            runner.external_runtime_descriptor = root_descriptor
            runner.external_runtime_parent = parent
            runner.external_runtime_parent_descriptor = parent_descriptor

            displaced = parent / "displaced-runtime"
            root.rename(displaced)
            make_private_directory(root)
            marker = write_file(root / "outside-marker", b"must survive")

            with self.assertRaisesRegex(EvidenceError, "pathname was replaced"):
                runner._remove_external_runtime()

            self.assertEqual(marker.read_bytes(), b"must survive")
            self.assertTrue(displaced.is_dir())
            self.assertTrue(root.is_dir())

    def test_source_identity_is_byte_for_byte_canonical_linux_runner_digest(
        self,
    ) -> None:
        expected = subprocess.run(
            [
                sys.executable,
                str(ROOT / "tools" / "linux-docker" / "source_digest.py"),
                str(ROOT),
                "--json",
            ],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        )
        canonical = json.loads(expected.stdout)
        identity = compute_source_identity(ROOT)
        self.assertEqual(identity["digest"], canonical["workspace_source_sha256"])
        self.assertEqual(identity["file_count"], canonical["workspace_file_count"])
        self.assertEqual(identity["algorithm"], canonical["algorithm"])
        self.assertEqual(
            identity["implementation"],
            "tools/linux-docker/source_digest.py::workspace_source_manifest",
        )


class LedgerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.config = base_config(self.root)
        self.binary_identities = {
            name: fake_identity(self.root / "identities" / name, name.encode())
            for name in self.config.expected_binary_hashes
        }
        self.claude = fake_identity(self.root / "identities" / "claude", b"claude")
        self.prompt = identify_prompt(self.config.prompt_paths[0])
        self.source = fake_source_identity()
        self.config = dataclasses.replace(
            self.config,
            expected_source_digest=self.source["digest"],
            expected_binary_hashes={
                name: identity.sha256
                for name, identity in self.binary_identities.items()
            },
            expected_claude_sha256=self.claude.sha256,
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _contract(
        self,
        config: CampaignConfig,
        environment: dict[str, str],
        version: dict[str, object],
    ) -> dict[str, object]:
        return build_campaign_contract(
            config,
            source_identity=self.source,
            binary_identities=self.binary_identities,
            claude_identity=self.claude,
            claude_version_identity=version,
            cwd_identity=identify_directory(config.cwd),
            prompt_identities=(self.prompt,),
            environment=environment,
            tested_profile_identity=None,
        )

    def _reserve(
        self,
        *,
        attempt_id: str = ATTEMPT_ID,
        campaign_id: str = CAMPAIGN_ID,
        run_id: str = RUN_ID,
        config_override: CampaignConfig | None = None,
    ) -> dict[str, object]:
        config = dataclasses.replace(
            config_override or self.config, campaign_id=campaign_id
        )
        environment = {"SECRET_TOKEN": "not-written"}
        version = fake_version_identity()
        contract = self._contract(config, environment, version)
        return reserve_attempt(
            config,
            attempt_id=attempt_id,
            session_id=SESSION_ID,
            generation_id=None,
            turn_id=TURN_ID,
            prompt_suite_index=1,
            scenario_role="test",
            campaign_contract=contract,
            source_identity=self.source,
            binary_identities=self.binary_identities,
            claude_identity=self.claude,
            claude_version_identity=version,
            prompt_identity=self.prompt,
            environment=environment,
            artifact_directory=self.config.evidence_root / f"attempt-{attempt_id}",
            run_id=run_id,
        )

    def test_reservation_is_one_durable_private_line_without_prompt_or_env_values(
        self,
    ) -> None:
        record = self._reserve()
        payload = self.config.ledger_path.read_bytes()
        self.assertTrue(payload.endswith(b"\n"))
        self.assertEqual(len(payload.splitlines()), 1)
        self.assertEqual(stat.S_IMODE(self.config.ledger_path.stat().st_mode), 0o600)
        self.assertEqual(record["schema"], RESERVATION_SCHEMA)
        self.assertEqual(record["global_attempt_ordinal"], 1)
        self.assertNotIn(b"bounded prompt", payload)
        self.assertNotIn(b"not-written", payload)
        inspected = inspect_ledger(
            self.config.ledger_path, self.config.ledger_prefix, CAMPAIGN_ID
        )
        self.assertEqual(inspected["tail_count"], 1)
        self.assertEqual(inspected["campaign_reservations"], [record])

    def test_contract_digest_is_bound_and_restart_changes_never_append(self) -> None:
        first = self._reserve()
        contract = first["campaign_contract"]
        self.assertEqual(
            first["campaign_contract_sha256"], campaign_contract_sha256(contract)
        )
        before = self.config.ledger_path.read_bytes()
        changed = (
            dataclasses.replace(self.config, max_observed_tokens=20_000),
            dataclasses.replace(self.config, model="different-selector"),
            dataclasses.replace(
                self.config, allowed_model_ids=("different-public-model",)
            ),
            dataclasses.replace(self.config, output_format="ndjson"),
        )
        for index, config in enumerate(changed):
            with (
                self.subTest(index=index),
                self.assertRaisesRegex(EvidenceError, "contract"),
            ):
                self._reserve(
                    attempt_id=str(uuid.uuid4()),
                    run_id=str(uuid.uuid4()),
                    config_override=config,
                )
            self.assertEqual(self.config.ledger_path.read_bytes(), before)

    def test_campaign_contract_rejects_nested_schema_and_type_substitution(
        self,
    ) -> None:
        contract = self._contract(
            self.config, {"VISIBLE": "value"}, fake_version_identity()
        )

        def clone() -> dict[str, object]:
            value = strict_json_loads(
                canonical_json_bytes(contract), label="campaign contract clone"
            )
            assert isinstance(value, dict)
            return value

        mutations = (
            lambda value: value["candidate"]["source"].__setitem__("extra", True),
            lambda value: value["candidate"]["source"].__setitem__("file_count", True),
            lambda value: value["candidate"]["source"].__setitem__(
                "git_head", "not-a-git-object"
            ),
            lambda value: value["candidate"]["binaries"].pop("pmux"),
            lambda value: value["candidate"]["binaries"]["pmux"].__setitem__(
                "path", str(self.root / "identities" / "wrong-name")
            ),
            lambda value: value["candidate"]["binaries"]["pmux"].__setitem__(
                "size", True
            ),
            lambda value: value["candidate"]["binaries"]["pmux"].__setitem__(
                "mode", 0o600
            ),
            lambda value: value["candidate"]["claude"]["binary"].__setitem__(
                "link_count", 0
            ),
            lambda value: value["candidate"]["claude"]["version_output"].__setitem__(
                "extra", True
            ),
            lambda value: value["candidate"]["claude"]["version_output"].__setitem__(
                "stdout_bytes", True
            ),
            lambda value: value["candidate"]["claude"]["version_output"].__setitem__(
                "normalized_version", "9.9.8"
            ),
            lambda value: value["candidate"]["rmux"].__setitem__(
                "sidecar_binary_sha256", "f" * 64
            ),
            lambda value: value["platform"].__setitem__("os", "MacOS"),
            lambda value: value["cwd"].__setitem__("mode", True),
            lambda value: value["prompt_suite"][0].__setitem__(
                "size", phase0_lib.MAX_PROMPT_BYTES + 1
            ),
            lambda value: value["prompt_suite"][0].__setitem__("link_count", 0),
            lambda value: value["environment"].__setitem__("entry_count", True),
            lambda value: value["environment"].__setitem__("sensitive_name_count", 2),
            lambda value: value["cell"].__setitem__("terminal_rows", True),
            lambda value: value["cell"].__setitem__(
                "input_transport", "attached_stream"
            ),
            lambda value: value["cell"].__setitem__("compatibility", "require-tested"),
        )
        for index, mutate in enumerate(mutations):
            candidate = clone()
            mutate(candidate)
            with self.subTest(index=index), self.assertRaises(EvidenceError):
                phase0_lib._validate_campaign_contract(
                    candidate, expected_campaign_id=CAMPAIGN_ID
                )

    def test_missing_prior_outcome_blocks_restart_without_append(self) -> None:
        self._reserve()
        before = self.config.ledger_path.read_bytes()
        with self.assertRaisesRegex(EvidenceError, "no outcome artifact"):
            self._reserve(attempt_id=str(uuid.uuid4()), run_id=str(uuid.uuid4()))
        self.assertEqual(self.config.ledger_path.read_bytes(), before)

    def test_append_chain_accepts_new_campaign_without_resetting_global_ordinal(
        self,
    ) -> None:
        first = self._reserve()
        second_campaign = "77777777-7777-4777-8777-777777777777"
        second_attempt = "88888888-8888-4888-8888-888888888888"
        second = self._reserve(
            attempt_id=second_attempt,
            campaign_id=second_campaign,
            run_id="99999999-9999-4999-8999-999999999999",
        )
        self.assertEqual(first["global_attempt_ordinal"], 1)
        self.assertEqual(second["global_attempt_ordinal"], 2)
        self.assertEqual(
            second["previous_reservation_sha256"], first["reservation_sha256"]
        )
        current = inspect_ledger(
            self.config.ledger_path, self.config.ledger_prefix, second_campaign
        )
        self.assertEqual(len(current["reservations"]), 2)
        self.assertEqual(current["campaign_reservations"], [second])

    def test_legacy_prefix_digest_and_last_global_ordinal_are_enforced(self) -> None:
        prefix_record = {"global_attempt_ordinal": 29, "legacy": True}
        prefix_payload = canonical_json_bytes(prefix_record) + b"\n"
        self.config.ledger_path.write_bytes(prefix_payload)
        self.config.ledger_path.chmod(0o600)
        prefix = LedgerPrefix(1, sha256_bytes(prefix_payload), 29)
        config = dataclasses.replace(self.config, ledger_prefix=prefix)
        contract = self._contract(config, {}, fake_version_identity())
        record = reserve_attempt(
            config,
            attempt_id=ATTEMPT_ID,
            session_id=SESSION_ID,
            generation_id=None,
            turn_id=TURN_ID,
            prompt_suite_index=1,
            scenario_role="legacy-prefix",
            campaign_contract=contract,
            source_identity=self.source,
            binary_identities=self.binary_identities,
            claude_identity=self.claude,
            claude_version_identity=fake_version_identity(),
            prompt_identity=self.prompt,
            environment={},
            artifact_directory=self.config.evidence_root / f"attempt-{ATTEMPT_ID}",
            run_id=RUN_ID,
        )
        self.assertEqual(record["global_attempt_ordinal"], 30)
        with self.assertRaisesRegex(EvidenceError, "disagrees"):
            inspect_ledger(
                self.config.ledger_path,
                LedgerPrefix(1, sha256_bytes(prefix_payload), 28),
                CAMPAIGN_ID,
            )

    def test_actual_legacy_global_attempt_shapes_5_through_29_are_preserved(
        self,
    ) -> None:
        records = [
            {
                "kind": "approved_prior_baseline"
                if ordinal == 5
                else "model_call_attempt",
                "global_attempt": ordinal,
            }
            for ordinal in range(5, 30)
        ]
        prefix_payload = b"".join(
            canonical_json_bytes(record) + b"\n" for record in records
        )
        self.config.ledger_path.write_bytes(prefix_payload)
        self.config.ledger_path.chmod(0o600)
        prefix = LedgerPrefix(len(records), sha256_bytes(prefix_payload), 29)
        inspected = inspect_ledger(self.config.ledger_path, prefix, CAMPAIGN_ID)
        self.assertEqual(inspected["next_global_attempt"], 30)
        config = dataclasses.replace(self.config, ledger_prefix=prefix)
        record = reserve_attempt(
            config,
            attempt_id=ATTEMPT_ID,
            session_id=SESSION_ID,
            generation_id=None,
            turn_id=TURN_ID,
            prompt_suite_index=1,
            scenario_role="post-legacy-prefix",
            campaign_contract=self._contract(config, {}, fake_version_identity()),
            source_identity=self.source,
            binary_identities=self.binary_identities,
            claude_identity=self.claude,
            claude_version_identity=fake_version_identity(),
            prompt_identity=self.prompt,
            environment={},
            artifact_directory=self.config.evidence_root / f"attempt-{ATTEMPT_ID}",
            run_id=RUN_ID,
        )
        self.assertEqual(record["global_attempt_ordinal"], 30)

    def test_duplicate_or_nonmonotonic_recognized_prefix_attempts_fail_closed(
        self,
    ) -> None:
        for ordinals in ([5, 6, 6], [5, 7, 6]):
            with self.subTest(ordinals=ordinals):
                payload = b"".join(
                    canonical_json_bytes(
                        {"kind": "model_call_attempt", "global_attempt": value}
                    )
                    + b"\n"
                    for value in ordinals
                )
                self.config.ledger_path.write_bytes(payload)
                self.config.ledger_path.chmod(0o600)
                with self.assertRaisesRegex(EvidenceError, "strictly increasing"):
                    inspect_ledger(
                        self.config.ledger_path,
                        LedgerPrefix(
                            len(ordinals), sha256_bytes(payload), ordinals[-1]
                        ),
                        CAMPAIGN_ID,
                    )

    def test_final_append_fence_rejects_ledger_path_replacement(self) -> None:
        ledger = self.config.ledger_path
        ledger.write_bytes(b"durable\n")
        ledger.chmod(0o600)
        descriptor = os.open(ledger, os.O_RDWR)
        try:
            displaced = ledger.with_name("displaced.ndjson")
            ledger.rename(displaced)
            ledger.write_bytes(b"replacement\n")
            ledger.chmod(0o600)
            with self.assertRaisesRegex(EvidenceError, "replaced"):
                _verify_open_path_identity(ledger, descriptor, expected_size=8)
        finally:
            os.close(descriptor)

    def test_parent_swap_before_ledger_open_cannot_authorize_a_replacement_path(
        self,
    ) -> None:
        parent = self.config.ledger_path.parent
        displaced = parent.with_name("displaced-ledger-parent")
        marker_payload = b"replacement parent must remain untouched"
        original_open = phase0_lib.os.open
        swapped = False

        def swap_then_open(
            path: os.PathLike[str] | str,
            flags: int,
            mode: int = 0o777,
            *,
            dir_fd: int | None = None,
        ) -> int:
            nonlocal swapped
            if (
                not swapped
                and dir_fd is not None
                and os.fspath(path) == self.config.ledger_path.name
            ):
                swapped = True
                parent.rename(displaced)
                make_private_directory(parent)
                write_file(parent / "outside-marker", marker_payload)
            return original_open(path, flags, mode, dir_fd=dir_fd)

        with (
            mock.patch.object(phase0_lib.os, "open", side_effect=swap_then_open),
            self.assertRaisesRegex(
                EvidenceError, "parent directory pathname was replaced"
            ),
        ):
            self._reserve()

        self.assertFalse(self.config.ledger_path.exists())
        self.assertEqual((parent / "outside-marker").read_bytes(), marker_payload)
        self.assertTrue((displaced / self.config.ledger_path.name).is_file())

    def test_prefix_tamper_and_torn_tail_fail_closed(self) -> None:
        prefix = canonical_json_bytes({"global_attempt_ordinal": 5}) + b"\n"
        self.config.ledger_path.write_bytes(prefix)
        self.config.ledger_path.chmod(0o600)
        with self.assertRaisesRegex(EvidenceError, "prefix digest changed"):
            inspect_ledger(
                self.config.ledger_path, LedgerPrefix(1, "f" * 64, 5), CAMPAIGN_ID
            )
        self.config.ledger_path.write_bytes(prefix + b'{"torn":true}')
        with self.assertRaisesRegex(EvidenceError, "torn"):
            inspect_ledger(
                self.config.ledger_path,
                LedgerPrefix(1, sha256_bytes(prefix), 5),
                CAMPAIGN_ID,
            )

    def test_reservation_tamper_breaks_record_digest(self) -> None:
        self._reserve()
        payload = self.config.ledger_path.read_text()
        self.config.ledger_path.write_text(payload.replace("test", "tampered"))
        self.config.ledger_path.chmod(0o600)
        with self.assertRaisesRegex(EvidenceError, "digest"):
            inspect_ledger(
                self.config.ledger_path, self.config.ledger_prefix, CAMPAIGN_ID
            )

    def test_budget_exhaustion_never_appends(self) -> None:
        config = dataclasses.replace(
            self.config,
            ledger_prefix=LedgerPrefix(0, EMPTY_SHA256, 60),
            global_attempt_ceiling=60,
        )
        before = (
            self.config.ledger_path.read_bytes()
            if self.config.ledger_path.exists()
            else b""
        )
        with self.assertRaises(BudgetExhausted):
            reserve_attempt(
                config,
                attempt_id=ATTEMPT_ID,
                session_id=SESSION_ID,
                generation_id=None,
                turn_id=TURN_ID,
                prompt_suite_index=1,
                scenario_role="exhausted",
                campaign_contract=self._contract(config, {}, fake_version_identity()),
                source_identity=self.source,
                binary_identities=self.binary_identities,
                claude_identity=self.claude,
                claude_version_identity=fake_version_identity(),
                prompt_identity=self.prompt,
                environment={},
                artifact_directory=self.config.evidence_root / f"attempt-{ATTEMPT_ID}",
                run_id=RUN_ID,
            )
        after = (
            self.config.ledger_path.read_bytes()
            if self.config.ledger_path.exists()
            else b""
        )
        self.assertEqual(after, before)

    def test_the_reservation_guard_stops_where_the_budget_report_says_it_does(
        self,
    ) -> None:
        """The refusal says "global"; its predicate used to say "this file".

        `reserve_attempt` compared the bare next ordinal against the ceiling and
        refused with "global real-Claude attempt ceiling is exhausted", while
        `summarize_attempt_ledger` -- the count `evidence/README.md` sends every
        reader to, and the one that calls the ceiling "a total across all
        campaigns" -- added `DETACHED_GLOBAL_ATTEMPTS` back first. The two
        therefore disagreed by exactly the detached reservations: against the
        real ledger at ordinal 81 and a ceiling of 100, `phase0.py budget`
        reported 15 remaining while the guard would still have handed out 19.
        Four attempts past a hard ceiling, spent believing the tool agreed.

        Both boundaries here are DERIVED from the shared constant and read back
        out of `summarize_attempt_ledger`, so this cannot be satisfied by
        writing today's numbers down: change `DETACHED_GLOBAL_ATTEMPTS` and the
        case moves with it.
        """

        ceiling = phase0_lib.MIN_GLOBAL_ATTEMPT_CEILING
        detached = phase0_lib.DETACHED_GLOBAL_ATTEMPTS
        self.assertGreater(detached, 0, "with none detached this proves nothing")
        # The last ordinal that still leaves exactly one global attempt.
        last_with_one_left = ceiling - detached - 1

        for offset, remaining in ((0, 1), (1, 0)):
            last = last_with_one_left + offset
            with self.subTest(remaining=remaining):
                # What `phase0.py budget` would report for a ledger sitting at
                # this ordinal under this ceiling -- computed, not asserted.
                report = self.root / f"report-{last}.ndjson"
                report.write_text(
                    "".join(
                        json.dumps(
                            {
                                "global_attempt_ordinal": ordinal,
                                "global_attempt_ceiling": ceiling,
                            }
                        )
                        + "\n"
                        for ordinal in range(1, last + 1)
                    ),
                    encoding="utf-8",
                )
                summary = phase0_lib.summarize_attempt_ledger(report)
                self.assertEqual(summary["remaining"], remaining)

                ledger_path = (
                    make_private_directory(self.root / f"guard-{last}")
                    / "attempts.ndjson"
                )
                config = dataclasses.replace(
                    self.config,
                    ledger_path=ledger_path,
                    ledger_prefix=LedgerPrefix(0, EMPTY_SHA256, last),
                    global_attempt_ceiling=ceiling,
                )
                if remaining:
                    record = self._reserve(config_override=config)
                    self.assertEqual(
                        record["global_attempt_ordinal"],
                        last + 1,
                        "the last attempt inside the ceiling must still reserve",
                    )
                else:
                    with self.assertRaises(BudgetExhausted):
                        self._reserve(config_override=config)
                    self.assertEqual(
                        ledger_path.read_bytes() if ledger_path.exists() else b"",
                        b"",
                        "a refused reservation appended a line anyway",
                    )

    def test_concurrent_reservations_are_contiguous_and_unique(self) -> None:
        attempt_ids = [str(uuid.uuid4()) for _ in range(12)]
        campaign_ids = [str(uuid.uuid4()) for _ in attempt_ids]
        run_ids = [str(uuid.uuid4()) for _ in attempt_ids]

        def reserve(values: tuple[str, str, str]) -> dict[str, object]:
            attempt_id, campaign_id, run_id = values
            return self._reserve(
                attempt_id=attempt_id, campaign_id=campaign_id, run_id=run_id
            )

        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
            records = list(
                executor.map(reserve, zip(attempt_ids, campaign_ids, run_ids))
            )
        self.assertEqual(
            sorted(record["global_attempt_ordinal"] for record in records),
            list(range(1, 13)),
        )
        inspected = inspect_ledger(
            self.config.ledger_path, self.config.ledger_prefix, CAMPAIGN_ID
        )
        self.assertEqual(inspected["next_global_attempt"], 13)


class ArtifactAndAuditTests(unittest.TestCase):
    def test_atomic_publication_modes_hashes_and_tamper_detection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = make_private_directory(Path(directory) / "evidence")
            writer = AtomicArtifactDirectory(root, "attempt-one")
            writer.write("raw.bin", b"evidence")
            final = writer.publish(status="ok", binding={"attempt_id": ATTEMPT_ID})
            self.assertEqual(stat.S_IMODE(final.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE((final / "raw.bin").stat().st_mode), 0o600)
            audited = audit_artifact_directory(final)
            self.assertEqual(audited["status"], "ok")
            manifest = json.loads((final / "artifact-manifest.json").read_text())
            self.assertEqual(manifest["schema"], ARTIFACT_SCHEMA)
            (final / "raw.bin").write_bytes(b"tampered")
            with self.assertRaisesRegex(EvidenceError, "manifest"):
                audit_artifact_directory(final)

    def test_duplicate_final_directory_is_never_overwritten(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = make_private_directory(Path(directory) / "evidence")
            first = AtomicArtifactDirectory(root, "same")
            first.write("a", b"one")
            first.publish(status="ok", binding={})
            with self.assertRaisesRegex(EvidenceError, "already exists"):
                AtomicArtifactDirectory(root, "same")

    def test_publication_race_uses_atomic_no_replace_and_preserves_collision(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = make_private_directory(Path(directory) / "evidence")
            writer = AtomicArtifactDirectory(root, "race")
            writer.write("owned", b"staging")
            original = phase0_lib._rename_directory_noreplace_at

            def collide(
                parent_descriptor: int,
                source_name: str,
                destination_name: str,
            ) -> None:
                os.mkdir(destination_name, mode=0o700, dir_fd=parent_descriptor)
                destination_descriptor, metadata = (
                    phase0_lib._open_private_child_directory_at(
                        parent_descriptor,
                        destination_name,
                        create=False,
                        expected_device=os.fstat(parent_descriptor).st_dev,
                    )
                )
                self.assertTrue(stat.S_ISDIR(metadata.st_mode))
                try:
                    phase0_lib._write_private_atomic_at(
                        destination_descriptor, "winner", b"other"
                    )
                finally:
                    os.close(destination_descriptor)
                original(parent_descriptor, source_name, destination_name)

            with (
                mock.patch.object(
                    phase0_lib,
                    "_rename_directory_noreplace_at",
                    side_effect=collide,
                ),
                self.assertRaisesRegex(EvidenceError, "collision"),
            ):
                writer.publish(status="ok", binding={})
            self.assertEqual((root / "race" / "winner").read_bytes(), b"other")
            self.assertTrue(writer.staging.exists())

    def test_artifact_audit_rejects_symlinks_and_nonprivate_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = make_private_directory(Path(directory) / "evidence")
            outside = write_file(Path(directory) / "outside", b"outside")
            writer = AtomicArtifactDirectory(root, "attempt-link")
            writer.write("raw.bin", b"evidence")
            final = writer.publish(status="ok", binding={})
            (final / "raw.bin").unlink()
            (final / "raw.bin").symlink_to(outside)
            with self.assertRaisesRegex(EvidenceError, "symlink"):
                audit_artifact_directory(final)

            writer = AtomicArtifactDirectory(root, "attempt-mode")
            writer.write("raw.bin", b"evidence")
            final = writer.publish(status="ok", binding={})
            (final / "raw.bin").chmod(0o644)
            with self.assertRaisesRegex(EvidenceError, "owner-only"):
                audit_artifact_directory(final)

    def test_artifact_audit_rejects_hardlinks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = make_private_directory(Path(directory) / "evidence")
            writer = AtomicArtifactDirectory(root, "attempt-hardlink")
            writer.write("raw.bin", b"evidence")
            final = writer.publish(status="ok", binding={})
            os.link(final / "raw.bin", Path(directory) / "alias")
            with self.assertRaisesRegex(EvidenceError, "multiple hard links"):
                audit_artifact_directory(final)

    def test_artifact_writer_detects_evidence_root_path_replacement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            root = make_private_directory(parent / "evidence")
            writer = AtomicArtifactDirectory(root, "attempt-root-swap")
            root.rename(parent / "displaced-evidence")
            make_private_directory(root)
            with self.assertRaisesRegex(
                EvidenceError, "identity changed|pathname was replaced"
            ):
                writer.write("raw.bin", b"must-not-publish")

    def test_atomic_file_parent_swap_never_writes_the_replacement_tree(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            owned = make_private_directory(parent / "owned")
            destination = owned / "evidence.json"
            displaced = parent / "displaced-owned"
            original_link = phase0_lib.os.link
            swapped = False

            def swap_then_link(
                source: os.PathLike[str] | str,
                target: os.PathLike[str] | str,
                *,
                src_dir_fd: int | None = None,
                dst_dir_fd: int | None = None,
                follow_symlinks: bool = True,
            ) -> None:
                nonlocal swapped
                if not swapped:
                    swapped = True
                    owned.rename(displaced)
                    make_private_directory(owned)
                    write_file(owned / "outside-marker", b"must survive")
                original_link(
                    source,
                    target,
                    src_dir_fd=src_dir_fd,
                    dst_dir_fd=dst_dir_fd,
                    follow_symlinks=follow_symlinks,
                )

            with (
                mock.patch.object(phase0_lib.os, "link", side_effect=swap_then_link),
                self.assertRaisesRegex(EvidenceError, "pathname was replaced"),
            ):
                phase0_lib.write_private_atomic(destination, b"owned evidence")

            self.assertFalse(destination.exists())
            self.assertEqual((owned / "outside-marker").read_bytes(), b"must survive")
            self.assertEqual(
                (displaced / destination.name).read_bytes(), b"owned evidence"
            )

    def test_nested_artifact_parent_swap_is_rejected_without_touching_replacement(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = make_private_directory(Path(directory) / "evidence")
            writer = AtomicArtifactDirectory(root, "attempt-nested-swap")
            nested = writer.staging / "nested"
            displaced = writer.staging / "displaced-nested"
            original = phase0_lib._write_private_atomic_at
            swapped = False

            def swap_then_write(
                parent_descriptor: int, name: str, data: bytes
            ) -> os.stat_result:
                nonlocal swapped
                if name == "payload.json" and not swapped:
                    swapped = True
                    nested.rename(displaced)
                    make_private_directory(nested)
                    write_file(nested / "outside-marker", b"must survive")
                return original(parent_descriptor, name, data)

            try:
                with (
                    mock.patch.object(
                        phase0_lib,
                        "_write_private_atomic_at",
                        side_effect=swap_then_write,
                    ),
                    self.assertRaisesRegex(EvidenceError, "changed during"),
                ):
                    writer.write("nested/payload.json", b"owned evidence")
            finally:
                writer.close()

            self.assertEqual((nested / "outside-marker").read_bytes(), b"must survive")
            self.assertEqual(
                (displaced / "payload.json").read_bytes(), b"owned evidence"
            )

    def test_artifact_audit_rejects_file_and_root_swaps(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            root = make_private_directory(parent / "evidence")

            writer = AtomicArtifactDirectory(root, "attempt-file-swap")
            writer.write("raw.bin", b"original")
            file_artifact = writer.publish(status="ok", binding={})
            original_read = phase0_lib._read_private_artifact_file_at
            swapped_file = False

            def swap_after_read(
                parent_descriptor: int,
                name: str,
                maximum: int,
                *,
                label: str,
            ) -> tuple[bytes, os.stat_result]:
                nonlocal swapped_file
                result = original_read(parent_descriptor, name, maximum, label=label)
                if name == "raw.bin" and not swapped_file:
                    swapped_file = True
                    os.rename(
                        name,
                        "displaced.bin",
                        src_dir_fd=parent_descriptor,
                        dst_dir_fd=parent_descriptor,
                    )
                    phase0_lib._write_private_atomic_at(
                        parent_descriptor, name, b"replacement"
                    )
                return result

            with (
                mock.patch.object(
                    phase0_lib,
                    "_read_private_artifact_file_at",
                    side_effect=swap_after_read,
                ),
                self.assertRaisesRegex(EvidenceError, "changed|manifest"),
            ):
                audit_artifact_directory(file_artifact)

            writer = AtomicArtifactDirectory(root, "attempt-root-swap")
            writer.write("raw.bin", b"original")
            root_artifact = writer.publish(status="ok", binding={})
            displaced_root = parent / "displaced-artifact"
            original_entries = phase0_lib._artifact_entries_from_descriptor
            swapped_root = False

            def swap_after_tree_read(
                descriptor: int,
            ) -> list[dict[str, object]]:
                nonlocal swapped_root
                result = original_entries(descriptor)
                if not swapped_root:
                    swapped_root = True
                    root_artifact.rename(displaced_root)
                    make_private_directory(root_artifact)
                    write_file(root_artifact / "outside-marker", b"must survive")
                return result

            with (
                mock.patch.object(
                    phase0_lib,
                    "_artifact_entries_from_descriptor",
                    side_effect=swap_after_tree_read,
                ),
                self.assertRaisesRegex(EvidenceError, "replaced|changed"),
            ):
                audit_artifact_directory(root_artifact)
            self.assertEqual(
                (root_artifact / "outside-marker").read_bytes(), b"must survive"
            )

    def test_artifact_manifest_types_and_tree_bounds_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = make_private_directory(Path(directory) / "evidence")

            writer = AtomicArtifactDirectory(root, "attempt-schema")
            writer.write("raw.bin", b"evidence")
            schema_artifact = writer.publish(status="ok", binding={})
            manifest_path = schema_artifact / "artifact-manifest.json"
            manifest = strict_json_loads(
                manifest_path.read_bytes(), label="test manifest"
            )
            manifest["files"][0]["sha256"] = True
            write_file(manifest_path, phase0_lib.pretty_json_bytes(manifest))
            with self.assertRaisesRegex(EvidenceError, "manifest file entry"):
                audit_artifact_directory(schema_artifact)

            writer = AtomicArtifactDirectory(root, "attempt-bounds")
            writer.write("nested/raw.bin", b"eight888")
            bounded_artifact = writer.publish(status="ok", binding={})
            with (
                mock.patch.object(phase0_lib, "MAX_ARTIFACT_TREE_BYTES", 4),
                self.assertRaisesRegex(EvidenceError, "cumulative byte"),
            ):
                audit_artifact_directory(bounded_artifact)
            with (
                mock.patch.object(phase0_lib, "MAX_ARTIFACT_ENTRIES", 1),
                self.assertRaisesRegex(EvidenceError, "entry-count"),
            ):
                audit_artifact_directory(bounded_artifact)
            with (
                mock.patch.object(phase0_lib, "MAX_ARTIFACT_DEPTH", 0),
                self.assertRaisesRegex(EvidenceError, "depth"),
            ):
                audit_artifact_directory(bounded_artifact)

    def test_unpublished_staging_cleanup_is_descriptor_anchored(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            root = make_private_directory(parent / "evidence")
            writer = AtomicArtifactDirectory(root, "attempt-cleanup")
            writer.write("nested/raw.bin", b"owned")
            staging = writer.staging
            writer.close()
            phase0_lib.safe_remove_unpublished_staging(staging, root)
            self.assertFalse(staging.exists())

            outside = make_private_directory(parent / "outside")
            write_file(outside / "marker", b"must survive")
            linked = root / ".linked.staging"
            linked.symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(EvidenceError, "non-symlink"):
                phase0_lib.safe_remove_unpublished_staging(linked, root)
            self.assertEqual((outside / "marker").read_bytes(), b"must survive")

    def test_staging_cleanup_root_swap_preserves_replacement_tree(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            root = make_private_directory(parent / "evidence")
            staging = make_private_directory(root / ".owned.staging")
            write_file(staging / "owned", b"remove")
            displaced = parent / "displaced-evidence"
            original_clear = phase0_lib._clear_directory_descriptor
            swapped = False

            def swap_then_clear(descriptor: int, *, expected_device: int) -> None:
                nonlocal swapped
                if not swapped:
                    swapped = True
                    root.rename(displaced)
                    make_private_directory(root)
                    write_file(root / "outside-marker", b"must survive")
                original_clear(descriptor, expected_device=expected_device)

            with (
                mock.patch.object(
                    phase0_lib,
                    "_clear_directory_descriptor",
                    side_effect=swap_then_clear,
                ),
                self.assertRaisesRegex(EvidenceError, "pathname was replaced"),
            ):
                phase0_lib.safe_remove_unpublished_staging(staging, root)
            self.assertEqual((root / "outside-marker").read_bytes(), b"must survive")

    def test_campaign_audit_rejects_complete_root_membership_change(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            write_file(config.ledger_path, b"")
            original_snapshot = phase0_lib._evidence_root_snapshot
            calls = 0

            def add_member_before_final_snapshot(root: Path) -> dict[str, object]:
                nonlocal calls
                calls += 1
                if calls == 2:
                    make_private_directory(root / "concurrent-artifact")
                return original_snapshot(root)

            with (
                mock.patch.object(
                    phase0_lib,
                    "_evidence_root_snapshot",
                    side_effect=add_member_before_final_snapshot,
                ),
                self.assertRaisesRegex(EvidenceError, "changed during campaign"),
            ):
                audit_campaign(
                    ledger_path=config.ledger_path,
                    prefix=config.ledger_prefix,
                    campaign_id=config.campaign_id,
                    evidence_root=config.evidence_root,
                )

    def test_campaign_audit_rejects_cross_bound_attempt_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = LedgerTests(
                methodName="test_reservation_is_one_durable_private_line_without_prompt_or_env_values"
            )
            fixture.temporary = tempfile.TemporaryDirectory(dir=root)
            fixture.root = Path(fixture.temporary.name)
            fixture.config = base_config(fixture.root)
            fixture.binary_identities = {
                name: fake_identity(fixture.root / "ids" / name, name.encode())
                for name in fixture.config.expected_binary_hashes
            }
            fixture.claude = fake_identity(fixture.root / "ids" / "claude")
            fixture.prompt = identify_prompt(fixture.config.prompt_paths[0])
            fixture.source = fake_source_identity()
            fixture.config = dataclasses.replace(
                fixture.config,
                expected_source_digest=fixture.source["digest"],
                expected_binary_hashes={
                    name: identity.sha256
                    for name, identity in fixture.binary_identities.items()
                },
                expected_claude_sha256=fixture.claude.sha256,
            )
            reservation = fixture._reserve()
            writer = AtomicArtifactDirectory(
                fixture.config.evidence_root, f"attempt-{ATTEMPT_ID}"
            )
            writer.write_json("reservation.json", reservation)
            writer.write_json(
                "outcome.json",
                {
                    "schema": phase0_lib.ATTEMPT_SCHEMA,
                    "campaign_id": CAMPAIGN_ID,
                    "run_id": RUN_ID,
                    "attempt_id": ATTEMPT_ID,
                    "global_attempt_ordinal": 1,
                    "reservation_sha256": reservation["reservation_sha256"],
                    "status": "pmux_exit_zero",
                },
            )
            writer.publish(
                status="pmux_exit_zero",
                binding={
                    "campaign_id": CAMPAIGN_ID,
                    "run_id": RUN_ID,
                    "attempt_id": ATTEMPT_ID,
                    "reservation_sha256": reservation["reservation_sha256"],
                    "campaign_contract_sha256": reservation["campaign_contract_sha256"],
                    "source_digest": "b" * 64,
                },
            )
            with self.assertRaisesRegex(EvidenceError, "binding differs"):
                audit_campaign(
                    ledger_path=fixture.config.ledger_path,
                    prefix=fixture.config.ledger_prefix,
                    campaign_id=CAMPAIGN_ID,
                    evidence_root=fixture.config.evidence_root,
                )
            fixture.temporary.cleanup()

    def test_audit_reports_reserved_crash_without_artifact_as_incomplete(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = LedgerTests(
                methodName="test_reservation_is_one_durable_private_line_without_prompt_or_env_values"
            )
            fixture.temporary = tempfile.TemporaryDirectory(dir=root)
            fixture.root = Path(fixture.temporary.name)
            fixture.config = base_config(fixture.root)
            fixture.binary_identities = {
                name: fake_identity(fixture.root / "ids" / name, name.encode())
                for name in fixture.config.expected_binary_hashes
            }
            fixture.claude = fake_identity(fixture.root / "ids" / "claude")
            fixture.prompt = identify_prompt(fixture.config.prompt_paths[0])
            fixture.source = fake_source_identity()
            fixture.config = dataclasses.replace(
                fixture.config,
                expected_source_digest=fixture.source["digest"],
                expected_binary_hashes={
                    name: identity.sha256
                    for name, identity in fixture.binary_identities.items()
                },
                expected_claude_sha256=fixture.claude.sha256,
            )
            fixture._reserve()
            audit = audit_campaign(
                ledger_path=fixture.config.ledger_path,
                prefix=fixture.config.ledger_prefix,
                campaign_id=CAMPAIGN_ID,
                evidence_root=fixture.config.evidence_root,
            )
            self.assertEqual(audit["verdict"], "incomplete")
            self.assertEqual(audit["missing_attempt_artifacts"], [ATTEMPT_ID])
            fixture.temporary.cleanup()


class PublicOutputTests(unittest.TestCase):
    def test_claude_version_probe_uses_the_production_first_semver_rule(self) -> None:
        self.assertEqual(
            phase0_lib.normalize_claude_version_output(
                b"Claude Code (9.9.9), build 10.0.0\n"
            ),
            "9.9.9",
        )
        for payload in (b"Claude Code 9.9\n", b"Claude Code v9.9.9\n", b"\xff"):
            with self.subTest(payload=payload), self.assertRaises(EvidenceError):
                phase0_lib.normalize_claude_version_output(payload)

    def test_json_and_ndjson_are_syntax_checked_without_transcript_semantics(
        self,
    ) -> None:
        result = {"usage": {"combined": self._usage(1, 2, 3, 4)}}
        parsed, count = parse_public_output(canonical_json_bytes(result), "json")
        self.assertEqual(count, 1)
        self.assertEqual(public_result(parsed, "json"), result)
        records = (
            canonical_json_bytes({"type": "event", "data": {"opaque": True}})
            + b"\n"
            + canonical_json_bytes({"type": "result", "data": result})
            + b"\n"
        )
        parsed, count = parse_public_output(records, "ndjson")
        self.assertEqual(count, 2)
        self.assertEqual(public_result(parsed, "ndjson"), result)

    def test_ndjson_requires_exactly_one_product_result_for_usage_accounting(
        self,
    ) -> None:
        for payload in (
            b'{"type":"event","data":{}}\n',
            b'{"type":"result","data":{}}\n{"type":"result","data":{}}\n',
            b'{"type":"result","data":{}}\n{"type":"event","data":{}}\n',
        ):
            parsed, _ = parse_public_output(payload, "ndjson")
            with self.assertRaises(EvidenceError):
                public_result(parsed, "ndjson")

    def test_public_json_rejects_duplicate_keys_and_nonfinite_numbers(self) -> None:
        for payload in (
            b'{"usage":{},"usage":{}}',
            b'{"value":NaN}',
            b'{"value":Infinity}',
            b'{"value":-Infinity}',
        ):
            with self.subTest(payload=payload), self.assertRaises(EvidenceError):
                parse_public_output(payload, "json")
        with self.assertRaises(EvidenceError):
            strict_json_loads('{"a":1,"a":2}', label="test")

    def test_public_result_binding_records_exact_resolved_runtime_cell(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = base_config(Path(directory))
            host = current_platform_identity()
            report = {
                "claude_version": "9.9.9",
                "os": host["os"],
                "arch": host["architecture"],
                "terminal_profile": "transparent",
                "input_transport": "sdk",
                "tested": False,
                "transcript_drain_ms": 2_000,
            }
            result = {
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "turn_id": TURN_ID,
                "model": "claude-sonnet-5-test",
                "claude_version": "9.9.9",
                "compatibility": report,
                "timings": dict(TURN_TIMINGS),
            }
            binding = public_result_binding(
                result,
                config,
                expected_session_id=SESSION_ID,
                expected_generation_id=GENERATION_ID,
                expected_turn_id=TURN_ID,
                expected_claude_version="9.9.9",
            )
            self.assertEqual(binding["compatibility"], report)
            with self.assertRaisesRegex(EvidenceError, "different turn"):
                public_result_binding(
                    result,
                    config,
                    expected_session_id=SESSION_ID,
                    expected_generation_id=GENERATION_ID,
                    expected_turn_id=ATTEMPT_ID,
                )
            with self.assertRaisesRegex(EvidenceError, "different generation"):
                public_result_binding(
                    result,
                    config,
                    expected_session_id=SESSION_ID,
                    expected_generation_id=ATTEMPT_ID,
                    expected_turn_id=TURN_ID,
                )
            with self.assertRaisesRegex(EvidenceError, "unauthorized model"):
                public_result_binding(
                    {**result, "model": "unapproved-model"},
                    config,
                    expected_session_id=SESSION_ID,
                    expected_generation_id=GENERATION_ID,
                    expected_turn_id=TURN_ID,
                )
            with self.assertRaisesRegex(EvidenceError, "frozen Claude probe"):
                public_result_binding(
                    result,
                    config,
                    expected_session_id=SESSION_ID,
                    expected_generation_id=GENERATION_ID,
                    expected_turn_id=TURN_ID,
                    expected_claude_version="9.9.8",
                )

    def test_tested_public_result_must_equal_the_bound_profile(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host = current_platform_identity()
            profile = {
                "claude_version": "9.9.9",
                "os": host["os"],
                "arch": host["architecture"],
                "terminal_profile": "transparent",
                "input_transport": "sdk",
                "transcript_drain_ms": 875,
            }
            profile_path = write_file(
                root / "profile.json", canonical_json_bytes(profile) + b"\n"
            )
            config = dataclasses.replace(
                base_config(root),
                compatibility="require-tested",
                tested_profile_path=profile_path,
            )
            report = {**profile, "tested": True}
            result = {
                "session_id": SESSION_ID,
                "generation_id": GENERATION_ID,
                "turn_id": TURN_ID,
                "model": "claude-sonnet-5-test",
                "claude_version": "9.9.9",
                "compatibility": report,
                "timings": dict(TURN_TIMINGS),
            }
            binding = public_result_binding(
                result,
                config,
                expected_session_id=SESSION_ID,
                expected_generation_id=GENERATION_ID,
                expected_turn_id=TURN_ID,
                tested_profile=profile,
            )
            self.assertEqual(binding["compatibility"], report)

            with self.assertRaisesRegex(EvidenceError, "without bound profile"):
                public_result_binding(
                    result,
                    config,
                    expected_session_id=SESSION_ID,
                    expected_generation_id=GENERATION_ID,
                    expected_turn_id=TURN_ID,
                )
            with self.assertRaisesRegex(
                EvidenceError, "differs from the bound profile"
            ):
                public_result_binding(
                    {
                        **result,
                        "compatibility": {
                            **report,
                            "transcript_drain_ms": 876,
                        },
                    },
                    config,
                    expected_session_id=SESSION_ID,
                    expected_generation_id=GENERATION_ID,
                    expected_turn_id=TURN_ID,
                    tested_profile=profile,
                )

    def test_usage_guard_consumes_only_public_combined_counters(self) -> None:
        result = {"usage": {"combined": self._usage(1, 2, 3, 4)}}
        self.assertEqual(observed_tokens_from_public_result(result), 10)
        for invalid in (-1, True, "1", None):
            bad = {"usage": {"combined": self._usage(1, 2, 3, 4)}}
            bad["usage"]["combined"]["input_tokens"] = invalid
            with self.subTest(invalid=invalid), self.assertRaises(EvidenceError):
                observed_tokens_from_public_result(bad)

    def test_public_handle_requires_canonical_uuid_strings(self) -> None:
        self.assertEqual(
            extract_public_handle(
                {"session_id": SESSION_ID, "generation_id": GENERATION_ID}
            ),
            (SESSION_ID, GENERATION_ID),
        )
        with self.assertRaises(EvidenceError):
            extract_public_handle({"session_id": SESSION_ID})

    def test_ping_and_close_require_exact_public_dtos(self) -> None:
        self.assertEqual(
            public_ping_binding({"server_version": "pmux-test", "protocol_version": 1})[
                "protocol_version"
            ],
            1,
        )
        for invalid in (
            {"server_version": "pmux-test", "protocol_version": True},
            {
                "server_version": "pmux-test",
                "protocol_version": 1,
                "extra": True,
            },
        ):
            with self.subTest(invalid=invalid), self.assertRaises(EvidenceError):
                public_ping_binding(invalid)
        close = {
            "session_id": SESSION_ID,
            "generation_id": GENERATION_ID,
            "already_closed": False,
            "process_reaped": True,
        }
        self.assertEqual(
            public_close_binding(
                close,
                expected_session_id=SESSION_ID,
                expected_generation_id=GENERATION_ID,
            ),
            close,
        )
        for invalid in (
            {**close, "generation_id": ATTEMPT_ID},
            {**close, "process_reaped": False},
            {**close, "extra": True},
        ):
            with self.subTest(invalid=invalid), self.assertRaises(EvidenceError):
                public_close_binding(
                    invalid,
                    expected_session_id=SESSION_ID,
                    expected_generation_id=GENERATION_ID,
                )

    @staticmethod
    def _usage(
        input_tokens: int, output_tokens: int, creation: int, read: int
    ) -> dict[str, int]:
        return {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_creation_input_tokens": creation,
            "cache_read_input_tokens": read,
        }


class DrainCalibrationTests(unittest.TestCase):
    """The campaign's real product: how much later than the terminal candidate
    the last transcript row actually arrived."""

    def test_the_product_publishes_exactly_one_discoverable_late_row_field(
        self,
    ) -> None:
        """The discovery rule is only unambiguous while TurnTimings carries one
        field beyond the five this tool knows. Fail here rather than in a live
        campaign if the product ever publishes a second one."""

        source = (ROOT / "crates" / "protocol" / "src" / "v1.rs").read_text()
        body = source.split("pub struct TurnTimings {", 1)[1].split("\n}", 1)[0]
        fields = re.findall(r"^\s{4}pub ([a-z0-9_]+):", body, re.MULTILINE)
        self.assertTrue(fields)
        unknown = [name for name in fields if name not in KNOWN_TURN_TIMING_FIELDS]
        self.assertEqual(len(unknown), 1, unknown)
        self.assertTrue(unknown[0].endswith("_ms"), unknown[0])
        live_shape = {
            name: 9_000 if name in {"terminal_candidate_at_ms", unknown[0]} else 11_400
            for name in fields
        }
        self.assertEqual(
            drain_calibration_from_timings(live_shape)["late_arrival_field"],
            unknown[0],
        )

    def test_whole_timings_object_is_captured_without_naming_its_fields(self) -> None:
        captured = turn_timings_binding({**TURN_TIMINGS, "some_future_field_ms": None})
        self.assertEqual(captured["some_future_field_ms"], None)
        self.assertEqual(captured["drain_ms"], 2_400)
        self.assertEqual(set(captured), set(TURN_TIMINGS) | {"some_future_field_ms"})
        for invalid in ({}, {"submitted_at_ms": 1}, {"completed_at_ms": 1}):
            with self.subTest(invalid=invalid), self.assertRaises(EvidenceError):
                turn_timings_binding(invalid)
        with self.assertRaises(EvidenceError):
            turn_timings_binding({**TURN_TIMINGS, "completed_at_ms": -1})
        # `*_at_ms` is a wall-clock epoch, not a small relative offset.
        epoch = {"submitted_at_ms": 1_760_000_000_000, "completed_at_ms": 2**53 - 1}
        self.assertEqual(turn_timings_binding(epoch), epoch)
        with self.assertRaises(EvidenceError):
            turn_timings_binding({**epoch, "completed_at_ms": 2**53})

    def test_late_arrival_field_is_discovered_rather_than_hardcoded(self) -> None:
        base = {
            key: value
            for key, value in TURN_TIMINGS.items()
            if key != "last_transcript_row_at_ms"
        }
        renamed = {**base, "an_entirely_different_name_at_ms": 9_600}
        calibration = drain_calibration_from_timings(renamed)
        self.assertEqual(
            calibration["late_arrival_field"], "an_entirely_different_name_at_ms"
        )
        self.assertEqual(calibration["late_arrival_basis"], "absolute_timestamp")
        self.assertEqual(calibration["late_arrival_gap_ms"], 600)
        duration = {**base, "late_row_gap_ms": 350}
        self.assertEqual(
            drain_calibration_from_timings(duration)["late_arrival_basis"], "duration"
        )
        self.assertEqual(
            drain_calibration_from_timings(duration)["late_arrival_gap_ms"], 350
        )

    def test_the_gap_is_signed_and_a_negative_gap_is_not_clamped(self) -> None:
        """The candidate stamp and the last-activity stamp come from one read, so
        the difference straddles zero. Clamping would erase the boundary between
        `the candidate row was the last row` and `one more row landed later`."""

        early = {**TURN_TIMINGS, "last_transcript_row_at_ms": 8_400}
        calibration = drain_calibration_from_timings(early)
        self.assertEqual(calibration["late_arrival_gap_ms"], -600)
        summary = summarize_drain_calibration(
            [calibration], configured_transcript_drain_ms=2_000
        )
        self.assertEqual(summary["gap_ms"]["min"], -600)
        self.assertEqual(summary["no_late_row_attempts"], 1)
        self.assertEqual(summary["late_row_attempts"], 0)
        # A maximum at or below the noise band is the zero-evidence case, so
        # there is no MEASURED worst case to have headroom against and the field
        # is null. It used to publish `configured - max(max, 0)` = the full
        # 2,000 ms, which reads as proven margin derived from an absence.
        self.assertIsNone(summary["headroom_ms"])

    def test_ambiguous_or_impossible_late_arrival_evidence_fails_loudly(self) -> None:
        with self.assertRaisesRegex(EvidenceError, "ambiguous"):
            drain_calibration_from_timings(
                {**TURN_TIMINGS, "another_unknown_at_ms": 9_100}
            )
        with self.assertRaisesRegex(EvidenceError, "later than the committed turn"):
            drain_calibration_from_timings(
                {**TURN_TIMINGS, "last_transcript_row_at_ms": 11_401}
            )
        with self.assertRaisesRegex(EvidenceError, "exceeds the observed drain"):
            drain_calibration_from_timings(
                {
                    **{
                        key: value
                        for key, value in TURN_TIMINGS.items()
                        if key != "last_transcript_row_at_ms"
                    },
                    "late_row_gap_ms": 2_401,
                }
            )

    def test_absent_evidence_is_reported_as_absent_not_as_a_zero_gap(self) -> None:
        without = {
            key: value
            for key, value in TURN_TIMINGS.items()
            if key != "last_transcript_row_at_ms"
        }
        calibration = drain_calibration_from_timings(without)
        self.assertIsNone(calibration["late_arrival_gap_ms"])
        self.assertEqual(
            calibration["uncomputable_reason"], "no_late_arrival_field_published"
        )
        summary = summarize_drain_calibration(
            [calibration], configured_transcript_drain_ms=2_000
        )
        self.assertEqual(summary["gap_ms"], None)
        self.assertEqual(summary["attempts_without_computable_gap"], 1)
        self.assertIn("cannot calibrate", summary["interpretation"])
        no_candidate = drain_calibration_from_timings(
            {**TURN_TIMINGS, "terminal_candidate_at_ms": None}
        )
        self.assertEqual(
            no_candidate["uncomputable_reason"], "no_terminal_candidate_timestamp"
        )

    def test_summary_reports_the_distribution_and_its_headroom(self) -> None:
        gaps = (0, 120, 400, 1_500)
        summary = summarize_drain_calibration(
            [
                drain_calibration_from_timings(
                    {**TURN_TIMINGS, "last_transcript_row_at_ms": 9_000 + gap}
                )
                for gap in gaps
            ],
            configured_transcript_drain_ms=2_000,
        )
        self.assertEqual(summary["schema"], DRAIN_CALIBRATION_SCHEMA)
        self.assertEqual(
            summary["gap_ms"],
            {"count": 4, "min": 0, "median": 120, "p95": 1_500, "max": 1_500},
        )
        self.assertEqual(summary["no_late_row_attempts"], 1)
        self.assertEqual(summary["late_row_attempts"], 3)
        self.assertEqual(summary["headroom_ms"], 500)
        self.assertEqual(summary["late_arrival_fields"], ["last_transcript_row_at_ms"])
        self.assertIn("only measured lower bound", summary["interpretation"])

    def test_an_all_zero_run_is_labelled_absence_of_evidence(self) -> None:
        """The count that matters: a drain calibrated only against turns where
        nothing arrived late is calibrated against absence of evidence."""

        summary = summarize_drain_calibration(
            [drain_calibration_from_timings(dict(TURN_TIMINGS)) for _ in range(6)],
            configured_transcript_drain_ms=2_000,
        )
        self.assertEqual(summary["no_late_row_attempts"], 6)
        self.assertEqual(summary["late_row_attempts"], 0)
        # No late row was measured, so there is no worst case to have headroom
        # against and the field is null. Publishing the configured drain here
        # presented an absence as 2,000 ms of proven margin -- the exact reading
        # the interpretation string below tells you not to make.
        self.assertIsNone(summary["headroom_ms"])
        self.assertIn("ABSENCE", summary["interpretation"])
        self.assertIn("not a worst case", summary["interpretation"])
        self.assertIn("Do not read it as permission to cut", summary["interpretation"])


class ProcessEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.fixture = compile_process_fixture(
            Path(self.temporary.name) / "process-fixture"
        ).resolve()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _run(self, mode: str, *, timeout: int = 5) -> phase0_lib.CommandResult:
        return run_command(
            [str(self.fixture), "--fixture-mode", mode],
            cwd=ROOT,
            environment=os.environ,
            timeout_seconds=timeout,
            argv_shape=["<native-fixture>", mode],
        )

    def assert_receipt_has_no_live_owned_process(
        self, result: phase0_lib.CommandResult
    ) -> None:
        ledger = result.process_receipt["process_ledger"]
        self.assertTrue(ledger)
        self.assertTrue(all(row["reaped"] is True for row in ledger))
        for row in ledger:
            self.assertNotEqual(
                phase0_lib.bounded_process.precise_process_started(row["pid"]),
                row["started"],
            )

    def test_command_timeout_is_receipted_and_every_owned_process_is_reaped(
        self,
    ) -> None:
        result = self._run("sleep", timeout=1)
        self.assertTrue(result.timed_out)
        self.assertEqual(result.supervision_failure_reason, "timeout")
        self.assertTrue(result.cleanup_complete)
        self.assert_receipt_has_no_live_owned_process(result)

    def test_command_output_flood_fails_at_one_combined_online_limit(self) -> None:
        with mock.patch.object(phase0_lib, "MAX_CAPTURE_BYTES", 4_096):
            result = self._run("flood")
        self.assertTrue(result.output_limit_exceeded)
        self.assertEqual(result.supervision_failure_reason, "output_limit")
        self.assertLessEqual(len(result.stdout) + len(result.stderr), 4_096)
        self.assertTrue(result.cleanup_complete)
        self.assert_receipt_has_no_live_owned_process(result)

    def test_leader_exit_with_pipe_holding_descendant_never_hangs_or_passes(
        self,
    ) -> None:
        result = self._run("pipe-holder")
        self.assertIsNotNone(result.supervision_failure_reason)
        # "timeout", not "drain_timeout". This scenario runs with
        # timeout_seconds == drain_timeout_seconds, and bounded_process used to
        # choose its label from `drain_deadline <= deadline` -- true by
        # construction, since drain_deadline is assigned min(deadline, ...). So
        # every post-exit expiry was labelled a drain overrun, and because
        # phase0_lib derives `timed_out = (reason == "timeout")`, a command that
        # burned its entire lifetime envelope was published with
        # `timed_out: false`. The reason now names the bound that actually bound.
        self.assertIn(
            result.supervision_failure_reason,
            {"descendant_survived", "timeout"},
        )
        self.assertTrue(result.cleanup_complete)
        self.assert_receipt_has_no_live_owned_process(result)

    def test_session_and_parent_escaped_descendant_is_detected_and_reaped(self) -> None:
        result = self._run("escaped-descendant")
        self.assertIsNotNone(result.supervision_failure_reason)
        # "timeout", not "drain_timeout". This scenario runs with
        # timeout_seconds == drain_timeout_seconds, and bounded_process used to
        # choose its label from `drain_deadline <= deadline` -- true by
        # construction, since drain_deadline is assigned min(deadline, ...). So
        # every post-exit expiry was labelled a drain overrun, and because
        # phase0_lib derives `timed_out = (reason == "timeout")`, a command that
        # burned its entire lifetime envelope was published with
        # `timed_out: false`. The reason now names the bound that actually bound.
        self.assertIn(
            result.supervision_failure_reason,
            {"descendant_survived", "timeout"},
        )
        self.assertTrue(result.cleanup_complete)
        self.assertGreaterEqual(len(result.process_receipt["process_ledger"]), 2)
        self.assert_receipt_has_no_live_owned_process(result)


class FakeNativeCampaignTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.release = self.root / "release"
        self.cwd = self.root / "work"
        self.evidence = make_private_directory(self.root / "evidence")
        self.ledger_parent = make_private_directory(self.root / "ledger")
        self.log = self.root / "fake-pmux-argv.ndjson"
        self.source.mkdir()
        self.release.mkdir()
        self.cwd.mkdir()
        self._make_source_repo()
        self._make_fake_binaries()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _make_source_repo(self) -> None:
        make_fixture_source_repo(self.source)

    def _make_fake_binaries(self) -> None:
        platform_identity = current_platform_identity()
        compatibility = {
            "claude_version": "9.9.9",
            "os": platform_identity["os"],
            "arch": platform_identity["architecture"],
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "tested": False,
            "transcript_drain_ms": 2_000,
        }
        tested_compatibility = {**compatibility, "tested": True}
        claude = """#!/usr/bin/env python3
import sys
if sys.argv[1:] == ["--version"]:
    print("fake Claude 9.9.9")
    raise SystemExit(0)
raise SystemExit(91)
"""
        daemon = """#!/usr/bin/env python3
import os, signal, socket, sys, time
args = sys.argv[1:]
path = args[args.index("--socket") + 1]
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.bind(path)
os.chmod(path, 0o600)
sock.listen()
running = True
def stop(_signum, _frame):
    global running
    running = False
signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
while running:
    time.sleep(0.02)
sock.close()
try:
    os.unlink(path)
except FileNotFoundError:
    pass
"""
        pmux = f"""#!/usr/bin/env python3
import hashlib, json, os, sys
args = sys.argv[1:]
commands = [name for name in ("ping", "run", "start", "turn", "close") if name in args]
command = commands[0] if commands else "unknown"
prompt = sys.stdin.buffer.read() if command in {{"run", "turn"}} else b""
with open(os.environ["FAKE_PMUX_ARGV_LOG"], "a", encoding="utf-8") as output:
    output.write(json.dumps({{"command": command, "argv": args, "stdin_sha256": hashlib.sha256(prompt).hexdigest()}}) + "\\n")
if command in {{"run", "start", "turn"}}:
    ledger = os.environ["FAKE_LEDGER"]
    if not os.path.exists(ledger) or not open(ledger, "rb").read().endswith(b"\\n"):
        raise SystemExit(92)
if command == "ping":
    ping = {{"server_version": "fake-pmux", "protocol_version": 1}}
    if os.environ.get("FAKE_BAD_PING"):
        ping["unexpected"] = True
    print(json.dumps(ping))
elif command == "start":
    if os.environ.get("FAKE_START_FAIL"):
        sys.stderr.write("fake pmux start refused\\n")
        raise SystemExit(94)
    flag = "--resume" if "--resume" in args else "--session-id"
    session = args[args.index(flag) + 1]
    print(json.dumps({{"session_id": session, "generation_id": "{GENERATION_ID}", "state": "ready", "compatibility": {compatibility!r}, "created_at_ms": 1, "last_sequence": 0}}))
elif command in {{"run", "turn"}}:
    session = args[args.index("--session-id") + 1] if command == "run" else args[args.index("turn") + 1]
    turn = args[args.index("--turn-id") + 1]
    generation = os.environ.get("FAKE_RESULT_GENERATION", "{GENERATION_ID}")
    model = os.environ.get("FAKE_RESULT_MODEL", "claude-sonnet-5-test")
    timings = dict({TURN_TIMINGS!r})
    late = os.environ.get("FAKE_LATE_ROW_AT_MS")
    if late == "absent":
        del timings["last_transcript_row_at_ms"]
    elif late is not None:
        timings["last_transcript_row_at_ms"] = int(late)
    result = {{"session_id": session, "generation_id": generation, "turn_id": turn, "model": model, "claude_version": "9.9.9", "compatibility": {compatibility!r}, "timings": timings, "usage": {{"combined": {{"input_tokens": 1, "output_tokens": 2, "cache_creation_input_tokens": 3, "cache_read_input_tokens": 4}}}}}}
    secret = os.environ.get("PHASE0_SECRET_TOKEN")
    if secret:
        result["opaque"] = secret
        sys.stderr.write(secret + "\\n" + json.dumps(secret, ensure_ascii=True) + "\\n")
    if "ndjson" in args:
        print(json.dumps({{"type": "result", "data": result}}))
    else:
        print(json.dumps(result))
elif command == "close":
    session = args[args.index("close") + 1]
    generation = os.environ.get("FAKE_CLOSE_GENERATION", args[args.index("--generation") + 1])
    reaped = not bool(os.environ.get("FAKE_CLOSE_NOT_REAPED"))
    print(json.dumps({{"session_id": session, "generation_id": generation, "already_closed": False, "process_reaped": reaped}}))
else:
    raise SystemExit(93)
"""
        claude_p = f"""#!/usr/bin/env python3
import hashlib, json, os, sys, uuid
args = sys.argv[1:]
prompt = sys.stdin.buffer.read()
with open(os.environ["FAKE_PMUX_ARGV_LOG"], "a", encoding="utf-8") as output:
    output.write(json.dumps({{"command": "claude-p", "argv": args, "stdin_sha256": hashlib.sha256(prompt).hexdigest()}}) + "\\n")
ledger = os.environ["FAKE_LEDGER"]
if not os.path.exists(ledger) or not open(ledger, "rb").read().endswith(b"\\n"):
    raise SystemExit(92)
session = args[args.index("--session-id") + 1]
result = {{"session_id": session, "generation_id": "{GENERATION_ID}", "turn_id": str(uuid.uuid4()), "model": "claude-sonnet-5-test", "claude_version": "9.9.9", "compatibility": {tested_compatibility!r}, "timings": dict({TURN_TIMINGS!r}), "usage": {{"combined": {{"input_tokens": 1, "output_tokens": 2, "cache_creation_input_tokens": 3, "cache_read_input_tokens": 4}}}}}}
print(json.dumps(result))
"""
        scripts = make_private_directory(self.root / "fixture-scripts")
        script_sources = {
            "claude": claude,
            "pmuxd": daemon,
            "pmux": pmux,
            "claude-p": claude_p,
            "pmux-mcp": "raise SystemExit(0)\n",
            "pmux-rmuxd": "raise SystemExit(0)\n",
            "pmux-launcher": "raise SystemExit(0)\n",
            "pmux-hook": "raise SystemExit(0)\n",
        }
        self.fixture_scripts: dict[str, Path] = {}
        shim = compile_process_fixture(self.root / "fixture-dispatch")
        for name, source in script_sources.items():
            script = write_file(scripts / f"{name}.py", source.encode(), 0o600)
            destination = (
                self.root / "claude" if name == "claude" else self.release / name
            )
            shutil.copy2(shim, destination)
            destination.chmod(0o700)
            self.fixture_scripts[name] = script.resolve()

    def _config(
        self, scenario: str = "one-shot", prompt_count: int = 1
    ) -> CampaignConfig:
        prompts = tuple(
            write_file(
                self.root / f"prompt-{index}.txt", f"private prompt {index}".encode()
            )
            for index in range(prompt_count)
        )
        source = compute_source_identity(self.source)
        hashes = {
            name: identify_file(self.release / name, executable=True).sha256
            for name in REQUIRED_RELEASE_BINARIES
        }
        facade = scenario == "claude-p-one-shot"
        platform_identity = current_platform_identity()
        tested_profile_value = {
            "claude_version": "9.9.9",
            "os": platform_identity["os"],
            "arch": platform_identity["architecture"],
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": 2_000,
        }
        tested_profile = (
            write_file(
                self.root / "tested-profile.json",
                canonical_json_bytes(tested_profile_value) + b"\n",
            )
            if facade
            else None
        )
        return CampaignConfig(
            source_root=self.source.resolve(),
            expected_source_digest=source["digest"],
            release_bin_dir=self.release.resolve(),
            expected_binary_hashes=hashes,
            claude_bin=(self.root / "claude").resolve(),
            expected_claude_sha256=identify_file(self.root / "claude").sha256,
            cwd=self.cwd.resolve(),
            prompt_paths=prompts,
            evidence_root=self.evidence.resolve(),
            ledger_path=(self.ledger_parent / "attempts.ndjson").resolve(),
            ledger_prefix=LedgerPrefix(0, EMPTY_SHA256, 0),
            prior_campaign_anchors={},
            campaign_id=CAMPAIGN_ID,
            global_attempt_ceiling=60,
            max_attempts_this_run=prompt_count,
            max_observed_tokens=10_000,
            scenario=scenario,
            resume_session_id=SESSION_ID if scenario == "resume" else None,
            model="sonnet",
            allowed_model_ids=("claude-sonnet-5-test",),
            effort="low",
            output_format="json",
            compatibility="require-tested" if facade else "allow-untested",
            tested_profile_path=tested_profile,
            terminal_rows=24,
            terminal_cols=120,
            terminal_profile="transparent",
            input_transport="auto" if facade else "sdk",
            lifecycle="transcript",
            untested_transcript_drain_ms=2_000,
            turn_timeout_seconds=5,
            daemon_ready_timeout_seconds=5,
            daemon_shutdown_timeout_seconds=5,
            live=True,
            acknowledge_usage=True,
            acknowledge_untested=True,
        )

    def _environment(self, config: CampaignConfig) -> dict[str, str]:
        environment = dict(os.environ)
        environment["FAKE_PMUX_ARGV_LOG"] = str(self.log)
        environment["FAKE_LEDGER"] = str(config.ledger_path)
        environment["PMUX_PHASE0_FIXTURE_PYTHON"] = str(Path(sys.executable).resolve())
        for name, script in self.fixture_scripts.items():
            key = "PMUX_PHASE0_SCRIPT_" + "".join(
                character.upper() if character.isalnum() else "_" for character in name
            )
            environment[key] = str(script)
        return environment

    def test_one_shot_uses_only_native_pmux_and_publishes_auditable_evidence(
        self,
    ) -> None:
        config = self._config()
        result = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(result["status"], "acquired")
        self.assertEqual(result["attempt_count"], 1)
        self.assertEqual(result["observed_tokens"], 10)
        self.assertEqual(result["cleanup"]["status"], "verified")
        self.assertEqual(
            result["campaign_manifest_sha256"],
            audit_artifact_directory(Path(result["evidence_directory"]))[
                "manifest_sha256"
            ],
        )
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        self.assertEqual(ledger["tail_count"], 1)
        reservation = ledger["campaign_reservations"][0]
        self.assertEqual(
            reservation["campaign_contract_sha256"],
            result["campaign_contract_sha256"],
        )
        self.assertEqual(
            campaign_contract_sha256(result["campaign_contract"]),
            result["campaign_contract_sha256"],
        )
        self.assertEqual(reservation["scenario_role"], "fresh_one_shot")
        self.assertEqual(set(reservation["binaries"]), set(REQUIRED_RELEASE_BINARIES))
        self.assertEqual(reservation["public_entrypoint"], "pmux")
        self.assertNotIn("pmux-mcp", reservation["exercised_binaries"])
        argv_log = self.log.read_text()
        self.assertIn('"command": "run"', argv_log)
        self.assertNotIn("private prompt", argv_log)
        records = [json.loads(line) for line in argv_log.splitlines()]
        run_record = next(record for record in records if record["command"] == "run")
        self.assertEqual(
            run_record["stdin_sha256"],
            sha256_bytes(config.prompt_paths[0].read_bytes()),
        )
        audit = audit_campaign(
            ledger_path=config.ledger_path,
            prefix=config.ledger_prefix,
            campaign_id=CAMPAIGN_ID,
            evidence_root=config.evidence_root,
            expected_campaign_anchors={
                result["run_id"]: result["campaign_manifest_sha256"]
            },
        )
        self.assertEqual(audit["verdict"], "complete")
        self.assertTrue(audit["promotion_eligible"])
        self.assertEqual(
            audit["campaign_contract_sha256"], result["campaign_contract_sha256"]
        )
        self.assertEqual(audit["durable_observed_tokens"], 10)

    def test_forwarded_launch_options_reach_pmux_and_bind_names_only(self) -> None:
        agent_file = write_file(self.root / "agent.json", b'{"model":"sonnet"}\n')
        secret = "s3cr3t-env-value"
        config = dataclasses.replace(
            self._config(),
            permission_mode="dangerously-skip-permissions",
            environment_set={"PHASE0_FORWARDED": secret},
            environment_passthrough_names=("PHASE0_PASSTHROUGH",),
            agent_name="reviewer",
            agent_file=agent_file,
        )
        environment = self._environment(config)
        environment["PHASE0_PASSTHROUGH"] = "inherited"
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "acquired")
        records = [json.loads(line) for line in self.log.read_text().splitlines()]
        argv = next(record for record in records if record["command"] == "run")["argv"]
        # `--profile`/`--profile-file`, not `--agent`/`--agent-file`. This test
        # asserted the old pair and passed for as long as the builder emitted
        # it: the fake pmux accepts any argv, so a spelling the real one refuses
        # by name was pinned here rather than caught. `LaunchSurfaceTests` reads
        # the clap declarations, which is the check that could have failed.
        for flag, value in (
            ("--permission-mode", "dangerously-skip-permissions"),
            ("--profile", "reviewer"),
            ("--profile-file", str(agent_file)),
        ):
            with self.subTest(flag=flag):
                self.assertEqual(argv[argv.index(flag) + 1], value)
        self.assertNotIn("--agent-file", argv)
        # --env is delivered name-only: the value goes into the child's
        # environment, never into an argv this envelope binds into a receipt.
        self.assertEqual(
            [
                argv[index + 1]
                for index, item in enumerate(argv)
                if item == "--env-passthrough"
            ],
            ["PHASE0_FORWARDED", "PHASE0_PASSTHROUGH"],
        )
        self.assertNotIn("--env", argv)
        options = result["campaign_contract"]["cell"]["launch_options"]
        self.assertEqual(options["permission_mode"], "dangerously-skip-permissions")
        self.assertEqual(options["environment_set_names"], ["PHASE0_FORWARDED"])
        self.assertFalse(options["environment_set_values_recorded"])
        self.assertEqual(
            options["environment_set_delivery"], "env_passthrough_name_only"
        )
        self.assertEqual(
            options["environment_passthrough_names"], ["PHASE0_PASSTHROUGH"]
        )
        self.assertEqual(options["agent_name"], "reviewer")
        self.assertEqual(
            options["agent_file"]["sha256"], identify_file(agent_file).sha256
        )
        self.assertNotIn("path", options["agent_file"])
        for path in [*config.evidence_root.rglob("*"), config.ledger_path]:
            if path.is_file():
                self.assertNotIn(secret.encode(), path.read_bytes(), path)

    def test_a_changed_agent_profile_stops_the_campaign(self) -> None:
        agent_file = write_file(self.root / "agent.json", b'{"model":"sonnet"}\n')
        config = dataclasses.replace(
            self._config(prompt_count=2), agent_name="reviewer", agent_file=agent_file
        )
        runner = CampaignRunner(config, self._environment(config))
        runner._bind_candidate()
        write_file(agent_file, b'{"model":"opus"}\n')
        with self.assertRaisesRegex(EvidenceError, "agent profile changed"):
            runner._verify_candidate_unchanged()

    def test_minified_launch_reaches_pmux_and_binds_the_replacement_by_digest(
        self,
    ) -> None:
        replacement = "You are a minified pmux cell. Marker PATHB-CELL-9D41.\n"
        system_prompt = write_file(
            self.root / "system-prompt.txt", replacement.encode()
        )
        config = dataclasses.replace(
            self._config(),
            permission_mode="dont-ask",
            denied_tools=("*",),
            system_prompt_file=system_prompt,
        )
        runner = CampaignRunner(config, self._environment(config))
        result = runner.run()
        self.assertEqual(result["status"], "acquired")
        records = [json.loads(line) for line in self.log.read_text().splitlines()]
        argv = next(record for record in records if record["command"] == "run")["argv"]
        # `*` reaches pmux as one argument. Nothing in this envelope ever joins
        # argv into a shell string, so it is never a glob.
        self.assertEqual(argv[argv.index("--denied-tool") + 1], "*")
        self.assertNotIn("--disallowedTools", argv)
        self.assertEqual(argv[argv.index("--system-prompt") + 1], replacement)
        options = result["campaign_contract"]["cell"]["launch_options"]
        self.assertEqual(options["denied_tools"], ["*"])
        self.assertEqual(options["system_prompt_policy"], "replace")
        self.assertEqual(
            options["system_prompt_file"]["sha256"],
            identify_file(system_prompt).sha256,
        )
        self.assertNotIn("path", options["system_prompt_file"])
        # The replacement joins the redaction set exactly as an --env value
        # does, so it cannot survive in captured output.
        self.assertIn(replacement, runner.redaction_values)
        self.assertNotIn(replacement.encode(), config.ledger_path.read_bytes())

        # The ONE place the text does land is the launched argv inside the
        # process receipt -- see SYSTEM_PROMPT_DELIVERY. That receipt's `argv`
        # is covered by `receipt_sha256`, so redacting it after the fact would
        # make a faithful receipt look forged. This asserts the exposure is
        # exactly that one route and nowhere else; a failure here means either
        # a new leak or that somebody rewrote a receipt instead of closing one.
        def locations(value: object, trail: tuple[object, ...] = ()):
            if isinstance(value, dict):
                for key, item in value.items():
                    yield from locations(item, (*trail, key))
            elif isinstance(value, list):
                for index, item in enumerate(value):
                    yield from locations(item, (*trail, index))
            elif isinstance(value, str) and replacement in value:
                yield trail

        seen = 0
        for path in sorted(config.evidence_root.rglob("*.json")):
            # Prefilter on the marker, not on the whole replacement: JSON
            # escapes the trailing newline, so the raw bytes never match.
            if b"PATHB-CELL-9D41" not in path.read_bytes():
                continue
            for trail in locations(json.loads(path.read_text("utf-8"))):
                seen += 1
                self.assertEqual(trail[-3:-1], ("process_receipt", "argv"), path)
        self.assertGreater(seen, 0)

    def test_the_facade_receives_the_disallowed_tools_spelling(self) -> None:
        config = dataclasses.replace(
            self._config(scenario="claude-p-one-shot"),
            permission_mode="dont-ask",
            denied_tools=("*",),
        )
        result = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(result["status"], "acquired")
        records = [json.loads(line) for line in self.log.read_text().splitlines()]
        argv = next(record for record in records if record["command"] == "claude-p")[
            "argv"
        ]
        self.assertEqual(argv[argv.index("--disallowedTools") + 1], "*")
        self.assertNotIn("--denied-tool", argv)

    def test_a_changed_system_prompt_replacement_stops_the_campaign(self) -> None:
        system_prompt = write_file(
            self.root / "system-prompt.txt", b"You are a minified pmux cell.\n"
        )
        config = dataclasses.replace(
            self._config(prompt_count=2), system_prompt_file=system_prompt
        )
        runner = CampaignRunner(config, self._environment(config))
        runner._bind_candidate()
        # Losing mode 0600 between attempts is as disqualifying as changed
        # bytes: the whole reason this document is admitted is that it stayed
        # private, and the re-check runs the full admission path to see it.
        os.chmod(system_prompt, 0o644)
        with self.assertRaisesRegex(EvidenceError, "owner-only"):
            runner._verify_candidate_unchanged()
        write_file(system_prompt, b"You are something else entirely.\n")
        with self.assertRaisesRegex(EvidenceError, "system prompt replacement changed"):
            runner._verify_candidate_unchanged()

    def test_drain_calibration_is_computed_published_and_reaudited(self) -> None:
        config = self._config(prompt_count=2)
        environment = self._environment(config)
        environment["FAKE_LATE_ROW_AT_MS"] = "9600"
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "acquired")
        calibration = result["drain_calibration"]
        self.assertEqual(
            calibration["gap_ms"],
            {"count": 2, "min": 600, "median": 600, "p95": 600, "max": 600},
        )
        self.assertEqual(calibration["no_late_row_attempts"], 0)
        self.assertEqual(calibration["late_row_attempts"], 2)
        self.assertEqual(calibration["configured_transcript_drain_ms"], 2_000)
        self.assertEqual(calibration["headroom_ms"], 1_400)
        attempt = json.loads(
            (
                Path(result["attempts"][0]["evidence_directory"]) / "outcome.json"
            ).read_text()
        )
        binding = attempt["public_result_binding"]
        self.assertEqual(
            binding["timings"], {**TURN_TIMINGS, "last_transcript_row_at_ms": 9_600}
        )
        self.assertEqual(
            binding["drain_calibration"]["terminal_candidate_at_ms"], 9_000
        )
        self.assertEqual(binding["drain_calibration"]["completed_at_ms"], 11_400)
        self.assertEqual(binding["drain_calibration"]["drain_ms"], 2_400)
        self.assertEqual(binding["drain_calibration"]["late_arrival_gap_ms"], 600)
        audit = audit_campaign(
            ledger_path=config.ledger_path,
            prefix=config.ledger_prefix,
            campaign_id=CAMPAIGN_ID,
            evidence_root=config.evidence_root,
            expected_campaign_anchors={
                result["run_id"]: result["campaign_manifest_sha256"]
            },
        )
        self.assertTrue(audit["promotion_eligible"])
        self.assertEqual(audit["drain_calibration"], calibration)

    def test_a_campaign_with_no_late_rows_is_labelled_absence_of_evidence(self) -> None:
        config = self._config()
        result = CampaignRunner(config, self._environment(config)).run()
        calibration = result["drain_calibration"]
        self.assertEqual(calibration["no_late_row_attempts"], 1)
        self.assertEqual(calibration["late_row_attempts"], 0)
        self.assertIn("ABSENCE", calibration["interpretation"])

    def test_a_result_without_a_late_arrival_field_cannot_calibrate(self) -> None:
        config = self._config()
        environment = self._environment(config)
        environment["FAKE_LATE_ROW_AT_MS"] = "absent"
        acquired = CampaignRunner(config, environment).run()
        self.assertEqual(acquired["status"], "acquired")
        self.assertEqual(
            acquired["drain_calibration"]["attempts_without_computable_gap"], 1
        )
        self.assertIn(
            "cannot calibrate", acquired["drain_calibration"]["interpretation"]
        )

    def test_restart_reconciles_durable_usage_before_reserving(self) -> None:
        config = self._config()
        first = CampaignRunner(config, self._environment(config)).run()
        resumed = dataclasses.replace(
            config,
            prior_campaign_anchors={first["run_id"]: first["campaign_manifest_sha256"]},
        )
        second = CampaignRunner(resumed, self._environment(resumed)).run()
        self.assertEqual(first["observed_tokens"], 10)
        self.assertEqual(second["observed_tokens"], 20)
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        reservations = ledger["campaign_reservations"]
        self.assertEqual(
            [item["prior_observed_tokens"] for item in reservations], [0, 10]
        )
        self.assertEqual(
            len({item["campaign_contract_sha256"] for item in reservations}), 1
        )

    def test_same_run_rejects_a_self_consistent_attempt_tree_replacement(self) -> None:
        config = self._config(prompt_count=2)
        original_execute = CampaignRunner._execute_one_shot

        def replace_after_first(runner: CampaignRunner, index: int) -> None:
            original_execute(runner, index)
            if index == 0:
                attempt = Path(runner.published_attempts[0]["evidence_directory"])
                original_anchor = runner.published_attempts[0][
                    "artifact_manifest_sha256"
                ]
                replacement_anchor = replace_artifact_tree_with_reissued_manifest(
                    attempt
                )
                self.assertNotEqual(replacement_anchor, original_anchor)

        with mock.patch.object(
            CampaignRunner, "_execute_one_shot", new=replace_after_first
        ):
            result = CampaignRunner(config, self._environment(config)).run()

        self.assertEqual(result["status"], "failed")
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        self.assertEqual(len(ledger["campaign_reservations"]), 1)
        self.assertIn("retained manifest anchor", result["error"])

    def test_restart_rejects_a_self_consistent_campaign_tree_replacement(self) -> None:
        config = self._config()
        first = CampaignRunner(config, self._environment(config)).run()
        original_anchor = first["campaign_manifest_sha256"]
        replacement_anchor = replace_artifact_tree_with_reissued_manifest(
            Path(first["evidence_directory"])
        )
        self.assertNotEqual(replacement_anchor, original_anchor)
        before = config.ledger_path.read_bytes()
        resumed = dataclasses.replace(
            config,
            prior_campaign_anchors={first["run_id"]: original_anchor},
        )

        second = CampaignRunner(resumed, self._environment(resumed)).run()

        self.assertEqual(second["status"], "failed")
        self.assertIn("externally retained manifest anchor", second["error"])
        self.assertEqual(config.ledger_path.read_bytes(), before)

    def test_final_audit_requires_exact_external_campaign_anchors(self) -> None:
        config = self._config()
        result = CampaignRunner(config, self._environment(config)).run()
        common = {
            "ledger_path": config.ledger_path,
            "prefix": config.ledger_prefix,
            "campaign_id": CAMPAIGN_ID,
            "evidence_root": config.evidence_root,
        }
        exact = {result["run_id"]: result["campaign_manifest_sha256"]}

        verified = audit_campaign(**common, expected_campaign_anchors=exact)
        self.assertTrue(verified["promotion_eligible"])
        self.assertTrue(verified["campaign_anchors"]["verified"])

        missing = audit_campaign(**common, expected_campaign_anchors={})
        self.assertFalse(missing["promotion_eligible"])
        self.assertEqual(
            missing["campaign_anchors"]["missing_run_ids"], [result["run_id"]]
        )

        wrong = audit_campaign(
            **common,
            expected_campaign_anchors={result["run_id"]: "f" * 64},
        )
        self.assertFalse(wrong["promotion_eligible"])
        self.assertEqual(
            wrong["campaign_anchors"]["mismatched_run_ids"], [result["run_id"]]
        )

        extra_run_id = "77777777-7777-4777-8777-777777777777"
        extra = audit_campaign(
            **common,
            expected_campaign_anchors={**exact, extra_run_id: "e" * 64},
        )
        self.assertFalse(extra["promotion_eligible"])
        self.assertEqual(extra["campaign_anchors"]["extra_run_ids"], [extra_run_id])

    def test_restart_at_durable_usage_guard_never_appends(self) -> None:
        config = dataclasses.replace(self._config(), max_observed_tokens=10)
        first = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(first["status"], "acquired")
        before = config.ledger_path.read_bytes()
        resumed = dataclasses.replace(
            config,
            prior_campaign_anchors={first["run_id"]: first["campaign_manifest_sha256"]},
        )
        second = CampaignRunner(resumed, self._environment(resumed)).run()
        self.assertEqual(second["status"], "failed")
        self.assertEqual(config.ledger_path.read_bytes(), before)

    def test_bad_ping_fails_before_any_attempt_reservation(self) -> None:
        config = self._config()
        environment = self._environment(config)
        environment["FAKE_BAD_PING"] = "1"
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "failed")
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        self.assertEqual(ledger["campaign_reservations"], [])

    def test_tested_profile_version_mismatch_fails_before_daemon_or_reservation(
        self,
    ) -> None:
        config = self._config(scenario="claude-p-one-shot")
        assert config.tested_profile_path is not None
        profile, _identity = read_profile(config.tested_profile_path)
        write_file(
            config.tested_profile_path,
            canonical_json_bytes({**profile, "claude_version": "9.9.8"}) + b"\n",
        )
        with self.assertRaisesRegex(EvidenceError, "frozen Claude probe"):
            CampaignRunner(config, self._environment(config)).run()
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        self.assertEqual(ledger["campaign_reservations"], [])
        self.assertFalse(self.log.exists())

    def test_wrong_public_model_is_a_tracked_failed_attempt(self) -> None:
        config = self._config()
        environment = self._environment(config)
        environment["FAKE_RESULT_MODEL"] = "unapproved-model"
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "failed")
        attempt = next(config.evidence_root.glob("attempt-*"))
        outcome = json.loads((attempt / "outcome.json").read_text())
        self.assertEqual(outcome["status"], "failed")
        self.assertIsNone(outcome["observed_tokens_from_public_result"])
        self.assertIn("unauthorized model", outcome["error"])
        audit = audit_campaign(
            ledger_path=config.ledger_path,
            prefix=config.ledger_prefix,
            campaign_id=CAMPAIGN_ID,
            evidence_root=config.evidence_root,
        )
        self.assertEqual(audit["failed_attempt_ids"], [outcome["attempt_id"]])
        self.assertFalse(audit["promotion_eligible"])

    def test_pmux_start_failure_publishes_an_auditable_failed_attempt(self) -> None:
        config = self._config(scenario="persistent")
        environment = self._environment(config)
        environment["FAKE_START_FAIL"] = "1"
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "failed")
        self.assertIn("pmux start failed", result["error"])
        attempt = next(config.evidence_root.glob("attempt-*"))
        outcome = json.loads((attempt / "outcome.json").read_text())
        self.assertEqual(outcome["status"], "failed")
        self.assertEqual(outcome["error"], "pmux_start_failed")
        # The public command never ran, so there is no post-command source
        # check to report; the outcome records None for that fact and the
        # evidence root must still audit end to end.
        self.assertIsNone(outcome["post_command_source_check"])
        self.assertIsNone(outcome["observed_tokens_from_public_result"])
        audit = audit_campaign(
            ledger_path=config.ledger_path,
            prefix=config.ledger_prefix,
            campaign_id=CAMPAIGN_ID,
            evidence_root=config.evidence_root,
        )
        self.assertEqual(audit["failed_attempt_ids"], [outcome["attempt_id"]])
        self.assertFalse(audit["promotion_eligible"])
        self.assertEqual(audit["verdict"], "complete")
        # A resumed campaign must refuse for the real reason -- a failed prior
        # attempt -- not reject its own evidence as schema-invalid, and must
        # never append a reservation while refusing.
        before = config.ledger_path.read_bytes()
        resumed = dataclasses.replace(
            config,
            prior_campaign_anchors={
                result["run_id"]: result["campaign_manifest_sha256"]
            },
        )
        # Same environment as the first run: the campaign contract binds the
        # launch environment, and a changed environment is its own refusal.
        second = CampaignRunner(resumed, environment).run()
        self.assertEqual(second["status"], "failed")
        self.assertIn("prior campaign attempt failed", second["error"])
        self.assertEqual(config.ledger_path.read_bytes(), before)

    def test_persistent_generation_and_close_proof_are_exact(self) -> None:
        config = self._config(scenario="persistent")
        environment = self._environment(config)
        environment["FAKE_RESULT_GENERATION"] = ATTEMPT_ID
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "failed")
        outcome = json.loads(
            (next(config.evidence_root.glob("attempt-*")) / "outcome.json").read_text()
        )
        self.assertIn("different generation", outcome["error"])

    def test_close_requires_positive_process_reap_proof(self) -> None:
        config = self._config(scenario="persistent")
        environment = self._environment(config)
        environment["FAKE_CLOSE_NOT_REAPED"] = "1"
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["attempts"][0]["status"], "pmux_exit_zero")

    def test_sensitive_inherited_values_are_absent_from_all_evidence(self) -> None:
        config = self._config()
        environment = self._environment(config)
        secret = 'sëcret "value" /+'
        environment["PHASE0_SECRET_TOKEN"] = secret
        result = CampaignRunner(config, environment).run()
        self.assertEqual(result["status"], "acquired")
        forbidden = {
            secret.encode(),
            json.dumps(secret, ensure_ascii=False)[1:-1].encode(),
            json.dumps(secret, ensure_ascii=True)[1:-1].encode(),
        }
        for path in config.evidence_root.rglob("*"):
            if not path.is_file():
                continue
            payload = path.read_bytes()
            for value in forbidden:
                self.assertNotIn(value, payload, path)
        ledger_payload = config.ledger_path.read_bytes()
        for value in forbidden:
            self.assertNotIn(value, ledger_payload)

    def test_audit_accounts_for_orphan_and_unknown_artifact_residue(self) -> None:
        config = self._config()
        CampaignRunner(config, self._environment(config)).run()
        original_path = next(config.evidence_root.glob("attempt-*"))
        reservation = json.loads((original_path / "reservation.json").read_text())
        outcome = json.loads((original_path / "outcome.json").read_text())
        orphan_id = str(uuid.uuid4())
        orphan_path = config.evidence_root / f"attempt-{orphan_id}"
        orphan_reservation = json.loads(json.dumps(reservation))
        orphan_reservation["attempt_id"] = orphan_id
        orphan_reservation["artifact_directory"] = str(orphan_path)
        orphan_reservation.pop("reservation_sha256")
        orphan_reservation["reservation_sha256"] = sha256_bytes(
            canonical_json_bytes(orphan_reservation)
        )
        orphan_outcome = json.loads(json.dumps(outcome))
        orphan_outcome["attempt_id"] = orphan_id
        orphan_outcome["reservation_sha256"] = orphan_reservation["reservation_sha256"]
        writer = AtomicArtifactDirectory(config.evidence_root, f"attempt-{orphan_id}")
        writer.write_json("reservation.json", orphan_reservation)
        writer.write_json("outcome.json", orphan_outcome)
        writer.publish(
            status=orphan_outcome["status"],
            binding={
                "campaign_id": CAMPAIGN_ID,
                "run_id": orphan_reservation["run_id"],
                "attempt_id": orphan_id,
                "reservation_sha256": orphan_reservation["reservation_sha256"],
                "campaign_contract_sha256": orphan_reservation[
                    "campaign_contract_sha256"
                ],
                "source_digest": orphan_reservation["campaign_contract"]["candidate"][
                    "source"
                ]["digest"],
            },
        )
        unknown = AtomicArtifactDirectory(config.evidence_root, "unknown-evidence")
        unknown.write("opaque", b"bounded")
        unknown.publish(status="unknown", binding={})
        audit = audit_campaign(
            ledger_path=config.ledger_path,
            prefix=config.ledger_prefix,
            campaign_id=CAMPAIGN_ID,
            evidence_root=config.evidence_root,
        )
        self.assertEqual(audit["orphan_attempt_artifact_ids"], [orphan_id])
        self.assertEqual(audit["unknown_artifact_entries"], ["unknown-evidence"])
        self.assertFalse(audit["promotion_eligible"])

    def test_persistent_scenario_reserves_start_before_launch_and_each_warm_turn(
        self,
    ) -> None:
        config = self._config(scenario="persistent", prompt_count=2)
        result = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(result["status"], "acquired")
        self.assertEqual(result["attempt_count"], 2)
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        roles = [record["scenario_role"] for record in ledger["campaign_reservations"]]
        self.assertEqual(roles, ["fresh_persistent_start_and_turn", "warm_turn"])
        commands = [
            json.loads(line)["command"] for line in self.log.read_text().splitlines()
        ]
        self.assertIn("start", commands)
        self.assertEqual(commands.count("turn"), 2)
        self.assertIn("close", commands)

    def test_resume_uses_the_explicit_session_and_reserves_before_start(self) -> None:
        config = self._config(scenario="resume")
        result = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(result["status"], "acquired")
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        reservation = ledger["campaign_reservations"][0]
        self.assertEqual(reservation["session_id"], SESSION_ID)
        self.assertEqual(reservation["scenario_role"], "resume_start_and_turn")
        records = [json.loads(line) for line in self.log.read_text().splitlines()]
        start = next(record for record in records if record["command"] == "start")
        self.assertIn("--resume", start["argv"])
        self.assertEqual(start["argv"][start["argv"].index("--resume") + 1], SESSION_ID)

    def test_native_ndjson_requires_and_accepts_the_final_result_commit(self) -> None:
        config = dataclasses.replace(self._config(), output_format="ndjson")
        result = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(result["status"], "acquired")
        attempt = next(config.evidence_root.glob("attempt-*"))
        raw = (attempt / "pmux-run.stdout.ndjson").read_text()
        self.assertEqual(json.loads(raw)["type"], "result")

    def test_usage_ceiling_stops_before_later_reservation(self) -> None:
        config = dataclasses.replace(
            self._config(prompt_count=2), max_observed_tokens=10
        )
        result = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(result["status"], "failed")
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        self.assertEqual(len(ledger["campaign_reservations"]), 1)
        self.assertEqual(result["observed_tokens"], 10)
        audit = audit_campaign(
            ledger_path=config.ledger_path,
            prefix=config.ledger_prefix,
            campaign_id=CAMPAIGN_ID,
            evidence_root=config.evidence_root,
        )
        self.assertEqual(audit["accounting_verdict"], "complete")
        self.assertFalse(audit["promotion_eligible"])
        self.assertEqual(audit["failed_campaign_run_ids"], [result["run_id"]])

    def test_claude_p_is_a_bounded_native_facade_entrypoint(self) -> None:
        config = self._config(scenario="claude-p-one-shot")
        result = CampaignRunner(config, self._environment(config)).run()
        self.assertEqual(result["status"], "acquired")
        ledger = inspect_ledger(config.ledger_path, config.ledger_prefix, CAMPAIGN_ID)
        reservation = ledger["campaign_reservations"][0]
        self.assertEqual(reservation["public_entrypoint"], "claude-p")
        self.assertIn("claude-p", reservation["exercised_binaries"])
        self.assertNotIn("pmux", reservation["exercised_binaries"])
        records = [json.loads(line) for line in self.log.read_text().splitlines()]
        facade = next(record for record in records if record["command"] == "claude-p")
        self.assertEqual(
            facade["stdin_sha256"], sha256_bytes(config.prompt_paths[0].read_bytes())
        )


if __name__ == "__main__":
    unittest.main()
