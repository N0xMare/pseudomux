#!/usr/bin/env python3
"""Thin CLI publisher around the shared bounded-process evidence core."""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import re
import signal
import sys
from collections.abc import Sequence

import evidence

bounded_process = evidence.bounded_process


class BoundedRunnerError(RuntimeError):
    """A command or its evidence outputs violated the wrapper contract."""


def _environment(values: Sequence[str]) -> dict[str, str]:
    environment: dict[str, str] = {}
    for value in values:
        name, separator, content = value.partition("=")
        if (
            not separator
            or re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) is None
            or name in environment
            or "\0" in content
        ):
            raise BoundedRunnerError("command environment is not exact")
        environment[name] = content
    return environment


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cwd", required=True, type=pathlib.Path)
    parser.add_argument("--timeout-seconds", required=True, type=int)
    parser.add_argument("--drain-timeout-seconds", required=True, type=int)
    parser.add_argument("--maximum-output-bytes", required=True, type=int)
    parser.add_argument("--stdout", required=True, type=pathlib.Path)
    parser.add_argument("--stderr", required=True, type=pathlib.Path)
    parser.add_argument("--receipt", required=True, type=pathlib.Path)
    parser.add_argument("--description", required=True)
    parser.add_argument("--env", action="append", default=[])
    parser.add_argument("command", nargs=argparse.REMAINDER)
    return parser


def _canonical_absolute(path: pathlib.Path, description: str) -> pathlib.Path:
    if not path.is_absolute() or str(path) != os.path.normpath(str(path)):
        raise BoundedRunnerError(f"{description} is not canonical and absolute")
    return path


def _interrupted(signal_number: int, _frame: object) -> None:
    raise BoundedRunnerError(f"bounded runner interrupted by signal {signal_number}")


def run(arguments: Sequence[str] | None = None) -> int:
    options = _parser().parse_args(arguments)
    command = list(options.command)
    if command and command[0] == "--":
        command.pop(0)
    if not command:
        raise BoundedRunnerError("bounded runner requires one command")
    executable_path = _canonical_absolute(
        pathlib.Path(command[0]), "command executable"
    )
    cwd = _canonical_absolute(options.cwd, "command cwd")
    stdout_path = _canonical_absolute(options.stdout, "stdout spool")
    stderr_path = _canonical_absolute(options.stderr, "stderr spool")
    receipt_path = _canonical_absolute(options.receipt, "execution receipt")
    if len({stdout_path, stderr_path, receipt_path}) != 3:
        raise BoundedRunnerError("bounded runner output paths must be distinct")
    if not options.description or any(
        character in options.description for character in "\0\r\n"
    ):
        raise BoundedRunnerError("bounded runner description is invalid")

    environment = _environment(options.env)
    witness = bounded_process.bind_executable(executable_path)
    command[0] = witness.path
    forwarded_signals = (
        signal.SIGHUP,
        signal.SIGINT,
        signal.SIGQUIT,
        signal.SIGTERM,
    )
    previous_handlers = {
        signal_number: signal.signal(signal_number, _interrupted)
        for signal_number in forwarded_signals
    }
    failure = False
    try:
        with evidence.private_output_spool(stdout_path) as stdout_fd:
            with evidence.private_output_spool(stderr_path) as stderr_fd:
                try:
                    result = bounded_process.run(
                        witness,
                        command,
                        cwd=cwd,
                        environment=environment,
                        timeout_seconds=options.timeout_seconds,
                        drain_timeout_seconds=options.drain_timeout_seconds,
                        maximum_output_bytes=options.maximum_output_bytes,
                        description=options.description,
                        stdout_spool_fd=stdout_fd,
                        stderr_spool_fd=stderr_fd,
                    )
                except bounded_process.BoundedProcessFailure as error:
                    result = error.result
                    failure = True
    finally:
        for signal_number, previous in previous_handlers.items():
            signal.signal(signal_number, previous)

    if failure:
        receipt = bounded_process.validate_failure_receipt(result.receipt)
        rendered_receipt = bounded_process.dump_failure_receipt(receipt)
    else:
        receipt = bounded_process.validate_execution_receipt(result.receipt)
        rendered_receipt = bounded_process.dump_execution_receipt(receipt)
    stdout = evidence._stable_regular_bytes(
        stdout_path,
        description="bounded stdout spool",
        maximum_bytes=options.maximum_output_bytes,
    )
    stderr = evidence._stable_regular_bytes(
        stderr_path,
        description="bounded stderr spool",
        maximum_bytes=options.maximum_output_bytes,
    )
    if (
        stdout != result.stdout
        or stderr != result.stderr
        or len(stdout) != receipt["stdout_size"]
        or len(stderr) != receipt["stderr_size"]
        or hashlib.sha256(stdout).hexdigest() != receipt["stdout_sha256"]
        or hashlib.sha256(stderr).hexdigest() != receipt["stderr_sha256"]
    ):
        raise BoundedRunnerError("bounded output spools differ from their receipt")
    evidence.atomic_write_bytes(receipt_path, rendered_receipt)
    published_bytes = evidence._stable_regular_bytes(
        receipt_path,
        description="bounded process receipt",
        maximum_bytes=4 * 1024 * 1024,
    )
    published = (
        bounded_process.load_failure_receipt(published_bytes)
        if failure
        else bounded_process.load_execution_receipt(published_bytes)
    )
    if published != receipt:
        raise BoundedRunnerError("published execution receipt changed")
    print(receipt["receipt_sha256"], flush=True)
    if failure:
        return 124
    return result.exit_code if 0 <= result.exit_code <= 125 else 1


def main() -> int:
    try:
        return run()
    except (
        BoundedRunnerError,
        bounded_process.BoundedProcessError,
        evidence.EvidenceError,
        OSError,
    ) as error:
        print(f"bounded-runner: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
