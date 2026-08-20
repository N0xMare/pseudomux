#!/usr/bin/env python3
"""Measure what one pmux turn costs, per leg, against a zero-latency driver.

WHAT THIS ANSWERS
-----------------

`docs/path-b.md` §9 carries a latency row for Path A and for Path B, and two
earlier revisions of it disagreed (571 ms in §1, 535.5 ms in §10.1) with no
surviving argv, receipt or commit behind either. This tool is the missing
receipt: it states exactly what is timed, runs it many times, and publishes the
distribution rather than a point estimate.

WHAT IS TIMED
-------------

Two clocks, both recorded for every turn, because they answer different
questions and conflating them is how "the same quantity" came to have two
values:

* `server_total_ms` -- `TurnTimings.completed_at_ms - submitted_at_ms`, the
  daemon's own view of one turn, from the instant it accepted the prompt to the
  instant it committed the result. This is the quantity pmux's machinery owns
  and the one the §9 row means.
* `client_wall_ms` -- wall clock around one `pmux run` process,
  measured with `time.monotonic()` in this process. It includes process spawn,
  socket connect, the request and the response, so it is always the larger of
  the two and it is what an operator's shell actually waits.

`server_total_ms` is decomposed into the legs `TurnTimings` publishes.  The leg
table is DERIVED from the shape the daemon returned, not restated here: every
`*_at_ms` field observed is either a declared boundary or the run FAILS.  A
silent default is how a leg that grew to dominate a turn comes to be invisible
in a summary that still says "total".

PATH B IS MEASURED WITH ONE CLOCK, NOT TWO, AND THAT IS NOT AN OVERSIGHT
------------------------------------------------------------------------

`StatelessResult` (`crates/protocol/src/v1.rs`) carries no `timings` member: a
Path B caller names no session and is told nothing about the instance that
served it, which is the product (`bin/pmux/src/cli.rs`, `Command::Run`). So
Path B has `client_wall_ms` only, and any sentence comparing Path A with Path B
must compare `client_wall_ms` with `client_wall_ms`. Earlier drafts described
both as "MEASURED the same way"; only the client clock is the same way.

THE DRAIN THIS RUNS AT, AND WHY THE DOUBLE CANNOT BE GIVEN THE SHIPPED ONE
--------------------------------------------------------------------------

`graduated_drain_ms` (`crates/service/src/v1/backend.rs`) lowers a turn's
stability requirement to `TURN_DURATION_DRAIN_FLOOR_MS` (250 ms) once Claude's
in-band `turn_duration` marker has been seen. Real Claude 2.1.220 writes that
marker, so a real turn against the promoted profile owes 250 ms, not the
profile's 1000 ms. `pmux-test-claude` never writes one, so handing the double a
1000 ms profile would measure a 1000 ms drain no real turn ever pays. The
default here is therefore `--drain-ms 250`: the requirement a real turn owes,
expressed in the only way the double can owe it. Run it at other values to see
the drain's contribution -- that is one variable, and it is the interesting one.

USAGE
-----

    python3 tools/promotion/measure_turn_latency.py \\
        --release-dir target/release \\
        --claude target/release/pmux-test-claude \\
        --turns 60 --json

The sandbox is created owner-private under `/tmp`, the daemon is stopped and
the tree removed before this exits, and the receipt is the durable artifact.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import platform
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from typing import Any, Iterable

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "evidence_common"))

import portable_paths  # noqa: E402 -- tools/evidence_common, resolved above

SCHEMA_VERSION = 1
MEASUREMENT_ALGORITHM = "pmux-turn-latency-v1"

# Every executable this tool runs, and the flag that overrides it. Derived from
# the release directory by default, because a hand-written path is how a
# measurement comes to describe a binary nobody built.
REQUIRED_BINARIES = ("pmuxd", "pmux", "pmux-rmuxd", "pmux-launcher")

# The exact environment `pmux-test-claude` refuses to start without
# (`crates/e2e/src/bin/pmux-test-claude.rs::attest_environment`). It is
# delivered to the daemon AND to the client: the daemon's copy reaches a Path B
# pool instance, the client's copy reaches a Path A child through the caller
# snapshot. Real Claude reads nothing in the `PMUX_TEST_` namespace, so the same
# environment serves both lanes.
DOUBLE_ATTESTATION = {
    "TERM": "xterm-256color",
    "PMUX_TEST_ENV_ATTESTATION": "pmux-e2e-environment-v1",
    "PMUX_TEST_PATCH_ORDER": "set-wins-after-unset",
    "PMUX_TEST_SET_ONLY": "set-only-value",
    "PMUX_TEST_CALLER_SAFE_CONFIG": "caller-config-preserved",
}

# The boundaries `TurnTimings` publishes, in the order a turn crosses them, and
# what the leg that ENDS at each one measures. `submitted_at_ms` opens the turn
# and so names no leg.
#
# This table is checked against the shape the daemon actually returned. A
# `*_at_ms` field that is not here fails the run rather than being dropped from
# a total that still calls itself one.
TIMING_BOUNDARIES: tuple[tuple[str, str | None], ...] = (
    ("submitted_at_ms", None),
    (
        "prompt_acknowledged_at_ms",
        "the input gate: wait for a stable control render, type the bracketed "
        "paste, press Enter, and observe the prompt leave the composer",
    ),
    (
        "terminal_candidate_at_ms",
        "generation: the turn's final assistant row appears in the transcript",
    ),
    (
        "completed_at_ms",
        "the commit gate: screen stability (`quiet_for`) AND the transcript "
        "drain AND the confirming re-poll",
    ),
)
# Members of `TurnTimings` that are not boundaries. Each is reported raw and
# excluded from the leg arithmetic, with the reason it is not a leg.
NON_BOUNDARY_TIMINGS = {
    "drain_ms": "a duration, not an instant: how long the transcript had been "
    "unchanged when the commit gate passed",
    "last_transcript_activity_at_ms": "derived at commit as "
    "`completed_at_ms - drain_ms`; an anchor for the duration above, not a "
    "leg boundary",
    "stop_hook_at_ms": "the hybrid lifecycle hook's own instant; absent unless "
    "a Stop hook ran, and off the critical path when it did",
    # These two are the shipped answer to `docs/path-b.md` §13 item 6, which
    # asked for the sub-turn ARRIVAL order of `turn_duration` and said the scan
    # behind it read finished files. pmux already stamps both instants against
    # reads it was going to perform anyway, so the arrival question is answered
    # by running turns rather than by a new instrument. The double publishes
    # neither, which is why this tool refused a real turn until they were
    # classified -- exactly the intended failure.
    "turn_duration_observed_at_ms": "the instant the batch carrying Claude's "
    "`turn_duration` marker reached pmux's reader; pure measurement, read by "
    "nothing",
    "post_turn_duration_row_observed_at_ms": "the instant the first "
    "analysis-changing row that arrived STRICTLY AFTER the marker's batch "
    "reached pmux's reader; absent means nothing followed the marker",
}


# A compatibility cell's identity is spelled in Rust's `std::env::consts`
# vocabulary, not Python's. `platform.system()` says `Darwin` where pmux says
# `macos` and `platform.machine()` says `arm64` where pmux says `aarch64`, and a
# profile spelled the Python way is refused at daemon boot with
# `has no tested pmux compatibility profile`. The translation is a table with no
# default: an unmapped host FAILS rather than shipping a receipt whose identity
# does not name the machine it ran on.
RUST_OS = {"darwin": "macos", "linux": "linux"}
RUST_ARCH = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}


class MeasurementError(RuntimeError):
    """A precondition this tool refuses to work around."""


def host_identity() -> tuple[str, str]:
    system, machine = platform.system().lower(), platform.machine().lower()
    if system not in RUST_OS or machine not in RUST_ARCH:
        raise MeasurementError(
            f"host {system}/{machine} has no pmux compatibility spelling in this "
            "tool; add it to RUST_OS/RUST_ARCH rather than guessing"
        )
    return RUST_OS[system], RUST_ARCH[machine]


def percentile(sorted_values: list[float], fraction: float) -> float:
    """Nearest-rank percentile. No interpolation, so every value is observed."""

    if not sorted_values:
        raise MeasurementError("percentile of an empty sample")
    rank = max(1, min(len(sorted_values), int(-(-fraction * len(sorted_values) // 1))))
    return sorted_values[rank - 1]


def summarize(values: Iterable[float]) -> dict[str, Any]:
    ordered = sorted(float(value) for value in values)
    if not ordered:
        return {"count": 0}
    return {
        "count": len(ordered),
        "min_ms": ordered[0],
        "p10_ms": percentile(ordered, 0.10),
        "median_ms": percentile(ordered, 0.50),
        "p90_ms": percentile(ordered, 0.90),
        "p99_ms": percentile(ordered, 0.99),
        "max_ms": ordered[-1],
        "mean_ms": round(sum(ordered) / len(ordered), 1),
        "spread_ms": ordered[-1] - ordered[0],
    }


def resolve_binaries(release: pathlib.Path) -> dict[str, pathlib.Path]:
    resolved = {}
    for name in REQUIRED_BINARIES:
        path = release / name
        if not path.is_file() or not os.access(path, os.X_OK):
            raise MeasurementError(f"{path} is not an executable file")
        resolved[name] = path.resolve()
    return resolved


def claude_version(executable: pathlib.Path) -> str:
    done = subprocess.run(
        [str(executable), "--version"],
        check=True,
        capture_output=True,
        text=True,
        timeout=60,
    )
    first = done.stdout.strip().splitlines()
    if not first:
        raise MeasurementError(f"{executable} --version printed nothing")
    return first[0].split()[0]


class Sandbox:
    """An owner-private tree that holds every byte this run writes."""

    def __init__(self, driver: str) -> None:
        self.driver = driver
        self.root = pathlib.Path(
            tempfile.mkdtemp(prefix="qv4-turn-latency-", dir="/tmp")
        ).resolve()
        os.chmod(self.root, stat.S_IRWXU)
        for name in (
            "private",
            "state",
            "home",
            "pool",
            "path",
            "cwd",
            "config",
            "isolation",
        ):
            child = self.root / name
            child.mkdir()
            os.chmod(child, stat.S_IRWXU)
        self.socket = self.root / "pmux.sock"

    def environment(self) -> dict[str, str]:
        """The environment the daemon and the client are both started under.

        Two spellings, because the two drivers have opposite requirements and
        running either under the other's environment fails LOUDLY rather than
        quietly measuring the wrong thing:

        * `double` -- a hermetic tree plus the attestation values
          `pmux-test-claude` refuses to start without. Nothing of the operator's
          reaches the child.
        * `operator` -- the real environment, exactly as
          `pool_concurrency.rs::Lane::real` uses it. A real Claude instance
          authenticates from it; `docs/path-b.md` §2.1 records that an empty
          snapshot MEASURED `needs_login` on the first turn.
        """

        if self.driver == "operator":
            return {**os.environ, "TMPDIR": str(self.root)}
        return {
            "PATH": str(self.root / "path"),
            "PMUX_TEST_EXPECTED_PATH": str(self.root / "path"),
            "HOME": str(self.root / "home"),
            "TMPDIR": str(self.root),
            "CLAUDE_CONFIG_DIR": str(self.root / "config"),
            "PMUX_TEST_STATE_DIR": str(self.root / "state"),
            **DOUBLE_ATTESTATION,
        }

    def remove(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)


class Daemon:
    def __init__(
        self,
        binaries: dict[str, pathlib.Path],
        sandbox: Sandbox,
        profile: dict[str, Any],
        claude: pathlib.Path,
        warm: str | None,
    ) -> None:
        self.sandbox = sandbox
        self.log = (sandbox.root / "pmuxd.log").open("wb")
        argv = [
            str(binaries["pmuxd"]),
            "serve",
            "--socket",
            str(sandbox.socket),
            "--rmuxd",
            str(binaries["pmux-rmuxd"]),
            "--launcher",
            str(binaries["pmux-launcher"]),
            "--runtime-parent",
            str(sandbox.root / "private"),
            "--tested-claude-profile",
            json.dumps(profile, sort_keys=True),
            "--pool-parent",
            str(sandbox.root / "pool"),
            "--pool-claude",
            str(claude),
            "--pool-size",
            "1",
            "--pool-recycle-turns",
            "250",
            "--pool-idle-ttl-ms",
            "600000",
            "--pool-turn-timeout-ms",
            "120000",
        ]
        if warm:
            argv += ["--pool-warm", warm]
        self.argv = argv
        self.process = subprocess.Popen(
            argv,
            env=sandbox.environment(),
            stdin=subprocess.DEVNULL,
            stdout=self.log,
            stderr=self.log,
            start_new_session=True,
        )
        deadline = time.monotonic() + 180.0
        while time.monotonic() < deadline:
            if sandbox.socket.is_socket():
                return
            if self.process.poll() is not None:
                raise MeasurementError(
                    "pmuxd exited during startup:\n"
                    + (sandbox.root / "pmuxd.log").read_text(errors="replace")
                )
            time.sleep(0.05)
        raise MeasurementError("pmuxd never bound its socket")

    def stop(self) -> None:
        for number in (signal.SIGTERM, signal.SIGKILL):
            if self.process.poll() is not None:
                break
            try:
                os.killpg(os.getpgid(self.process.pid), number)
            except (ProcessLookupError, PermissionError):
                break
            try:
                self.process.wait(timeout=15)
                break
            except subprocess.TimeoutExpired:
                continue
        self.process.wait()
        self.log.close()


def run_client(
    binaries: dict[str, pathlib.Path],
    sandbox: Sandbox,
    arguments: list[str],
    timeout: float,
) -> tuple[dict[str, Any], float]:
    """One `pmux` invocation, timed by this process's monotonic clock."""

    argv = [
        str(binaries["pmux"]),
        "--socket",
        str(sandbox.socket),
        "--output",
        "json",
        *arguments,
    ]
    started = time.monotonic()
    done = subprocess.run(
        argv,
        env=sandbox.environment(),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    wall_ms = (time.monotonic() - started) * 1000.0
    if done.returncode != 0:
        raise MeasurementError(
            f"{' '.join(argv)} exited {done.returncode}\n{done.stdout}\n{done.stderr}"
        )
    payloads = [line for line in done.stdout.splitlines() if line.startswith("{")]
    if not payloads:
        raise MeasurementError(f"{' '.join(argv)} printed no JSON:\n{done.stdout}")
    return json.loads(payloads[-1]), wall_ms


def legs_of(timings: dict[str, Any]) -> dict[str, float]:
    """Split one turn into its legs, refusing an instant nobody classified."""

    declared = {name for name, _ in TIMING_BOUNDARIES}
    observed = set(timings)
    unclassified = observed - declared - set(NON_BOUNDARY_TIMINGS)
    if unclassified:
        raise MeasurementError(
            "TurnTimings carried fields this tool has never classified, so its "
            f"legs would not sum to its total: {sorted(unclassified)}"
        )
    missing = declared - observed
    if missing:
        raise MeasurementError(f"TurnTimings omitted a boundary: {sorted(missing)}")
    legs: dict[str, float] = {}
    ordered = [name for name, _ in TIMING_BOUNDARIES]
    for previous, current in zip(ordered, ordered[1:]):
        legs[current] = float(timings[current] - timings[previous])
    return legs


def measure_path_b(
    binaries: dict[str, pathlib.Path],
    sandbox: Sandbox,
    model: str,
    effort: str | None,
    turns: int,
    warmup: int,
) -> dict[str, Any]:
    samples: list[dict[str, Any]] = []
    for index in range(turns + warmup):
        arguments = ["run", "--model", model]
        if effort:
            arguments += ["--effort", effort]
        arguments.append(f"latency probe {uuid.uuid4()}")
        result, wall_ms = run_client(binaries, sandbox, arguments, timeout=180.0)
        if not result.get("text"):
            raise MeasurementError("`pmux run` returned no text")
        samples.append({"warmup": index < warmup, "client_wall_ms": round(wall_ms, 1)})
    return {"samples": samples}


def marker_arrival(measured: list[dict[str, Any]]) -> dict[str, Any]:
    """`docs/path-b.md` §13 item 6, answered in ARRIVAL order rather than in
    file order.

    `post_marker_row_at_ms` present on even one turn means an analysis-changing
    row REACHED pmux's reader after the batch that carried the marker, and the
    completion fast path would have dropped it. Absent on every turn means the
    marker was last, on this many turns, on this host, at this version.
    """

    with_marker = [s for s in measured if s["marker_observed_at_ms"] is not None]
    followed = [s for s in with_marker if s["post_marker_row_at_ms"] is not None]
    result: dict[str, Any] = {
        "turns": len(measured),
        "turns_with_a_turn_duration_marker": len(with_marker),
        "turns_where_a_row_arrived_after_the_marker": len(followed),
        "claim": (
            "no analysis-changing row arrived after the marker on any measured turn"
            if with_marker and not followed
            else "at least one row arrived after the marker; the completion "
            "fast path would have dropped it"
            if followed
            else "no turn carried a `turn_duration` marker, so this says nothing"
        ),
    }
    if with_marker:
        result["commit_after_marker_ms"] = summarize(
            float(s["completed_at_ms"] - s["marker_observed_at_ms"])
            for s in with_marker
        )
    if followed:
        result["row_after_marker_ms"] = summarize(
            float(s["post_marker_row_at_ms"] - s["marker_observed_at_ms"])
            for s in followed
        )
    return result


def distributions(samples: list[dict[str, Any]]) -> dict[str, Any]:
    measured = [sample for sample in samples if not sample["warmup"]]
    if not measured:
        raise MeasurementError("every sample was a warm-up")
    summary: dict[str, Any] = {
        "client_wall_ms": summarize(s["client_wall_ms"] for s in measured)
    }
    if "server_total_ms" in measured[0]:
        summary["server_total_ms"] = summarize(s["server_total_ms"] for s in measured)
        summary["legs"] = {
            name: {
                "measures": description,
                **summarize(s["legs"][name] for s in measured),
            }
            for name, description in TIMING_BOUNDARIES
            if description is not None
        }
        summary["turn_duration_arrival"] = marker_arrival(measured)
        summary["drain_ms"] = summarize(
            s["drain_ms"] for s in measured if s["drain_ms"] is not None
        )
    return summary


def build_receipt(args: argparse.Namespace, run: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "algorithm": MEASUREMENT_ALGORITHM,
        "measured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "host": {
            "os": platform.system().lower(),
            "arch": platform.machine(),
            "release": platform.release(),
            "platform": platform.platform(),
            "cpu_count": os.cpu_count(),
            "load_average_1m": os.getloadavg()[0],
        },
        "driver": {
            "environment": args.driver_environment,
            "claude_executable": str(args.claude),
            "claude_version": run["claude_version"],
            "zero_latency": run["claude_version"] == "9.9.9",
            "writes_turn_duration_marker": run["writes_turn_duration_marker"],
        },
        "configuration": {
            "compatibility_profile": run["profile"],
            "pmuxd_argv": run["pmuxd_argv"],
            "model": args.model,
            "effort": args.effort,
            "turns_measured": args.turns,
            "warmup_turns_discarded": args.warmup,
            "graduated_drain_note": (
                "`graduated_drain_ms(configured, turn_duration_seen)` lowers the "
                "requirement to 250 ms once Claude's marker is seen. The double "
                "never writes one, so `transcript_drain_ms` here IS the "
                "requirement, and 250 is the value that reproduces what a real "
                "2.1.220 turn owes against the promoted 1000 ms profile."
            ),
        },
        "path_a": run["path_a"],
        "path_b": run["path_b"],
        "what_would_invalidate_it": [
            "a different Claude executable, version, OS or arch: the identity is "
            "the key, exactly as it is for a compatibility profile",
            "a change to `quiet_for` (`crates/service/src/driver_io.rs`), which "
            "is what the commit-gate leg is currently made of",
            "a change to `TURN_DURATION_DRAIN_FLOOR_MS` or to "
            "`graduated_drain_ms` (`crates/service/src/v1/backend.rs`)",
            "a change to `wait_for_stable_control_render` or to the composer "
            "gate, which is what the input-gate leg is currently made of",
            "a new `TurnTimings` boundary: this tool FAILS on one rather than "
            "reporting a total that silently excludes it",
            "a loaded host: `load_average_1m` is recorded for exactly this "
            "reason, and a receipt taken above ~2 should be re-taken",
        ],
    }


def execute(args: argparse.Namespace) -> dict[str, Any]:
    binaries = resolve_binaries(args.release_dir)
    claude = args.claude.resolve(strict=True)
    version = claude_version(claude)
    host_os, host_arch = host_identity()
    profile = {
        "claude_version": version,
        "os": host_os,
        "arch": host_arch,
        "terminal_profile": "transparent",
        "input_transport": "sdk",
        "transcript_drain_ms": args.drain_ms,
    }
    sandbox = Sandbox(args.driver_environment)
    daemon: Daemon | None = None
    try:
        warm = f"{args.model}={1}" if args.effort is None else None
        if args.effort is not None:
            warm = f"{args.model}/{args.effort}=1"
        daemon = Daemon(binaries, sandbox, profile, claude, warm)
        path_b = measure_path_b(
            binaries, sandbox, args.model, args.effort, args.turns, args.warmup
        )
        run = {
            "claude_version": version,
            "writes_turn_duration_marker": False,
            "profile": profile,
            "pmuxd_argv": daemon.argv,
            "path_a": {
                "removed": True,
                "reason": "session CLI is not a product; this tool times only pmux run",
                "samples": [],
            },
            "path_b": {
                "summary": distributions(path_b["samples"]),
                "samples": path_b["samples"],
            },
        }
    finally:
        if daemon is not None:
            daemon.stop()
        if not args.keep_sandbox:
            sandbox.remove()
    return build_receipt(args, run)


def _parse(arguments: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir", type=pathlib.Path, required=True)
    parser.add_argument("--claude", type=pathlib.Path, required=True)
    parser.add_argument("--model", default="sonnet")
    parser.add_argument("--effort", default=None)
    parser.add_argument("--turns", type=int, default=60)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument(
        "--drain-ms",
        type=int,
        default=250,
        help="`transcript_drain_ms` of the operator profile the daemon is given",
    )
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--keep-sandbox", action="store_true")
    parser.add_argument(
        "--driver-environment",
        dest="driver_environment",
        choices=("double", "operator"),
        default="double",
        help="`double` for pmux-test-claude, `operator` for real Claude",
    )
    return parser.parse_args(arguments)


def main(arguments: list[str] | None = None) -> int:
    args = _parse(arguments)
    try:
        receipt = execute(args)
    except MeasurementError as error:
        print(f"measurement refused: {error}", file=sys.stderr)
        return 2
    # Rendered at the ONE point the receipt becomes bytes, and over the whole
    # document rather than over the fields that carry a path today. This
    # receipt records four release binaries, the Claude executable and a
    # sandbox root; a renderer that named those six fields would say nothing
    # about the seventh.
    encoded = json.dumps(
        portable_paths.render_document(receipt), indent=1, sort_keys=True
    )
    if args.output:
        args.output.write_text(encoded + "\n", encoding="utf-8")
    if args.json or not args.output:
        print(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
