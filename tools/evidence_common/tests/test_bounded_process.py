from __future__ import annotations

import copy
import dataclasses
import fcntl
import hashlib
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

TOOLS = pathlib.Path(__file__).resolve().parents[2]
COMMON = TOOLS / "evidence_common"
sys.path.insert(0, str(COMMON))

import bounded_process  # noqa: E402


class BoundedProcessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.python = bounded_process.bind_executable(
            pathlib.Path(sys.executable).resolve()
        )

    def environment(self) -> dict[str, str]:
        return {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}

    def run_python(
        self,
        cwd: pathlib.Path,
        script: str,
        *arguments: str,
        timeout_seconds: int = 5,
        drain_timeout_seconds: int = 1,
        maximum_output_bytes: int = 4096,
        stdout_spool_fd: int | None = None,
        stderr_spool_fd: int | None = None,
        stdin_bytes: bytes | None = None,
        stdin_fd: int | None = None,
    ) -> bounded_process.RunResult:
        return bounded_process.run(
            self.python,
            [self.python.path, "-c", script, *arguments],
            cwd=cwd,
            environment=self.environment(),
            timeout_seconds=timeout_seconds,
            drain_timeout_seconds=drain_timeout_seconds,
            maximum_output_bytes=maximum_output_bytes,
            stdout_spool_fd=stdout_spool_fd,
            stderr_spool_fd=stderr_spool_fd,
            stdin_bytes=stdin_bytes,
            stdin_fd=stdin_fd,
        )

    def assert_pid_gone(self, pid: int) -> None:
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.05)
        self.fail(f"bounded process left PID {pid} alive")

    def rehash_receipt(self, receipt: dict[str, object]) -> dict[str, object]:
        receipt["process_ledger_sha256"] = bounded_process._canonical_json_sha256(
            receipt["process_ledger"],
            domain=bounded_process.PROCESS_LEDGER_DOMAIN,
        )
        body = dict(receipt)
        del body["receipt_sha256"]
        domain = (
            bounded_process.EXECUTION_RECEIPT_DOMAIN
            if receipt["kind"] == "pmux_bounded_process"
            else bounded_process.FAILURE_RECEIPT_DOMAIN
        )
        receipt["receipt_sha256"] = bounded_process._canonical_json_sha256(
            body, domain=domain
        )
        return receipt

    def test_fast_native_exit_always_has_exact_nonempty_receipt(self) -> None:
        executable = bounded_process.bind_executable(
            pathlib.Path(shutil.which("true") or "/usr/bin/true").resolve()
        )
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            for repetition in range(120):
                with self.subTest(repetition=repetition):
                    result = bounded_process.run(
                        executable,
                        [executable.path],
                        cwd=cwd,
                        environment=self.environment(),
                        timeout_seconds=5,
                        drain_timeout_seconds=1,
                        maximum_output_bytes=1024,
                    )
                    self.assertEqual(result.exit_code, 0)
                    self.assertEqual(len(result.process_ledger), 1)
                    record = result.process_ledger[0]
                    self.assertEqual(
                        frozenset(record), bounded_process.PROCESS_LEDGER_KEYS
                    )
                    self.assertIs(type(record["sid"]), int)
                    self.assertRegex(
                        str(record["ownership_marker_sha256"]), r"^[0-9a-f]{64}$"
                    )

    def test_stdout_stderr_and_private_spools_are_exactly_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            stdout_path = cwd / "stdout.log"
            stderr_path = cwd / "stderr.log"
            stdout_fd = os.open(stdout_path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
            stderr_fd = os.open(stderr_path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
            try:
                result = self.run_python(
                    cwd,
                    "import sys; sys.stdout.buffer.write(b'out'); "
                    "sys.stderr.buffer.write(b'err')",
                    stdout_spool_fd=stdout_fd,
                    stderr_spool_fd=stderr_fd,
                )
            finally:
                os.close(stdout_fd)
                os.close(stderr_fd)
            self.assertEqual(result.stdout, b"out")
            self.assertEqual(result.stderr, b"err")
            self.assertEqual(stdout_path.read_bytes(), b"out")
            self.assertEqual(stderr_path.read_bytes(), b"err")

            overflow_path = cwd / "overflow.log"
            overflow_fd = os.open(
                overflow_path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600
            )
            try:
                with self.assertRaisesRegex(
                    bounded_process.BoundedProcessFailure, "bounded output"
                ) as caught:
                    self.run_python(
                        cwd,
                        "import sys; sys.stdout.buffer.write(b'x'*4096); "
                        "sys.stdout.flush()",
                        maximum_output_bytes=128,
                        stdout_spool_fd=overflow_fd,
                    )
            finally:
                os.close(overflow_fd)
            self.assertEqual(len(overflow_path.read_bytes()), 128)
            failure = caught.exception.result
            self.assertEqual(failure.reason, "output_limit")
            self.assertTrue(failure.cleanup_complete)
            self.assertFalse(failure.output_complete)
            self.assertEqual(failure.stdout, b"x" * 128)
            self.assertEqual(failure.receipt["output_limit_stream"], "stdout")

    def test_leader_exit_with_pipe_holder_hits_independent_drain_deadline(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            child_pid = cwd / "child-pid"
            script = (
                "import subprocess,sys; "
                "child=subprocess.Popen([sys.executable,'-c',"
                "'import time; time.sleep(30)']); "
                "open(sys.argv[1],'w').write(str(child.pid))"
            )
            started = time.monotonic()
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "I/O drain timed out"
            ) as caught:
                self.run_python(
                    cwd,
                    script,
                    str(child_pid),
                    timeout_seconds=10,
                    drain_timeout_seconds=1,
                )
            self.assertLess(time.monotonic() - started, 5)
            self.assert_pid_gone(int(child_pid.read_text(encoding="utf-8")))
            failure = caught.exception.result
            self.assertEqual(failure.reason, "drain_timeout")
            self.assertTrue(failure.cleanup_complete)
            self.assertTrue(failure.output_complete)

    def test_equal_lifetime_and_drain_bounds_expire_as_timeout_not_drain(self) -> None:
        # `drain_deadline` is clamped to the lifetime deadline, so when the two
        # configured bounds are equal the drain bound is never the binding one:
        # the command consumed its entire lifetime envelope and the receipt must
        # say "timeout" (phase0 derives `timed_out` from exactly that string).
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            child_pid = cwd / "child-pid"
            script = (
                "import subprocess,sys; "
                "child=subprocess.Popen([sys.executable,'-c',"
                "'import time; time.sleep(30)']); "
                "open(sys.argv[1],'w').write(str(child.pid))"
            )
            started = time.monotonic()
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "timed out"
            ) as caught:
                self.run_python(
                    cwd,
                    script,
                    str(child_pid),
                    timeout_seconds=2,
                    drain_timeout_seconds=2,
                )
            elapsed = time.monotonic() - started
            self.assert_pid_gone(int(child_pid.read_text(encoding="utf-8")))
            failure = caught.exception.result
            self.assertEqual(failure.reason, "timeout")
            self.assertEqual(failure.receipt["failure_reason"], "timeout")
            self.assertNotIn("I/O drain timed out", str(caught.exception))
            self.assertGreaterEqual(elapsed, 1.5)
            self.assertLess(elapsed, 8)
            self.assertTrue(failure.cleanup_complete)
            self.assertTrue(failure.output_complete)

    def test_drain_bound_strictly_inside_lifetime_still_reports_drain_timeout(
        self,
    ) -> None:
        # The converse of the equal-bounds case: a leader that exits early under
        # a wide lifetime envelope makes the drain bound genuinely binding, and
        # the expiry must land at the drain bound with the drain reason.
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            child_pid = cwd / "child-pid"
            script = (
                "import subprocess,sys,time; "
                "child=subprocess.Popen([sys.executable,'-c',"
                "'import time; time.sleep(30)']); "
                "open(sys.argv[1],'w').write(str(child.pid)); "
                "time.sleep(0.2)"
            )
            started = time.monotonic()
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "I/O drain timed out"
            ) as caught:
                self.run_python(
                    cwd,
                    script,
                    str(child_pid),
                    timeout_seconds=60,
                    drain_timeout_seconds=1,
                )
            elapsed = time.monotonic() - started
            self.assert_pid_gone(int(child_pid.read_text(encoding="utf-8")))
            failure = caught.exception.result
            self.assertEqual(failure.reason, "drain_timeout")
            self.assertEqual(failure.receipt["failure_reason"], "drain_timeout")
            self.assertGreaterEqual(elapsed, 1.0)
            self.assertLess(elapsed, 10)
            self.assertTrue(failure.cleanup_complete)
            self.assertTrue(failure.output_complete)

    def test_descendant_closes_pipes_ignores_term_and_is_killed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            child_pid = cwd / "child-pid"
            script = (
                "import os,subprocess,sys; "
                "child=subprocess.Popen([sys.executable,'-c',"
                "'import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                "os.close(0); os.close(1); os.close(2); time.sleep(30)'],"
                "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,"
                "stderr=subprocess.DEVNULL); "
                "open(sys.argv[1],'w').write(str(child.pid))"
            )
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "owned descendant"
            ) as caught:
                self.run_python(cwd, script, str(child_pid))
            self.assert_pid_gone(int(child_pid.read_text(encoding="utf-8")))
            failure = caught.exception.result
            self.assertEqual(failure.reason, "descendant_survived")
            self.assertTrue(failure.cleanup_complete)
            self.assertGreaterEqual(len(failure.process_ledger), 2)

    def test_immediate_double_fork_setsid_escape_is_reaped_on_all_paths(self) -> None:
        for behavior in ("success", "timeout", "output"):
            with (
                self.subTest(behavior=behavior),
                tempfile.TemporaryDirectory() as temporary,
            ):
                cwd = pathlib.Path(temporary).resolve()
                child_pid = cwd / "escaped-pid"
                escaped = (
                    "import os,sys,time; first=os.fork(); "
                    "(os._exit(0) if first else None); os.setsid(); second=os.fork(); "
                    "(os._exit(0) if second else None); os.close(0); os.close(1); "
                    "os.close(2); open(sys.argv[1],'w').write(str(os.getpid())); "
                    "time.sleep(30)"
                )
                trailer = {
                    "success": "raise SystemExit(0)",
                    "timeout": "time.sleep(30)",
                    "output": "sys.stdout.buffer.write(b'x'*4096); sys.stdout.flush(); time.sleep(30)",
                }[behavior]
                leader = (
                    "import os,sys,time; child=os.fork(); "
                    "(os.execv(sys.executable,[sys.executable,'-c',sys.argv[2],sys.argv[1]]) "
                    "if child==0 else None); deadline=time.monotonic()+2; "
                    "\nwhile not os.path.exists(sys.argv[1]) and time.monotonic()<deadline: time.sleep(0.01)\n"
                    f"{trailer}"
                )
                expected = {
                    "success": "owned descendant",
                    "timeout": "timed out",
                    "output": "bounded output",
                }[behavior]
                with self.assertRaisesRegex(
                    bounded_process.BoundedProcessError, expected
                ):
                    self.run_python(
                        cwd,
                        leader,
                        str(child_pid),
                        escaped,
                        timeout_seconds=1 if behavior == "timeout" else 5,
                        maximum_output_bytes=128 if behavior == "output" else 4096,
                    )
                self.assertTrue(child_pid.is_file())
                self.assert_pid_gone(int(child_pid.read_text(encoding="utf-8")))

    def test_unrelated_same_uid_process_start_is_untouched(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            ready = cwd / "ready"
            trigger = cwd / "trigger"
            child_pid = cwd / "unrelated-pid"
            helper = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    "import pathlib,subprocess,sys,time; "
                    "ready,trigger,pidfile=map(pathlib.Path,sys.argv[1:]); ready.touch(); "
                    "\nwhile not trigger.exists(): time.sleep(0.01)\n"
                    "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'],"
                    "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,"
                    "stderr=subprocess.DEVNULL); pidfile.write_text(str(child.pid)); "
                    "time.sleep(30)",
                    str(ready),
                    str(trigger),
                    str(child_pid),
                ],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                deadline = time.monotonic() + 2
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists())
                result = self.run_python(
                    cwd,
                    "import pathlib,sys,time; pathlib.Path(sys.argv[1]).touch(); "
                    "time.sleep(.4)",
                    str(trigger),
                )
                self.assertEqual(result.exit_code, 0)
                self.assertIsNone(helper.poll())
                unrelated_pid = int(child_pid.read_text(encoding="utf-8"))
                os.kill(unrelated_pid, 0)
            finally:
                if child_pid.is_file():
                    try:
                        os.kill(
                            int(child_pid.read_text(encoding="utf-8")), signal.SIGKILL
                        )
                    except ProcessLookupError:
                        pass
                helper.terminate()
                helper.wait(timeout=5)

    def test_direct_script_is_rejected_but_explicit_interpreter_passes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            script = cwd / "script.py"
            marker = cwd / "marker"
            script.write_text(
                "#!/usr/bin/env python3\nimport pathlib,sys\npathlib.Path(sys.argv[1]).touch()\n",
                encoding="utf-8",
            )
            script.chmod(0o700)
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "direct script execution"
            ):
                bounded_process.bind_executable(script)
            result = bounded_process.run(
                self.python,
                [self.python.path, str(script), str(marker)],
                cwd=cwd,
                environment=self.environment(),
                timeout_seconds=5,
                drain_timeout_seconds=1,
                maximum_output_bytes=4096,
            )
            self.assertEqual(result.exit_code, 0)
            self.assertTrue(marker.is_file())

    def test_missing_live_root_fails_before_spawn(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            marker = cwd / "spawned"
            with (
                mock.patch.object(
                    bounded_process,
                    "_snapshot",
                    side_effect=bounded_process.BoundedProcessError(
                        "live process root is absent"
                    ),
                ),
                self.assertRaisesRegex(
                    bounded_process.BoundedProcessError, "live process root"
                ),
            ):
                self.run_python(
                    cwd,
                    "import pathlib,sys; pathlib.Path(sys.argv[1]).touch()",
                    str(marker),
                )
            self.assertFalse(marker.exists())

    def test_pid_reuse_and_exact_ledger_types_fail_closed(self) -> None:
        original = bounded_process.ProcessIdentity(7, 1, 7, 7, "a", "tool")
        reused = bounded_process.ProcessIdentity(7, 1, 7, 7, "b", "tool")
        self.assertFalse(bounded_process._same_process(original, reused))
        with self.assertRaisesRegex(
            bounded_process.BoundedProcessError, "owned PID 7 identity changed"
        ):
            bounded_process._assert_owned_pid_identities(
                {original.pid: original}, {reused.pid: reused}
            )
        bounded_process._assert_owned_pid_identities(
            {original.pid: original}, {original.pid: original}
        )
        valid = {
            "pid": 7,
            "ppid": 1,
            "pgid": 7,
            "sid": 7,
            "started": "birth",
            "command": "tool",
            "ownership_marker_sha256": "a" * 64,
            "reaped": True,
        }
        bounded_process.validate_process_ledger([valid])
        for field in ("pid", "ppid", "pgid", "sid"):
            mutated = {**valid, field: True}
            with (
                self.subTest(field=field),
                self.assertRaises(bounded_process.BoundedProcessError),
            ):
                bounded_process.validate_process_ledger([mutated])

    def test_owned_pid_rebirth_fails_finite_scan_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            original_assert = bounded_process._assert_owned_pid_identities
            injected = False

            def inject_rebirth(
                owned: dict[int, bounded_process.ProcessIdentity],
                current: dict[int, bounded_process.ProcessIdentity],
            ) -> None:
                nonlocal injected
                if owned and not injected:
                    pid, identity = next(iter(owned.items()))
                    current[pid] = dataclasses.replace(
                        identity, started=f"{identity.started}-reused"
                    )
                    injected = True
                original_assert(owned, current)

            with (
                mock.patch.object(
                    bounded_process,
                    "_assert_owned_pid_identities",
                    side_effect=inject_rebirth,
                ),
                self.assertRaises(bounded_process.BoundedProcessFailure) as caught,
            ):
                self.run_python(cwd, "import time; time.sleep(1)")
            self.assertTrue(injected)
            self.assertEqual(caught.exception.result.reason, "observation_incomplete")
            self.assertTrue(caught.exception.result.cleanup_complete)

    def test_tampered_executable_witness_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            tampered = dataclasses.replace(self.python, sha256="0" * 64)
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "content changed"
            ):
                bounded_process.run(
                    tampered,
                    [tampered.path, "-c", "raise SystemExit(0)"],
                    cwd=cwd,
                    environment=self.environment(),
                    timeout_seconds=5,
                    drain_timeout_seconds=1,
                    maximum_output_bytes=1024,
                )

    def test_cwd_swap_and_restore_still_executes_from_anchored_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            cwd = root / "cwd"
            alternate = root / "alternate"
            held = root / "held"
            cwd.mkdir()
            alternate.mkdir()
            (cwd / "identity").write_text("original", encoding="utf-8")
            (alternate / "identity").write_text("alternate", encoding="utf-8")
            result_path = root / "result"
            if sys.platform == "darwin":
                original_spawn = bounded_process._darwin_spawn_suspended

                def swap_around_spawn(*args: object, **kwargs: object) -> int:
                    pid = original_spawn(*args, **kwargs)  # type: ignore[arg-type]
                    cwd.rename(held)
                    alternate.rename(cwd)
                    cwd.rename(alternate)
                    held.rename(cwd)
                    return pid

                launch_patch = mock.patch.object(
                    bounded_process,
                    "_darwin_spawn_suspended",
                    side_effect=swap_around_spawn,
                )
            else:
                original_write = os.write

                def swap_around_release(descriptor: int, payload: bytes) -> int:
                    if payload == b"G":
                        cwd.rename(held)
                        alternate.rename(cwd)
                        cwd.rename(alternate)
                        held.rename(cwd)
                    return original_write(descriptor, payload)

                launch_patch = mock.patch.object(
                    bounded_process.os,
                    "write",
                    side_effect=swap_around_release,
                )
            with launch_patch:
                result = self.run_python(
                    cwd,
                    "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text("
                    "pathlib.Path('identity').read_text())",
                    str(result_path),
                )
            self.assertEqual(result.exit_code, 0)
            self.assertEqual(result_path.read_text(encoding="utf-8"), "original")

    def test_execution_receipt_has_exact_types_and_self_consistent_hashes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            result = self.run_python(cwd, "print('receipt')")
            receipt = bounded_process.validate_execution_receipt(result.receipt)
            self.assertEqual(receipt["stdout_size"], len(result.stdout))
            serialized = bounded_process.dump_execution_receipt(receipt)
            self.assertEqual(
                bounded_process.load_execution_receipt(serialized), receipt
            )
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "repeats key"
            ):
                bounded_process.load_execution_receipt(
                    serialized.replace(b"{", b'{"kind":"duplicate",', 1)
                )
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "non-finite"
            ):
                bounded_process.load_execution_receipt(
                    serialized.replace(b'"schema_version":1', b'"schema_version":NaN')
                )
            integer_paths = (
                ("schema_version",),
                ("timeout_seconds",),
                ("drain_timeout_seconds",),
                ("maximum_output_bytes",),
                ("exit_code",),
                ("stdout_size",),
                ("stderr_size",),
                ("cwd_witness", "device"),
                ("cwd_witness", "inode"),
                ("cwd_witness", "uid"),
                ("cwd_witness", "gid"),
                ("cwd_witness", "mode"),
                ("environment", "schema_version"),
                ("environment", "variable_count"),
                ("standard_input", "schema_version"),
                ("standard_input", "maximum_bytes"),
                ("standard_input", "size"),
                ("ownership_marker", "schema_version"),
                ("ownership_marker", "fd"),
                ("ownership_marker", "device"),
                ("ownership_marker", "inode"),
                ("ownership_marker", "uid"),
                ("ownership_marker", "gid"),
                ("ownership_marker", "mode"),
                ("ownership_marker", "nlink"),
                ("ownership_marker", "size"),
                ("executable", "device"),
                ("executable", "inode"),
                ("executable", "uid"),
                ("executable", "gid"),
                ("executable", "mode"),
                ("executable", "nlink"),
                ("executable", "size"),
                ("executable", "mtime_ns"),
                ("executable", "ctime_ns"),
                ("process_ledger", 0, "pid"),
                ("process_ledger", 0, "ppid"),
                ("process_ledger", 0, "pgid"),
                ("process_ledger", 0, "sid"),
            )
            for path in integer_paths:
                with self.subTest(path=path):
                    mutated = copy.deepcopy(receipt)
                    target: object = mutated
                    for component in path[:-1]:
                        target = target[component]  # type: ignore[index]
                    target[path[-1]] = True  # type: ignore[index]
                    if path[0] == "process_ledger":
                        mutated["process_ledger_sha256"] = (
                            bounded_process._canonical_json_sha256(
                                mutated["process_ledger"],
                                domain=bounded_process.PROCESS_LEDGER_DOMAIN,
                            )
                        )
                    body = dict(mutated)
                    del body["receipt_sha256"]
                    mutated["receipt_sha256"] = bounded_process._canonical_json_sha256(
                        body, domain=bounded_process.EXECUTION_RECEIPT_DOMAIN
                    )
                    with self.assertRaises(bounded_process.BoundedProcessError):
                        bounded_process.validate_execution_receipt(mutated)

            for container in (
                "cwd_witness",
                "environment",
                "standard_input",
                "ownership_marker",
            ):
                for operation in ("extra", "missing"):
                    with self.subTest(container=container, operation=operation):
                        mutated = copy.deepcopy(receipt)
                        nested = mutated[container]
                        if operation == "extra":
                            nested["unexpected"] = None
                        else:
                            del nested[next(iter(nested))]
                        self.rehash_receipt(mutated)
                        with self.assertRaises(bounded_process.BoundedProcessError):
                            bounded_process.validate_execution_receipt(mutated)

            for field in (
                "unlinked_before_launch",
                "writer_closed_before_launch",
                "inherited_read_only",
                "parent_fd_collision",
                "leader_verified_before_release",
                "leader_marker_loss_observed_at_exit",
                "supervisor_descriptor_closed",
            ):
                with self.subTest(ownership_marker_boolean=field):
                    mutated = copy.deepcopy(receipt)
                    mutated["ownership_marker"][field] = 1
                    self.rehash_receipt(mutated)
                    with self.assertRaises(bounded_process.BoundedProcessError):
                        bounded_process.validate_execution_receipt(mutated)

            inconsistent_marker = copy.deepcopy(receipt)
            inconsistent_marker["ownership_marker"][
                "leader_marker_loss_observed_at_exit"
            ] = True
            inconsistent_marker["ownership_marker"][
                "leader_verified_before_release"
            ] = False
            self.rehash_receipt(inconsistent_marker)
            with self.assertRaises(bounded_process.BoundedProcessError):
                bounded_process.validate_execution_receipt(inconsistent_marker)

    def test_receipt_context_binds_cwd_inode_and_redacted_environment(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            cwd = root / "cwd"
            alternate = root / "alternate"
            held = root / "held"
            outside = root / "outside"
            cwd.mkdir()
            alternate.mkdir()
            outside.write_text("untouched", encoding="utf-8")
            environment = self.environment()
            result = self.run_python(cwd, "raise SystemExit(0)")
            receipt = dict(result.receipt)
            self.assertEqual(receipt["cwd_witness"]["path"], str(cwd))
            self.assertEqual(
                receipt["environment"],
                bounded_process.environment_identity(environment),
            )
            self.assertNotIn(
                next(iter(environment.values())).encode("utf-8"),
                bounded_process.dump_execution_receipt(receipt),
            )
            bounded_process.verify_receipt_context(
                receipt,
                cwd=cwd,
                environment=environment,
            )

            changed_environment = {**environment, "PMUX_CONTEXT_PROBE": "changed"}
            substituted_environment = copy.deepcopy(receipt)
            substituted_environment["environment"] = (
                bounded_process.environment_identity(changed_environment)
            )
            self.rehash_receipt(substituted_environment)
            bounded_process.validate_execution_receipt(substituted_environment)
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "environment context"
            ):
                bounded_process.verify_receipt_context(
                    substituted_environment,
                    cwd=cwd,
                    environment=environment,
                )

            cwd.rename(held)
            alternate.rename(cwd)
            substituted_cwd = copy.deepcopy(receipt)
            try:
                with self.assertRaisesRegex(
                    bounded_process.BoundedProcessError, "cwd context"
                ):
                    bounded_process.verify_receipt_context(
                        receipt,
                        cwd=cwd,
                        environment=environment,
                    )
                canonical, descriptor, metadata = bounded_process._open_cwd(cwd)
                try:
                    substituted_cwd["cwd_witness"] = bounded_process._cwd_witness(
                        canonical, metadata
                    )
                finally:
                    os.close(descriptor)
                self.rehash_receipt(substituted_cwd)
                bounded_process.validate_execution_receipt(substituted_cwd)
            finally:
                cwd.rename(alternate)
                held.rename(cwd)
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "cwd context"
            ):
                bounded_process.verify_receipt_context(
                    substituted_cwd,
                    cwd=cwd,
                    environment=environment,
                )
            bounded_process.verify_receipt_context(
                receipt,
                cwd=cwd,
                environment=environment,
            )
            self.assertEqual(outside.read_text(encoding="utf-8"), "untouched")

    def test_vnode_marker_does_not_change_child_environment_or_protocol_inputs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            environment = self.environment()
            native_env = bounded_process.bind_executable(
                pathlib.Path(shutil.which("env") or "/usr/bin/env").resolve()
            )
            native_result = bounded_process.run(
                native_env,
                [native_env.path],
                cwd=cwd,
                environment=environment,
                timeout_seconds=5,
                drain_timeout_seconds=1,
                maximum_output_bytes=16 * 1024,
            )
            native_observed = dict(
                line.decode("utf-8").split("=", 1)
                for line in native_result.stdout.splitlines()
            )
            self.assertEqual(native_observed, environment)
            script = (
                "import fcntl,hashlib,json,os,stat; fd="
                f"{bounded_process.OWNERSHIP_MARKER_FD}; metadata=os.fstat(fd); "
                "flags=fcntl.fcntl(fd,fcntl.F_GETFL); "
                "write_failed=False\n"
                "try: os.write(fd,b'x')\n"
                "except OSError: write_failed=True\n"
                "print(json.dumps({'environment':dict(os.environ),"
                "'marker':{'fd':fd,'mode':metadata.st_mode,'nlink':metadata.st_nlink,"
                "'size':metadata.st_size,'sha256':hashlib.sha256(os.pread(fd,"
                "metadata.st_size,0)).hexdigest(),'read_only':"
                "(flags & os.O_ACCMODE)==os.O_RDONLY,'write_failed':write_failed}},"
                "sort_keys=True,separators=(',',':')))"
            )
            result = self.run_python(cwd, script, maximum_output_bytes=16 * 1024)
            observed = json.loads(result.stdout)
            self.assertEqual(
                {name: observed["environment"][name] for name in environment},
                environment,
            )
            self.assertFalse(
                any(
                    name.startswith("PMUX_EVIDENCE_")
                    for name in observed["environment"]
                )
            )
            marker = result.receipt["ownership_marker"]
            self.assertEqual(observed["marker"]["fd"], marker["fd"])
            self.assertEqual(observed["marker"]["mode"], marker["mode"])
            self.assertEqual(observed["marker"]["nlink"], 0)
            self.assertEqual(observed["marker"]["size"], marker["size"])
            self.assertEqual(observed["marker"]["sha256"], marker["sha256"])
            self.assertIs(observed["marker"]["read_only"], True)
            self.assertIs(observed["marker"]["write_failed"], True)
            self.assertIs(marker["unlinked_before_launch"], True)
            self.assertIs(marker["writer_closed_before_launch"], True)
            self.assertIs(marker["inherited_read_only"], True)
            self.assertIs(marker["leader_verified_before_release"], True)
            self.assertIs(marker["supervisor_descriptor_closed"], True)
            serialized = bounded_process.dump_execution_receipt(result.receipt)
            self.assertNotIn(b"pmux-evidence-owner-", serialized)
            self.assertNotIn(b"PMUX_EVIDENCE_", serialized)
            self.assertNotIn("ownership", result.receipt["argv"])
            for entry in pathlib.Path("/dev/fd").iterdir():
                try:
                    metadata = entry.stat()
                except (FileNotFoundError, OSError):
                    continue
                self.assertNotEqual(
                    (metadata.st_dev, metadata.st_ino),
                    (marker["device"], marker["inode"]),
                )

    def test_reserved_marker_fd_collision_is_remapped_only_in_child(self) -> None:
        original_backup: int | None = None
        original_inheritable = False
        sentinel_fd = os.open(os.devnull, os.O_RDONLY)
        if sentinel_fd == bounded_process.OWNERSHIP_MARKER_FD:
            replacement = fcntl.fcntl(
                sentinel_fd,
                getattr(fcntl, "F_DUPFD_CLOEXEC", fcntl.F_DUPFD),
                3,
            )
            os.close(sentinel_fd)
            sentinel_fd = replacement
        try:
            try:
                original_inheritable = os.get_inheritable(
                    bounded_process.OWNERSHIP_MARKER_FD
                )
                original_backup = fcntl.fcntl(
                    bounded_process.OWNERSHIP_MARKER_FD,
                    getattr(fcntl, "F_DUPFD_CLOEXEC", fcntl.F_DUPFD),
                    bounded_process.OWNERSHIP_MARKER_FD + 1,
                )
            except OSError:
                original_backup = None
            os.dup2(
                sentinel_fd,
                bounded_process.OWNERSHIP_MARKER_FD,
                inheritable=False,
            )
            sentinel_identity = os.fstat(bounded_process.OWNERSHIP_MARKER_FD)
            with tempfile.TemporaryDirectory() as temporary:
                cwd = pathlib.Path(temporary).resolve()
                result = self.run_python(
                    cwd,
                    "import hashlib,os; fd="
                    f"{bounded_process.OWNERSHIP_MARKER_FD}; metadata=os.fstat(fd); "
                    "print(hashlib.sha256(os.pread(fd,metadata.st_size,0)).hexdigest())",
                )
            marker = result.receipt["ownership_marker"]
            self.assertIs(marker["parent_fd_collision"], True)
            self.assertEqual(result.stdout.strip().decode("ascii"), marker["sha256"])
            parent_after = os.fstat(bounded_process.OWNERSHIP_MARKER_FD)
            self.assertEqual(
                (parent_after.st_dev, parent_after.st_ino),
                (sentinel_identity.st_dev, sentinel_identity.st_ino),
            )
        finally:
            try:
                os.close(bounded_process.OWNERSHIP_MARKER_FD)
            except OSError:
                pass
            if original_backup is not None:
                os.dup2(
                    original_backup,
                    bounded_process.OWNERSHIP_MARKER_FD,
                    inheritable=original_inheritable,
                )
                os.close(original_backup)
            os.close(sentinel_fd)

    def test_leader_closing_marker_fails_observation_and_is_reaped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "lost its ownership marker"
            ) as caught:
                self.run_python(
                    cwd,
                    "import os,time; os.close("
                    f"{bounded_process.OWNERSHIP_MARKER_FD}); "
                    "print('closed',flush=True); time.sleep(30)",
                    timeout_seconds=5,
                )
            failure = caught.exception.result
            self.assertEqual(failure.reason, "observation_incomplete")
            self.assertIs(
                failure.receipt["ownership_marker"]["leader_verified_before_release"],
                True,
            )
            self.assertIs(
                failure.receipt["ownership_marker"][
                    "leader_marker_loss_observed_at_exit"
                ],
                False,
            )
            self.assertIs(
                failure.receipt["ownership_marker"]["supervisor_descriptor_closed"],
                True,
            )
            self.assertTrue(failure.cleanup_complete)
            self.assertTrue(all(row["reaped"] for row in failure.process_ledger))
            self.assert_pid_gone(int(failure.receipt["leader_pid"]))

    def test_fast_exit_descriptor_teardown_is_not_a_live_marker_loss(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            script = (
                "import os; "
                f"os.close({bounded_process.OWNERSHIP_MARKER_FD}); "
                "os._exit(0)"
            )
            for repetition in range(50):
                with self.subTest(repetition=repetition):
                    result = self.run_python(cwd, script)
                    self.assertEqual(result.exit_code, 0)
                    marker = result.receipt["ownership_marker"]
                    self.assertIs(marker["leader_verified_before_release"], True)
                    self.assertIs(
                        type(marker["leader_marker_loss_observed_at_exit"]), bool
                    )
                    self.assertIs(marker["supervisor_descriptor_closed"], True)
                    self.assertTrue(all(row["reaped"] for row in result.process_ledger))

    def test_live_marker_loss_fails_before_delayed_escape_crosses_the_boundary(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            child_pid = cwd / "child-pid"
            script = (
                "import os,sys,time; "
                f"os.close({bounded_process.OWNERSHIP_MARKER_FD}); "
                "child=os.fork(); "
                "(open(sys.argv[1],'w').write(str(os.getpid())),time.sleep(2),"
                "os.setsid(),time.sleep(30)) if child==0 else time.sleep(30)"
            )
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "lost its ownership marker"
            ) as caught:
                self.run_python(cwd, script, str(child_pid), timeout_seconds=5)
            failure = caught.exception.result
            self.assertEqual(failure.reason, "observation_incomplete")
            self.assertTrue(failure.cleanup_complete)
            self.assertFalse(
                failure.receipt["ownership_marker"][
                    "leader_marker_loss_observed_at_exit"
                ]
            )
            self.assertGreaterEqual(len(failure.process_ledger), 2)
            deadline = time.monotonic() + 2
            while not child_pid.is_file() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(child_pid.is_file())
            self.assert_pid_gone(int(child_pid.read_text(encoding="utf-8")))

    def test_real_isolated_pip_fast_exit_has_a_complete_receipt(self) -> None:
        pip = (
            pathlib.Path(sys.prefix)
            / "lib"
            / (f"python{sys.version_info.major}.{sys.version_info.minor}")
            / "site-packages"
            / "pip"
        )
        if not pip.is_dir():
            self.skipTest("isolated pip import tree is unavailable")
        stdlib = pathlib.Path(os.__file__).resolve().parent
        lib_dynload = stdlib / "lib-dynload"
        site_packages = pip.parent
        bootstrap = (
            "import sys\n"
            "roots = sys.argv[1:4]\n"
            "module = sys.argv[4]\n"
            "arguments = sys.argv[5:]\n"
            "sys.path[:] = roots\n"
            "import runpy\n"
            "sys.argv[:] = [module, *arguments]\n"
            "runpy.run_module(module, run_name='__main__', alter_sys=True)\n"
        )
        environment = {
            "HOME": "/private/tmp" if sys.platform == "darwin" else "/tmp",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "PIP_DISABLE_PIP_VERSION_CHECK": "1",
            "PIP_NO_INDEX": "1",
            "PYTHONDONTWRITEBYTECODE": "1",
            "TMPDIR": "/private/tmp" if sys.platform == "darwin" else "/tmp",
        }
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            for repetition in range(20):
                with self.subTest(repetition=repetition):
                    result = bounded_process.run(
                        self.python,
                        [
                            self.python.path,
                            "-I",
                            "-S",
                            "-B",
                            "-c",
                            bootstrap,
                            str(stdlib),
                            str(lib_dynload),
                            str(site_packages),
                            "pip",
                            "--version",
                        ],
                        cwd=cwd,
                        environment=environment,
                        timeout_seconds=5,
                        drain_timeout_seconds=1,
                        maximum_output_bytes=4096,
                        description="isolated pip version probe",
                    )
                    self.assertEqual(result.exit_code, 0)
                    self.assertIn(b"pip ", result.stdout)
                    self.assertTrue(all(row["reaped"] for row in result.process_ledger))
                    self.assertIs(
                        type(
                            result.receipt["ownership_marker"][
                                "leader_marker_loss_observed_at_exit"
                            ]
                        ),
                        bool,
                    )

    def test_bounded_standard_input_is_exact_private_and_not_in_receipts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            secret = b"private-pmux-prompt-that-must-not-enter-receipts\n"
            expected_hash = hashlib.sha256(secret).hexdigest().encode("ascii")
            script = (
                "import fcntl,hashlib,os,stat,sys; data=sys.stdin.buffer.read(); "
                "metadata=os.fstat(0); flags=fcntl.fcntl(0,fcntl.F_GETFL); "
                "print(len(data)); print(hashlib.sha256(data).hexdigest()); "
                "print(stat.S_ISREG(metadata.st_mode) and "
                "(flags & os.O_ACCMODE)==os.O_RDONLY)"
            )
            result = self.run_python(cwd, script, stdin_bytes=secret)
            self.assertEqual(
                result.stdout.splitlines(),
                [str(len(secret)).encode("ascii"), expected_hash, b"True"],
            )
            standard_input = result.receipt["standard_input"]
            self.assertEqual(standard_input["source"], "bytes")
            self.assertIs(standard_input["present"], True)
            self.assertEqual(standard_input["size"], len(secret))
            self.assertEqual(standard_input["sha256"], expected_hash.decode("ascii"))
            self.assertIsNone(standard_input["source_descriptor"])
            serialized = bounded_process.dump_execution_receipt(result.receipt)
            self.assertNotIn(secret.rstrip(), serialized)
            self.assertNotIn(secret.decode("utf-8").strip(), result.receipt["argv"])
            bounded_process.verify_receipt_context(
                result.receipt,
                cwd=cwd,
                environment=self.environment(),
                stdin_bytes=secret,
            )
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "standard-input context"
            ):
                bounded_process.verify_receipt_context(
                    result.receipt,
                    cwd=cwd,
                    environment=self.environment(),
                    stdin_bytes=b"different",
                )

            empty = self.run_python(
                cwd,
                "import sys; print(len(sys.stdin.buffer.read()))",
                stdin_bytes=b"",
            )
            default = self.run_python(
                cwd,
                "import sys; print(len(sys.stdin.buffer.read()))",
            )
            self.assertEqual(empty.stdout, b"0\n")
            self.assertEqual(default.stdout, b"0\n")
            self.assertEqual(empty.receipt["standard_input"]["source"], "bytes")
            self.assertIs(empty.receipt["standard_input"]["present"], True)
            self.assertEqual(default.receipt["standard_input"]["source"], "none")
            self.assertIs(default.receipt["standard_input"]["present"], False)

    def test_private_standard_input_descriptor_is_bound_and_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            secret = b"descriptor-only-private-prompt\n"
            input_path = cwd / "private-input"
            write_fd = os.open(input_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            try:
                os.write(write_fd, secret)
                os.fsync(write_fd)
            finally:
                os.close(write_fd)
            input_fd = os.open(
                input_path,
                os.O_RDONLY | getattr(os, "O_CLOEXEC", 0),
            )
            input_path.unlink()
            try:
                result = self.run_python(
                    cwd,
                    "import sys; sys.stdout.buffer.write(sys.stdin.buffer.read())",
                    stdin_fd=input_fd,
                )
                self.assertEqual(result.stdout, secret)
                standard_input = result.receipt["standard_input"]
                self.assertEqual(standard_input["source"], "descriptor")
                self.assertEqual(standard_input["size"], len(secret))
                self.assertEqual(standard_input["source_descriptor"]["nlink"], 0)
                self.assertEqual(standard_input["source_descriptor"]["offset"], 0)
                self.assertNotIn(
                    secret.rstrip(),
                    bounded_process.dump_execution_receipt(result.receipt),
                )
                bounded_process.verify_receipt_context(
                    result.receipt,
                    cwd=cwd,
                    environment=self.environment(),
                    stdin_fd=input_fd,
                )

                for field in bounded_process.STANDARD_INPUT_DESCRIPTOR_KEYS:
                    with self.subTest(field=field):
                        mutated = copy.deepcopy(result.receipt)
                        mutated["standard_input"]["source_descriptor"][field] = True
                        self.rehash_receipt(mutated)
                        with self.assertRaises(bounded_process.BoundedProcessError):
                            bounded_process.validate_execution_receipt(mutated)
            finally:
                os.close(input_fd)

            named_path = cwd / "named-input"
            named_path.write_bytes(b"named")
            named_path.chmod(0o600)
            named_fd = os.open(
                named_path,
                os.O_RDONLY | getattr(os, "O_CLOEXEC", 0),
            )
            try:
                with self.assertRaises(bounded_process.BoundedProcessError):
                    self.run_python(cwd, "raise SystemExit(0)", stdin_fd=named_fd)
            finally:
                os.close(named_fd)

            read_write_fd, read_write_name = tempfile.mkstemp(dir=cwd)
            pathlib.Path(read_write_name).unlink()
            try:
                with self.assertRaisesRegex(
                    bounded_process.BoundedProcessError, "must be read-only"
                ):
                    self.run_python(cwd, "raise SystemExit(0)", stdin_fd=read_write_fd)
            finally:
                os.close(read_write_fd)

            flags_path = cwd / "descriptor-flags"
            flags_path.write_bytes(b"flags")
            flags_path.chmod(0o600)
            flags_fd = os.open(
                flags_path,
                os.O_RDONLY | getattr(os, "O_CLOEXEC", 0),
            )
            flags_path.unlink()
            try:
                os.set_inheritable(flags_fd, True)
                with self.assertRaisesRegex(
                    bounded_process.BoundedProcessError, "must be close-on-exec"
                ):
                    self.run_python(cwd, "raise SystemExit(0)", stdin_fd=flags_fd)
                os.set_inheritable(flags_fd, False)
                os.lseek(flags_fd, 1, os.SEEK_SET)
                with self.assertRaisesRegex(
                    bounded_process.BoundedProcessError, "positioned at zero"
                ):
                    self.run_python(cwd, "raise SystemExit(0)", stdin_fd=flags_fd)
            finally:
                os.close(flags_fd)

            marker = cwd / "oversize-launched"
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "exceeded their exact bound"
            ):
                self.run_python(
                    cwd,
                    "import pathlib,sys; pathlib.Path(sys.argv[1]).touch()",
                    str(marker),
                    stdin_bytes=b"x" * (bounded_process.MAX_STANDARD_INPUT_BYTES + 1),
                )
            self.assertFalse(marker.exists())

    def test_standard_input_early_exit_and_timeout_have_exact_receipts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            unread = b"u" * (1024 * 1024)
            early = self.run_python(cwd, "raise SystemExit(0)", stdin_bytes=unread)
            self.assertEqual(early.exit_code, 0)
            self.assertEqual(early.receipt["standard_input"]["size"], len(unread))

            secret = b"timeout-private-prompt\n"
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "timed out"
            ) as caught:
                self.run_python(
                    cwd,
                    "import hashlib,sys,time; data=sys.stdin.buffer.read(); "
                    "print(hashlib.sha256(data).hexdigest(),flush=True); time.sleep(30)",
                    stdin_bytes=secret,
                    timeout_seconds=1,
                )
            failure = caught.exception.result
            self.assertEqual(
                failure.stdout,
                hashlib.sha256(secret).hexdigest().encode("ascii") + b"\n",
            )
            self.assertNotIn(
                secret.rstrip(), bounded_process.dump_failure_receipt(failure.receipt)
            )
            bounded_process.verify_receipt_context(
                failure.receipt,
                cwd=cwd,
                environment=self.environment(),
                stdin_bytes=secret,
            )

    def test_timeout_failure_receipt_preserves_bounded_output_and_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessFailure, "timed out"
            ) as caught:
                self.run_python(
                    cwd,
                    "import sys,time; print('before-timeout', flush=True); "
                    "sys.stderr.write('diagnostic\\n'); sys.stderr.flush(); time.sleep(30)",
                    timeout_seconds=1,
                )
            failure = caught.exception.result
            self.assertEqual(failure.reason, "timeout")
            self.assertTrue(failure.cleanup_complete)
            self.assertTrue(failure.output_complete)
            self.assertEqual(failure.stdout, b"before-timeout\n")
            self.assertEqual(failure.stderr, b"diagnostic\n")
            self.assertTrue(
                all(row["reaped"] is True for row in failure.process_ledger)
            )
            self.assertIs(
                failure.receipt["ownership_marker"]["leader_verified_before_release"],
                True,
            )
            self.assertIs(
                failure.receipt["ownership_marker"]["supervisor_descriptor_closed"],
                True,
            )

    def test_cwd_binding_loss_has_a_structured_failure_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            cwd = root / "cwd"
            moved = root / "moved"
            cwd.mkdir()
            try:
                with self.assertRaisesRegex(
                    bounded_process.BoundedProcessFailure, "cwd binding changed"
                ) as caught:
                    self.run_python(
                        cwd,
                        "import pathlib,sys; original=pathlib.Path(sys.argv[1]); "
                        "original.rename(sys.argv[2]); original.mkdir()",
                        str(cwd),
                        str(moved),
                    )
                failure = caught.exception.result
                self.assertEqual(failure.reason, "binding_changed")
                self.assertTrue(failure.cleanup_complete)
                self.assertTrue(failure.output_complete)
            finally:
                if moved.is_dir():
                    if cwd.is_dir():
                        cwd.rmdir()
                    moved.rename(cwd)

    def test_failure_receipt_schema_rejects_self_consistent_mutations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            with self.assertRaises(bounded_process.BoundedProcessFailure) as caught:
                self.run_python(
                    cwd,
                    "import time; time.sleep(30)",
                    timeout_seconds=1,
                )
            receipt = bounded_process.validate_failure_receipt(
                caught.exception.result.receipt
            )
            serialized = bounded_process.dump_failure_receipt(receipt)
            self.assertEqual(bounded_process.load_failure_receipt(serialized), receipt)

            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "repeats key"
            ):
                bounded_process.load_failure_receipt(
                    serialized.replace(b"{", b'{"kind":"duplicate",', 1)
                )
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "non-finite"
            ):
                bounded_process.load_failure_receipt(
                    serialized.replace(b'"schema_version":1', b'"schema_version":NaN')
                )

            def rehash(mutated: dict[str, object]) -> None:
                ledger = mutated["process_ledger"]
                mutated["process_ledger_sha256"] = (
                    bounded_process._canonical_json_sha256(
                        ledger,
                        domain=bounded_process.PROCESS_LEDGER_DOMAIN,
                    )
                )
                body = dict(mutated)
                del body["receipt_sha256"]
                mutated["receipt_sha256"] = bounded_process._canonical_json_sha256(
                    body,
                    domain=bounded_process.FAILURE_RECEIPT_DOMAIN,
                )

            mutations: list[dict[str, object]] = []
            for field in (
                "schema_version",
                "timeout_seconds",
                "drain_timeout_seconds",
                "maximum_output_bytes",
                "leader_pid",
                "stdout_size",
                "stderr_size",
            ):
                mutated = copy.deepcopy(receipt)
                mutated[field] = True
                mutations.append(mutated)
            for field in (
                "cleanup_complete",
                "output_complete",
                "process_observation_complete",
            ):
                mutated = copy.deepcopy(receipt)
                mutated[field] = 1
                mutations.append(mutated)
            mutated_reason = copy.deepcopy(receipt)
            mutated_reason["failure_reason"] = "unknown"
            mutations.append(mutated_reason)
            unhashable_reason = copy.deepcopy(receipt)
            unhashable_reason["failure_reason"] = []
            mutations.append(unhashable_reason)
            malformed_format = copy.deepcopy(receipt)
            malformed_format["executable"]["executable_format"] = []
            mutations.append(malformed_format)
            mutated_reaped = copy.deepcopy(receipt)
            mutated_reaped["process_ledger"][0]["reaped"] = 1
            mutations.append(mutated_reaped)
            marker_without_launch_identity = copy.deepcopy(receipt)
            marker_without_launch_identity["ownership_marker"][
                "leader_marker_loss_observed_at_exit"
            ] = True
            marker_without_launch_identity["ownership_marker"][
                "leader_verified_before_release"
            ] = False
            mutations.append(marker_without_launch_identity)
            marker_loss_without_exit = copy.deepcopy(receipt)
            marker_loss_without_exit["ownership_marker"][
                "leader_marker_loss_observed_at_exit"
            ] = True
            marker_loss_without_exit["exit_code"] = None
            marker_loss_without_exit["cleanup_complete"] = False
            mutations.append(marker_loss_without_exit)
            for path in (
                ("cwd_witness", "device"),
                ("cwd_witness", "inode"),
                ("cwd_witness", "uid"),
                ("cwd_witness", "gid"),
                ("cwd_witness", "mode"),
                ("environment", "schema_version"),
                ("environment", "variable_count"),
                ("standard_input", "schema_version"),
                ("standard_input", "maximum_bytes"),
                ("standard_input", "size"),
                ("ownership_marker", "schema_version"),
                ("ownership_marker", "fd"),
                ("ownership_marker", "device"),
                ("ownership_marker", "inode"),
                ("ownership_marker", "uid"),
                ("ownership_marker", "gid"),
                ("ownership_marker", "mode"),
                ("ownership_marker", "nlink"),
                ("ownership_marker", "size"),
            ):
                mutated = copy.deepcopy(receipt)
                mutated[path[0]][path[1]] = True
                mutations.append(mutated)
            for field in (
                "unlinked_before_launch",
                "writer_closed_before_launch",
                "inherited_read_only",
                "parent_fd_collision",
                "leader_verified_before_release",
                "leader_marker_loss_observed_at_exit",
                "supervisor_descriptor_closed",
            ):
                mutated = copy.deepcopy(receipt)
                mutated["ownership_marker"][field] = 1
                mutations.append(mutated)
            for container in (
                "cwd_witness",
                "environment",
                "standard_input",
                "ownership_marker",
            ):
                extra_nested = copy.deepcopy(receipt)
                extra_nested[container]["unexpected"] = None
                mutations.append(extra_nested)
                missing_nested = copy.deepcopy(receipt)
                del missing_nested[container][next(iter(missing_nested[container]))]
                mutations.append(missing_nested)
            extra = copy.deepcopy(receipt)
            extra["extra"] = None
            mutations.append(extra)
            missing = copy.deepcopy(receipt)
            del missing["output_complete"]
            mutations.append(missing)

            for index, mutated in enumerate(mutations):
                with self.subTest(index=index):
                    rehash(mutated)
                    with self.assertRaises(bounded_process.BoundedProcessError):
                        bounded_process.validate_failure_receipt(mutated)

    def test_cleanup_failure_receipt_truthfully_marks_unreaped_process(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            real_kill = os.kill

            def suppress_cleanup(pid: int, signal_number: int) -> None:
                if signal_number in (signal.SIGTERM, signal.SIGKILL):
                    return
                real_kill(pid, signal_number)

            leader_pid: int | None = None
            try:
                with (
                    mock.patch.object(
                        bounded_process,
                        "TERMINATE_GRACE_SECONDS",
                        0.05,
                    ),
                    mock.patch.object(
                        bounded_process,
                        "KILL_GRACE_SECONDS",
                        0.05,
                    ),
                    mock.patch.object(
                        bounded_process,
                        "LEADER_REAP_SECONDS",
                        0.05,
                    ),
                    mock.patch.object(
                        bounded_process.os,
                        "kill",
                        side_effect=suppress_cleanup,
                    ),
                    self.assertRaisesRegex(
                        bounded_process.BoundedProcessFailure,
                        "cleanup is incomplete",
                    ) as caught,
                ):
                    self.run_python(
                        cwd,
                        "import time; time.sleep(30)",
                        timeout_seconds=1,
                    )
                failure = caught.exception.result
                leader_pid = int(failure.receipt["leader_pid"])
                self.assertEqual(failure.reason, "timeout")
                self.assertFalse(failure.cleanup_complete)
                self.assertFalse(failure.output_complete)
                self.assertTrue(
                    any(row["reaped"] is False for row in failure.process_ledger)
                )
                bounded_process.validate_failure_receipt(failure.receipt)
                real_kill(leader_pid, 0)
            finally:
                if leader_pid is not None:
                    try:
                        real_kill(leader_pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    try:
                        os.waitpid(leader_pid, 0)
                    except ChildProcessError:
                        pass

    def test_darwin_mapped_vnode_rejects_alternate_before_user_code(self) -> None:
        if sys.platform != "darwin":
            self.skipTest("Darwin START_SUSPENDED contract")
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            parent = root / "tool"
            alternate = root / "alternate"
            held = root / "held"
            parent.mkdir()
            alternate.mkdir()
            tool = parent / "python"
            shutil.copyfile(pathlib.Path(sys.executable).resolve(), tool)
            tool.chmod(0o500)
            witness = bounded_process.bind_executable(tool)
            alternate_marker = root / "alternate-marker"
            alternate_tool = alternate / "python"
            alternate_tool.write_text(
                f"#!/bin/sh\nprintf alternate > {alternate_marker}\n",
                encoding="utf-8",
            )
            alternate_tool.chmod(0o500)
            original_spawn = bounded_process._darwin_spawn_suspended

            def swap_during_spawn(*args: object, **kwargs: object) -> int:
                parent.rename(held)
                alternate.rename(parent)
                try:
                    return original_spawn(*args, **kwargs)  # type: ignore[arg-type]
                finally:
                    parent.rename(alternate)
                    held.rename(parent)

            with (
                mock.patch.object(
                    bounded_process,
                    "_darwin_spawn_suspended",
                    side_effect=swap_during_spawn,
                ),
                self.assertRaisesRegex(
                    bounded_process.BoundedProcessError,
                    "suspended executable identity",
                ),
            ):
                bounded_process.run(
                    witness,
                    [witness.path, "-c", "raise SystemExit(0)"],
                    cwd=root,
                    environment=self.environment(),
                    timeout_seconds=5,
                    drain_timeout_seconds=1,
                    maximum_output_bytes=1024,
                )
            self.assertFalse(alternate_marker.exists())

    def test_linux_descriptor_exec_never_runs_swapped_alternate(self) -> None:
        if not sys.platform.startswith("linux"):
            self.skipTest("Linux descriptor-exec contract")
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            parent = root / "tool"
            alternate = root / "alternate"
            held = root / "held"
            parent.mkdir()
            alternate.mkdir()
            tool = parent / "python"
            shutil.copyfile(pathlib.Path(sys.executable).resolve(), tool)
            tool.chmod(0o500)
            witness = bounded_process.bind_executable(tool)
            original_marker = root / "original-marker"
            alternate_marker = root / "alternate-marker"
            alternate_tool = alternate / "python"
            alternate_tool.write_text(
                f"#!/bin/sh\nprintf alternate > {alternate_marker}\n",
                encoding="utf-8",
            )
            alternate_tool.chmod(0o500)
            original_write = os.write
            swapped = False

            def swap_before_release(descriptor: int, payload: bytes) -> int:
                nonlocal swapped
                if payload == b"G" and not swapped:
                    parent.rename(held)
                    alternate.rename(parent)
                    swapped = True
                return original_write(descriptor, payload)

            try:
                with (
                    mock.patch.object(
                        bounded_process.os,
                        "write",
                        side_effect=swap_before_release,
                    ),
                    self.assertRaisesRegex(
                        bounded_process.BoundedProcessError,
                        "executable binding changed",
                    ),
                ):
                    bounded_process.run(
                        witness,
                        [
                            witness.path,
                            "-c",
                            "import pathlib,sys; pathlib.Path(sys.argv[1]).touch()",
                            str(original_marker),
                        ],
                        cwd=root,
                        environment=self.environment(),
                        timeout_seconds=5,
                        drain_timeout_seconds=1,
                        maximum_output_bytes=1024,
                    )
                self.assertTrue(original_marker.is_file())
                self.assertFalse(alternate_marker.exists())
            finally:
                if swapped:
                    parent.rename(alternate)
                    held.rename(parent)


if __name__ == "__main__":
    unittest.main()
