from __future__ import annotations

import copy
import dataclasses
import fcntl
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

TOOLS = pathlib.Path(__file__).resolve().parents[2]
COMMON = TOOLS / "evidence_common"
sys.path.insert(0, str(COMMON))

import bounded_process  # noqa: E402
import managed_process  # noqa: E402


class ManagedProcessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        compiler = shutil.which("cc")
        if compiler is None:
            raise unittest.SkipTest("native C compiler is unavailable")
        cls.build_directory = tempfile.TemporaryDirectory()
        output = pathlib.Path(cls.build_directory.name).resolve() / "fake-pmuxd"
        source = pathlib.Path(__file__).resolve().parent / "fixtures" / "fake_pmuxd.c"
        subprocess.run(
            [
                compiler,
                "-std=gnu11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-O2",
                str(source),
                "-o",
                str(output),
            ],
            check=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        output.chmod(0o500)
        cls.fake = bounded_process.bind_executable(output)

    @classmethod
    def tearDownClass(cls) -> None:
        cls.build_directory.cleanup()

    def environment(self) -> dict[str, str]:
        return {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}

    def start(
        self,
        cwd: pathlib.Path,
        *arguments: str,
        timeout_seconds: int = 10,
        graceful_stop_timeout_seconds: int = 2,
        maximum_output_bytes: int = 16 * 1024,
        stdout_spool_fd: int | None = None,
        stderr_spool_fd: int | None = None,
    ) -> managed_process.ManagedProcess:
        return managed_process.start_managed(
            self.fake,
            [self.fake.path, *arguments],
            cwd=cwd,
            environment=self.environment(),
            timeout_seconds=timeout_seconds,
            graceful_stop_timeout_seconds=graceful_stop_timeout_seconds,
            drain_timeout_seconds=1,
            maximum_output_bytes=maximum_output_bytes,
            stdout_spool_fd=stdout_spool_fd,
            stderr_spool_fd=stderr_spool_fd,
        )

    def wait_for_output(
        self,
        handle: managed_process.ManagedProcess,
        minimum: int = 1,
    ) -> managed_process.ManagedProcessHealth:
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            health = handle.health()
            if health.stdout_size >= minimum:
                return health
            time.sleep(0.01)
        self.fail("managed fake did not produce readiness output")

    def assert_pid_gone(self, pid: int) -> None:
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.02)
        self.fail(f"managed process left PID {pid} alive")

    def wait_for_pid_file(self, path: pathlib.Path) -> int:
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                return int(path.read_text(encoding="utf-8"))
            except (FileNotFoundError, ValueError):
                time.sleep(0.01)
        self.fail("managed fake did not publish escaped PID")

    def rehash(self, receipt: dict[str, object], *, failure: bool) -> None:
        receipt["process_ledger_sha256"] = bounded_process._canonical_json_sha256(
            receipt["process_ledger"],
            domain=bounded_process.PROCESS_LEDGER_DOMAIN,
        )
        body = dict(receipt)
        del body["receipt_sha256"]
        receipt["receipt_sha256"] = bounded_process._canonical_json_sha256(
            body,
            domain=(
                managed_process.MANAGED_FAILURE_RECEIPT_DOMAIN
                if failure
                else managed_process.MANAGED_EXECUTION_RECEIPT_DOMAIN
            ),
        )

    def test_native_health_expected_stop_receipt_and_idempotent_close(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            before_threads = {
                thread.ident
                for thread in threading.enumerate()
                if thread.name.startswith("pmux-managed-")
            }
            stdout_path = cwd / "stdout"
            stderr_path = cwd / "stderr"
            stdout_fd = os.open(stdout_path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
            stderr_fd = os.open(stderr_path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
            try:
                handle = self.start(
                    cwd,
                    stdout_spool_fd=stdout_fd,
                    stderr_spool_fd=stderr_fd,
                )
                health = self.wait_for_output(handle, len(b"READY\n"))
                self.assertTrue(health.running)
                self.assertFalse(health.stop_requested)
                self.assertEqual(health.identity.leader_pid, handle.identity.leader_pid)
                with self.assertRaises(dataclasses.FrozenInstanceError):
                    health.running = False  # type: ignore[misc]
                result = handle.finalize()
                self.assertEqual(result.exit_code, 0)
                self.assertEqual(result.stdout, b"READY\nSTOP\n")
                self.assertEqual(result.stderr, b"")
                self.assertIs(handle.close(), result)
                self.assertIs(handle.close(), result)
            finally:
                os.close(stdout_fd)
                os.close(stderr_fd)
            self.assertEqual(stdout_path.read_bytes(), b"READY\nSTOP\n")
            self.assertEqual(stderr_path.read_bytes(), b"")
            receipt = managed_process.validate_managed_execution_receipt(result.receipt)
            self.assertEqual(receipt["stop_request"]["kind"], "expected")
            self.assertEqual(receipt["stop_request"]["signal"], signal.SIGTERM)
            self.assertIs(
                receipt["ownership_marker"]["supervisor_descriptor_closed"], True
            )
            serialized = managed_process.dump_managed_execution_receipt(receipt)
            self.assertEqual(
                managed_process.load_managed_execution_receipt(serialized), receipt
            )
            self.assert_pid_gone(handle.identity.leader_pid)
            after_threads = {
                thread.ident
                for thread in threading.enumerate()
                if thread.name.startswith("pmux-managed-")
            }
            self.assertEqual(after_threads, before_threads)

    def test_owned_pid_rebirth_fails_managed_scan_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            handle = self.start(cwd)
            try:
                leader = handle._leader
                reused = dataclasses.replace(leader, started=f"{leader.started}-reused")
                with (
                    mock.patch.object(
                        bounded_process,
                        "_snapshot",
                        return_value={reused.pid: reused},
                    ),
                    self.assertRaisesRegex(
                        managed_process._ManagedFault,
                        "managed owned PID identity changed",
                    ),
                ):
                    handle._scan_owned()
            finally:
                handle.close()
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_abort_is_structured_reaped_and_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            handle = self.start(cwd)
            self.wait_for_output(handle)
            result = handle.close()
            self.assertIsInstance(result, bounded_process.FailureResult)
            self.assertEqual(result.reason, "abort_requested")
            self.assertTrue(result.cleanup_complete)
            self.assertTrue(result.output_complete)
            self.assertEqual(result.receipt["stop_request"]["kind"], "abort")
            self.assertIs(handle.close(), result)
            with self.assertRaises(bounded_process.BoundedProcessFailure) as caught:
                handle.abort()
            self.assertIs(caught.exception.result, result)
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_term_handler_escape_is_attributed_and_reaped_on_abort(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            escaped_path = cwd / "term-escaped-pid"
            handle = self.start(
                cwd,
                "--spawn-escape-on-term",
                str(escaped_path),
                graceful_stop_timeout_seconds=1,
            )
            self.wait_for_output(handle)
            result = handle.close()
            self.assertIsInstance(result, bounded_process.FailureResult)
            escaped_pid = self.wait_for_pid_file(escaped_path)
            self.assertTrue(result.cleanup_complete)
            self.assertIn(escaped_pid, {row["pid"] for row in result.process_ledger})
            self.assert_pid_gone(escaped_pid)
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_term_handler_escape_cannot_false_succeed_expected_stop(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            escaped_path = cwd / "term-escaped-pid"
            handle = self.start(
                cwd,
                "--spawn-escape-on-term",
                str(escaped_path),
                graceful_stop_timeout_seconds=1,
            )
            self.wait_for_output(handle)
            with self.assertRaises(bounded_process.BoundedProcessFailure) as caught:
                handle.finalize()
            result = caught.exception.result
            escaped_pid = self.wait_for_pid_file(escaped_path)
            self.assertEqual(result.reason, "descendant_survived")
            self.assertTrue(result.cleanup_complete)
            self.assertIn(escaped_pid, {row["pid"] for row in result.process_ledger})
            self.assert_pid_gone(escaped_pid)
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_unexpected_exit_and_lifetime_timeout_are_structured(self) -> None:
        for arguments, expected in (
            (["--exit-early"], "unexpected_exit"),
            ([], "timeout"),
        ):
            with (
                self.subTest(expected=expected),
                tempfile.TemporaryDirectory() as temporary,
            ):
                cwd = pathlib.Path(temporary).resolve()
                handle = self.start(
                    cwd,
                    *arguments,
                    timeout_seconds=1 if expected == "timeout" else 5,
                    graceful_stop_timeout_seconds=1 if expected == "timeout" else 2,
                )
                deadline = time.monotonic() + 4
                failure: bounded_process.FailureResult | None = None
                while time.monotonic() < deadline:
                    try:
                        handle.health()
                    except bounded_process.BoundedProcessFailure as error:
                        failure = error.result
                        break
                    time.sleep(0.02)
                self.assertIsNotNone(failure)
                assert failure is not None
                self.assertEqual(failure.reason, expected)
                self.assertTrue(failure.cleanup_complete)
                self.assertEqual(failure.receipt["stop_request"]["kind"], "none")
                self.assertIs(handle.close(), failure)
                self.assert_pid_gone(handle.identity.leader_pid)

    def test_output_flood_is_bounded_and_reaped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            with (
                mock.patch.object(
                    managed_process, "MANAGED_TERMINATE_GRACE_SECONDS", 0.05
                ),
                mock.patch.object(managed_process, "MANAGED_KILL_GRACE_SECONDS", 0.2),
            ):
                handle = self.start(cwd, "--flood", maximum_output_bytes=128)
                deadline = time.monotonic() + 3
                with self.assertRaises(bounded_process.BoundedProcessFailure) as caught:
                    while True:
                        handle.health()
                        if time.monotonic() >= deadline:
                            self.fail("managed output flood was not bounded")
                        time.sleep(0.01)
            failure = caught.exception.result
            self.assertEqual(failure.reason, "output_limit")
            self.assertEqual(len(failure.stdout) + len(failure.stderr), 128)
            self.assertFalse(failure.output_complete)
            self.assertTrue(failure.cleanup_complete)
            self.assertEqual(failure.receipt["output_limit_stream"], "stdout")
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_graceful_stop_timeout_escalates_and_reaps(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            handle = self.start(
                cwd,
                "--ignore-term",
                graceful_stop_timeout_seconds=1,
            )
            self.wait_for_output(handle)
            with (
                mock.patch.object(
                    managed_process, "MANAGED_TERMINATE_GRACE_SECONDS", 0.05
                ),
                mock.patch.object(managed_process, "MANAGED_KILL_GRACE_SECONDS", 0.2),
                self.assertRaises(bounded_process.BoundedProcessFailure) as caught,
            ):
                handle.finalize()
            failure = caught.exception.result
            self.assertEqual(failure.reason, "graceful_stop_timeout")
            self.assertEqual(failure.receipt["stop_request"]["kind"], "expected")
            self.assertTrue(failure.cleanup_complete)
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_escaped_setsid_descendant_is_attributed_and_killed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            escaped_pid_path = cwd / "escaped-pid"
            handle = self.start(cwd, "--spawn-escape", str(escaped_pid_path))
            self.wait_for_output(handle)
            deadline = time.monotonic() + 3
            while not escaped_pid_path.is_file() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(escaped_pid_path.is_file())
            escaped_pid = int(escaped_pid_path.read_text(encoding="utf-8"))
            with self.assertRaises(bounded_process.BoundedProcessFailure) as caught:
                handle.finalize()
            failure = caught.exception.result
            self.assertEqual(failure.reason, "descendant_survived")
            self.assertGreaterEqual(len(failure.process_ledger), 2)
            self.assertTrue(failure.cleanup_complete)
            self.assert_pid_gone(escaped_pid)
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_cwd_binding_swap_and_marker_close_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            cwd = root / "cwd"
            alternate = root / "alternate"
            held = root / "held"
            cwd.mkdir()
            alternate.mkdir()
            handle = self.start(cwd)
            self.wait_for_output(handle)
            cwd.rename(held)
            alternate.rename(cwd)
            try:
                deadline = time.monotonic() + 3
                with self.assertRaises(bounded_process.BoundedProcessFailure) as caught:
                    while True:
                        handle.health()
                        if time.monotonic() >= deadline:
                            self.fail("managed cwd swap was not detected")
                        time.sleep(0.01)
                self.assertEqual(caught.exception.result.reason, "binding_changed")
            finally:
                cwd.rename(alternate)
                held.rename(cwd)
            self.assert_pid_gone(handle.identity.leader_pid)

            marker_handle = self.start(
                cwd,
                "--close-marker",
                str(bounded_process.OWNERSHIP_MARKER_FD),
            )
            deadline = time.monotonic() + 3
            with self.assertRaises(bounded_process.BoundedProcessFailure) as caught:
                while True:
                    marker_handle.health()
                    if time.monotonic() >= deadline:
                        self.fail("managed marker close was not detected")
                    time.sleep(0.01)
            self.assertEqual(caught.exception.result.reason, "observation_incomplete")
            # Once the child discards its marker, the supervisor can reap the
            # known leader but cannot truthfully prove that no unobserved child
            # escaped before marker loss.
            self.assertFalse(caught.exception.result.cleanup_complete)
            self.assertFalse(
                caught.exception.result.receipt["process_observation_complete"]
            )
            self.assert_pid_gone(marker_handle.identity.leader_pid)

    def test_concurrent_expected_stop_race_is_one_idempotent_result(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            handle = self.start(cwd)
            self.wait_for_output(handle)
            barrier = threading.Barrier(3)
            results: list[bounded_process.RunResult] = []
            errors: list[BaseException] = []

            def finalize() -> None:
                barrier.wait()
                try:
                    results.append(handle.finalize(signal_number=signal.SIGUSR1))
                except BaseException as error:
                    errors.append(error)

            workers = [threading.Thread(target=finalize) for _index in range(2)]
            for worker in workers:
                worker.start()
            barrier.wait()
            for worker in workers:
                worker.join(timeout=5)
                self.assertFalse(worker.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(len(results), 2)
            self.assertIs(results[0], results[1])
            self.assertEqual(
                results[0].receipt["stop_request"]["signal"], signal.SIGUSR1
            )
            self.assertEqual(results[0].stdout, b"READY\nSTOP\n")

    def test_close_concurrent_with_expected_stop_joins_success_publication(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            handle = self.start(cwd)
            self.wait_for_output(handle)
            signal_entered = threading.Event()
            release_signal = threading.Event()
            real_kill = os.kill
            results: list[
                bounded_process.RunResult | bounded_process.FailureResult
            ] = []
            errors: list[BaseException] = []

            def delay_expected_stop(pid: int, signal_number: int) -> None:
                if (
                    pid == handle.identity.leader_pid
                    and signal_number == signal.SIGUSR1
                ):
                    signal_entered.set()
                    if not release_signal.wait(timeout=3):
                        raise AssertionError("expected-stop race was not released")
                real_kill(pid, signal_number)

            def finalize() -> None:
                try:
                    results.append(handle.finalize(signal_number=signal.SIGUSR1))
                except BaseException as error:
                    errors.append(error)

            def close() -> None:
                try:
                    results.append(handle.close())
                except BaseException as error:
                    errors.append(error)

            with mock.patch.object(
                managed_process.os, "kill", side_effect=delay_expected_stop
            ):
                finalizer = threading.Thread(target=finalize)
                closer = threading.Thread(target=close)
                finalizer.start()
                self.assertTrue(signal_entered.wait(timeout=3))
                closer.start()
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    with handle._condition:
                        if handle._stop_request["kind"] == "expected":
                            self.assertIsNone(handle._requested_fault)
                            break
                    time.sleep(0.01)
                else:
                    self.fail("expected-stop request was not published")
                release_signal.set()
                finalizer.join(timeout=5)
                closer.join(timeout=5)
            self.assertFalse(finalizer.is_alive())
            self.assertFalse(closer.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(len(results), 2)
            self.assertIs(results[0], results[1])
            self.assertIsInstance(results[0], bounded_process.RunResult)
            self.assertEqual(results[0].receipt["stop_request"]["kind"], "expected")
            self.assertEqual(
                results[0].receipt["stop_request"]["signal"], signal.SIGUSR1
            )
            self.assert_pid_gone(handle.identity.leader_pid)

    def test_postlaunch_identity_failure_is_structured_and_reaped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            real_probe = bounded_process._process_has_ownership_marker
            calls = 0

            def reject_first(pid: int, marker: object) -> bool:
                nonlocal calls
                calls += 1
                if calls == 1:
                    return False
                return real_probe(pid, marker)  # type: ignore[arg-type]

            with (
                mock.patch.object(
                    bounded_process,
                    "_process_has_ownership_marker",
                    side_effect=reject_first,
                ),
                self.assertRaises(bounded_process.BoundedProcessFailure) as caught,
            ):
                self.start(cwd)
            failure = caught.exception.result
            self.assertEqual(failure.reason, "launch_identity")
            self.assertTrue(failure.cleanup_complete)
            self.assertFalse(
                failure.receipt["ownership_marker"]["leader_verified_before_release"]
            )
            self.assert_pid_gone(int(failure.receipt["leader_pid"]))

    def test_cleanup_failure_is_truthful_and_thread_terminates(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            handle = self.start(cwd)
            self.wait_for_output(handle)
            leader_pid = handle.identity.leader_pid
            real_kill = os.kill

            def suppress(pid: int, signal_number: int) -> None:
                if pid == leader_pid and signal_number in (
                    signal.SIGTERM,
                    signal.SIGKILL,
                ):
                    return
                real_kill(pid, signal_number)

            try:
                with (
                    mock.patch.object(
                        managed_process, "MANAGED_TERMINATE_GRACE_SECONDS", 0.05
                    ),
                    mock.patch.object(
                        managed_process, "MANAGED_KILL_GRACE_SECONDS", 0.05
                    ),
                    mock.patch.object(managed_process.os, "kill", side_effect=suppress),
                ):
                    failure = handle.close()
                self.assertIsInstance(failure, bounded_process.FailureResult)
                self.assertFalse(failure.cleanup_complete)
                self.assertTrue(
                    any(row["reaped"] is False for row in failure.process_ledger)
                )
                real_kill(leader_pid, 0)
            finally:
                try:
                    real_kill(leader_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    os.waitpid(leader_pid, 0)
                except ChildProcessError:
                    pass
            self.assertFalse(handle._thread.is_alive())

    def test_terminal_receipt_construction_fault_never_strands_waiters(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            handle = self.start(cwd)
            self.wait_for_output(handle)
            with (
                mock.patch.object(
                    handle,
                    "_ledger",
                    side_effect=bounded_process.BoundedProcessError(
                        "injected receipt-construction fault"
                    ),
                ),
                self.assertRaisesRegex(
                    bounded_process.BoundedProcessError,
                    "terminated without a terminal receipt",
                ),
            ):
                handle.close()
            handle._thread.join(timeout=3)
            self.assertFalse(handle._thread.is_alive())
            self.assert_pid_gone(handle.identity.leader_pid)
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError,
                "terminated without a terminal receipt",
            ):
                handle.health()

    def test_managed_receipt_schema_rejects_self_consistent_mutations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cwd = pathlib.Path(temporary).resolve()
            success_handle = self.start(cwd)
            self.wait_for_output(success_handle)
            success = dict(success_handle.finalize().receipt)
            failure_handle = self.start(cwd, "--exit-early")
            deadline = time.monotonic() + 3
            failure: dict[str, object] | None = None
            while time.monotonic() < deadline:
                try:
                    failure_handle.health()
                except bounded_process.BoundedProcessFailure as error:
                    failure = dict(error.result.receipt)
                    break
                time.sleep(0.01)
            self.assertIsNotNone(failure)
            assert failure is not None

            for receipt, is_failure, validator in (
                (success, False, managed_process.validate_managed_execution_receipt),
                (failure, True, managed_process.validate_managed_failure_receipt),
            ):
                for path in (
                    ("graceful_stop_timeout_seconds",),
                    ("stop_request", "schema_version"),
                    ("stop_request", "signal"),
                    ("stop_request", "target_pid"),
                ):
                    with self.subTest(failure=is_failure, path=path):
                        mutated = copy.deepcopy(receipt)
                        target: object = mutated
                        for component in path[:-1]:
                            target = target[component]  # type: ignore[index]
                        target[path[-1]] = True  # type: ignore[index]
                        self.rehash(mutated, failure=is_failure)
                        with self.assertRaises(bounded_process.BoundedProcessError):
                            validator(mutated)
                extra = copy.deepcopy(receipt)
                extra["stop_request"]["extra"] = None
                self.rehash(extra, failure=is_failure)
                with self.assertRaises(bounded_process.BoundedProcessError):
                    validator(extra)

            serialized = managed_process.dump_managed_execution_receipt(success)
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "repeats key"
            ):
                managed_process.load_managed_execution_receipt(
                    serialized.replace(b"{", b'{"kind":"duplicate",', 1)
                )
            with self.assertRaisesRegex(
                bounded_process.BoundedProcessError, "non-finite"
            ):
                managed_process.load_managed_execution_receipt(
                    serialized.replace(b'"schema_version":1', b'"schema_version":NaN')
                )

    def test_reserved_fd_collision_preserves_parent_descriptor(self) -> None:
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
            before = os.fstat(bounded_process.OWNERSHIP_MARKER_FD)
            with tempfile.TemporaryDirectory() as temporary:
                cwd = pathlib.Path(temporary).resolve()
                handle = self.start(cwd)
                self.wait_for_output(handle)
                result = handle.finalize()
            self.assertTrue(result.receipt["ownership_marker"]["parent_fd_collision"])
            after = os.fstat(bounded_process.OWNERSHIP_MARKER_FD)
            self.assertEqual(
                (before.st_dev, before.st_ino), (after.st_dev, after.st_ino)
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


if __name__ == "__main__":
    unittest.main()
