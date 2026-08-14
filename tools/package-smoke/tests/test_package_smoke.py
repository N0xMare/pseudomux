from __future__ import annotations

import base64
import csv
import hashlib
import importlib.metadata
import importlib.util
import io
import json
import os
import shutil
import stat
import sys
import sysconfig
import tarfile
import tempfile
import time
import unittest
import zipfile
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).resolve().parents[1] / "package_smoke.py"
SPEC = importlib.util.spec_from_file_location("pmux_package_smoke", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
package_smoke = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = package_smoke
SPEC.loader.exec_module(package_smoke)


class PackageSmokeUnitTests(unittest.TestCase):
    def run_python_command(
        self,
        cwd: Path,
        script: str,
        *arguments: str,
        timeout: int = 5,
        drain_timeout: int = 1,
        maximum_output_bytes: int = 4096,
        environment: dict[str, str] | None = None,
        stdin_bytes: bytes | None = None,
    ) -> package_smoke.PackageCommandResult:
        return package_smoke.run_checked(
            [str(Path(sys.executable).resolve(strict=True)), "-c", script, *arguments],
            cwd=cwd,
            environment=(
                {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}
                if environment is None
                else environment
            ),
            label="package adapter unit command",
            timeout=timeout,
            drain_timeout=drain_timeout,
            maximum_output_bytes=maximum_output_bytes,
            stdin_bytes=stdin_bytes,
        )

    def assert_pid_gone(self, pid: int) -> None:
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.05)
        self.fail(f"package command adapter left PID {pid} alive")

    @staticmethod
    def write_tar(path: Path, entries: list[tuple[str, bytes]]) -> None:
        with tarfile.open(path, mode="w:gz") as archive:
            for name, payload in entries:
                member = tarfile.TarInfo(name)
                member.size = len(payload)
                member.mode = 0o600
                archive.addfile(member, io.BytesIO(payload))

    @staticmethod
    def write_zip(path: Path, entries: list[tuple[str, bytes]]) -> None:
        with zipfile.ZipFile(
            path, mode="w", compression=zipfile.ZIP_DEFLATED
        ) as archive:
            for name, payload in entries:
                archive.writestr(name, payload)

    def test_archive_names_are_canonical_and_cannot_escape(self) -> None:
        self.assertEqual(
            package_smoke.safe_archive_name("package/dist/index.js"),
            "package/dist/index.js",
        )
        for value in (
            "",
            "/absolute",
            "C:/drive-escape",
            "../escape",
            "package/../escape",
            "package\\escape",
            "package//escape",
            "package/./escape",
            "package/escape\x00tail",
        ):
            with self.assertRaises(package_smoke.SmokeError, msg=value):
                package_smoke.safe_archive_name(value)

    def test_owned_temporary_root_removes_only_its_exact_tree(self) -> None:
        with package_smoke.OwnedTemporaryRoot("pmux-package-smoke-unit-") as root:
            retained = root
            (root / "nested").mkdir()
            (root / "nested/file").write_text("temporary", encoding="utf-8")
        self.assertFalse(retained.exists())

    def test_owned_temporary_root_refuses_to_remove_a_replacement(self) -> None:
        owned = package_smoke.OwnedTemporaryRoot("pmux-package-smoke-unit-")
        replacement = owned.path
        original = replacement.with_name(f"{replacement.name}-original")
        replacement.rename(original)
        replacement.mkdir(mode=0o700)
        marker = replacement / "do-not-remove"
        marker.write_text("replacement", encoding="utf-8")
        try:
            with self.assertRaises(package_smoke.SmokeError):
                owned.__exit__(None, None, None)
            self.assertEqual(marker.read_text(encoding="utf-8"), "replacement")
        finally:
            shutil.rmtree(replacement)
            shutil.rmtree(original)

    def test_build_environment_does_not_inherit_credentials_or_injection(self) -> None:
        secret_names = (
            "ANTHROPIC_API_KEY",
            "NODE_OPTIONS",
            "PIP_INDEX_URL",
            "PYTHONPATH",
            "SSH_AUTH_SOCK",
            "https_proxy",
        )
        injected = {name: f"secret-{name}" for name in secret_names}
        with package_smoke.OwnedTemporaryRoot("pmux-package-smoke-unit-") as root:
            with patch.dict(os.environ, injected, clear=False):
                environment = package_smoke.deterministic_environment(
                    root, executables=(Path(sys.executable),)
                )
            self.assertTrue(set(secret_names).isdisjoint(environment))
            self.assertEqual(environment["PIP_NO_INDEX"], "1")
            self.assertEqual(environment["npm_config_offline"], "true")
            self.assertTrue(Path(environment["TMPDIR"]).is_relative_to(root))

    def test_bounded_command_receipt_binds_environment_stdin_and_authority(
        self,
    ) -> None:
        environment = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "PMUX_PACKAGE_CONTEXT": "environment-value-not-for-receipt",
        }
        stdin_payload = b"stdin-value-not-for-receipt"
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory).resolve()
            result = self.run_python_command(
                cwd,
                "import hashlib,os,sys; data=sys.stdin.buffer.read(); "
                "print(os.environ['PMUX_PACKAGE_CONTEXT']); "
                "print(hashlib.sha256(data).hexdigest())",
                environment=environment,
                stdin_bytes=stdin_payload,
            )
        self.assertEqual(
            result.stdout.splitlines(),
            [
                environment["PMUX_PACKAGE_CONTEXT"],
                hashlib.sha256(stdin_payload).hexdigest(),
            ],
        )
        receipt = result.receipt
        self.assertEqual(receipt["kind"], "pmux_bounded_process")
        self.assertEqual(receipt["environment"]["names"], sorted(environment))
        self.assertEqual(receipt["standard_input"]["source"], "bytes")
        self.assertEqual(receipt["standard_input"]["size"], len(stdin_payload))
        self.assertEqual(
            receipt["standard_input"]["sha256"],
            hashlib.sha256(stdin_payload).hexdigest(),
        )
        rendered = json.dumps(receipt, sort_keys=True)
        self.assertNotIn(environment["PMUX_PACKAGE_CONTEXT"], rendered)
        self.assertNotIn(stdin_payload.decode("ascii"), rendered)
        with package_smoke.BoundedProcessAuthority() as authority:
            self.assertEqual(
                authority.report(),
                {
                    "path": package_smoke.BOUNDED_PROCESS_RELATIVE_PATH,
                    "bytes": (
                        MODULE_PATH.parents[1]
                        / "evidence_common"
                        / "bounded_process.py"
                    )
                    .stat()
                    .st_size,
                    "sha256": package_smoke.EXPECTED_BOUNDED_PROCESS_SHA256,
                },
            )

    def test_bounded_command_output_limit_has_exact_failure_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(package_smoke.PackageCommandFailure) as caught:
                self.run_python_command(
                    Path(directory).resolve(),
                    "import sys; sys.stdout.buffer.write(b'x'*4096); "
                    "sys.stdout.flush()",
                    maximum_output_bytes=128,
                )
        receipt = caught.exception.receipt
        self.assertEqual(receipt["kind"], "pmux_bounded_process_failure")
        self.assertEqual(receipt["failure_reason"], "output_limit")
        self.assertTrue(receipt["cleanup_complete"])
        self.assertFalse(receipt["output_complete"])
        self.assertEqual(receipt["stdout_size"], 128)

    def test_bounded_command_timeout_has_exact_failure_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            started = time.monotonic()
            with self.assertRaises(package_smoke.PackageCommandFailure) as caught:
                self.run_python_command(
                    Path(directory).resolve(),
                    "import time; time.sleep(30)",
                    timeout=1,
                )
        self.assertLess(time.monotonic() - started, 5)
        receipt = caught.exception.receipt
        self.assertEqual(receipt["failure_reason"], "timeout")
        self.assertTrue(receipt["cleanup_complete"])

    def test_bounded_command_reaps_inherited_pipe_holder(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory).resolve()
            child_pid = cwd / "child-pid"
            script = (
                "import subprocess,sys; "
                "child=subprocess.Popen([sys.executable,'-c',"
                "'import time; time.sleep(30)']); "
                "open(sys.argv[1],'w').write(str(child.pid))"
            )
            with self.assertRaises(package_smoke.PackageCommandFailure) as caught:
                self.run_python_command(cwd, script, str(child_pid))
            pid = int(child_pid.read_text(encoding="utf-8"))
            self.assert_pid_gone(pid)
        self.assertEqual(caught.exception.receipt["failure_reason"], "drain_timeout")
        self.assertTrue(caught.exception.receipt["cleanup_complete"])

    def test_bounded_command_reaps_double_fork_setsid_descendant(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory).resolve()
            child_pid = cwd / "escaped-pid"
            escaped = (
                "import os,sys,time; first=os.fork(); "
                "(os._exit(0) if first else None); os.setsid(); second=os.fork(); "
                "(os._exit(0) if second else None); os.close(0); os.close(1); "
                "os.close(2); open(sys.argv[1],'w').write(str(os.getpid())); "
                "time.sleep(30)"
            )
            leader = (
                "import os,sys,time; child=os.fork(); "
                "(os.execv(sys.executable,[sys.executable,'-c',sys.argv[2],sys.argv[1]]) "
                "if child==0 else None); deadline=time.monotonic()+2; "
                "\nwhile not os.path.exists(sys.argv[1]) and time.monotonic()<deadline: "
                "time.sleep(0.01)\nraise SystemExit(0)"
            )
            with self.assertRaises(package_smoke.PackageCommandFailure) as caught:
                self.run_python_command(
                    cwd,
                    leader,
                    str(child_pid),
                    escaped,
                )
            pid = int(child_pid.read_text(encoding="utf-8"))
            self.assert_pid_gone(pid)
        self.assertEqual(
            caught.exception.receipt["failure_reason"], "descendant_survived"
        )
        self.assertTrue(caught.exception.receipt["cleanup_complete"])

    def test_file_manifest_is_stable_and_detects_content_changes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "a").mkdir()
            path = root / "a/value"
            path.write_text("first", encoding="utf-8")
            first = package_smoke.file_manifest(root)
            self.assertEqual(first, package_smoke.file_manifest(root))
            path.write_text("second", encoding="utf-8")
            second = package_smoke.file_manifest(root)
            self.assertNotEqual(first, second)

    def test_wheel_record_requires_exact_hash_size_and_file_set(self) -> None:
        dist_info = "pmux_client-0.1.0.dist-info"
        payloads = {
            "pmux_client/__init__.py": b"VERSION = 1\n",
            f"{dist_info}/METADATA": b"Name: pmux-client\nVersion: 0.1.0\n\n",
        }
        rows: list[list[str]] = []
        for name, payload in payloads.items():
            encoded = (
                base64.urlsafe_b64encode(hashlib.sha256(payload).digest())
                .rstrip(b"=")
                .decode()
            )
            rows.append([name, f"sha256={encoded}", str(len(payload))])
        record_name = f"{dist_info}/RECORD"
        rows.append([record_name, "", ""])
        stream = io.StringIO()
        csv.writer(stream, lineterminator="\n").writerows(rows)
        record_payload = stream.getvalue().encode()
        file_entries = {
            name: {
                "bytes": len(payload),
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
            for name, payload in {**payloads, record_name: record_payload}.items()
        }
        package_smoke.validate_wheel_record(
            file_entries,
            record_payload,
            dist_info,
        )

        corrupted = {name: dict(identity) for name, identity in file_entries.items()}
        corrupted["pmux_client/__init__.py"]["sha256"] = hashlib.sha256(
            b"VERSION = 2\n"
        ).hexdigest()
        with self.assertRaises(package_smoke.SmokeError):
            package_smoke.validate_wheel_record(corrupted, record_payload, dist_info)

        missing = dict(file_entries)
        missing["pmux_client/extra.py"] = {
            "bytes": 0,
            "sha256": hashlib.sha256(b"").hexdigest(),
        }
        with self.assertRaises(package_smoke.SmokeError):
            package_smoke.validate_wheel_record(missing, record_payload, dist_info)

    def test_strict_json_rejects_duplicate_keys_and_nonfinite_numbers(self) -> None:
        self.assertEqual(
            package_smoke.strict_json_loads(
                '{"outer":{"value":1},"items":[2]}',
                label="unit report",
            ),
            {"outer": {"value": 1}, "items": [2]},
        )
        for payload in (
            '{"value":1,"value":2}',
            '{"outer":{"value":1,"value":2}}',
            '{"value":NaN}',
            '{"value":Infinity}',
            '{"value":-Infinity}',
        ):
            with self.subTest(payload=payload):
                with self.assertRaises(package_smoke.SmokeError):
                    package_smoke.strict_json_loads(payload, label="unit report")

    def test_npm_report_hashes_must_bind_the_created_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "pmux-client.tgz"
            artifact.write_bytes(b"exact artifact bytes")
            identity = package_smoke.npm_artifact_identity(artifact)
            self.assertEqual(
                package_smoke.verify_npm_artifact_identity(identity, artifact),
                identity,
            )
            for field in ("shasum", "integrity"):
                with self.subTest(field=field):
                    changed = dict(identity)
                    changed[field] += "changed"
                    with self.assertRaises(package_smoke.SmokeError):
                        package_smoke.verify_npm_artifact_identity(changed, artifact)

    def test_archive_entry_streaming_does_not_retain_ordinary_payloads(self) -> None:
        payload = b"x" * (package_smoke.ARCHIVE_READ_CHUNK_BYTES + 17)
        identity, retained = package_smoke.stream_archive_entry(
            io.BytesIO(payload),
            declared_size=len(payload),
            label="unit archive entry",
            retain=False,
        )
        self.assertIsNone(retained)
        self.assertEqual(identity["bytes"], len(payload))
        self.assertEqual(identity["sha256"], hashlib.sha256(payload).hexdigest())

    def test_typescript_archive_rejects_entry_and_cumulative_bombs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            entry_bomb = root / "entry-bomb.tgz"
            self.write_tar(entry_bomb, [("package/large", b"0" * 4096)])
            with patch.object(package_smoke, "MAX_ARCHIVE_ENTRY_BYTES", 8):
                with self.assertRaisesRegex(package_smoke.SmokeError, "per-entry"):
                    package_smoke.validate_typescript_archive(entry_bomb, {})

            cumulative_bomb = root / "cumulative-bomb.tgz"
            self.write_tar(
                cumulative_bomb,
                [(f"package/entry-{index}", b"12") for index in range(8)],
            )
            with (
                patch.object(package_smoke, "MAX_ARCHIVE_ENTRY_BYTES", 8),
                patch.object(package_smoke, "MAX_ARCHIVE_DECOMPRESSED_BYTES", 10),
            ):
                with self.assertRaisesRegex(package_smoke.SmokeError, "cumulative"):
                    package_smoke.validate_typescript_archive(cumulative_bomb, {})

    def test_wheel_rejects_bombs_path_hazards_and_nonregular_modes(self) -> None:
        source_project = {
            "name": "pmux-client",
            "version": "0.1.0",
            "requires-python": ">=3.11",
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            entry_bomb = root / "entry-bomb.whl"
            self.write_zip(entry_bomb, [("pmux_client/large.py", b"0" * 4096)])
            with patch.object(package_smoke, "MAX_ARCHIVE_ENTRY_BYTES", 8):
                with self.assertRaisesRegex(package_smoke.SmokeError, "per-entry"):
                    package_smoke.validate_python_wheel(
                        entry_bomb,
                        source_project=source_project,
                    )

            cumulative_bomb = root / "cumulative-bomb.whl"
            self.write_zip(
                cumulative_bomb,
                [(f"pmux_client/entry_{index}.py", b"12") for index in range(8)],
            )
            with (
                patch.object(package_smoke, "MAX_ARCHIVE_ENTRY_BYTES", 8),
                patch.object(package_smoke, "MAX_ARCHIVE_DECOMPRESSED_BYTES", 10),
            ):
                with self.assertRaisesRegex(package_smoke.SmokeError, "cumulative"):
                    package_smoke.validate_python_wheel(
                        cumulative_bomb,
                        source_project=source_project,
                    )

            hazardous = root / "hazardous.whl"
            self.write_zip(hazardous, [("../escape", b"outside")])
            with self.assertRaisesRegex(
                package_smoke.SmokeError, "unsafe archive path"
            ):
                package_smoke.validate_python_wheel(
                    hazardous,
                    source_project=source_project,
                )

            nonregular = root / "nonregular.whl"
            with zipfile.ZipFile(nonregular, mode="w") as archive:
                info = zipfile.ZipInfo("pmux_client/pipe")
                info.create_system = 3
                info.external_attr = (stat.S_IFIFO | 0o600) << 16
                archive.writestr(info, b"")
            with self.assertRaisesRegex(package_smoke.SmokeError, "non-regular"):
                package_smoke.validate_python_wheel(
                    nonregular,
                    source_project=source_project,
                )

    def test_wheel_entry_count_is_bounded_before_zipfile_allocation(self) -> None:
        source_project = {
            "name": "pmux-client",
            "version": "0.1.0",
            "requires-python": ">=3.11",
        }
        with tempfile.TemporaryDirectory() as directory:
            wheel = Path(directory) / "too-many.whl"
            self.write_zip(
                wheel,
                [
                    (f"pmux_client/entry_{index}.py", b"")
                    for index in range(package_smoke.MAX_ARCHIVE_ENTRIES + 1)
                ],
            )
            with patch.object(
                package_smoke.zipfile,
                "ZipFile",
                side_effect=AssertionError("ZipFile must not parse an oversized index"),
            ):
                with self.assertRaisesRegex(package_smoke.SmokeError, "too many"):
                    package_smoke.validate_python_wheel(
                        wheel,
                        source_project=source_project,
                    )

    def test_valid_archives_are_streamed_and_fully_validated(self) -> None:
        typescript_manifest = {
            "name": "pmux-client",
            "version": "0.1.0",
            "type": "module",
            "main": "dist/index.js",
            "types": "dist/index.d.ts",
            "exports": {".": "./dist/index.js"},
            "engines": {"node": ">=18"},
            "files": ["dist", "README.md"],
        }
        python_project = {
            "name": "pmux-client",
            "version": "0.1.0",
            "requires-python": ">=3.11",
        }
        dist_info = "pmux_client-0.1.0.dist-info"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tar_path = root / "pmux-client.tgz"
            tar_entries = [
                (name, b"export {};\n")
                for name in sorted(
                    package_smoke.TYPESCRIPT_REQUIRED_FILES - {"package/package.json"}
                )
            ]
            tar_entries.append(
                (
                    "package/package.json",
                    json.dumps(typescript_manifest).encode("utf-8"),
                )
            )
            self.write_tar(tar_path, tar_entries)
            tar_report = package_smoke.validate_typescript_archive(
                tar_path,
                typescript_manifest,
            )
            self.assertEqual(tar_report["entry_count"], len(tar_entries))

            wheel_path = root / "pmux_client-0.1.0-py3-none-any.whl"
            wheel_payloads = {
                name: b"# packaged\n"
                for name in package_smoke.PYTHON_REQUIRED_PACKAGE_FILES
            }
            wheel_payloads[f"{dist_info}/METADATA"] = (
                b"Name: pmux-client\nVersion: 0.1.0\nRequires-Python: >=3.11\n\n"
            )
            wheel_payloads[f"{dist_info}/WHEEL"] = (
                b"Root-Is-Purelib: true\nTag: py3-none-any\n\n"
            )
            wheel_payloads[f"{dist_info}/top_level.txt"] = b"pmux_client\n"
            record_name = f"{dist_info}/RECORD"
            record_rows = []
            for name, payload in sorted(wheel_payloads.items()):
                encoded = (
                    base64.urlsafe_b64encode(hashlib.sha256(payload).digest())
                    .rstrip(b"=")
                    .decode("ascii")
                )
                record_rows.append([name, f"sha256={encoded}", str(len(payload))])
            record_rows.append([record_name, "", ""])
            record_stream = io.StringIO()
            csv.writer(record_stream, lineterminator="\n").writerows(record_rows)
            wheel_payloads[record_name] = record_stream.getvalue().encode("utf-8")
            self.write_zip(wheel_path, sorted(wheel_payloads.items()))
            wheel_report = package_smoke.validate_python_wheel(
                wheel_path,
                source_project=python_project,
            )
            self.assertEqual(wheel_report["entry_count"], len(wheel_payloads))

    def test_descriptor_tree_rejects_symlink_hardlink_special_and_bounds(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            outside = base / "outside"
            outside.write_text("outside", encoding="utf-8")

            symlink_root = base / "symlink-root"
            symlink_root.mkdir()
            os.symlink(outside, symlink_root / "alias")
            with self.assertRaisesRegex(package_smoke.SmokeError, "symlink"):
                package_smoke.tree_snapshot(symlink_root)

            hardlink_root = base / "hardlink-root"
            hardlink_root.mkdir()
            first = hardlink_root / "first"
            first.write_text("same inode", encoding="utf-8")
            os.link(first, hardlink_root / "second")
            with self.assertRaisesRegex(package_smoke.SmokeError, "multiply linked"):
                package_smoke.tree_snapshot(hardlink_root)

            special_root = base / "special-root"
            special_root.mkdir()
            os.mkfifo(special_root / "pipe", mode=0o600)
            with self.assertRaisesRegex(package_smoke.SmokeError, "special node"):
                package_smoke.tree_snapshot(special_root)

            bounded = base / "bounded"
            bounded.mkdir()
            (bounded / "one").write_bytes(b"1234")
            (bounded / "two").write_bytes(b"5678")
            with self.assertRaisesRegex(package_smoke.SmokeError, "entry-count"):
                package_smoke.tree_snapshot(bounded, max_entries=2)
            with self.assertRaisesRegex(package_smoke.SmokeError, "cumulative byte"):
                package_smoke.tree_snapshot(bounded, max_bytes=7)
            nested = bounded / "nested"
            nested.mkdir()
            (nested / "deeper").mkdir()
            with self.assertRaisesRegex(package_smoke.SmokeError, "depth"):
                package_smoke.tree_snapshot(bounded, max_depth=1)

    def test_anchored_directory_and_artifact_replacements_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / "root"
            root.mkdir()
            (root / "value").write_text("original", encoding="utf-8")
            anchor = package_smoke.AnchoredDirectory(root)
            original = base / "root-original"
            root.rename(original)
            root.mkdir()
            marker = root / "replacement"
            marker.write_text("untouched", encoding="utf-8")
            try:
                with self.assertRaisesRegex(
                    package_smoke.SmokeError, "membership changed"
                ):
                    anchor.snapshot()
                self.assertEqual(marker.read_text(encoding="utf-8"), "untouched")
            finally:
                anchor.close()

            artifact = base / "artifact.tgz"
            artifact.write_bytes(b"original artifact")
            artifact_anchor = package_smoke.AnchoredRegularFile(artifact)
            displaced = base / "artifact-original.tgz"
            artifact.rename(displaced)
            artifact.write_bytes(b"replacement artifact")
            try:
                with self.assertRaisesRegex(
                    package_smoke.SmokeError, "identity or content changed"
                ):
                    artifact_anchor.verify()
                self.assertEqual(artifact.read_bytes(), b"replacement artifact")
            finally:
                artifact_anchor.close()

    def test_tree_file_swap_during_hash_is_rejected_and_replacement_untouched(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "tree"
            root.mkdir()
            value = root / "value"
            value.write_bytes(b"original")
            displaced = root / "displaced"
            real_pread = os.pread
            swapped = False

            def swap_after_read(descriptor: int, size: int, offset: int) -> bytes:
                nonlocal swapped
                payload = real_pread(descriptor, size, offset)
                if not swapped:
                    swapped = True
                    value.rename(displaced)
                    value.write_bytes(b"replacement")
                return payload

            with patch.object(package_smoke.os, "pread", side_effect=swap_after_read):
                with self.assertRaisesRegex(
                    package_smoke.SmokeError, "changed during read|membership changed"
                ):
                    package_smoke.tree_snapshot(root)
            self.assertEqual(value.read_bytes(), b"replacement")
            self.assertEqual(displaced.read_bytes(), b"original")

    def test_descriptor_cleanup_unlinks_nested_alias_without_touching_target(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            outside = Path(directory) / "outside"
            outside.mkdir()
            marker = outside / "marker"
            marker.write_text("retain", encoding="utf-8")
            with package_smoke.OwnedTemporaryRoot(
                "pmux-package-smoke-cleanup-"
            ) as root:
                nested = root / "nested"
                nested.mkdir()
                os.symlink(outside, nested / "outside-alias", target_is_directory=True)
            self.assertEqual(marker.read_text(encoding="utf-8"), "retain")

    def test_installed_typescript_closure_rejects_addition_and_removal(self) -> None:
        archive_entries = {
            "package/package.json": {
                "bytes": 3,
                "sha256": hashlib.sha256(b"{}\n").hexdigest(),
            },
            "package/dist/index.js": {
                "bytes": 11,
                "sha256": hashlib.sha256(b"export {};\n").hexdigest(),
            },
        }
        with tempfile.TemporaryDirectory() as directory:
            installed = Path(directory) / "pmux-client"
            (installed / "dist").mkdir(parents=True)
            (installed / "package.json").write_bytes(b"{}\n")
            (installed / "dist/index.js").write_bytes(b"export {};\n")
            snapshot = package_smoke.tree_snapshot(installed)
            package_smoke.validate_installed_typescript_closure(
                snapshot,
                archive_entries,
            )
            (installed / "extra.js").write_text("extra", encoding="utf-8")
            with self.assertRaisesRegex(package_smoke.SmokeError, "file closure"):
                package_smoke.validate_installed_typescript_closure(
                    package_smoke.tree_snapshot(installed),
                    archive_entries,
                )
            (installed / "extra.js").unlink()
            (installed / "dist/index.js").unlink()
            with self.assertRaisesRegex(package_smoke.SmokeError, "file closure"):
                package_smoke.validate_installed_typescript_closure(
                    package_smoke.tree_snapshot(installed),
                    archive_entries,
                )

    def test_installed_python_closure_binds_wheel_and_generated_metadata(self) -> None:
        dist_info = "pmux_client-0.1.0.dist-info"
        archive_payloads = {
            "pmux_client/__init__.py": b"VERSION = 1\n",
            f"{dist_info}/METADATA": b"Name: pmux-client\n\n",
            f"{dist_info}/WHEEL": b"Root-Is-Purelib: true\n\n",
            f"{dist_info}/RECORD": b"",
            f"{dist_info}/top_level.txt": b"pmux_client\n",
        }
        archive_entries = {
            name: {
                "bytes": len(payload),
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
            for name, payload in archive_payloads.items()
        }
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            artifact = base / "pmux_client.whl"
            artifact.write_bytes(b"exact wheel")
            installed = base / "installed"
            for name, payload in archive_payloads.items():
                path = installed / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(payload)
            with package_smoke.AnchoredRegularFile(artifact) as artifact_anchor:
                generated = installed / dist_info
                (generated / "INSTALLER").write_bytes(b"pip\n")
                (generated / "REQUESTED").write_bytes(b"")
                direct_url = {
                    "archive_info": {
                        "hash": f"sha256={artifact_anchor.sha256()}",
                        "hashes": {"sha256": artifact_anchor.sha256()},
                    },
                    "url": artifact_anchor.path.as_uri(),
                }
                direct_url_payload = json.dumps(direct_url, sort_keys=True).encode(
                    "utf-8"
                )
                (generated / "direct_url.json").write_bytes(direct_url_payload)
                installed_payloads = {
                    name: payload
                    for name, payload in archive_payloads.items()
                    if name != f"{dist_info}/RECORD"
                }
                installed_payloads.update(
                    {
                        f"{dist_info}/INSTALLER": b"pip\n",
                        f"{dist_info}/REQUESTED": b"",
                        f"{dist_info}/direct_url.json": direct_url_payload,
                    }
                )
                rows = []
                for name, payload in sorted(installed_payloads.items()):
                    encoded = (
                        base64.urlsafe_b64encode(hashlib.sha256(payload).digest())
                        .rstrip(b"=")
                        .decode("ascii")
                    )
                    rows.append([name, f"sha256={encoded}", str(len(payload))])
                rows.append([f"{dist_info}/RECORD", "", ""])
                record_stream = io.StringIO()
                csv.writer(record_stream, lineterminator="\n").writerows(rows)
                (generated / "RECORD").write_text(
                    record_stream.getvalue(),
                    encoding="utf-8",
                )
                with package_smoke.AnchoredDirectory(installed) as installed_anchor:
                    snapshot = installed_anchor.snapshot()
                    package_smoke.validate_installed_python_closure(
                        installed_anchor,
                        snapshot,
                        archive_entries,
                        dist_info=dist_info,
                        artifact=artifact_anchor,
                    )
                    (installed / "unexpected").write_text("extra", encoding="utf-8")
                    with self.assertRaisesRegex(
                        package_smoke.SmokeError, "file closure"
                    ):
                        package_smoke.validate_installed_python_closure(
                            installed_anchor,
                            installed_anchor.snapshot(),
                            archive_entries,
                            dist_info=dist_info,
                            artifact=artifact_anchor,
                        )

    @staticmethod
    def declared_file_record(
        role: str,
        usage: str,
        path: Path,
    ) -> dict[str, str]:
        with package_smoke.AnchoredRegularFile(
            path,
            maximum_bytes=package_smoke.MAX_TREE_BYTES,
        ) as anchor:
            portable, witness = package_smoke._declared_file_digests(anchor)
        return {
            "role": role,
            "kind": "file",
            "usage": usage,
            "path": str(path),
            "portable_sha256": portable,
            "witness_sha256": witness,
        }

    @staticmethod
    def declared_tree_record(
        role: str,
        path: Path,
        usage: str = "dependency_tree",
    ) -> dict[str, str]:
        snapshot = package_smoke.tree_snapshot(path)
        return {
            "role": role,
            "kind": "tree",
            "usage": usage,
            "path": str(path),
            "portable_sha256": snapshot.portable_sha256,
            "witness_sha256": snapshot.witness_sha256,
        }

    def create_declared_closure_fixture(
        self,
        base: Path,
        gate: str,
    ) -> tuple[dict[str, str], dict[str, Path]]:
        candidate = "1" * 64
        source = "2" * 64
        previous = "3" * 64
        paths: dict[str, Path] = {}
        if gate == "typescript":
            npm_support = base / "npm-support"
            typescript = base / "typescript"
            node_types = base / "node-types"
            undici_types = base / "undici-types"
            for directory in (
                npm_support,
                typescript / "bin",
                node_types,
                undici_types,
            ):
                directory.mkdir(parents=True)
            paths = {
                "node_executable": base / "node",
                "npm_executable": npm_support / "bin/npm-cli.js",
                "npm_support_tree": npm_support,
                "typescript_compiler": typescript / "bin/tsc",
                "typescript_dependency_tree": typescript,
                "node_types_dependency_tree": node_types,
                "undici_types_dependency_tree": undici_types,
            }
            for role in (
                "node_executable",
                "npm_executable",
                "typescript_compiler",
            ):
                paths[role].parent.mkdir(parents=True, exist_ok=True)
                paths[role].write_text(f"{role}\n", encoding="utf-8")
                paths[role].chmod(0o700)
            (node_types / "index.d.ts").write_text("types\n", encoding="utf-8")
            (undici_types / "index.d.ts").write_text("undici types\n", encoding="utf-8")
        elif gate == "python":
            stdlib = base / "python-stdlib"
            dynload = base / "python-dynload"
            build_support = base / "python-build-support"
            for directory in (
                stdlib,
                dynload,
                build_support / "pip",
                build_support / "setuptools",
            ):
                directory.mkdir(parents=True)
            paths = {
                "python_executable": base / "python",
                "python_stdlib_tree": stdlib,
                "python_dynload_tree": dynload,
                "python_build_support_tree": build_support,
            }
            paths["python_executable"].write_text("python\n", encoding="utf-8")
            paths["python_executable"].chmod(0o700)
            (stdlib / "os.py").write_text("# stdlib\n", encoding="utf-8")
            (dynload / "_ssl.so").write_bytes(b"extension")
            (build_support / "pip/__init__.py").write_text(
                "__version__ = '1'\n", encoding="utf-8"
            )
            (build_support / "setuptools/__init__.py").write_text(
                "__version__ = '1'\n", encoding="utf-8"
            )
        else:
            raise AssertionError(f"unknown closure fixture gate: {gate}")

        support = {
            "schema_version": 1,
            "kind": "pmux_gate_a_package_support_closure",
            "gate": gate,
            "candidate_manifest_sha256": candidate,
            "source_manifest_sha256": source,
            "previous_anchor_sha256": previous,
            "attestation_sha256": "4" * 64,
        }
        support["support_closure_sha256"] = package_smoke.domain_json_digest(
            support,
            domain=package_smoke.SUPPORT_CLOSURE_DOMAIN,
        )
        support_path = base / "support.json"
        support_path.write_text(
            json.dumps(support, sort_keys=True, separators=(",", ":")),
            encoding="utf-8",
        )
        support_path.chmod(0o600)
        paths["support_closure"] = support_path

        records = []
        for role, (kind, usage) in package_smoke.EXPECTED_DECLARED_INPUTS[gate].items():
            path = paths[role]
            records.append(
                self.declared_file_record(role, usage, path)
                if kind == "file"
                else self.declared_tree_record(role, path, usage)
            )
        manifest = {
            "schema_version": 1,
            "kind": "pmux_package_smoke_declared_closure",
            "gate": gate,
            "candidate_manifest_sha256": candidate,
            "source_manifest_sha256": source,
            "previous_anchor_sha256": previous,
            "inputs": records,
        }
        manifest["closure_sha256"] = package_smoke.domain_json_digest(
            manifest,
            domain=package_smoke.DECLARED_CLOSURE_DOMAIN,
        )
        manifest_path = base / "closure.json"
        manifest_payload = json.dumps(
            manifest,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
        manifest_path.write_bytes(manifest_payload)
        manifest_path.chmod(0o600)
        environment = {
            "PMUX_PACKAGE_SMOKE_CLOSURE_FILE": str(manifest_path),
            "PMUX_PACKAGE_SMOKE_CLOSURE_SHA256": hashlib.sha256(
                manifest_payload
            ).hexdigest(),
            "PMUX_PACKAGE_SMOKE_CANDIDATE_SHA256": candidate,
            "PMUX_PACKAGE_SMOKE_SOURCE_SHA256": source,
            "PMUX_PACKAGE_SMOKE_PREVIOUS_ANCHOR_SHA256": previous,
        }
        return environment, paths

    def replace_declared_fixture_paths(
        self,
        environment: dict[str, str],
        gate: str,
        paths: dict[str, Path],
    ) -> dict[str, str]:
        manifest_path = Path(environment["PMUX_PACKAGE_SMOKE_CLOSURE_FILE"])
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        records = []
        for role, (kind, usage) in package_smoke.EXPECTED_DECLARED_INPUTS[gate].items():
            path = paths[role]
            records.append(
                self.declared_file_record(role, usage, path)
                if kind == "file"
                else self.declared_tree_record(role, path, usage)
            )
        manifest["inputs"] = records
        manifest.pop("closure_sha256")
        manifest["closure_sha256"] = package_smoke.domain_json_digest(
            manifest,
            domain=package_smoke.DECLARED_CLOSURE_DOMAIN,
        )
        payload = json.dumps(
            manifest,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
        manifest_path.write_bytes(payload)
        manifest_path.chmod(0o600)
        return {
            **environment,
            "PMUX_PACKAGE_SMOKE_CLOSURE_SHA256": hashlib.sha256(payload).hexdigest(),
        }

    def rewrite_declared_fixture(
        self,
        environment: dict[str, str],
        mutate: object,
        *,
        recompute_digest: bool = True,
    ) -> dict[str, str]:
        manifest_path = Path(environment["PMUX_PACKAGE_SMOKE_CLOSURE_FILE"])
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        assert callable(mutate)
        mutate(manifest)
        if recompute_digest:
            manifest.pop("closure_sha256", None)
            manifest["closure_sha256"] = package_smoke.domain_json_digest(
                manifest,
                domain=package_smoke.DECLARED_CLOSURE_DOMAIN,
            )
        payload = json.dumps(
            manifest,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
        manifest_path.write_bytes(payload)
        manifest_path.chmod(0o600)
        return {
            **environment,
            "PMUX_PACKAGE_SMOKE_CLOSURE_SHA256": hashlib.sha256(payload).hexdigest(),
        }

    def require_python_build_support_interpreter(self) -> None:
        """The interpreter contract this fixture needs, checked before it is used.

        `package_smoke.build_python_package` never touches the ambient
        interpreter: it takes a DECLARED, hashed `python_build_support_tree` and
        refuses anything else. This fixture is what materializes that tree, and
        it does so out of whatever distributions the running interpreter
        happens to have installed -- so it silently required of the host exactly
        what the product requires of a declared input, and required it without
        saying so. Python 3.12 stopped shipping `setuptools` through
        `ensurepip`, and on a 3.13 that never had it the assumption surfaced as
        `PackageNotFoundError` from inside `importlib.metadata`, three frames
        below anything naming a package: a gate cell reporting a host property
        as a product failure.

        Named and checked here instead. The set is read from
        `package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS` -- the same tuple the
        validator enforces -- so this can never drift into checking for a
        different toolchain than the one the gate demands.
        """

        missing = []
        for name in package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS:
            try:
                importlib.metadata.distribution(name)
            except importlib.metadata.PackageNotFoundError:
                missing.append(name)
        if missing:
            self.skipTest(
                "this fixture materializes the declared Python build-support "
                "tree from the running interpreter, and "
                f"{Path(sys.executable)} (Python "
                f"{'.'.join(str(part) for part in sys.version_info[:3])}) "
                f"publishes no distribution metadata for: {', '.join(missing)}. "
                "The real Python package flow was NOT exercised. Re-run the "
                "cell under an interpreter that has "
                f"{' and '.join(package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS)} "
                "installed (`python3 -m pip install setuptools` is enough); "
                "the offline wheel build itself needs setuptools because "
                "clients/python/pyproject.toml declares "
                "setuptools.build_meta as its backend"
            )

    def materialize_python_support(self, base: Path) -> dict[str, Path]:
        self.require_python_build_support_interpreter()
        python = Path(sys.executable).resolve(strict=True)
        stdlib_source = Path(sysconfig.get_path("stdlib")).resolve(strict=True)
        dynload_source = Path(sysconfig.get_config_var("DESTSHARED")).resolve(
            strict=True
        )
        stdlib = base / "python-stdlib-materialized"
        dynload = base / "python-dynload-materialized"
        build_support = base / "python-build-support-materialized"
        shutil.copytree(
            stdlib_source,
            stdlib,
            symlinks=False,
            ignore=shutil.ignore_patterns(
                "site-packages",
                "lib-dynload",
                "__pycache__",
                "*.pyc",
            ),
        )
        shutil.copytree(
            dynload_source,
            dynload,
            symlinks=False,
            ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
        )
        build_support.mkdir()
        for distribution_name in package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS:
            distribution = importlib.metadata.distribution(distribution_name)
            distribution_root = Path(distribution.locate_file("")).resolve(strict=True)
            for relative in distribution.files or ():
                source = Path(distribution.locate_file(relative)).resolve(strict=True)
                if not source.is_file() or not source.is_relative_to(distribution_root):
                    continue
                destination = build_support / source.relative_to(distribution_root)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
        package_smoke.tree_snapshot(stdlib)
        package_smoke.tree_snapshot(dynload)
        package_smoke.tree_snapshot(build_support)
        return {
            "python_executable": python,
            "python_stdlib_tree": stdlib,
            "python_dynload_tree": dynload,
            "python_build_support_tree": build_support,
        }

    @unittest.skipUnless(
        os.environ.get("PMUX_PACKAGE_SMOKE_TEST_NODE")
        and os.environ.get("PMUX_PACKAGE_SMOKE_TEST_NPM_TREE"),
        "requires explicit real Node and npm support-tree fixture paths",
    )
    def test_real_typescript_package_flow_with_materialized_fixture_closure(
        self,
    ) -> None:
        workspace = MODULE_PATH.parents[2]
        node = Path(os.environ["PMUX_PACKAGE_SMOKE_TEST_NODE"]).resolve(strict=True)
        npm_source = Path(os.environ["PMUX_PACKAGE_SMOKE_TEST_NPM_TREE"]).resolve(
            strict=True
        )
        typescript_tree = (
            workspace / "clients/typescript/node_modules/typescript"
        ).resolve(strict=True)
        node_types = (workspace / "clients/typescript/node_modules/@types").resolve(
            strict=True
        )
        undici_types = (
            workspace / "clients/typescript/node_modules/undici-types"
        ).resolve(strict=True)
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            npm_support = base / "npm-support-materialized"
            shutil.copytree(
                npm_source,
                npm_support,
                symlinks=False,
            )
            package_smoke.tree_snapshot(npm_support)
            metadata = base / "metadata"
            metadata.mkdir()
            environment, synthetic_paths = self.create_declared_closure_fixture(
                metadata,
                "typescript",
            )
            paths = {
                "node_executable": node,
                "npm_executable": npm_support / "bin/npm-cli.js",
                "npm_support_tree": npm_support,
                "typescript_compiler": typescript_tree / "bin/tsc",
                "typescript_dependency_tree": typescript_tree,
                "node_types_dependency_tree": node_types,
                "undici_types_dependency_tree": undici_types,
                "support_closure": synthetic_paths["support_closure"],
            }
            environment = self.replace_declared_fixture_paths(
                environment,
                "typescript",
                paths,
            )
            with patch.dict(os.environ, environment, clear=False):
                report = package_smoke.build_typescript_package(workspace)
        self.assertEqual(report["gate"], "typescript_package_artifact")
        self.assertEqual(report["artifact"]["entry_count"], 18)
        self.assertEqual(len(report["command_receipts"]), 8)
        self.assertEqual(
            report["bounded_process_implementation"]["sha256"],
            package_smoke.EXPECTED_BOUNDED_PROCESS_SHA256,
        )
        self.assertTrue(report["temporary_state_removed"])
        self.assertTrue(report["repository_client_trees_unchanged"])
        for receipt in report["command_receipts"]:
            self.assertEqual(receipt["kind"], "pmux_bounded_process")
            self.assertEqual(receipt["exit_code"], 0)

    def test_real_python_package_flow_with_materialized_fixture_closure(self) -> None:
        workspace = MODULE_PATH.parents[2]
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            paths = self.materialize_python_support(base)
            metadata = base / "metadata"
            metadata.mkdir()
            environment, synthetic_paths = self.create_declared_closure_fixture(
                metadata,
                "python",
            )
            paths["support_closure"] = synthetic_paths["support_closure"]
            environment = self.replace_declared_fixture_paths(
                environment,
                "python",
                paths,
            )
            with patch.dict(os.environ, environment, clear=False):
                report = package_smoke.build_python_package(workspace)
        self.assertEqual(report["gate"], "python_package_artifact")
        self.assertEqual(
            report["artifact"]["entry_count"],
            len(package_smoke.PYTHON_REQUIRED_PACKAGE_FILES) + 4,
        )
        self.assertEqual(len(report["command_receipts"]), 4)
        self.assertEqual(
            report["bounded_process_implementation"]["sha256"],
            package_smoke.EXPECTED_BOUNDED_PROCESS_SHA256,
        )
        self.assertEqual(
            report["toolchain"]["distributions"],
            [
                [name, importlib.metadata.version(name)]
                for name in package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS
            ],
        )
        self.assertIsNone(report["toolchain"]["wheel"])
        self.assertIn(
            "wheel",
            {
                item[0]
                for vendor in report["toolchain"]["vendor_distributions"]
                for item in vendor["distributions"]
            },
        )
        self.assertTrue(report["temporary_state_removed"])
        self.assertTrue(report["repository_client_trees_unchanged"])
        for receipt in report["command_receipts"]:
            self.assertEqual(receipt["kind"], "pmux_bounded_process")
            self.assertEqual(receipt["exit_code"], 0)
            self.assertEqual(receipt["argv"][1:4], ["-I", "-S", "-B"])
            self.assertEqual(len(receipt["process_ledger"]), 1)

    def test_declared_closure_rejects_hostile_exact_schema_substitutions(self) -> None:
        mutations = {
            "unknown top-level field": lambda value: value.__setitem__("extra", 1),
            "Boolean schema version": lambda value: value.__setitem__(
                "schema_version", True
            ),
            "wrong gate": lambda value: value.__setitem__("gate", "python"),
            "missing role": lambda value: value["inputs"].pop(),
            "duplicate role": lambda value: value["inputs"].append(
                dict(value["inputs"][0])
            ),
            "relative path": lambda value: value["inputs"][0].__setitem__(
                "path", "relative"
            ),
            "wrong usage": lambda value: value["inputs"][0].__setitem__(
                "usage", "tool_support_tree"
            ),
            "unknown input field": lambda value: value["inputs"][0].__setitem__(
                "extra", None
            ),
            "non-string digest": lambda value: value["inputs"][0].__setitem__(
                "portable_sha256", 7
            ),
        }
        for label, mutation in mutations.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                environment, _paths = self.create_declared_closure_fixture(
                    Path(directory).resolve(),
                    "typescript",
                )
                environment = self.rewrite_declared_fixture(environment, mutation)
                with patch.dict(os.environ, environment, clear=False):
                    with self.assertRaises(package_smoke.SmokeError):
                        package_smoke.load_declared_closure("typescript")

    def test_public_closure_builders_round_trip_and_reject_hostile_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            synthetic_environment, paths = self.create_declared_closure_fixture(
                base,
                "typescript",
            )
            candidate = synthetic_environment["PMUX_PACKAGE_SMOKE_CANDIDATE_SHA256"]
            source = synthetic_environment["PMUX_PACKAGE_SMOKE_SOURCE_SHA256"]
            previous = synthetic_environment[
                "PMUX_PACKAGE_SMOKE_PREVIOUS_ANCHOR_SHA256"
            ]
            support = package_smoke.candidate_support_closure_payload(
                "typescript",
                candidate_sha256=candidate,
                source_sha256=source,
                previous_anchor_sha256=previous,
                attestation_sha256="4" * 64,
            )
            support_path = paths["support_closure"]
            support_path.write_text(
                json.dumps(support, sort_keys=True, separators=(",", ":")),
                encoding="utf-8",
            )
            support_path.chmod(0o600)
            records = [
                package_smoke.declared_input_record("typescript", role, paths[role])
                for role in reversed(
                    package_smoke.EXPECTED_DECLARED_INPUTS["typescript"]
                )
            ]
            manifest = package_smoke.declared_closure_payload(
                "typescript",
                candidate_sha256=candidate,
                source_sha256=source,
                previous_anchor_sha256=previous,
                inputs=records,
            )
            self.assertEqual(
                [item["role"] for item in manifest["inputs"]],
                sorted(package_smoke.EXPECTED_DECLARED_INPUTS["typescript"]),
            )
            manifest_path = base / "public-builder-closure.json"
            payload = json.dumps(
                manifest,
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
            manifest_path.write_bytes(payload)
            manifest_path.chmod(0o600)
            environment = {
                "PMUX_PACKAGE_SMOKE_CLOSURE_FILE": str(manifest_path),
                "PMUX_PACKAGE_SMOKE_CLOSURE_SHA256": hashlib.sha256(
                    payload
                ).hexdigest(),
                "PMUX_PACKAGE_SMOKE_CANDIDATE_SHA256": candidate,
                "PMUX_PACKAGE_SMOKE_SOURCE_SHA256": source,
                "PMUX_PACKAGE_SMOKE_PREVIOUS_ANCHOR_SHA256": previous,
            }
            with patch.dict(os.environ, environment, clear=False):
                with package_smoke.load_declared_closure("typescript") as closure:
                    self.assertEqual(closure.digest, manifest["closure_sha256"])

            hostile_inputs = [dict(item) for item in records]
            hostile_inputs[0]["usage"] = "runtime_support_tree"
            with self.assertRaises(package_smoke.SmokeError):
                package_smoke.declared_closure_payload(
                    "typescript",
                    candidate_sha256=candidate,
                    source_sha256=source,
                    previous_anchor_sha256=previous,
                    inputs=hostile_inputs,
                )
            with self.assertRaises(package_smoke.SmokeError):
                package_smoke.candidate_support_closure_payload(
                    "typescript",
                    candidate_sha256=True,
                    source_sha256=source,
                    previous_anchor_sha256=previous,
                    attestation_sha256="4" * 64,
                )

    def test_candidate_support_manifest_rejects_hostile_exact_schema(self) -> None:
        mutations = {
            "unknown field": lambda value: value.__setitem__("extra", 1),
            "Boolean schema": lambda value: value.__setitem__("schema_version", True),
            "wrong gate": lambda value: value.__setitem__("gate", "python"),
            "wrong candidate": lambda value: value.__setitem__(
                "candidate_manifest_sha256", "9" * 64
            ),
            "invalid attestation": lambda value: value.__setitem__(
                "attestation_sha256", "not-a-digest"
            ),
        }
        for label, mutation in mutations.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                base = Path(directory).resolve()
                environment, paths = self.create_declared_closure_fixture(
                    base,
                    "typescript",
                )
                support_path = paths["support_closure"]
                support = json.loads(support_path.read_text(encoding="utf-8"))
                mutation(support)
                support.pop("support_closure_sha256", None)
                support["support_closure_sha256"] = package_smoke.domain_json_digest(
                    support,
                    domain=package_smoke.SUPPORT_CLOSURE_DOMAIN,
                )
                support_path.write_text(
                    json.dumps(support, sort_keys=True, separators=(",", ":")),
                    encoding="utf-8",
                )
                support_path.chmod(0o600)
                environment = self.replace_declared_fixture_paths(
                    environment,
                    "typescript",
                    paths,
                )
                with patch.dict(os.environ, environment, clear=False):
                    with self.assertRaises(package_smoke.SmokeError):
                        package_smoke.load_declared_closure("typescript")

    def test_declared_layout_rejects_overlapping_materialized_role_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            environment, paths = self.create_declared_closure_fixture(base, "python")
            nested = paths["python_stdlib_tree"] / "build-support"
            paths["python_build_support_tree"].rename(nested)
            paths["python_build_support_tree"] = nested
            environment = self.replace_declared_fixture_paths(
                environment,
                "python",
                paths,
            )
            with patch.dict(os.environ, environment, clear=False):
                with self.assertRaisesRegex(
                    package_smoke.SmokeError,
                    "support trees overlap",
                ):
                    package_smoke.load_declared_closure("python")

    def test_python_tool_report_rejects_boolean_and_ambient_substitutions(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            python = base / "python"
            python.write_text("runtime", encoding="utf-8")
            stdlib = base / "stdlib"
            dynload = base / "dynload"
            support = base / "support"
            for root in (stdlib, dynload, support / "pip", support / "setuptools"):
                root.mkdir(parents=True)
            (support / "pip/__init__.py").write_text("", encoding="utf-8")
            (support / "setuptools/__init__.py").write_text("", encoding="utf-8")
            vendor = support / "setuptools/_vendor"
            vendor.mkdir()
            valid = {
                "executable": str(python),
                "python": "3.12.4",
                "sys_path_before": [str(stdlib), str(dynload), str(support)],
                "sys_path_after": [
                    str(stdlib),
                    str(dynload),
                    str(support),
                    str(vendor),
                ],
                "isolation": {
                    "isolated": 1,
                    "ignore_environment": 1,
                    "no_site": 1,
                },
                "module_files": {
                    "pip": str(support / "pip/__init__.py"),
                    "setuptools": str(support / "setuptools/__init__.py"),
                },
                "distributions": [["pip", "25.1"], ["setuptools", "80.9"]],
                "vendor_distributions": [
                    {
                        "path": str(vendor),
                        "distributions": [["wheel", "0.45"]],
                    }
                ],
                "pip": "25.1",
                "setuptools": "80.9",
                "build": None,
                "wheel": None,
                "ruff": None,
            }
            self.assertEqual(
                package_smoke.validate_python_tool_report(
                    valid,
                    python=python,
                    stdlib=stdlib,
                    dynload=dynload,
                    build_support=support,
                ),
                valid,
            )
            hostile = []
            boolean_isolation = json.loads(json.dumps(valid))
            boolean_isolation["isolation"]["isolated"] = True
            hostile.append(boolean_isolation)
            ambient_distribution = json.loads(json.dumps(valid))
            ambient_distribution["distributions"].append(["wheel", "0.45"])
            hostile.append(ambient_distribution)
            ambient_origin = json.loads(json.dumps(valid))
            ambient_origin["module_files"]["pip"] = str(python)
            hostile.append(ambient_origin)
            missing_vendor = json.loads(json.dumps(valid))
            missing_vendor["sys_path_after"][-1] = str(support / "missing-vendor")
            missing_vendor["vendor_distributions"][0]["path"] = str(
                support / "missing-vendor"
            )
            hostile.append(missing_vendor)
            additive = json.loads(json.dumps(valid))
            additive["extra"] = None
            hostile.append(additive)
            for report in hostile:
                with self.assertRaises(package_smoke.SmokeError):
                    package_smoke.validate_python_tool_report(
                        report,
                        python=python,
                        stdlib=stdlib,
                        dynload=dynload,
                        build_support=support,
                    )

    def test_a_build_support_tree_missing_a_distribution_is_refused_by_name(
        self,
    ) -> None:
        # The interpreter contract, stated as a refusal rather than as a note in
        # a README. A declared build-support tree that carries no `setuptools`
        # cannot build the wheel -- `clients/python/pyproject.toml` names
        # `setuptools.build_meta` as its backend and the build runs with the
        # index disabled -- and Python 3.12 stopped supplying setuptools through
        # `ensurepip`, so this is now the ordinary case on a current
        # interpreter, not an exotic one. Every other failure in this validator
        # says "is not exact"; the one a caller can actually act on says which
        # distribution is missing.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            python = base / "python"
            python.write_text("runtime", encoding="utf-8")
            stdlib = base / "stdlib"
            dynload = base / "dynload"
            support = base / "support"
            for root in (stdlib, dynload):
                root.mkdir(parents=True)
            module_files = {}
            for name in package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS:
                (support / name).mkdir(parents=True)
                module_file = support / name / "__init__.py"
                module_file.write_text("", encoding="utf-8")
                module_files[name] = str(module_file)
            for absent in package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS:
                with self.subTest(absent=absent):
                    report = {
                        "executable": str(python),
                        "python": "3.13.0",
                        "sys_path_before": [str(stdlib), str(dynload), str(support)],
                        "sys_path_after": [str(stdlib), str(dynload), str(support)],
                        "isolation": {
                            "isolated": 1,
                            "ignore_environment": 1,
                            "no_site": 1,
                        },
                        "module_files": dict(module_files),
                        "distributions": [
                            [name, "1.0"]
                            for name in package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS
                            if name != absent
                        ],
                        "vendor_distributions": [],
                        "build": None,
                        "wheel": None,
                        "ruff": None,
                        **{
                            name: (None if name == absent else "1.0")
                            for name in package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS
                        },
                    }
                    with self.assertRaisesRegex(
                        package_smoke.SmokeError,
                        f"publishes no metadata for: {absent}",
                    ):
                        package_smoke.validate_python_tool_report(
                            report,
                            python=python,
                            stdlib=stdlib,
                            dynload=dynload,
                            build_support=support,
                        )

    def test_every_declared_role_detects_mutate_restore_and_substitution(self) -> None:
        for gate, expected in package_smoke.EXPECTED_DECLARED_INPUTS.items():
            for role, (kind, _usage) in expected.items():
                with self.subTest(gate=gate, role=role, attack="mutate-restore"):
                    with tempfile.TemporaryDirectory() as directory:
                        base = Path(directory).resolve()
                        environment, paths = self.create_declared_closure_fixture(
                            base, gate
                        )
                        with patch.dict(os.environ, environment, clear=False):
                            closure = package_smoke.load_declared_closure(gate)
                            try:
                                target = paths[role]
                                if kind == "file":
                                    original = target.read_bytes()
                                    target.write_bytes(b"mutated\n")
                                    target.write_bytes(original)
                                else:
                                    tree = package_smoke.file_manifest(target)
                                    relative = next(
                                        name
                                        for name, identity in tree.items()
                                        if identity.get("type") == "file"
                                    )
                                    member = target / relative
                                    original = member.read_bytes()
                                    member.write_bytes(b"mutated\n")
                                    member.write_bytes(original)
                                with self.assertRaisesRegex(
                                    package_smoke.SmokeError,
                                    "changed|misbound",
                                ):
                                    closure.verify()
                            finally:
                                closure.close()

                with self.subTest(gate=gate, role=role, attack="substitution"):
                    with tempfile.TemporaryDirectory() as directory:
                        base = Path(directory).resolve()
                        environment, paths = self.create_declared_closure_fixture(
                            base, gate
                        )
                        with patch.dict(os.environ, environment, clear=False):
                            closure = package_smoke.load_declared_closure(gate)
                            try:
                                target = paths[role]
                                original_mode = stat.S_IMODE(target.stat().st_mode)
                                displaced = target.with_name(f"{target.name}-original")
                                target.rename(displaced)
                                if kind == "file":
                                    package_smoke.copy_direct_file(displaced, target)
                                else:
                                    package_smoke.copy_direct_tree(displaced, target)
                                    target.chmod(original_mode)
                                with self.assertRaisesRegex(
                                    package_smoke.SmokeError,
                                    "changed|misbound|membership",
                                ):
                                    closure.verify()
                                self.assertTrue(target.exists())
                                self.assertTrue(displaced.exists())
                            finally:
                                closure.close()

    def test_declared_closure_detects_tool_and_dependency_mutate_restore(self) -> None:
        candidate = "1" * 64
        source = "2" * 64
        previous = "3" * 64
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            node = base / "node"
            npm_support = base / "npm-support"
            npm_support.mkdir()
            npm = npm_support / "npm-cli.js"
            typescript = base / "typescript"
            node_types = base / "node-types"
            undici_types = base / "undici-types"
            typescript.mkdir()
            node_types.mkdir()
            undici_types.mkdir()
            tsc = typescript / "tsc.js"
            for path in (node, npm, tsc):
                path.write_text(f"{path.name}\n", encoding="utf-8")
                path.chmod(0o700)
            (typescript / "compiler.js").write_text("compiler\n", encoding="utf-8")
            (node_types / "index.d.ts").write_text("types\n", encoding="utf-8")
            (undici_types / "index.d.ts").write_text("undici types\n", encoding="utf-8")

            support = {
                "schema_version": 1,
                "kind": "pmux_gate_a_package_support_closure",
                "gate": "typescript",
                "candidate_manifest_sha256": candidate,
                "source_manifest_sha256": source,
                "previous_anchor_sha256": previous,
                "attestation_sha256": "4" * 64,
            }
            support["support_closure_sha256"] = package_smoke.domain_json_digest(
                support,
                domain=package_smoke.SUPPORT_CLOSURE_DOMAIN,
            )
            support_path = base / "support.json"
            support_path.write_text(
                json.dumps(support, sort_keys=True, separators=(",", ":")),
                encoding="utf-8",
            )
            support_path.chmod(0o600)
            inputs = [
                self.declared_file_record("node_executable", "native_executable", node),
                self.declared_file_record("npm_executable", "interpreter_script", npm),
                self.declared_tree_record(
                    "npm_support_tree",
                    npm_support,
                    "tool_support_tree",
                ),
                self.declared_file_record(
                    "typescript_compiler", "interpreter_script", tsc
                ),
                self.declared_tree_record("typescript_dependency_tree", typescript),
                self.declared_tree_record("node_types_dependency_tree", node_types),
                self.declared_tree_record(
                    "undici_types_dependency_tree",
                    undici_types,
                ),
                self.declared_file_record(
                    "support_closure", "support_manifest", support_path
                ),
            ]
            manifest = {
                "schema_version": 1,
                "kind": "pmux_package_smoke_declared_closure",
                "gate": "typescript",
                "candidate_manifest_sha256": candidate,
                "source_manifest_sha256": source,
                "previous_anchor_sha256": previous,
                "inputs": inputs,
            }
            manifest["closure_sha256"] = package_smoke.domain_json_digest(
                manifest,
                domain=package_smoke.DECLARED_CLOSURE_DOMAIN,
            )
            manifest_path = base / "closure.json"
            manifest_payload = json.dumps(
                manifest,
                sort_keys=True,
                separators=(",", ":"),
            ).encode()
            manifest_path.write_bytes(manifest_payload)
            manifest_path.chmod(0o600)
            environment = {
                "PMUX_PACKAGE_SMOKE_CLOSURE_FILE": str(manifest_path),
                "PMUX_PACKAGE_SMOKE_CLOSURE_SHA256": hashlib.sha256(
                    manifest_payload
                ).hexdigest(),
                "PMUX_PACKAGE_SMOKE_CANDIDATE_SHA256": candidate,
                "PMUX_PACKAGE_SMOKE_SOURCE_SHA256": source,
                "PMUX_PACKAGE_SMOKE_PREVIOUS_ANCHOR_SHA256": previous,
            }
            required = {
                "node_executable": node,
                "npm_executable": npm,
                "npm_support_tree": npm_support,
                "typescript_compiler": tsc,
                "typescript_dependency_tree": typescript,
                "node_types_dependency_tree": node_types,
                "undici_types_dependency_tree": undici_types,
            }
            with patch.dict(os.environ, environment, clear=False):
                closure = package_smoke.load_declared_closure(
                    "typescript",
                    required_paths=required,
                )
                try:
                    closure.verify()
                    original = node.read_bytes()
                    node.write_bytes(b"changed tool\n")
                    node.write_bytes(original)
                    with self.assertRaisesRegex(
                        package_smoke.SmokeError, "changed|misbound"
                    ):
                        closure.verify()
                finally:
                    closure.close()

            # A fresh closure then detects a dependency add/remove restoration.
            inputs = [
                self.declared_file_record("node_executable", "native_executable", node),
                self.declared_file_record("npm_executable", "interpreter_script", npm),
                self.declared_tree_record(
                    "npm_support_tree",
                    npm_support,
                    "tool_support_tree",
                ),
                self.declared_file_record(
                    "typescript_compiler", "interpreter_script", tsc
                ),
                self.declared_tree_record("typescript_dependency_tree", typescript),
                self.declared_tree_record("node_types_dependency_tree", node_types),
                self.declared_tree_record(
                    "undici_types_dependency_tree",
                    undici_types,
                ),
                self.declared_file_record(
                    "support_closure", "support_manifest", support_path
                ),
            ]
            manifest["inputs"] = inputs
            manifest.pop("closure_sha256")
            manifest["closure_sha256"] = package_smoke.domain_json_digest(
                manifest,
                domain=package_smoke.DECLARED_CLOSURE_DOMAIN,
            )
            manifest_payload = json.dumps(
                manifest,
                sort_keys=True,
                separators=(",", ":"),
            ).encode()
            manifest_path.write_bytes(manifest_payload)
            manifest_path.chmod(0o600)
            environment["PMUX_PACKAGE_SMOKE_CLOSURE_SHA256"] = hashlib.sha256(
                manifest_payload
            ).hexdigest()
            with patch.dict(os.environ, environment, clear=False):
                closure = package_smoke.load_declared_closure(
                    "typescript",
                    required_paths=required,
                )
                try:
                    transient = typescript / "transient"
                    transient.write_text("added", encoding="utf-8")
                    transient.unlink()
                    with self.assertRaisesRegex(
                        package_smoke.SmokeError, "changed|misbound"
                    ):
                        closure.verify()
                finally:
                    closure.close()

    def test_canonical_json_digest_is_order_independent(self) -> None:
        self.assertEqual(
            package_smoke.canonical_json_digest({"b": 2, "a": 1}),
            package_smoke.canonical_json_digest({"a": 1, "b": 2}),
        )

    def test_main_emits_one_compact_canonical_json_value(self) -> None:
        output = io.StringIO()
        with (
            patch.object(
                package_smoke,
                "build_python_package",
                return_value={"z": 1, "a": 2},
            ),
            redirect_stdout(output),
        ):
            self.assertEqual(package_smoke.main(["python"]), 0)
        self.assertEqual(output.getvalue(), '{"a":2,"z":1}\n')


if __name__ == "__main__":
    unittest.main()
