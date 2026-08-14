from __future__ import annotations

import json
import hashlib
import os
import pathlib
import re
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True

TOOLS = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))

import evidence  # noqa: E402
import source_digest  # noqa: E402


BASE_IMAGE = "docker.io/library/rust:1.88.0-bookworm@sha256:" + "b" * 64


class PrivateArtifactTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_atomic_json_is_private_valid_and_never_replaces(self) -> None:
        destination = self.root / "result.json"
        evidence.atomic_write_json(destination, {"ok": True})
        self.assertEqual(
            json.loads(destination.read_text(encoding="utf-8")), {"ok": True}
        )
        self.assertEqual(stat.S_IMODE(destination.lstat().st_mode), 0o600)
        with self.assertRaisesRegex(evidence.EvidenceError, "replace"):
            evidence.atomic_write_json(destination, {"ok": False})

    def test_atomic_json_refuses_symlink_destination(self) -> None:
        target = self.root / "target"
        target.write_text("do not replace", encoding="utf-8")
        destination = self.root / "result.json"
        destination.symlink_to(target)
        with self.assertRaises(evidence.EvidenceError):
            evidence.atomic_write_json(destination, {"ok": True})
        self.assertEqual(target.read_text(encoding="utf-8"), "do not replace")

    def test_atomic_json_loses_a_publication_race_without_replacing(self) -> None:
        destination = self.root / "result.json"
        real_link = os.link

        def race(
            source: os.PathLike[str], target: os.PathLike[str], **kwargs: object
        ) -> None:
            destination_fd = kwargs.get("dst_dir_fd")
            assert isinstance(destination_fd, int)
            descriptor = os.open(
                target,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                0o600,
                dir_fd=destination_fd,
            )
            try:
                os.write(descriptor, b"raced")
            finally:
                os.close(descriptor)
            real_link(source, target, **kwargs)

        with mock.patch("evidence.os.link", side_effect=race):
            with self.assertRaisesRegex(evidence.EvidenceError, "appeared"):
                evidence.atomic_write_json(destination, {"ok": True})
        self.assertEqual(destination.read_text(encoding="utf-8"), "raced")

    def test_private_spools_are_exclusive_anchored_and_can_coexist(self) -> None:
        stdout = self.root / "stdout.log"
        stderr = self.root / "stderr.log"
        with evidence.private_output_spool(stdout) as stdout_fd:
            with evidence.private_output_spool(stderr) as stderr_fd:
                os.write(stdout_fd, b"out")
                os.write(stderr_fd, b"err")
        self.assertEqual(stdout.read_bytes(), b"out")
        self.assertEqual(stderr.read_bytes(), b"err")
        self.assertEqual(stat.S_IMODE(stdout.lstat().st_mode), 0o600)
        with self.assertRaisesRegex(evidence.EvidenceError, "already exists"):
            with evidence.private_output_spool(stdout):
                pass
        target = self.root / "target"
        target.write_bytes(b"preserved")
        symlink = self.root / "symlink"
        symlink.symlink_to(target)
        with self.assertRaises(evidence.EvidenceError):
            with evidence.private_output_spool(symlink):
                pass
        self.assertEqual(target.read_bytes(), b"preserved")

    def test_private_jsonl_is_bounded_locked_and_fsynced_per_record(self) -> None:
        ledger = self.root / "ledger.ndjson"
        first = evidence.append_private_jsonl(
            ledger,
            {"ordinal": 1},
            expected_ordinal=1,
            expected_prior_sha256=None,
        )
        second = evidence.append_private_jsonl(
            ledger,
            {"ordinal": 2},
            expected_ordinal=2,
            expected_prior_sha256=first,
        )
        records = [
            json.loads(line) for line in ledger.read_text(encoding="utf-8").splitlines()
        ]
        self.assertEqual(
            [record["payload"] for record in records],
            [{"ordinal": 1}, {"ordinal": 2}],
        )
        self.assertEqual(records[0]["record_sha256"], first)
        self.assertEqual(records[1]["record_sha256"], second)
        self.assertEqual(stat.S_IMODE(ledger.lstat().st_mode), 0o600)
        with self.assertRaisesRegex(evidence.EvidenceError, "64 KiB"):
            evidence.append_private_jsonl(
                ledger,
                {"large": "x" * (65 * 1024)},
                expected_ordinal=3,
                expected_prior_sha256=second,
            )

    def test_bounded_command_ledger_retains_and_validates_full_receipts(self) -> None:
        executable = pathlib.Path("/bin/echo").resolve(strict=True)
        result = evidence.bounded_process.run(
            evidence.bounded_process.bind_executable(executable),
            [str(executable), "bounded"],
            cwd=self.root,
            environment={
                "LANG": "C",
                "LC_ALL": "C",
                "PYTHONDONTWRITEBYTECODE": "1",
            },
            timeout_seconds=10,
            drain_timeout_seconds=2,
            maximum_output_bytes=4096,
            description="bounded command ledger test",
        )
        receipt_path = self.root / "echo.receipt.json"
        evidence.atomic_write_bytes(self.root / "echo.stdout", result.stdout)
        evidence.atomic_write_bytes(self.root / "echo.stderr", result.stderr)
        evidence.atomic_write_bytes(
            receipt_path,
            evidence.bounded_process.dump_execution_receipt(result.receipt),
        )
        ledger = self.root / "command-ledger.ndjson"
        tail = evidence.append_bounded_command_receipt(
            ledger,
            receipt_path,
            label="host.echo",
            scope="host",
            expected_ordinal=1,
            expected_prior_sha256=None,
        )
        report = evidence.bounded_command_ledger_report(
            ledger, expected_count=1, expected_tail_sha256=tail
        )
        self.assertTrue(report["all_receipts_valid"])
        self.assertEqual(report["labels"], ["host.echo"])

        parsed = evidence.strict_json_loads(
            ledger.read_bytes().splitlines()[0], description="test command row"
        )
        parsed["payload"]["receipt_sha256"] = "0" * 64
        body = dict(parsed)
        del body["record_sha256"]
        parsed["record_sha256"] = evidence.canonical_json_sha256(
            body, domain="pmux.evidence.private-jsonl-record.v1"
        )
        ledger.write_text(
            json.dumps(parsed, separators=(",", ":"), sort_keys=True) + "\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(evidence.EvidenceError, "substituted"):
            evidence.bounded_command_ledger_report(
                ledger,
                expected_count=1,
                expected_tail_sha256=parsed["record_sha256"],
            )

    def test_secure_tree_enforces_modes_and_rejects_links_and_special_nodes(
        self,
    ) -> None:
        child = self.root / "child"
        child.mkdir(mode=0o755)
        artifact = child / "artifact"
        artifact.write_text("evidence", encoding="utf-8")
        artifact.chmod(0o644)
        evidence.secure_private_tree(self.root)
        self.assertEqual(stat.S_IMODE(child.lstat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(artifact.lstat().st_mode), 0o600)
        alias = child / "alias"
        alias.symlink_to(artifact)
        with self.assertRaisesRegex(evidence.EvidenceError, "symlink"):
            evidence.secure_private_tree(self.root)

    def test_secure_tree_rejects_hard_link_aliases(self) -> None:
        artifact = self.root / "artifact"
        artifact.write_text("evidence", encoding="utf-8")
        os.link(artifact, self.root / "alias")
        with self.assertRaisesRegex(evidence.EvidenceError, "hard-link"):
            evidence.secure_private_tree(self.root)

    def test_prepare_output_requires_absolute_empty_non_symlink_path(self) -> None:
        with self.assertRaisesRegex(evidence.EvidenceError, "absolute"):
            evidence.prepare_empty_private_directory(pathlib.Path("relative"))
        output = self.root / "new" / "evidence"
        evidence.prepare_empty_private_directory(output)
        self.assertEqual(stat.S_IMODE(output.lstat().st_mode), 0o700)
        (output / "occupied").write_text("x", encoding="utf-8")
        with self.assertRaisesRegex(evidence.EvidenceError, "non-empty"):
            evidence.prepare_empty_private_directory(output)

    def test_regular_tree_manifest_binds_modes_paths_and_bytes(self) -> None:
        artifact = self.root / "wheel.whl"
        artifact.write_bytes(b"wheel")
        artifact.chmod(0o600)
        first = evidence.regular_tree_manifest(self.root)
        artifact.chmod(0o644)
        second = evidence.regular_tree_manifest(self.root)
        self.assertNotEqual(first["tree_sha256"], second["tree_sha256"])
        self.assertEqual(first["files"][0]["path"], "wheel.whl")

    def test_stable_json_and_tree_reads_reject_path_replacement(self) -> None:
        document = self.root / "document.json"
        document.write_text('{"value":1}\n', encoding="utf-8")
        before = document.lstat()
        replacement = self.root / "replacement"
        replacement.write_text('{"value":2}\n', encoding="utf-8")
        document.unlink()
        replacement.rename(document)
        with self.assertRaisesRegex(evidence.EvidenceError, "changed before read"):
            evidence._stable_regular_bytes(
                document,
                description="JSON evidence",
                maximum_bytes=1024,
                before=before,
            )

        manifest = evidence.regular_tree_manifest(self.root)
        document.write_text('{"value":3}\n', encoding="utf-8")
        with self.assertRaisesRegex(evidence.EvidenceError, "changed"):
            evidence.verify_regular_tree_manifest(self.root, manifest)

    def test_self_excluded_whole_tree_manifest_verifies_exactly(self) -> None:
        artifact = self.root / "artifact"
        artifact.write_text("stable", encoding="utf-8")
        output = self.root / "final-tree.json"
        result = evidence.main(["tree-manifest", str(self.root), str(output)])
        self.assertEqual(result, 0)
        manifest = evidence.load_json(output)
        self.assertEqual(manifest["excluded_paths"], ["final-tree.json"])
        self.assertEqual(
            evidence.verify_regular_tree_manifest(self.root, manifest), manifest
        )
        with self.assertRaisesRegex(evidence.EvidenceError, "unexpected"):
            evidence.verify_regular_tree_manifest(
                self.root,
                manifest,
                expected_excluded_paths=frozenset(("different.json",)),
            )
        relocated = self.root.parent / f"{self.root.name}-relocated"
        shutil.copytree(self.root, relocated)
        try:
            self.assertEqual(
                evidence.verify_regular_tree_manifest(relocated, manifest), manifest
            )
        finally:
            shutil.rmtree(relocated)


class ReleaseBinaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = pathlib.Path(self.temporary.name).resolve()
        for index, name in enumerate(evidence.REQUIRED_RELEASE_BINARIES):
            path = self.directory / name
            path.write_bytes(f"binary-{index}".encode())
            path.chmod(0o755)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def copy_release(self, directory: pathlib.Path) -> None:
        for name in evidence.REQUIRED_RELEASE_BINARIES:
            shutil.copy2(self.directory / name, directory / name)

    def test_complete_manifest_verifies_exactly(self) -> None:
        manifest = evidence.release_binary_manifest(self.directory)
        self.assertEqual(
            manifest["required_names"], list(evidence.REQUIRED_RELEASE_BINARIES)
        )
        self.assertEqual(evidence.verify_release_binary_manifest(manifest), manifest)
        for name, identity in manifest["binaries"].items():
            self.assertEqual(pathlib.Path(identity["path"]).parent, self.directory)
            self.assertEqual(pathlib.Path(identity["path"]).name, name)
            self.assertRegex(identity["sha256"], r"^[0-9a-f]{64}$")

    def test_binary_capture_cli_accepts_one_explicit_expected_owner(self) -> None:
        output = self.directory.parent / f"{self.directory.name}-manifest.json"
        try:
            self.assertEqual(
                evidence.main(
                    [
                        "binary-capture",
                        str(self.directory),
                        str(output),
                        "--expected-owner-uid",
                        str(os.geteuid()),
                    ]
                ),
                0,
            )
            self.assertEqual(evidence.load_json(output)["directory_uid"], os.geteuid())
        finally:
            output.unlink(missing_ok=True)
        with self.assertRaisesRegex(evidence.EvidenceError, "owned"):
            evidence.main(
                [
                    "binary-capture",
                    str(self.directory),
                    str(output),
                    "--expected-owner-uid",
                    str(os.geteuid() + 1),
                ]
            )

    def test_manifest_schema_rejects_hostile_types_paths_and_aliases(self) -> None:
        manifest = evidence.release_binary_manifest(self.directory)
        self.assertEqual(
            evidence.validate_release_binary_manifest_schema(manifest), manifest
        )

        def mutated() -> dict[str, object]:
            return json.loads(json.dumps(manifest))

        mutations: list[tuple[str, object]] = []
        extra = mutated()
        extra["unexpected"] = True
        mutations.append(("extra top-level field", extra))
        wrong_names = mutated()
        wrong_names["required_names"] = list(
            reversed(evidence.REQUIRED_RELEASE_BINARIES)
        )
        mutations.append(("reordered names", wrong_names))
        relative = mutated()
        relative["directory"] = "relative"
        mutations.append(("relative directory", relative))
        numeric_bool = mutated()
        numeric_bool["directory_uid"] = True
        mutations.append(("Boolean integer", numeric_bool))
        unsafe_directory_mode = mutated()
        unsafe_directory_mode["directory_mode"] = "0777"
        mutations.append(("unsafe directory mode", unsafe_directory_mode))
        wrong_path = mutated()
        wrong_path["binaries"]["pmux"]["path"] = str(self.directory / "pmuxd")
        mutations.append(("wrong binary path", wrong_path))
        uppercase_digest = mutated()
        uppercase_digest["binaries"]["pmux"]["sha256"] = "A" * 64
        mutations.append(("uppercase digest", uppercase_digest))
        unsafe_binary_mode = mutated()
        unsafe_binary_mode["binaries"]["pmux"]["mode"] = "6755"
        mutations.append(("special binary mode", unsafe_binary_mode))
        wrong_owner = mutated()
        wrong_owner["binaries"]["pmux"]["uid"] += 1
        mutations.append(("owner mismatch", wrong_owner))
        hardlink_alias = mutated()
        hardlink_alias["binaries"]["pmuxd"]["device"] = hardlink_alias["binaries"][
            "pmux"
        ]["device"]
        hardlink_alias["binaries"]["pmuxd"]["inode"] = hardlink_alias["binaries"][
            "pmux"
        ]["inode"]
        mutations.append(("duplicate file identity", hardlink_alias))

        for description, hostile in mutations:
            with self.subTest(description=description):
                with self.assertRaises(evidence.EvidenceError):
                    evidence.validate_release_binary_manifest_schema(hostile)

    def test_missing_mutated_or_non_executable_binary_fails(self) -> None:
        manifest = evidence.release_binary_manifest(self.directory)
        target = self.directory / "pmux"
        target.write_bytes(b"mutated")
        with self.assertRaisesRegex(evidence.EvidenceError, "changed"):
            evidence.verify_release_binary_manifest(manifest)
        target.chmod(0o644)
        with self.assertRaisesRegex(evidence.EvidenceError, "executable"):
            evidence.release_binary_manifest(self.directory)
        target.unlink()
        with self.assertRaisesRegex(evidence.EvidenceError, "missing"):
            evidence.release_binary_manifest(self.directory)

    def test_same_content_stat_rewrite_fails_verification(self) -> None:
        manifest = evidence.release_binary_manifest(self.directory)
        target = self.directory / "pmux"
        metadata = target.stat()
        os.utime(
            target,
            ns=(metadata.st_atime_ns, metadata.st_mtime_ns + 1_000_000_000),
        )
        with self.assertRaisesRegex(evidence.EvidenceError, "changed"):
            evidence.verify_release_binary_manifest(manifest)

    def test_group_world_writable_directory_or_binary_fails(self) -> None:
        target = self.directory / "pmux"
        target.chmod(0o777)
        with self.assertRaisesRegex(evidence.EvidenceError, "writable"):
            evidence.release_binary_manifest(self.directory)
        target.chmod(0o755)
        self.directory.chmod(0o777)
        with self.assertRaisesRegex(evidence.EvidenceError, "writable"):
            evidence.release_binary_manifest(self.directory)

    def test_wrong_expected_owner_fails(self) -> None:
        with self.assertRaisesRegex(evidence.EvidenceError, "owned"):
            evidence.release_binary_manifest(
                self.directory, expected_owner_uid=os.geteuid() + 1
            )

    def test_symlink_and_alias_outside_exact_directory_fail(self) -> None:
        target = self.directory / "pmux"
        target.unlink()
        outside = self.directory.parent / f"{self.directory.name}-outside"
        outside.write_bytes(b"outside")
        outside.chmod(0o755)
        target.symlink_to(outside)
        try:
            with self.assertRaisesRegex(
                evidence.EvidenceError, "real regular|escaped|aliased"
            ):
                evidence.release_binary_manifest(self.directory)
        finally:
            outside.unlink()

    def test_hard_linked_release_candidate_fails_closed(self) -> None:
        os.link(self.directory / "pmux", self.directory / "outside-alias")
        with self.assertRaisesRegex(evidence.EvidenceError, "hard-link"):
            evidence.release_binary_manifest(self.directory)

    def test_fresh_reproduction_compares_portable_bytes_hashes_and_modes(self) -> None:
        candidate = evidence.release_binary_manifest(self.directory)
        with tempfile.TemporaryDirectory() as reproduced_temporary:
            reproduced = pathlib.Path(reproduced_temporary).resolve()
            self.copy_release(reproduced)
            report = evidence.compare_reproduced_release_binaries(candidate, reproduced)
            self.assertTrue(report["verified"])
            self.assertEqual(
                report["required_names"], list(evidence.REQUIRED_RELEASE_BINARIES)
            )
            self.assertEqual(
                report["comparison_fields"],
                ["name", "mode", "size", "sha256", "bytes"],
            )
            self.assertNotEqual(
                candidate["binaries"]["pmux"]["inode"],
                evidence.release_binary_manifest(reproduced)["binaries"]["pmux"][
                    "inode"
                ],
            )
            reproduced_manifest = evidence.release_binary_manifest(reproduced)
            self.assertEqual(
                evidence.portable_release_binary_projection(candidate),
                evidence.portable_release_binary_projection(reproduced_manifest),
            )
            self.assertEqual(
                evidence.validate_release_reproduction_comparison(
                    report,
                    candidate_manifest=candidate,
                    reproduced_manifest=reproduced_manifest,
                ),
                report,
            )
            body = dict(report)
            digest = body.pop("comparison_sha256")
            self.assertEqual(
                digest,
                evidence.canonical_json_sha256(
                    body,
                    domain="pmux.evidence.release-reproduction-comparison.v1",
                ),
            )

    def test_reproduction_stage_is_descriptor_bound_and_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            destination = root / "destination"
            destination.mkdir(mode=0o700)
            manifest = evidence.stage_reproduced_release_binaries(
                self.directory, destination
            )
            self.assertEqual(
                manifest["required_names"], list(evidence.REQUIRED_RELEASE_BINARIES)
            )
            self.assertEqual(
                sorted(item.name for item in destination.iterdir()),
                sorted(evidence.REQUIRED_RELEASE_BINARIES),
            )
            self.assertTrue(
                all(
                    stat.S_IMODE(item.stat().st_mode) == 0o500
                    for item in destination.iterdir()
                )
            )

    def test_reproduction_stage_rejects_links_and_cleans_partial_output(self) -> None:
        for mutation in ("source_symlink", "source_hardlink", "destination_member"):
            with (
                self.subTest(mutation=mutation),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = pathlib.Path(temporary).resolve()
                source = root / "source"
                destination = root / "destination"
                source.mkdir(mode=0o700)
                self.copy_release(source)
                destination.mkdir(mode=0o700)
                if mutation == "source_symlink":
                    (source / "pmuxd").unlink()
                    (source / "pmuxd").symlink_to(source / "pmux")
                elif mutation == "source_hardlink":
                    (source / "pmuxd").unlink()
                    os.link(source / "pmux", source / "pmuxd")
                else:
                    (destination / "occupied").write_bytes(b"x")
                with self.assertRaises((evidence.EvidenceError, OSError)):
                    evidence.stage_reproduced_release_binaries(source, destination)
                expected = ["occupied"] if mutation == "destination_member" else []
                self.assertEqual(
                    sorted(item.name for item in destination.iterdir()), expected
                )

    def test_reproduction_schema_rejects_substitution_and_wrong_types(self) -> None:
        candidate = evidence.release_binary_manifest(self.directory)
        with tempfile.TemporaryDirectory() as reproduced_temporary:
            reproduced = pathlib.Path(reproduced_temporary).resolve()
            self.copy_release(reproduced)
            reproduced_manifest = evidence.release_binary_manifest(reproduced)
            report = evidence.compare_reproduced_release_binaries(candidate, reproduced)

            hostile_reports: list[dict[str, object]] = []
            extra = json.loads(json.dumps(report))
            extra["unexpected"] = True
            hostile_reports.append(extra)
            false_verdict = json.loads(json.dumps(report))
            false_verdict["verified"] = False
            hostile_reports.append(false_verdict)
            numeric_verdict = json.loads(json.dumps(report))
            numeric_verdict["verified"] = 1
            hostile_reports.append(numeric_verdict)
            wrong_digest = json.loads(json.dumps(report))
            wrong_digest["candidate_manifest_sha256"] = "0" * 64
            hostile_reports.append(wrong_digest)
            wrong_row = json.loads(json.dumps(report))
            wrong_row["binaries"][0]["name"] = "pmuxd"
            hostile_reports.append(wrong_row)
            wrong_bytes_type = json.loads(json.dumps(report))
            wrong_bytes_type["binaries"][0]["bytes_identical"] = 1
            hostile_reports.append(wrong_bytes_type)

            for hostile in hostile_reports:
                with self.subTest(hostile=hostile):
                    with self.assertRaises(evidence.EvidenceError):
                        evidence.validate_release_reproduction_comparison(
                            hostile,
                            candidate_manifest=candidate,
                            reproduced_manifest=reproduced_manifest,
                        )

    def test_fresh_reproduction_mismatch_or_ambiguous_identity_fails(self) -> None:
        candidate = evidence.release_binary_manifest(self.directory)
        for mutation in ("bytes", "mode", "missing", "hardlink"):
            with (
                self.subTest(mutation=mutation),
                tempfile.TemporaryDirectory() as temporary,
            ):
                reproduced = pathlib.Path(temporary).resolve()
                self.copy_release(reproduced)
                target = reproduced / "pmux"
                if mutation == "bytes":
                    target.write_bytes(b"different")
                    target.chmod(0o755)
                elif mutation == "mode":
                    target.chmod(0o700)
                elif mutation == "missing":
                    target.unlink()
                else:
                    os.link(target, reproduced / "pmux-alias")
                with self.assertRaises(evidence.EvidenceError):
                    evidence.compare_reproduced_release_binaries(candidate, reproduced)

    def test_binary_repro_compare_cli_publishes_private_receipt(self) -> None:
        candidate = evidence.release_binary_manifest(self.directory)
        with (
            tempfile.TemporaryDirectory() as reproduced_temporary,
            tempfile.TemporaryDirectory() as evidence_temporary,
        ):
            reproduced = pathlib.Path(reproduced_temporary).resolve()
            evidence_root = pathlib.Path(evidence_temporary).resolve()
            self.copy_release(reproduced)
            candidate_path = evidence_root / "candidate.json"
            output = evidence_root / "comparison.json"
            evidence.atomic_write_json(candidate_path, candidate)
            self.assertEqual(
                evidence.main(
                    [
                        "binary-repro-compare",
                        str(candidate_path),
                        str(reproduced),
                        str(output),
                    ]
                ),
                0,
            )
            self.assertTrue(evidence.load_json(output)["verified"])
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)


class GuardAndBindingTests(unittest.TestCase):
    def test_docker_transport_identity_is_socket_only_and_stable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            path = root / "docker.sock"
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                server.bind(str(path))
                server.listen(1)
                before = evidence.docker_transport_identity(path)
                after = evidence.docker_transport_identity(path)
                report = evidence.compare_docker_transport_identities(before, after)
                self.assertTrue(report["verified"])
                changed = dict(after)
                changed["socket_inode"] += 1
                with self.assertRaisesRegex(evidence.EvidenceError, "changed"):
                    evidence.compare_docker_transport_identities(before, changed)
            finally:
                server.close()
            path.unlink()
            path.write_bytes(b"not a socket")
            with self.assertRaisesRegex(evidence.EvidenceError, "Unix socket"):
                evidence.docker_transport_identity(path)

    def test_docker_control_plane_binds_executables_receipts_and_transport(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            docker = root / "docker"
            compiler = shutil.which("cc")
            if compiler is None:
                self.skipTest("a native C compiler is required")
            source = root / "docker.c"
            source.write_text(
                "#include <stdio.h>\n#include <string.h>\n"
                "int main(int argc,char **argv){"
                'if(argc==2&&!strcmp(argv[1],"version")){puts("docker-test");return 0;}'
                'if(argc==3&&!strcmp(argv[1],"buildx")&&!strcmp(argv[2],"version")){puts("buildx-test");return 0;}'
                'if(argc==4&&!strcmp(argv[1],"info")){printf("[{\\"Name\\":\\"buildx\\",\\"Path\\":\\"%s\\",\\"Version\\":\\"test\\"}]\\n",argv[0]);return 0;}'
                "return 2;}\n",
                encoding="utf-8",
            )
            subprocess.run(
                [compiler, "-std=c11", "-O0", "-o", str(docker), str(source)],
                check=True,
                timeout=30,
            )
            docker.chmod(0o500)
            socket_path = root / "docker.sock"
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            server.bind(str(socket_path))
            server.listen(1)
            try:
                transport = evidence.docker_transport_identity(socket_path)
                environment = {"LANG": "C"}
                probes = (
                    ("docker", [str(docker), "version"]),
                    ("buildx", [str(docker), "buildx", "version"]),
                    (
                        "plugins",
                        [
                            str(docker),
                            "info",
                            "--format",
                            "{{json .ClientInfo.Plugins}}",
                        ],
                    ),
                )
                artifacts: dict[
                    str, tuple[pathlib.Path, pathlib.Path, pathlib.Path]
                ] = {}
                for label, argv in probes:
                    result = evidence.bounded_process.run(
                        evidence.bounded_process.bind_executable(docker),
                        argv,
                        cwd=root,
                        environment=environment,
                        timeout_seconds=5,
                        drain_timeout_seconds=1,
                        maximum_output_bytes=1024 * 1024,
                        description=f"{label} fixture",
                    )
                    receipt = root / f"{label}.receipt.json"
                    stdout = root / f"{label}.stdout"
                    stderr = root / f"{label}.stderr"
                    evidence.atomic_write_bytes(
                        receipt,
                        evidence.bounded_process.dump_execution_receipt(result.receipt),
                    )
                    evidence.atomic_write_bytes(stdout, result.stdout)
                    evidence.atomic_write_bytes(stderr, result.stderr)
                    artifacts[label] = (receipt, stdout, stderr)
                report = evidence.docker_control_plane_report(
                    workspace=root,
                    docker_version_receipt=artifacts["docker"][0],
                    docker_version_stdout=artifacts["docker"][1],
                    docker_version_stderr=artifacts["docker"][2],
                    buildx_version_receipt=artifacts["buildx"][0],
                    buildx_version_stdout=artifacts["buildx"][1],
                    buildx_version_stderr=artifacts["buildx"][2],
                    plugin_inventory_receipt=artifacts["plugins"][0],
                    plugin_inventory_stdout=artifacts["plugins"][1],
                    plugin_inventory_stderr=artifacts["plugins"][2],
                    transport_identity=transport,
                )
                self.assertTrue(report["verified"])
                self.assertEqual(
                    report["docker_executable"]["sha256"],
                    report["buildx_plugin_executable"]["sha256"],
                )
                hostile = json.loads(artifacts["plugins"][1].read_text())
                hostile[0]["Path"] = str(root / "missing")
                artifacts["plugins"][1].write_text(
                    json.dumps(hostile), encoding="utf-8"
                )
                with self.assertRaises(evidence.EvidenceError):
                    evidence.docker_control_plane_report(
                        workspace=root,
                        docker_version_receipt=artifacts["docker"][0],
                        docker_version_stdout=artifacts["docker"][1],
                        docker_version_stderr=artifacts["docker"][2],
                        buildx_version_receipt=artifacts["buildx"][0],
                        buildx_version_stdout=artifacts["buildx"][1],
                        buildx_version_stderr=artifacts["buildx"][2],
                        plugin_inventory_receipt=artifacts["plugins"][0],
                        plugin_inventory_stdout=artifacts["plugins"][1],
                        plugin_inventory_stderr=artifacts["plugins"][2],
                        transport_identity=transport,
                    )
            finally:
                server.close()

    def test_gate_evidence_chain_binds_execution_skip_spools_and_external_tail(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            ledger = root / "gate-evidence-ledger.ndjson"
            executable = pathlib.Path("/bin/echo").resolve(strict=True)
            result = evidence.bounded_process.run(
                evidence.bounded_process.bind_executable(executable),
                [str(executable), "ok"],
                cwd=root,
                environment={"LANG": "C", "PYTHONDONTWRITEBYTECODE": "1"},
                timeout_seconds=5,
                drain_timeout_seconds=1,
                maximum_output_bytes=1024,
                description="gate evidence fixture",
            )
            evidence.atomic_write_bytes(root / "one.log", result.stdout)
            evidence.atomic_write_bytes(root / "one.stderr", result.stderr)
            evidence.atomic_write_bytes(
                root / "one.receipt.json",
                evidence.bounded_process.dump_execution_receipt(result.receipt),
            )
            tail = evidence.append_gate_execution(
                ledger,
                root / "one.receipt.json",
                gate="one",
                outcome="PASS",
                elapsed_seconds=1,
                expected_ordinal=1,
                expected_prior_sha256=None,
            )
            skip_path = evidence.publish_gate_skip(root, "two")
            skip = evidence.load_json(skip_path)
            tail = evidence.append_gate_skip(
                ledger,
                skip_path,
                gate="two",
                expected_ordinal=2,
                expected_prior_sha256=tail,
            )
            manifest = {
                "schema_version": 1,
                "platform": "linux/arm64",
                "gates": [
                    {"ordinal": 1, "phase": "A", "name": "one"},
                    {"ordinal": 2, "phase": "A", "name": "two"},
                ],
            }
            summary = root / "gates.tsv"
            summary.write_text(
                f"one\tPASS\t1\t{result.receipt['receipt_sha256']}\n"
                f"two\tFAIL(SKIPPED_PREREQUISITE)\t0\t{skip['skip_sha256']}\n",
                encoding="utf-8",
            )
            parsed = evidence.parse_gate_summary(summary, 1, manifest)
            report = evidence.bind_gate_evidence_ledger(
                parsed, ledger, expected_count=2, expected_tail_sha256=tail
            )
            self.assertTrue(report["all_gate_evidence_verified"])
            self.assertEqual(report["gate_evidence_count"], 2)
            with self.assertRaisesRegex(evidence.EvidenceError, "external anchor"):
                evidence.bind_gate_evidence_ledger(
                    parsed,
                    ledger,
                    expected_count=2,
                    expected_tail_sha256="0" * 64,
                )
            hostile = json.loads(json.dumps(parsed))
            hostile["gates"][1]["command_sha256"] = "0" * 64
            with self.assertRaisesRegex(evidence.EvidenceError, "summary"):
                evidence.bind_gate_evidence_ledger(
                    hostile, ledger, expected_count=2, expected_tail_sha256=tail
                )

    def test_platform_parser_is_exact_and_normalizes_builder_wildcards(self) -> None:
        text = "Name: test\nPlatforms: linux/amd64*, linux/arm64, linux/amd64/v2\n"
        report = evidence.platform_report("linux/arm64", text)
        self.assertTrue(report["supported"])
        self.assertEqual(
            report["reported_platforms"],
            ["linux/amd64", "linux/amd64/v2", "linux/arm64"],
        )
        self.assertFalse(
            evidence.platform_report("linux/arm64", "Platforms: darwin/arm64")[
                "supported"
            ]
        )
        with self.assertRaises(evidence.EvidenceError):
            evidence.platform_report("linux/s390x", text)

    def test_credential_guard_rejects_root_caps_secrets_config_and_claude(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            home = pathlib.Path(temporary) / "home"
            path = pathlib.Path(temporary) / "bin"
            home.mkdir()
            path.mkdir()
            base = {
                "home": home,
                "path_value": str(path),
                "environment": {"HOME": str(home)},
                "effective_uid": 10001,
                "effective_capabilities_hex": "0" * 16,
                "require_linux": False,
            }
            self.assertTrue(evidence.credential_free_guard(**base)["credential_free"])
            for change in (
                {"effective_uid": 0},
                {"effective_capabilities_hex": "1"},
                {"environment": {"ANTHROPIC_API_KEY": "secret"}},
            ):
                with self.subTest(change=change):
                    arguments = base | change
                    with self.assertRaises(evidence.EvidenceError):
                        evidence.credential_free_guard(**arguments)
            (home / ".claude").mkdir()
            with self.assertRaisesRegex(evidence.EvidenceError, "credential/config"):
                evidence.credential_free_guard(**base)
            (home / ".claude").rmdir()
            claude = path / "claude"
            claude.write_text("#!/bin/sh\n", encoding="utf-8")
            claude.chmod(0o755)
            with self.assertRaisesRegex(evidence.EvidenceError, "Claude executable"):
                evidence.credential_free_guard(**base)

    def test_runtime_system_manifest_is_exact_and_snapshot_bound(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source_root = pathlib.Path(temporary).resolve()
            requirements = source_root / "tools/linux-docker/python-requirements.txt"
            requirements.parent.mkdir(parents=True)
            shutil.copy2(TOOLS / "python-requirements.txt", requirements)
            source = source_digest.workspace_source_manifest(source_root)
        package_bytes = b"alpha\t1.0\n"
        snapshot = {
            "schema_version": 1,
            "snapshot": evidence.DEBIAN_SNAPSHOT,
            "inrelease_sha256": dict(evidence.DEBIAN_SNAPSHOT_INRELEASE_SHA256),
        }
        workspace_permission_body = {
            "schema_version": 1,
            "root": "/workspace",
            "owner_uid": 0,
            "node_count": 1,
            "directory_count": 1,
            "file_count": 0,
            "symlink_count": 0,
            "root_owned": True,
            "group_world_nonwritable": True,
            "records": [
                {
                    "path": ".",
                    "kind": "directory",
                    "mode": "0755",
                    "uid": 0,
                    "gid": 0,
                    "device": 1,
                    "inode": 1,
                    "nlink": 2,
                    "link_target": None,
                }
            ],
        }
        workspace_permissions = {
            **workspace_permission_body,
            "permissions_sha256": evidence.canonical_json_sha256(
                workspace_permission_body,
                domain="pmux.evidence.runtime-workspace-permissions.v1",
            ),
        }
        system = {
            "schema_version": 1,
            "kernel": "linux",
            "machine": "aarch64",
            "platform": "Linux-test",
            "container_platform": "linux/arm64",
            "uid": 10001,
            "gid": 10001,
            "rustc": "rustc 1.88.0",
            "cargo": "cargo 1.88.0",
            "node": "v22.0.0",
            "python": "3.11.0",
            "base_image": BASE_IMAGE,
            "installed_packages_sha256": hashlib.sha256(package_bytes).hexdigest(),
            "installed_packages_line_count": 1,
            "installed_packages": [{"package": "alpha", "version": "1.0"}],
            "apt_reproducibility": (
                "snapshot_pinned_exact_inrelease_and_installed_closure"
            ),
            "debian_snapshot": snapshot,
            "python_requirements_sha256": evidence.PYTHON_REQUIREMENTS_SHA256,
            "workspace_permissions": workspace_permissions,
            "source": source,
            "test_storage_filesystem": "tmpfs",
            "real_claude_invoked": False,
            "credential_free": True,
            "real_claude_available": False,
            "effective_uid": 10001,
            "effective_capabilities_hex": "0000000000000000",
        }
        self.assertEqual(
            evidence.validate_runtime_system_manifest(
                system,
                expected_source_sha256=source["workspace_source_sha256"],
                expected_platform="linux/arm64",
                expected_base_image=BASE_IMAGE,
            ),
            system,
        )

        hostile_systems: list[dict[str, object]] = []
        extra = json.loads(json.dumps(system))
        extra["unexpected"] = True
        hostile_systems.append(extra)
        bool_uid = json.loads(json.dumps(system))
        bool_uid["uid"] = True
        hostile_systems.append(bool_uid)
        wrong_snapshot = json.loads(json.dumps(system))
        wrong_snapshot["debian_snapshot"]["snapshot"] = "latest"
        hostile_systems.append(wrong_snapshot)
        wrong_requirement = json.loads(json.dumps(system))
        wrong_requirement["python_requirements_sha256"] = "0" * 64
        hostile_systems.append(wrong_requirement)
        unsorted_packages = json.loads(json.dumps(system))
        unsorted_packages["installed_packages"] = [
            {"package": "zulu", "version": "1"},
            {"package": "alpha", "version": "1"},
        ]
        unsorted_packages["installed_packages_line_count"] = 2
        hostile_systems.append(unsorted_packages)
        for hostile in hostile_systems:
            with self.subTest(hostile=hostile):
                with self.assertRaises(evidence.EvidenceError):
                    evidence.validate_runtime_system_manifest(
                        hostile,
                        expected_source_sha256=source["workspace_source_sha256"],
                        expected_platform="linux/arm64",
                        expected_base_image=BASE_IMAGE,
                    )

    def test_runtime_workspace_permissions_are_complete_and_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            root.chmod(0o755)
            artifact = root / "artifact"
            artifact.write_bytes(b"immutable\n")
            artifact.chmod(0o644)
            nested = root / "nested"
            nested.mkdir(mode=0o755)
            (nested / "relative-link").symlink_to("../artifact")

            manifest = evidence.runtime_workspace_permission_manifest(
                root, expected_owner_uid=os.getuid()
            )
            self.assertEqual(
                evidence.validate_runtime_workspace_permission_manifest(
                    manifest,
                    expected_root=str(root),
                    expected_owner_uid=os.getuid(),
                ),
                manifest,
            )
            self.assertEqual(manifest["node_count"], 4)
            self.assertEqual(manifest["symlink_count"], 1)

            for hostile_target in ("/etc/passwd", "../../outside"):
                hostile = json.loads(json.dumps(manifest))
                link_row = next(
                    row for row in hostile["records"] if row["kind"] == "symlink"
                )
                link_row["link_target"] = hostile_target
                body = dict(hostile)
                del body["permissions_sha256"]
                hostile["permissions_sha256"] = evidence.canonical_json_sha256(
                    body, domain="pmux.evidence.runtime-workspace-permissions.v1"
                )
                with self.subTest(hostile_target=hostile_target):
                    with self.assertRaisesRegex(
                        evidence.EvidenceError, "absolute|escapes"
                    ):
                        evidence.validate_runtime_workspace_permission_manifest(
                            hostile,
                            expected_root=str(root),
                            expected_owner_uid=os.getuid(),
                        )

            orphaned = json.loads(json.dumps(manifest))
            orphan_row = next(
                row for row in orphaned["records"] if row["path"] == "artifact"
            )
            orphan_row["path"] = "missing/artifact"
            orphaned["records"] = sorted(
                orphaned["records"], key=lambda row: row["path"]
            )
            body = dict(orphaned)
            del body["permissions_sha256"]
            orphaned["permissions_sha256"] = evidence.canonical_json_sha256(
                body, domain="pmux.evidence.runtime-workspace-permissions.v1"
            )
            with self.assertRaisesRegex(evidence.EvidenceError, "directory parent"):
                evidence.validate_runtime_workspace_permission_manifest(
                    orphaned,
                    expected_root=str(root),
                    expected_owner_uid=os.getuid(),
                )

            artifact.chmod(0o666)
            with self.assertRaisesRegex(evidence.EvidenceError, "group/world writable"):
                evidence.runtime_workspace_permission_manifest(
                    root, expected_owner_uid=os.getuid()
                )

    def test_source_and_cell_binding_require_every_exact_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source_root = pathlib.Path(temporary).resolve()
            (source_root / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
            requirements = source_root / "tools" / "linux-docker"
            requirements.mkdir(parents=True)
            shutil.copy2(
                TOOLS / "python-requirements.txt",
                requirements / "python-requirements.txt",
            )
            shutil.copy2(source_digest.__file__, requirements / "source_digest.py")
            source = source_digest.workspace_source_manifest(source_root)
            expected_source = source["workspace_source_sha256"]
            stable = evidence.compare_source_manifests(source, source, expected_source)
            self.assertTrue(stable["verified"])
            subprocess.run(
                ["git", "init", "-q", str(source_root)], check=True, timeout=10
            )
            subprocess.run(
                ["git", "-C", str(source_root), "add", "-A"],
                check=True,
                timeout=10,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(source_root),
                    "-c",
                    "user.name=pmux-test",
                    "-c",
                    "user.email=pmux-test.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "initial",
                ],
                check=True,
                timeout=10,
            )
            candidate_dir = source_root / "candidate"
            reproduced_dir = source_root / "reproduced"
            candidate_dir.mkdir(mode=0o700)
            reproduced_dir.mkdir(mode=0o700)
            for name in evidence.REQUIRED_RELEASE_BINARIES:
                (candidate_dir / name).write_bytes(name.encode())
                (candidate_dir / name).chmod(0o500)
                (reproduced_dir / name).write_bytes(name.encode())
                (reproduced_dir / name).chmod(0o500)
            binaries = evidence.release_binary_manifest(candidate_dir)
            reproduced = evidence.release_binary_manifest(reproduced_dir)
            comparison = evidence.compare_reproduced_release_binaries(
                binaries, reproduced_dir
            )
            package_bytes = b"alpha\t1.0\n"
            permission_body = {
                "schema_version": 1,
                "root": "/workspace",
                "owner_uid": 0,
                "node_count": 1,
                "directory_count": 1,
                "file_count": 0,
                "symlink_count": 0,
                "root_owned": True,
                "group_world_nonwritable": True,
                "records": [
                    {
                        "path": ".",
                        "kind": "directory",
                        "mode": "0755",
                        "uid": 0,
                        "gid": 0,
                        "device": 1,
                        "inode": 1,
                        "nlink": 2,
                        "link_target": None,
                    }
                ],
            }
            permissions = {
                **permission_body,
                "permissions_sha256": evidence.canonical_json_sha256(
                    permission_body,
                    domain="pmux.evidence.runtime-workspace-permissions.v1",
                ),
            }
            system = {
                "schema_version": 1,
                "kernel": "linux",
                "machine": "aarch64",
                "platform": "Linux-test",
                "container_platform": "linux/arm64",
                "uid": 10001,
                "gid": 10001,
                "rustc": "rustc 1.88.0",
                "cargo": "cargo 1.88.0",
                "node": "v22",
                "python": "3.11",
                "base_image": BASE_IMAGE,
                "installed_packages_sha256": hashlib.sha256(package_bytes).hexdigest(),
                "installed_packages_line_count": 1,
                "installed_packages": [{"package": "alpha", "version": "1.0"}],
                "apt_reproducibility": "snapshot_pinned_exact_inrelease_and_installed_closure",
                "debian_snapshot": {
                    "schema_version": 1,
                    "snapshot": evidence.DEBIAN_SNAPSHOT,
                    "inrelease_sha256": dict(evidence.DEBIAN_SNAPSHOT_INRELEASE_SHA256),
                },
                "python_requirements_sha256": evidence.PYTHON_REQUIREMENTS_SHA256,
                "workspace_permissions": permissions,
                "source": source,
                "test_storage_filesystem": "tmpfs",
                "real_claude_invoked": False,
                "credential_free": True,
                "real_claude_available": False,
                "effective_uid": 10001,
                "effective_capabilities_hex": "0000000000000000",
            }
            gate_manifest = {
                "schema_version": 1,
                "platform": "linux/arm64",
                "gate_count": 1,
                "gates": [{"ordinal": 1, "phase": "A", "name": "one"}],
                "declared_manifest_sha256": "a" * 64,
            }
            result = {
                "schema_version": 1,
                "status": "pass",
                "failure_count": 0,
                "gate_count": 1,
                "platform": "linux/arm64",
                "expected_manifest_sha256": evidence.canonical_json_sha256(
                    gate_manifest, domain="pmux.evidence.platform-gate-manifest.v1"
                ),
                "gates": [
                    {
                        "name": "one",
                        "outcome": "PASS",
                        "elapsed_seconds": 0,
                        "command_sha256": "c" * 64,
                    }
                ],
                "gate_evidence_count": 1,
                "gate_evidence_tail_sha256": "d" * 64,
                "gate_evidence_ledger_bytes": 1,
                "gate_evidence_ledger_sha256": "e" * 64,
                "gate_evidence_rows_sha256": "f" * 64,
                "all_gate_evidence_verified": True,
            }
            manifest_sha = evidence.canonical_json_sha256(
                binaries, domain="pmux.evidence.release-binary-manifest.v1"
            )
            uds = {
                "schema_version": 1,
                "verified": True,
                "release_binary_manifest_sha256": manifest_sha,
                "uds_report_sha256": "1" * 64,
                "owner_receipt_sha256": "2" * 64,
                "intruder_receipt_sha256": "3" * 64,
                "candidate_write_receipt_sha256": "4" * 64,
                "outer_probe_receipt_sha256": "5" * 64,
                "server_version": "test",
            }
            revision_before = source_digest.workspace_revision_capture(source_root)
            revision_after = source_digest.workspace_revision_capture(source_root)
            revision_stability = evidence.compare_workspace_revision_captures(
                revision_before, revision_after
            )
            arguments = dict(
                host_source=source,
                host_revision_before=revision_before,
                host_revision_after=revision_after,
                host_revision_stability=revision_stability,
                container_system=system,
                image_binaries=binaries,
                binaries_before=binaries,
                binaries_after=binaries,
                reproduced_binaries=reproduced,
                reproduction_comparison=comparison,
                uds_binding=uds,
                suite_result=result,
                gate_manifest=gate_manifest,
                expected_source_sha256=expected_source,
                expected_platform="linux/arm64",
                expected_base_image=BASE_IMAGE,
            )
            report = evidence.verify_cell_binding(**arguments)
            self.assertTrue(report["verified"])
            for field in (
                "host_revision_before",
                "host_revision_after",
                "host_revision_stability",
                "container_system",
                "image_binaries",
                "reproduced_binaries",
                "reproduction_comparison",
                "uds_binding",
                "suite_result",
                "gate_manifest",
            ):
                hostile_arguments = json.loads(json.dumps(arguments))
                hostile_arguments[field]["unexpected"] = True
                with (
                    self.subTest(field=field),
                    self.assertRaises(evidence.EvidenceError),
                ):
                    evidence.verify_cell_binding(**hostile_arguments)
            hostile_values = []
            hostile = json.loads(json.dumps(arguments))
            hostile["suite_result"]["gate_count"] = True
            hostile_values.append(hostile)
            hostile = json.loads(json.dumps(arguments))
            hostile["suite_result"]["all_gate_evidence_verified"] = 1
            hostile_values.append(hostile)
            hostile = json.loads(json.dumps(arguments))
            hostile["gate_manifest"]["gate_count"] = True
            hostile_values.append(hostile)
            hostile = json.loads(json.dumps(arguments))
            hostile["uds_binding"]["verified"] = 1
            hostile_values.append(hostile)
            hostile = json.loads(json.dumps(arguments))
            hostile["image_binaries"]["binaries"]["pmux"]["nlink"] = True
            hostile_values.append(hostile)
            hostile = json.loads(json.dumps(arguments))
            hostile["host_revision_before"], hostile["host_revision_after"] = (
                hostile["host_revision_after"],
                hostile["host_revision_before"],
            )
            hostile_values.append(hostile)
            hostile = json.loads(json.dumps(arguments))
            hostile["host_revision_stability"]["before_capture_sha256"] = "9" * 64
            hostile_values.append(hostile)
            for hostile in hostile_values:
                with (
                    self.subTest(hostile=hostile),
                    self.assertRaises(evidence.EvidenceError),
                ):
                    evidence.verify_cell_binding(**hostile)

    def test_cleanup_plans_are_exact_and_reject_unowned_names_or_ids(self) -> None:
        identities = (
            evidence.DockerResourceIdentity(
                "builder", "pmux-linux-builder-arm64-abcd-1234", "a" * 64
            ),
            evidence.DockerResourceIdentity(
                "container", "pmux-linux-amd64-abcd-1234", "b" * 64
            ),
            evidence.DockerResourceIdentity(
                "image",
                "pmux-linux-deterministic:arm64-abcd-1234",
                "sha256:" + "c" * 64,
            ),
        )
        plans = [evidence.cleanup_plan(identity) for identity in identities]
        self.assertEqual(
            plans[0], ("docker", "buildx", "rm", "--force", identities[0].name)
        )
        self.assertEqual(plans[1][-1], identities[1].object_id)
        self.assertEqual(plans[2][-1], identities[2].object_id)
        for invalid in (
            evidence.DockerResourceIdentity("builder", "default", "a" * 64),
            evidence.DockerResourceIdentity(
                "container", "pmux-linux-arm64-x", "not-an-id"
            ),
            evidence.DockerResourceIdentity(
                "image", "ubuntu:latest", "sha256:" + "c" * 64
            ),
        ):
            with self.subTest(invalid=invalid):
                with self.assertRaises(evidence.EvidenceError):
                    evidence.cleanup_plan(invalid)

    def test_gate_summary_reconciles_failure_count(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            summary = pathlib.Path(temporary).resolve() / "gates.tsv"
            summary.write_text(
                f"one\tPASS\t1\t{'a' * 64}\ntwo\tFAIL(7)\t2\t{'b' * 64}\n",
                encoding="utf-8",
            )
            manifest = {
                "schema_version": 1,
                "platform": "linux/arm64",
                "gates": [
                    {"ordinal": 1, "phase": "A", "name": "one"},
                    {"ordinal": 2, "phase": "A", "name": "two"},
                ],
            }
            result = evidence.parse_gate_summary(summary, 1, manifest)
            self.assertEqual(result["status"], "fail")
            with self.assertRaisesRegex(evidence.EvidenceError, "count"):
                evidence.parse_gate_summary(summary, 0, manifest)

    def test_gate_summary_rejects_missing_duplicate_reordered_extra_and_zero(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            summary = pathlib.Path(temporary).resolve() / "gates.tsv"

            def row(name: str) -> str:
                return f"{name}\tPASS\t0\t{'a' * 64}\n"

            manifest = {
                "schema_version": 1,
                "platform": "linux/arm64",
                "gates": [
                    {"ordinal": 1, "phase": "A", "name": "one"},
                    {"ordinal": 2, "phase": "B", "name": "two"},
                ],
            }
            for names in (
                (),
                ("one",),
                ("one", "one"),
                ("two", "one"),
                ("one", "two", "three"),
            ):
                with self.subTest(names=names):
                    summary.write_text(
                        "".join(row(name) for name in names), encoding="utf-8"
                    )
                    with self.assertRaisesRegex(
                        evidence.EvidenceError,
                        "missing, duplicated, reordered, or extra",
                    ):
                        evidence.parse_gate_summary(summary, 0, manifest)

    def test_declared_gate_manifest_is_nonempty_unique_and_phase_ordered(self) -> None:
        declared = evidence.load_json(TOOLS / "gate-a-manifest.json")
        self.assertEqual(evidence.validate_declared_gate_manifest(declared), declared)
        report = evidence.platform_gate_manifest(declared, "linux/amd64")
        self.assertEqual(
            evidence.validate_platform_gate_manifest(
                report, expected_platform="linux/amd64", declared=declared
            ),
            report,
        )
        self.assertEqual(report["platform"], "linux/amd64")
        self.assertGreater(report["gate_count"], 0)
        names = [row["name"] for row in report["gates"]]
        self.assertEqual(len(names), len(set(names)))
        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        observed: list[str] = []
        for match in re.finditer(
            r"^\s*(?:run_gate|skip_gate)\s+([a-z0-9_]+)", suite, re.MULTILINE
        ):
            name = match.group(1)
            if name not in observed:
                observed.append(name)
        self.assertEqual(observed, names)

    def test_gate_manifests_reject_extra_wrong_type_and_substitution(self) -> None:
        declared = evidence.load_json(TOOLS / "gate-a-manifest.json")
        platform_manifest = evidence.platform_gate_manifest(declared, "linux/arm64")

        hostile_declared: list[dict[str, object]] = []
        extra = json.loads(json.dumps(declared))
        extra["unexpected"] = True
        hostile_declared.append(extra)
        numeric_schema = json.loads(json.dumps(declared))
        numeric_schema["schema_version"] = True
        hostile_declared.append(numeric_schema)
        duplicate = json.loads(json.dumps(declared))
        duplicate["gates"].append(dict(duplicate["gates"][0]))
        hostile_declared.append(duplicate)
        reordered = json.loads(json.dumps(declared))
        reordered["gates"] = list(reversed(reordered["gates"]))
        hostile_declared.append(reordered)
        for hostile in hostile_declared:
            with self.subTest(kind="declared", hostile=hostile):
                with self.assertRaises(evidence.EvidenceError):
                    evidence.validate_declared_gate_manifest(hostile)

        hostile_platform: list[dict[str, object]] = []
        extra_platform = json.loads(json.dumps(platform_manifest))
        extra_platform["unexpected"] = True
        hostile_platform.append(extra_platform)
        bool_count = json.loads(json.dumps(platform_manifest))
        bool_count["gate_count"] = True
        hostile_platform.append(bool_count)
        wrong_ordinal = json.loads(json.dumps(platform_manifest))
        wrong_ordinal["gates"][0]["ordinal"] = 2
        hostile_platform.append(wrong_ordinal)
        substituted_digest = json.loads(json.dumps(platform_manifest))
        substituted_digest["declared_manifest_sha256"] = "0" * 64
        hostile_platform.append(substituted_digest)
        for hostile in hostile_platform:
            with self.subTest(kind="platform", hostile=hostile):
                with self.assertRaises(evidence.EvidenceError):
                    evidence.validate_platform_gate_manifest(
                        hostile,
                        expected_platform="linux/arm64",
                        declared=declared,
                    )

    def test_base_image_index_binds_raw_digest_and_both_platforms(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary).resolve() / "index.json"
            payload = json.dumps(
                {
                    "schemaVersion": 2,
                    "mediaType": "application/vnd.oci.image.index.v1+json",
                    "manifests": [
                        {
                            "mediaType": "application/vnd.oci.image.manifest.v1+json",
                            "digest": "sha256:" + "a" * 64,
                            "size": 100,
                            "platform": {
                                "os": "linux",
                                "architecture": "arm64",
                                "variant": "v8",
                            },
                        },
                        {
                            "mediaType": "application/vnd.oci.image.manifest.v1+json",
                            "digest": "sha256:" + "b" * 64,
                            "size": 101,
                            "platform": {"os": "linux", "architecture": "amd64"},
                        },
                    ],
                },
                separators=(",", ":"),
                sort_keys=True,
            ).encode()
            path.write_bytes(payload)
            reference = (
                "docker.io/library/rust:1.88.0-bookworm@sha256:"
                + hashlib.sha256(payload).hexdigest()
            )
            self.assertTrue(
                evidence.verify_base_image_index(reference, path)["verified"]
            )
            self.assertEqual(
                evidence.verify_base_image_index(reference, path)[
                    "required_platform_descriptors"
                ][0]["platform"],
                "linux/amd64",
            )
            path.write_bytes(payload + b"\n")
            self.assertTrue(
                evidence.verify_base_image_index(reference, path)[
                    "stripped_cli_newline"
                ]
            )
            with self.assertRaisesRegex(evidence.EvidenceError, "requested digest"):
                evidence.verify_base_image_index(BASE_IMAGE, path)

    def test_base_image_index_rejects_duplicate_nonfinite_and_nested_types(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary).resolve() / "index.json"

            def verify(payload: bytes) -> None:
                path.write_bytes(payload)
                reference = (
                    "docker.io/library/rust:1.88.0-bookworm@sha256:"
                    + hashlib.sha256(payload).hexdigest()
                )
                evidence.verify_base_image_index(reference, path)

            duplicate = (
                b'{"schemaVersion":2,"schemaVersion":2,'
                b'"mediaType":"application/vnd.oci.image.index.v1+json",'
                b'"manifests":[]}'
            )
            nonfinite = b'{"schemaVersion":2,"mediaType":NaN,"manifests":[]}'
            malformed_rows = [
                {
                    "schemaVersion": 2,
                    "mediaType": "application/vnd.oci.image.index.v1+json",
                    "unexpected": True,
                    "manifests": [],
                },
                {
                    "schemaVersion": True,
                    "mediaType": "application/vnd.oci.image.index.v1+json",
                    "manifests": [],
                },
                {
                    "schemaVersion": 2,
                    "mediaType": "application/vnd.oci.image.index.v1+json",
                    "manifests": [
                        {
                            "mediaType": "application/vnd.oci.image.manifest.v1+json",
                            "digest": "sha256:" + "a" * 64,
                            "size": True,
                            "platform": {
                                "os": "linux",
                                "architecture": "arm64",
                            },
                        }
                    ],
                },
            ]
            for payload in (
                duplicate,
                nonfinite,
                *(
                    json.dumps(value, separators=(",", ":")).encode()
                    for value in malformed_rows
                ),
            ):
                with self.subTest(payload=payload):
                    with self.assertRaises(evidence.EvidenceError):
                        verify(payload)

    def test_uds_report_is_bound_to_the_complete_candidate_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            candidate = root / "candidate"
            candidate.mkdir(mode=0o700)
            for index, name in enumerate(evidence.REQUIRED_RELEASE_BINARIES):
                binary = candidate / name
                binary.write_bytes(f"binary-{index}".encode())
                binary.chmod(0o555)
            candidate.chmod(0o555)
            manifest = evidence.release_binary_manifest(candidate)
            candidate.chmod(0o700)

            workspace = root / "workspace"
            probe_script = workspace / "tools" / "linux-docker" / "permissions_probe.py"
            probe_script.parent.mkdir(parents=True)
            probe_script.write_text("# fixture identity only\n", encoding="utf-8")
            manifest_path = root / "image-release-binaries.json"
            manifest_path.write_text("fixture\n", encoding="utf-8")
            fixture_bin = root / "bin"
            fixture_bin.mkdir()
            fake_runuser = fixture_bin / "runuser"
            fake_python = fixture_bin / "python3"
            receipt_executable = pathlib.Path(sys.executable).resolve(strict=True)
            receipt_bases: dict[tuple[object, ...], dict[str, object]] = {}

            def resign_receipt(receipt: dict[str, object]) -> None:
                receipt["process_ledger_sha256"] = (
                    evidence.bounded_process._canonical_json_sha256(
                        receipt["process_ledger"],
                        domain=evidence.bounded_process.PROCESS_LEDGER_DOMAIN,
                    )
                )
                body = dict(receipt)
                body.pop("receipt_sha256")
                receipt["receipt_sha256"] = (
                    evidence.bounded_process._canonical_json_sha256(
                        body,
                        domain=evidence.bounded_process.EXECUTION_RECEIPT_DOMAIN,
                    )
                )

            def receipt(
                executable: pathlib.Path,
                argv: list[str],
                *,
                environment: dict[str, str],
                timeout_seconds: int,
                drain_timeout_seconds: int,
                maximum_output_bytes: int,
                stdout: bytes,
            ) -> dict[str, object]:
                witness = evidence.bounded_process.bind_executable(receipt_executable)
                argv[0] = str(executable)
                cache_key = (
                    tuple(sorted(environment.items())),
                    timeout_seconds,
                    drain_timeout_seconds,
                    maximum_output_bytes,
                )
                if cache_key not in receipt_bases:
                    receipt_bases[cache_key] = dict(
                        evidence.bounded_process.run(
                            witness,
                            [
                                witness.path,
                                "-c",
                                "import time; time.sleep(0.5)",
                            ],
                            cwd=workspace,
                            environment=environment,
                            timeout_seconds=timeout_seconds,
                            drain_timeout_seconds=drain_timeout_seconds,
                            maximum_output_bytes=maximum_output_bytes,
                            description="UDS receipt fixture",
                        ).receipt
                    )
                value = json.loads(json.dumps(receipt_bases[cache_key]))
                value["executable"]["path"] = str(executable)
                value["argv"] = argv
                value["stdout_size"] = len(stdout)
                value["stdout_sha256"] = hashlib.sha256(stdout).hexdigest()
                resign_receipt(value)
                return value

            child_environment = dict(evidence._UDS_CHILD_ENVIRONMENT)
            socket_path = "/var/tmp/pmux-uds-" + "a" * 32 + "/pmux.sock"
            runuser_prefix = [
                str(fake_runuser),
                "-u",
                "pmux",
                "--",
                "/usr/bin/env",
                "-i",
                "HOME=/home/pmux",
                "LOGNAME=pmux",
                "PATH=/usr/local/bin:/usr/bin:/bin",
                "USER=pmux",
            ]
            owner_receipt = receipt(
                fake_runuser,
                runuser_prefix
                + [
                    manifest["binaries"]["pmux"]["path"],
                    "--socket",
                    socket_path,
                    "--output",
                    "json",
                    "ping",
                ],
                environment=child_environment,
                timeout_seconds=15,
                drain_timeout_seconds=5,
                maximum_output_bytes=1024 * 1024,
                stdout=b'{"protocol_version":1,"server_version":"0.1.0"}\n',
            )
            intruder_prefix = [
                str(fake_runuser),
                "-u",
                "intruder",
                "--",
                "/usr/bin/env",
                "-i",
                "HOME=/home/intruder",
                "LOGNAME=intruder",
                "PATH=/usr/local/bin:/usr/bin:/bin",
                "USER=intruder",
            ]
            intruder_receipt = receipt(
                fake_runuser,
                intruder_prefix
                + [
                    "/usr/bin/python3",
                    str(probe_script),
                    "--connect-denied",
                    socket_path,
                ],
                environment=child_environment,
                timeout_seconds=15,
                drain_timeout_seconds=5,
                maximum_output_bytes=1024 * 1024,
                stdout=evidence._EACCES_JSON_LINE,
            )
            candidate_write_receipt = receipt(
                fake_runuser,
                runuser_prefix
                + [
                    "/usr/bin/python3",
                    str(probe_script),
                    "--write-denied",
                    manifest["binaries"]["pmuxd"]["path"],
                ],
                environment=child_environment,
                timeout_seconds=15,
                drain_timeout_seconds=5,
                maximum_output_bytes=1024 * 1024,
                stdout=evidence._EACCES_JSON_LINE,
            )

            outer_receipt = receipt(
                fake_python,
                [
                    str(fake_python),
                    str(probe_script),
                    "/var/tmp/pmux-root-evidence.A1b2C3d4/uds-permissions.json",
                    str(manifest_path),
                ],
                environment=dict(evidence._UDS_PROBE_ENVIRONMENT),
                timeout_seconds=90,
                drain_timeout_seconds=10,
                maximum_output_bytes=16 * 1024 * 1024,
                stdout=b"",
            )
            outer_process = outer_receipt["process_ledger"][0]
            assert isinstance(outer_process, dict)
            outer_process["pgid"] = outer_process["pid"]
            outer_process["sid"] = outer_process["pid"]
            outer_process["started"] = "linux:fixture-boot:200"
            resign_receipt(outer_receipt)

            managed_receipt = receipt(
                fake_runuser,
                runuser_prefix
                + [
                    manifest["binaries"]["pmuxd"]["path"],
                    "serve",
                    "--socket",
                    socket_path,
                    "--runtime-parent",
                    str(pathlib.PurePosixPath(socket_path).parent / "runtimes"),
                ],
                environment=dict(evidence._UDS_DAEMON_ENVIRONMENT),
                timeout_seconds=90,
                drain_timeout_seconds=10,
                maximum_output_bytes=16 * 1024 * 1024,
                stdout=b"",
            )
            managed_receipt["kind"] = "pmux_managed_process"
            managed_receipt["graceful_stop_timeout_seconds"] = 20
            managed_leader = managed_receipt["process_ledger"][0]
            managed_leader["pid"] = outer_process["pid"]
            managed_leader["pgid"] = outer_process["pid"]
            managed_leader["sid"] = outer_process["pid"]
            managed_leader["started"] = "linux:fixture-boot:200"
            managed_receipt["process_ledger_sha256"] = (
                evidence.bounded_process._canonical_json_sha256(
                    managed_receipt["process_ledger"],
                    domain=evidence.bounded_process.PROCESS_LEDGER_DOMAIN,
                )
            )
            managed_receipt["stop_request"] = {
                "schema_version": 1,
                "kind": "expected",
                "requested": True,
                "signal": 15,
                "target_pid": managed_leader["pid"],
                "target_started": managed_leader["started"],
            }
            managed_body = dict(managed_receipt)
            managed_body.pop("receipt_sha256")
            managed_receipt["receipt_sha256"] = (
                evidence.bounded_process._canonical_json_sha256(
                    managed_body,
                    domain=evidence.managed_process.MANAGED_EXECUTION_RECEIPT_DOMAIN,
                )
            )

            body = {
                "schema_version": 3,
                "status": "pass",
                "release_binary_manifest_sha256": evidence.canonical_json_sha256(
                    manifest, domain="pmux.evidence.release-binary-manifest.v1"
                ),
                "pmuxd_sha256": manifest["binaries"]["pmuxd"]["sha256"],
                "pmux_sha256": manifest["binaries"]["pmux"]["sha256"],
                "pmuxd_process": {
                    "pid": outer_process["pid"],
                    "process_group": outer_process["pid"],
                    "session": outer_process["pid"],
                    "start_ticks": 200,
                },
                "pmuxd_managed_receipt": managed_receipt,
                "managed_process_implementation": evidence.MANAGED_PROCESS_IMPLEMENTATION,
                "daemon_exit_code": 0,
                "socket_identity": {
                    "device": 1,
                    "inode": 2,
                    "uid": 10001,
                    "gid": 10001,
                    "mode": "0600",
                },
                "socket_parent_device": 1,
                "socket_parent_inode": 3,
                "socket_parent_uid": 10001,
                "socket_parent_gid": 10001,
                "socket_parent_mode": "0700",
                "socket_revalidated": True,
                "runtime_parent_device": 1,
                "runtime_parent_inode": 4,
                "runtime_parent_uid": 10001,
                "runtime_parent_gid": 10001,
                "runtime_parent_mode": "0700",
                "runtime_parent_revalidated": True,
                "owner_exit_code": 0,
                "owner_process_receipt": owner_receipt,
                "intruder_exit_code": 0,
                "intruder_process_receipt": intruder_receipt,
                "intruder_denial": {
                    "denied": True,
                    "errno_name": "EACCES",
                    "errno_number": 13,
                },
                "candidate_write_exit_code": 0,
                "candidate_write_process_receipt": candidate_write_receipt,
                "candidate_write_denial": {
                    "denied": True,
                    "errno_name": "EACCES",
                    "errno_number": 13,
                },
                "candidate_manifest_revalidated": True,
                "protocol_version": 1,
                "server_version": "0.1.0",
                "different_uid_denied": True,
                "process_session_empty": True,
                "residual_processes": [],
                "socket_removed": True,
                "runtime_entries_after_shutdown": [],
                "private_probe_tree_removed": True,
                "failure_type": None,
                "failure_message": None,
            }
            report = {
                **body,
                "report_sha256": evidence.canonical_json_sha256(
                    body, domain="pmux.evidence.uds-permissions-report.v3"
                ),
            }

            def publication_receipt(value: dict[str, object]) -> dict[str, object]:
                published = json.loads(json.dumps(outer_receipt))
                publication = f"{value['report_sha256']}\n".encode()
                published["stdout_size"] = len(publication)
                published["stdout_sha256"] = hashlib.sha256(publication).hexdigest()
                resign_receipt(published)
                return published

            outer_publication = publication_receipt(report)
            binding = evidence.verify_uds_report(
                report, manifest, outer_publication, manifest_path
            )
            self.assertTrue(binding["verified"])
            self.assertEqual(
                binding["owner_receipt_sha256"],
                report["owner_process_receipt"]["receipt_sha256"],
            )
            self.assertEqual(
                binding["candidate_write_receipt_sha256"],
                report["candidate_write_process_receipt"]["receipt_sha256"],
            )

            substituted = json.loads(json.dumps(report))
            substituted["release_binary_manifest_sha256"] = "0" * 64
            substituted_body = dict(substituted)
            del substituted_body["report_sha256"]
            substituted["report_sha256"] = evidence.canonical_json_sha256(
                substituted_body, domain="pmux.evidence.uds-permissions-report.v3"
            )
            self.assertFalse(
                evidence.verify_uds_report(
                    substituted,
                    manifest,
                    publication_receipt(substituted),
                    manifest_path,
                )["verified"]
            )
            wrong_candidate_receipt = json.loads(json.dumps(candidate_write_receipt))
            write_flag = wrong_candidate_receipt["argv"].index("--write-denied")
            wrong_candidate_receipt["argv"][write_flag] = "--connect-denied"
            resign_receipt(wrong_candidate_receipt)
            hostile_reports = [
                report | {"candidate_manifest_revalidated": False},
                report
                | {
                    "candidate_write_denial": {
                        "denied": False,
                        "errno_name": "WRITABLE",
                    }
                },
                report | {"server_version": "0.1.0\nforged"},
                report | {"candidate_write_process_receipt": wrong_candidate_receipt},
                report | {"runtime_parent_revalidated": False},
            ]
            for hostile in hostile_reports:
                hostile_body = dict(hostile)
                hostile_body.pop("report_sha256")
                hostile["report_sha256"] = evidence.canonical_json_sha256(
                    hostile_body, domain="pmux.evidence.uds-permissions-report.v3"
                )
                with self.subTest(hostile=hostile):
                    self.assertFalse(
                        evidence.verify_uds_report(
                            hostile,
                            manifest,
                            publication_receipt(hostile),
                            manifest_path,
                        )["verified"]
                    )
            with self.assertRaisesRegex(evidence.EvidenceError, "schema"):
                evidence.verify_uds_report(
                    report | {"extra": True},
                    manifest,
                    outer_publication,
                    manifest_path,
                )


if __name__ == "__main__":
    unittest.main()
