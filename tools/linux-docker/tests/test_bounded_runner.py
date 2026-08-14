from __future__ import annotations

import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

TOOLS = pathlib.Path(__file__).resolve().parents[1]
SHARED = TOOLS.parent / "evidence_common"
RUNNER = TOOLS / "bounded_runner.py"
sys.path.insert(0, str(TOOLS))
sys.path.insert(0, str(SHARED))

import bounded_process  # noqa: E402
import evidence  # noqa: E402


def child_environment() -> dict[str, str]:
    """The environment every child of this module gets.

    `PYTHONDONTWRITEBYTECODE` is not decoration. These tests spawn
    `tools/linux-docker/bounded_runner.py`, which imports `evidence` and
    `source_digest` from beside itself, and the constructed `env=` used to carry
    only `PATH` -- so the guard the parent was launched with was dropped at the
    boundary and every run left `tools/linux-docker/__pycache__/evidence.pyc`
    and `.../source_digest.pyc` in the tracked tree. `scripts/gate-a-residue.sh`
    then failed the whole gate on three findings whose cause was this line.
    """

    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "PYTHONDONTWRITEBYTECODE": "1",
    }


class BoundedRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name).resolve()
        self.root.chmod(0o700)
        self.executable = pathlib.Path(sys.executable).resolve()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def invoke(
        self,
        code: str,
        *,
        maximum_output_bytes: int = 4096,
        timeout_seconds: int = 5,
        suffix: str = "run",
        extra_runner_arguments: tuple[str, ...] = (),
    ) -> tuple[
        subprocess.CompletedProcess[str], pathlib.Path, pathlib.Path, pathlib.Path
    ]:
        stdout = self.root / f"{suffix}.stdout"
        stderr = self.root / f"{suffix}.stderr"
        receipt = self.root / f"{suffix}.receipt.json"
        result = subprocess.run(
            [
                str(RUNNER),
                "--cwd",
                str(self.root),
                "--timeout-seconds",
                str(timeout_seconds),
                "--drain-timeout-seconds",
                "1",
                "--maximum-output-bytes",
                str(maximum_output_bytes),
                "--stdout",
                str(stdout),
                "--stderr",
                str(stderr),
                "--receipt",
                str(receipt),
                "--description",
                "bounded runner test",
                "--env",
                "PATH=/usr/bin:/bin",
                *extra_runner_arguments,
                "--",
                str(self.executable),
                "-c",
                code,
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=15,
            check=False,
            env=child_environment(),
        )
        return result, stdout, stderr, receipt

    def test_success_publishes_exact_spools_and_full_receipt(self) -> None:
        result, stdout, stderr, receipt_path = self.invoke(
            "import sys; print('out'); print('err', file=sys.stderr)"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(stdout.read_bytes(), b"out\n")
        self.assertEqual(stderr.read_bytes(), b"err\n")
        receipt = bounded_process.load_execution_receipt(receipt_path.read_bytes())
        self.assertEqual(result.stdout, receipt["receipt_sha256"] + "\n")
        self.assertEqual(receipt["stdout_size"], 4)
        self.assertEqual(receipt["stderr_size"], 4)
        self.assertTrue(receipt["process_ledger"])
        self.assertTrue(all(row["reaped"] for row in receipt["process_ledger"]))

    def test_spawning_the_runner_writes_no_bytecode_into_the_source_tree(self) -> None:
        """Measured, not asserted about the environment dict.

        `scripts/gate-a-residue.sh` fails the gate on any `__pycache__` or
        `*.pyc` under the source root, and this module is what put them there:
        the runner imports `evidence` and `source_digest` from beside itself, and
        the constructed `env=` dropped the guard the parent ran under. The
        observation is the cache directory next to the runner, before and after.
        """

        cache = RUNNER.parent / "__pycache__"
        before = {path.name for path in cache.glob("*.pyc")} if cache.is_dir() else None
        result, *_ = self.invoke("raise SystemExit(0)", suffix="bytecode")
        self.assertEqual(result.returncode, 0, result.stderr)
        after = {path.name for path in cache.glob("*.pyc")} if cache.is_dir() else None
        if before is None:
            self.assertIsNone(after, f"the runner created {cache}")
        else:
            self.assertEqual(after - before, set(), f"the runner added to {cache}")

    def test_nonzero_exit_is_receipted_and_propagated(self) -> None:
        result, _stdout, _stderr, receipt_path = self.invoke(
            "raise SystemExit(7)", suffix="nonzero"
        )
        self.assertEqual(result.returncode, 7, result.stderr)
        receipt = bounded_process.load_execution_receipt(receipt_path.read_bytes())
        self.assertEqual(receipt["exit_code"], 7)
        self.assertEqual(result.stdout, receipt["receipt_sha256"] + "\n")

    def test_output_overflow_publishes_an_exact_failure_receipt(self) -> None:
        result, stdout, _stderr, receipt_path = self.invoke(
            "import sys,time; sys.stdout.write('x'*8192); sys.stdout.flush(); time.sleep(5)",
            maximum_output_bytes=128,
            suffix="overflow",
        )
        self.assertEqual(result.returncode, 124, result.stderr)
        receipt = bounded_process.load_failure_receipt(receipt_path.read_bytes())
        self.assertEqual(receipt["failure_reason"], "output_limit")
        self.assertTrue(receipt["cleanup_complete"])
        self.assertFalse(receipt["output_complete"])
        self.assertEqual(result.stdout, receipt["receipt_sha256"] + "\n")
        self.assertLessEqual(stdout.stat().st_size, 128)

    def test_timeout_publishes_an_exact_reaped_failure_receipt(self) -> None:
        result, _stdout, _stderr, receipt_path = self.invoke(
            "import time; time.sleep(10)",
            timeout_seconds=1,
            suffix="timeout",
        )
        self.assertEqual(result.returncode, 124, result.stderr)
        receipt = bounded_process.load_failure_receipt(receipt_path.read_bytes())
        self.assertEqual(receipt["failure_reason"], "timeout")
        self.assertTrue(receipt["cleanup_complete"])
        self.assertTrue(receipt["output_complete"])
        self.assertTrue(receipt["process_observation_complete"])
        self.assertTrue(all(row["reaped"] for row in receipt["process_ledger"]))

    def test_duplicate_environment_and_existing_outputs_fail_closed(self) -> None:
        result, stdout, stderr, receipt = self.invoke(
            "raise SystemExit(0)",
            suffix="duplicate-env",
            extra_runner_arguments=("--env", "PATH=/different"),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("environment", result.stderr)
        self.assertFalse(stdout.exists())
        self.assertFalse(stderr.exists())
        self.assertFalse(receipt.exists())

        protected = self.root / "protected"
        protected.write_bytes(b"preserve")
        symlink = self.root / "symlink.stdout"
        symlink.symlink_to(protected)
        result = subprocess.run(
            [
                str(RUNNER),
                "--cwd",
                str(self.root),
                "--timeout-seconds",
                "5",
                "--drain-timeout-seconds",
                "1",
                "--maximum-output-bytes",
                "1024",
                "--stdout",
                str(symlink),
                "--stderr",
                str(self.root / "symlink.stderr"),
                "--receipt",
                str(self.root / "symlink.receipt"),
                "--description",
                "symlink rejection",
                "--",
                str(self.executable),
                "-c",
                "raise SystemExit(0)",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=15,
            check=False,
            env=child_environment(),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(protected.read_bytes(), b"preserve")

    def test_receipt_loader_rejects_duplicate_and_nonfinite_json(self) -> None:
        result, _stdout, _stderr, receipt_path = self.invoke(
            "raise SystemExit(0)", suffix="strict-json"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = evidence._stable_regular_bytes(
            receipt_path,
            description="test receipt",
            maximum_bytes=4 * 1024 * 1024,
        )
        with self.assertRaises(bounded_process.BoundedProcessError):
            bounded_process.load_execution_receipt(
                payload.replace(b"{", b'{"schema_version":1,', 1)
            )
        with self.assertRaises(bounded_process.BoundedProcessError):
            bounded_process.load_execution_receipt(
                payload.replace(b'"schema_version":1', b'"schema_version":NaN')
            )


if __name__ == "__main__":
    unittest.main()
