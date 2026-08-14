#!/usr/bin/env python3
"""Dry-run-first evidence envelope for frozen pmux candidates."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

from phase0_lib import (
    DETACHED_GLOBAL_ATTEMPTS,
    PERMISSION_MODES,
    REQUIRED_RELEASE_BINARIES,
    BudgetExhausted,
    CampaignConfig,
    CampaignRunner,
    EvidenceError,
    LedgerPrefix,
    audit_campaign,
    compute_source_identity,
    dry_run_manifest,
    real_claude_turns_outside_the_ledger,
    summarize_attempt_ledger,
)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Acquire bounded promotion evidence through frozen pmux/pmuxd release "
            "binaries. Dry-run is the default; this tool never parses Claude transcripts "
            "or drives rmux directly."
        )
    )
    parser.add_argument("--version", action="version", version="pmux-phase0-envelope 1")
    subparsers = parser.add_subparsers(dest="command", required=True)

    source = subparsers.add_parser(
        "source-digest",
        help="print the canonical digest from tools/linux-docker/source_digest.py",
    )
    source.add_argument("--source-root", type=Path, required=True)
    source.set_defaults(handler=_run_source_digest)

    matrix = subparsers.add_parser(
        "matrix", help="print native evidence-envelope scenario/cell capabilities"
    )
    matrix.set_defaults(handler=_run_matrix)

    probe = subparsers.add_parser(
        "probe",
        help="print a no-write plan, or acquire evidence with every explicit live guard",
    )
    _add_probe_arguments(probe)
    probe.set_defaults(handler=_run_probe)

    audit = subparsers.add_parser(
        "audit",
        help="verify ledger prefix/hash chains and atomically published artifact hashes",
    )
    audit.add_argument("--ledger", type=Path, required=True)
    audit.add_argument("--ledger-prefix-records", type=int, required=True)
    audit.add_argument("--ledger-prefix-sha256", required=True)
    audit.add_argument("--prefix-last-global-attempt", type=int, required=True)
    audit.add_argument("--campaign-id", required=True)
    audit.add_argument("--evidence-root", type=Path, required=True)
    audit.add_argument(
        "--campaign-anchor",
        action="append",
        required=True,
        metavar="RUN_ID=SHA256",
        help="externally retained final artifact-manifest digest for every campaign run",
    )
    audit.set_defaults(handler=_run_audit)

    budget = subparsers.add_parser(
        "budget",
        help="recount the global real-Claude attempt budget from the ledger itself",
    )
    budget.add_argument("--ledger", type=Path, required=True)
    budget.add_argument(
        "--detached",
        type=int,
        default=DETACHED_GLOBAL_ATTEMPTS,
        help=(
            "attempts consumed outside the ledger; defaults to the four detached "
            "reservations of 2026-07-28 described in evidence/README.md"
        ),
    )
    budget.add_argument(
        "--evidence-dir",
        type=Path,
        default=None,
        help=(
            "scan this directory's committed receipts for real Claude turns "
            "that reserved no ordinal, and report the shortfall beside the "
            "budget. Defaults to the `evidence/` directory beside the ledger"
        ),
    )
    budget.set_defaults(handler=_run_budget)
    return parser


def _add_probe_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--expected-source-digest", required=True)
    parser.add_argument("--release-bin-dir", type=Path, required=True)
    parser.add_argument(
        "--binary-sha256",
        action="append",
        default=[],
        metavar="NAME=SHA256",
        help=(
            "frozen digest; repeat exactly for pmux, pmuxd, pmux-mcp, claude-p, "
            "pmux-rmuxd, pmux-launcher, and pmux-hook"
        ),
    )
    parser.add_argument("--claude-bin", type=Path, required=True)
    parser.add_argument("--claude-sha256", required=True)
    parser.add_argument("--cwd", type=Path, required=True)
    parser.add_argument("--prompt-file", type=Path, action="append", required=True)
    parser.add_argument("--evidence-root", type=Path, required=True)
    parser.add_argument("--ledger", type=Path, required=True)
    parser.add_argument("--ledger-prefix-records", type=int, required=True)
    parser.add_argument("--ledger-prefix-sha256", required=True)
    parser.add_argument("--prefix-last-global-attempt", type=int, required=True)
    parser.add_argument(
        "--prior-campaign-anchor",
        action="append",
        default=[],
        metavar="RUN_ID=SHA256",
        help="externally retained final manifest for each prior run of this campaign",
    )
    parser.add_argument("--campaign-id", required=True)
    parser.add_argument(
        "--global-attempt-ceiling",
        type=int,
        required=True,
        help="explicit approved global ceiling; must be in 60..100",
    )
    parser.add_argument("--max-attempts-this-run", type=int, required=True)
    parser.add_argument(
        "--max-observed-tokens",
        type=int,
        required=True,
        help=(
            "stop before a later attempt once pmux public results reach this observed total; "
            "not a provider-side cap"
        ),
    )
    parser.add_argument(
        "--scenario",
        choices=("one-shot", "persistent", "resume", "claude-p-one-shot"),
        default="one-shot",
    )
    parser.add_argument("--resume-session-id")
    parser.add_argument("--model", required=True)
    parser.add_argument(
        "--allowed-model-id",
        action="append",
        required=True,
        help=(
            "exact public TurnResult model ID authorized for this campaign; "
            "repeat only for explicitly approved IDs"
        ),
    )
    parser.add_argument("--effort", choices=("low", "medium"), required=True)
    parser.add_argument("--output-format", choices=("json", "ndjson"), default="json")
    parser.add_argument(
        "--compatibility",
        choices=("require-tested", "allow-untested"),
        default="require-tested",
    )
    parser.add_argument("--tested-profile-file", type=Path)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument(
        "--terminal-profile", choices=("transparent",), default="transparent"
    )
    parser.add_argument("--input-transport", choices=("auto", "sdk"), default="sdk")
    parser.add_argument(
        "--lifecycle", choices=("transcript", "hybrid"), default="transcript"
    )
    parser.add_argument(
        "--permission-mode",
        choices=PERMISSION_MODES,
        help=(
            "forwarded to the pmux CLI verbatim; the same seven PermissionArg "
            "values as bin/pmux/src/cli.rs. dangerously-skip-permissions makes "
            "every turn republish the dangerous_permission_bypass warning and is "
            "rejected for --scenario claude-p-one-shot"
        ),
    )
    parser.add_argument(
        "--env",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help=(
            "repeatable. VALUE is a secret: it is delivered through pmux's "
            "name-only --env-passthrough channel rather than argv, only KEY is "
            "written to an evidence artifact, and every VALUE joins the "
            "redaction set"
        ),
    )
    parser.add_argument(
        "--env-passthrough",
        action="append",
        default=[],
        metavar="KEY",
        help="forwarded as pmux --env-passthrough; repeatable, name only",
    )
    parser.add_argument(
        "--denied-tool",
        dest="denied_tools",
        action="append",
        default=[],
        metavar="PATTERN",
        help=(
            "repeatable, order preserved. One ClaudeLaunchConfig::denied_tools "
            "element, forwarded as pmux --denied-tool (the facade spells the "
            "same field --disallowedTools). '*' empties builtins AND MCP. A "
            "comma is rejected: pmux splits on it and the facade does not, so "
            "one value would mean two denied tools on one entrypoint only"
        ),
    )
    parser.add_argument(
        "--system-prompt-file",
        type=Path,
        metavar="PATH",
        help=(
            "absolute owner-only (0600) UTF-8 text document whose contents "
            "REPLACE Claude's system prompt. Only the file's identity is "
            "recorded, never its text -- but unlike --env there is no name-only "
            "channel for it, so the text does reach the launched argv; a "
            "credential-shaped document is refused before anything launches"
        ),
    )
    parser.add_argument(
        "--agent",
        metavar="NAME",
        help="client-side agent profile name; requires --agent-file",
    )
    parser.add_argument(
        "--agent-file",
        type=Path,
        metavar="PATH",
        help="absolute agent profile document; required with --agent",
    )
    parser.add_argument("--untested-transcript-drain-ms", type=int, default=2_000)
    parser.add_argument("--turn-timeout-seconds", type=int, default=300)
    parser.add_argument("--daemon-ready-timeout-seconds", type=int, default=30)
    parser.add_argument("--daemon-shutdown-timeout-seconds", type=int, default=15)
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--acknowledge-claude-usage", action="store_true")
    parser.add_argument("--acknowledge-untested-compatibility", action="store_true")


def _parse_binary_hashes(values: list[str]) -> dict[str, str]:
    parsed: dict[str, str] = {}
    for value in values:
        name, separator, digest = value.partition("=")
        if not separator or not name or not digest:
            raise EvidenceError("--binary-sha256 must use NAME=SHA256")
        if name in parsed:
            raise EvidenceError(f"duplicate binary digest for {name}")
        parsed[name] = digest.lower()
    if set(parsed) != set(REQUIRED_RELEASE_BINARIES):
        raise EvidenceError(
            "--binary-sha256 must be repeated exactly for "
            + ", ".join(REQUIRED_RELEASE_BINARIES)
        )
    return parsed


def _parse_campaign_anchors(values: list[str], *, option: str) -> dict[str, str]:
    parsed: dict[str, str] = {}
    for value in values:
        run_id, separator, digest = value.partition("=")
        if not separator or not run_id or not digest:
            raise EvidenceError(f"{option} must use RUN_ID=SHA256")
        run_id = run_id.lower()
        if run_id in parsed:
            raise EvidenceError(f"duplicate {option} run ID {run_id}")
        parsed[run_id] = digest.lower()
    return parsed


def _parse_environment_set(values: list[str]) -> dict[str, str]:
    """Parse --env KEY=VALUE. The value is a secret and is never recorded."""

    parsed: dict[str, str] = {}
    for value in values:
        name, separator, payload = value.partition("=")
        if not separator or not name or not payload:
            raise EvidenceError("--env must use KEY=VALUE")
        if name in parsed:
            raise EvidenceError(f"duplicate --env name {name}")
        parsed[name] = payload
    return parsed


def _config(arguments: argparse.Namespace) -> CampaignConfig:
    return CampaignConfig(
        source_root=arguments.source_root,
        expected_source_digest=arguments.expected_source_digest.lower(),
        release_bin_dir=arguments.release_bin_dir,
        expected_binary_hashes=_parse_binary_hashes(arguments.binary_sha256),
        claude_bin=arguments.claude_bin,
        expected_claude_sha256=arguments.claude_sha256.lower(),
        cwd=arguments.cwd,
        prompt_paths=tuple(arguments.prompt_file),
        evidence_root=arguments.evidence_root,
        ledger_path=arguments.ledger,
        ledger_prefix=LedgerPrefix(
            records=arguments.ledger_prefix_records,
            sha256=arguments.ledger_prefix_sha256.lower(),
            last_global_attempt=arguments.prefix_last_global_attempt,
        ),
        prior_campaign_anchors=_parse_campaign_anchors(
            arguments.prior_campaign_anchor,
            option="--prior-campaign-anchor",
        ),
        campaign_id=arguments.campaign_id.lower(),
        global_attempt_ceiling=arguments.global_attempt_ceiling,
        max_attempts_this_run=arguments.max_attempts_this_run,
        max_observed_tokens=arguments.max_observed_tokens,
        scenario=arguments.scenario,
        resume_session_id=(
            arguments.resume_session_id.lower() if arguments.resume_session_id else None
        ),
        model=arguments.model,
        allowed_model_ids=tuple(arguments.allowed_model_id),
        effort=arguments.effort,
        output_format=arguments.output_format,
        compatibility=arguments.compatibility,
        tested_profile_path=arguments.tested_profile_file,
        terminal_rows=arguments.rows,
        terminal_cols=arguments.cols,
        terminal_profile=arguments.terminal_profile,
        input_transport=arguments.input_transport,
        lifecycle=arguments.lifecycle,
        untested_transcript_drain_ms=arguments.untested_transcript_drain_ms,
        turn_timeout_seconds=arguments.turn_timeout_seconds,
        daemon_ready_timeout_seconds=arguments.daemon_ready_timeout_seconds,
        daemon_shutdown_timeout_seconds=arguments.daemon_shutdown_timeout_seconds,
        live=arguments.live,
        acknowledge_usage=arguments.acknowledge_claude_usage,
        acknowledge_untested=arguments.acknowledge_untested_compatibility,
        permission_mode=arguments.permission_mode,
        environment_set=_parse_environment_set(arguments.env),
        environment_passthrough_names=tuple(arguments.env_passthrough),
        agent_name=arguments.agent,
        agent_file=arguments.agent_file,
        denied_tools=tuple(arguments.denied_tools),
        system_prompt_file=arguments.system_prompt_file,
    )


def _print_json(value: Any) -> None:
    print(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False, allow_nan=False)
    )


def _run_source_digest(arguments: argparse.Namespace) -> int:
    _print_json(compute_source_identity(arguments.source_root))
    return 0


def _run_matrix(_: argparse.Namespace) -> int:
    _print_json(
        {
            "schema_version": 1,
            "authority": "native pmux public surface",
            "scenarios": {
                "one-shot": "one reserved fresh pmux run per prompt file",
                "persistent": "one reserved start+turn, then reserved same-process warm turns",
                "resume": "one reserved explicit resume+turn, then reserved warm turns",
                "claude-p-one-shot": (
                    "one reserved call per prompt through the bounded native facade"
                ),
            },
            "terminal_profiles": ["transparent"],
            "input_transports": ["sdk", "auto"],
            "lifecycle_modes": ["transcript", "hybrid"],
            "efforts": ["low", "medium"],
            "unsupported_by_envelope": [
                "direct rmux control",
                "direct PTY input",
                "terminal-screen classification",
                "Claude transcript parsing",
                "rmux_standard",
                "attached_stream",
            ],
        }
    )
    return 0


def _run_probe(arguments: argparse.Namespace) -> int:
    config = _config(arguments)
    if not config.live:
        _print_json(dry_run_manifest(config))
        return 0
    old_umask = os.umask(0o077)
    try:
        result = CampaignRunner(config, os.environ).run()
    finally:
        os.umask(old_umask)
    # `CampaignRunner.run()` never re-raises: it catches everything into
    # `error` and returns a summary. Projecting nine hand-listed keys without
    # `error` left the operator of a run that just spent irreplaceable ordinals
    # with `"status": "failed"` and nothing else, which reads as "pmux failed"
    # even when what actually fired was a harness-side fence. The per-attempt
    # rows say which ordinal reached which verdict, and the error also goes to
    # stderr so it survives `phase0.py probe --live | jq`.
    _print_json(
        {
            "campaign_id": result["campaign_id"],
            "run_id": result["run_id"],
            "status": result["status"],
            "error": result.get("error"),
            "attempt_count": result["attempt_count"],
            "attempts": [
                {
                    "global_attempt_ordinal": attempt.get("global_attempt_ordinal"),
                    "attempt_id": attempt.get("attempt_id"),
                    "status": attempt.get("status"),
                    "error": attempt.get("error"),
                }
                for attempt in result.get("attempts") or ()
            ],
            "observed_tokens": result["observed_tokens"],
            "cleanup": result["cleanup"],
            "evidence_directory": result["evidence_directory"],
            "campaign_manifest_sha256": result["campaign_manifest_sha256"],
            "drain_calibration": result["drain_calibration"],
        }
    )
    if result["status"] != "acquired":
        print(
            f"campaign status={result['status']} error={result.get('error')!r}",
            file=sys.stderr,
        )
        for attempt in result.get("attempts") or ():
            if attempt.get("status") != "pmux_exit_zero":
                print(
                    f"  attempt ordinal={attempt.get('global_attempt_ordinal')} "
                    f"id={attempt.get('attempt_id')} "
                    f"status={attempt.get('status')} "
                    f"error={attempt.get('error')!r}",
                    file=sys.stderr,
                )
        return 1
    return 0


def _run_audit(arguments: argparse.Namespace) -> int:
    result = audit_campaign(
        ledger_path=arguments.ledger,
        prefix=LedgerPrefix(
            records=arguments.ledger_prefix_records,
            sha256=arguments.ledger_prefix_sha256.lower(),
            last_global_attempt=arguments.prefix_last_global_attempt,
        ),
        campaign_id=arguments.campaign_id.lower(),
        evidence_root=arguments.evidence_root,
        expected_campaign_anchors=_parse_campaign_anchors(
            arguments.campaign_anchor,
            option="--campaign-anchor",
        ),
    )
    _print_json(result)
    return 0 if result["promotion_eligible"] else 1


def _run_budget(arguments: argparse.Namespace) -> int:
    report = dict(
        summarize_attempt_ledger(arguments.ledger, detached=arguments.detached)
    )
    # The ledger under-reports real exposure, and used to say it did not. The
    # count is reported BESIDE `consumed` and never folded into it: `remaining`
    # stays the number the reservation guard enforces, so nothing here silently
    # re-prices decision D4.
    evidence_dir = arguments.evidence_dir or arguments.ledger.parent
    if evidence_dir.is_dir():
        report["real_turns_outside_the_ledger"] = real_claude_turns_outside_the_ledger(
            evidence_dir
        )
    _print_json(report)
    return 0


def main(argv: list[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        return int(arguments.handler(arguments))
    except BudgetExhausted as error:
        print(f"phase0: {error}", file=sys.stderr)
        return 75
    except (EvidenceError, FileNotFoundError, NotADirectoryError) as error:
        print(f"phase0: {error}", file=sys.stderr)
        return 64


if __name__ == "__main__":
    raise SystemExit(main())
