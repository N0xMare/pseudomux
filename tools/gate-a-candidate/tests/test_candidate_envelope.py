from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

TOOLS = pathlib.Path(__file__).resolve().parents[2]
CANDIDATE_TOOLS = TOOLS / "gate-a-candidate"
LINUX_TOOLS = TOOLS / "linux-docker"
sys.path.insert(0, str(CANDIDATE_TOOLS))
sys.path.insert(0, str(LINUX_TOOLS))

import candidate_envelope  # noqa: E402
import evidence  # noqa: E402


class CandidateHarness:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name).resolve()
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        (self.workspace / "Cargo.toml").write_text(
            '[workspace]\nresolver = "2"\nmembers = []\n', encoding="utf-8"
        )
        (self.workspace / ".gitignore").write_text(
            ".context/\ntarget/\n", encoding="utf-8"
        )
        source_tools = self.workspace / "tools" / "linux-docker"
        common_tools = self.workspace / "tools" / "evidence_common"
        source_tools.mkdir(parents=True)
        common_tools.mkdir(parents=True)
        shutil.copy2(
            pathlib.Path(candidate_envelope.source_digest.__file__),
            source_tools / "source_digest.py",
        )
        shutil.copy2(
            pathlib.Path(candidate_envelope.bounded_process.__file__),
            common_tools / "bounded_process.py",
        )
        (self.workspace / ".context").mkdir()
        git_environment = {
            **os.environ,
            "GIT_AUTHOR_NAME": "pmux test",
            "GIT_AUTHOR_EMAIL": "pmux-test@example.invalid",
            "GIT_COMMITTER_NAME": "pmux test",
            "GIT_COMMITTER_EMAIL": "pmux-test@example.invalid",
        }
        subprocess.run(
            ["git", "init", "--quiet", str(self.workspace)],
            check=True,
            env=git_environment,
        )
        subprocess.run(
            ["git", "-C", str(self.workspace), "add", "."],
            check=True,
            env=git_environment,
        )
        subprocess.run(
            ["git", "-C", str(self.workspace), "commit", "--quiet", "-m", "fixture"],
            check=True,
            env=git_environment,
        )
        self.target = self.workspace / "target"
        self.release = self.target / "release"
        self.release.mkdir(parents=True)
        for index, name in enumerate(evidence.REQUIRED_RELEASE_BINARIES):
            path = self.release / name
            path.write_bytes(f"candidate-{index}".encode())
            path.chmod(0o755)
        self.evidence_directory = self.workspace / ".context" / "candidate"
        self.validation_parent = self.root / "validation-parent"
        self.validation_root = self.validation_parent / "gate-a-validation"
        self.validation_root.mkdir(parents=True)
        for name in candidate_envelope.VALIDATION_CHILD_NAMES:
            child = self.validation_root / name
            child.mkdir(mode=0o700)
            child.chmod(0o700)
        self.fake_cargo = pathlib.Path("/usr/bin/false")
        self.metadata_workspace = self.workspace
        self.metadata_target = self.target
        self.old_cwd: pathlib.Path | None = None
        self.environment: mock._patch_dict[str, str] | None = None
        self.candidate: dict[str, object] | None = None
        self.anchor: str | None = None
        self.command_ids: list[str] = []

    def __enter__(self) -> CandidateHarness:
        self.old_cwd = pathlib.Path.cwd()
        os.chdir(self.workspace)
        self.environment = mock.patch.dict(
            os.environ,
            {
                "PWD": str(self.workspace),
                "CARGO_TARGET_DIR": str(self.root / "ambient-target-is-ignored"),
            },
        )
        self.environment.start()
        return self

    def __exit__(self, *_args: object) -> None:
        assert self.old_cwd is not None
        assert self.environment is not None
        self.environment.stop()
        os.chdir(self.old_cwd)
        self.temporary.cleanup()

    def metadata(
        self,
        _workspace: pathlib.Path,
        _target: pathlib.Path,
        _runtime_identity: dict[str, object],
    ) -> dict[str, str]:
        return {
            "workspace_root": str(self.metadata_workspace),
            "target_directory": str(self.metadata_target),
        }

    def capture(self) -> dict[str, object]:
        result = candidate_envelope.capture_candidate(
            self.workspace,
            self.evidence_directory,
            self.build_receipt(),
            self.build_context(),
            metadata_loader=self.metadata,
        )
        self.candidate = dict(result)
        self.anchor = self.digest
        return self.candidate

    def build_receipt(
        self,
        _workspace: pathlib.Path | None = None,
        _target: pathlib.Path | None = None,
        _command: list[str] | tuple[str, ...] | None = None,
        _environment: dict[str, str] | None = None,
    ) -> dict[str, object]:
        process_ledger = [self.process_record("release-build")]
        return {
            "schema_version": candidate_envelope.SCHEMA_VERSION,
            "kind": "pmux_gate_a_release_build",
            "command": [
                str(self.fake_cargo),
                *candidate_envelope.RELEASE_BUILD_COMMAND,
            ],
            "workspace": str(self.workspace),
            "cargo_target_dir": str(self.target),
            "status": "PASS",
            "exit_code": 0,
            "stdout_size": 0,
            "stdout_sha256": hashlib.sha256(b"").hexdigest(),
            "stderr_size": 0,
            "stderr_sha256": hashlib.sha256(b"").hexdigest(),
            "process_ledger": process_ledger,
            "process_ledger_sha256": evidence.canonical_json_sha256(
                process_ledger,
                domain=candidate_envelope.HASH_DOMAINS["process_ledger"],
            ),
            "executables": [
                {
                    "name": name,
                    "path": str(self.release / name),
                    "package_id": f"test-package-{name}",
                    "fresh": False,
                }
                for name in evidence.REQUIRED_RELEASE_BINARIES
            ],
        }

    @staticmethod
    def process_record(command: str = "test-command") -> dict[str, object]:
        return {
            "pid": 4242,
            "ppid": 1,
            "pgid": 4242,
            "sid": 4242,
            "started": "Mon Jan  1 00:00:00 2024.000000000",
            "command": command,
            "ownership_marker_sha256": "a" * 64,
            "reaped": True,
        }

    def build_context(self) -> dict[str, object]:
        runtime = self.runtime_identity(self.workspace, self.validation_root)
        revision_before = candidate_envelope._workspace_revision_capture(self.workspace)
        revision_after = candidate_envelope._workspace_revision_capture(self.workspace)
        return {
            "schema_version": candidate_envelope.SCHEMA_VERSION,
            "validation_root_directory": str(self.validation_root),
            "source_guard": candidate_envelope.source_digest.workspace_source_guard(
                self.workspace
            ),
            "cargo_layout": candidate_envelope._cargo_layout(
                self.workspace,
                self.validation_root,
                self.metadata,
                runtime,
                require_empty_validation=True,
            ),
            "source_revision_identity": revision_before["identity"],
            "source_revision_captures": {
                "before_release_build": revision_before,
                "after_release_build": revision_after,
            },
            **runtime,
        }

    def runtime_identity(
        self, _workspace: pathlib.Path, _validation_root: pathlib.Path
    ) -> dict[str, object]:
        tool_paths = {
            name: str(self.fake_cargo)
            for name in (
                "cargo",
                "rustfmt",
                "python",
                "node",
                "bash",
                "shellcheck",
                "nightly_cargo",
                "nightly_rustc",
            )
        }
        # DERIVED from the envelope's own pinned-tool set rather than listed, so
        # a second tool the gate installs beside the workspace is bound in this
        # fake the day it lands. It was one hand-written `cargo_fuzz` line, and
        # adding `cargo_mutants` beside it made every phase expansion in this
        # file abort on `unknown placeholder: {cargo_mutants}`.
        for pinned in candidate_envelope.WORKSPACE_TOOLS:
            tool_paths[pinned] = f"/usr/bin/{pinned.replace('_', '-')}"
        return {
            "toolchain": {"test": "identity"},
            "tool_paths": tool_paths,
            "selected_build_environment": {
                "selected_values": {
                    "HOME": str(self.validation_root / "home"),
                    "TMPDIR": str(self.validation_root / "tmp"),
                    "PATH": "/usr/bin:/bin",
                }
            },
            "evidence_authorities": candidate_envelope._evidence_authorities(),
        }

    @property
    def digest(self) -> str:
        assert self.candidate is not None
        value = self.candidate["candidate_manifest_sha256"]
        assert isinstance(value, str)
        return value

    def checkpoint(self, label: str) -> dict[str, object]:
        assert self.anchor is not None
        result = dict(
            candidate_envelope.record_checkpoint(
                self.workspace,
                self.evidence_directory,
                label,
                self.digest,
                self.anchor,
                metadata_loader=self.metadata,
                runtime_identity_loader=self.runtime_identity,
            )
        )
        self.anchor = str(result["receipt_sha256"])
        return result

    def command(
        self,
        _workspace: pathlib.Path,
        argv: list[str] | tuple[str, ...],
        _environment: dict[str, str],
        _timeout_seconds: int,
        _maximum_output_bytes: int,
    ) -> candidate_envelope.CommandExecution:
        self.command_ids.append(str(argv[0]))
        if any(str(value).endswith("/dist-stage.mjs") for value in argv) and (
            "prepare" in argv
        ):
            stage = pathlib.Path(argv[3])
            self.assert_stage_path(stage)
            package = stage / "package.json"
            package.write_bytes(b'{"type":"module"}\n')
            package.chmod(0o600)
            output = b""
        elif "--outDir" in argv:
            stage = pathlib.Path(argv[argv.index("--outDir") + 1])
            self.assert_stage_path(stage)
            for index, name in enumerate(candidate_envelope.TYPESCRIPT_STAGE_FILES):
                if name == "package.json":
                    continue
                path = stage / name
                path.write_bytes(f"compiled-{index}-{name}".encode("utf-8"))
                path.chmod(0o600)
            output = b""
        elif any(str(value).endswith("/dist-stage.mjs") for value in argv) and (
            "verify" in argv
        ):
            stage = pathlib.Path(argv[3])
            self.assert_stage_path(stage)
            manifest = evidence.regular_tree_manifest(stage)
            digest = candidate_envelope._typescript_stage_verifier_digest(manifest)
            output = f"{digest}\n".encode("ascii")
        elif "--version" in argv and any(
            str(argv[0]).endswith("/" + pinned.replace("_", "-"))
            for pinned in candidate_envelope.WORKSPACE_TOOLS
        ):
            output = self.pinned_version_stdout(str(argv[0]))
        else:
            output = b""
        return candidate_envelope.CommandExecution(
            exit_code=0,
            stdout=output,
            process_ledger=(self.process_record(str(argv[0])),),
        )

    def pinned_version_stdout(self, binary: str) -> bytes:
        """The exact bytes the manifest's own `<tool>_version` cell asserts.

        READ from the manifest rather than restated here. A fake that answered
        a version string this file remembers would keep passing after the pin
        moved, which is the one thing those cells exist to catch -- and it was
        a literal `b"cargo-fuzz 0.13.2\\n"` until a second pinned tool arrived.
        """

        manifest, _ = candidate_envelope._load_phase_manifest()
        name = pathlib.PurePath(binary).name.replace("-", "_")
        for cells in manifest["phases"].values():
            for entry in cells:
                if entry["id"] == f"{name}_version":
                    expected = entry["stdout_equals"]
                    assert isinstance(expected, str), name
                    return expected.encode("utf-8")
        raise AssertionError(f"the manifest declares no {name}_version cell")

    def assert_stage_path(self, stage: pathlib.Path) -> None:
        if stage != self.validation_root / "typescript-dist":
            raise AssertionError(f"unexpected TypeScript stage: {stage}")

    def phase(self, phase: str) -> dict[str, object]:
        assert self.anchor is not None
        result = dict(
            candidate_envelope.run_phase(
                self.workspace,
                self.evidence_directory,
                phase,
                self.digest,
                self.anchor,
                metadata_loader=self.metadata,
                runtime_identity_loader=self.runtime_identity,
                command_runner=self.command,
            )
        )
        self.anchor = str(result["phase_report_sha256"])
        return result

    def all_checkpoints(self) -> None:
        for phase in candidate_envelope.PHASES:
            self.checkpoint(candidate_envelope.PHASE_BEFORE_LABEL[phase])
            self.phase(phase)
            self.checkpoint(candidate_envelope.PHASE_AFTER_LABEL[phase])

    def audit(self) -> dict[str, object]:
        assert self.anchor is not None
        result = dict(
            candidate_envelope.audit_candidate(
                self.workspace,
                self.evidence_directory,
                self.digest,
                self.anchor,
                metadata_loader=self.metadata,
                runtime_identity_loader=self.runtime_identity,
            )
        )
        self.anchor = str(result["final_audit_sha256"])
        return result

    def verify(self) -> dict[str, object]:
        assert self.anchor is not None
        return dict(
            candidate_envelope.verify_final_candidate(
                self.workspace,
                self.evidence_directory,
                self.digest,
                self.anchor,
                metadata_loader=self.metadata,
                runtime_identity_loader=self.runtime_identity,
            )
        )


def rewrite_private_json(path: pathlib.Path, payload: object) -> None:
    path.write_text(
        json.dumps(payload, separators=(",", ":"), sort_keys=True) + "\n",
        encoding="utf-8",
    )
    path.chmod(0o600)


class CandidateEnvelopeTests(unittest.TestCase):
    def test_happy_path_captures_all_eight_and_exact_ordered_chain(self) -> None:
        with CandidateHarness() as harness:
            candidate = harness.capture()
            binaries = candidate["release_binary_manifest"]
            self.assertIsInstance(binaries, dict)
            assert isinstance(binaries, dict)
            self.assertEqual(
                binaries["required_names"], list(evidence.REQUIRED_RELEASE_BINARIES)
            )
            for identity in binaries["binaries"].values():
                self.assertEqual(identity["nlink"], 1)
                self.assertIsInstance(identity["mtime_ns"], int)
                self.assertIsInstance(identity["ctime_ns"], int)

            harness.all_checkpoints()
            audit = harness.audit()
            self.assertEqual(audit["verdict"], "verified")
            self.assertEqual(harness.verify(), audit)
            expected_names = {
                candidate_envelope.CANDIDATE_FILE,
                candidate_envelope.FINAL_AUDIT_FILE,
                *(
                    candidate_envelope._checkpoint_filename(index, label)
                    for index, label in enumerate(
                        candidate_envelope.CHECKPOINTS, start=1
                    )
                ),
            }
            for phase in candidate_envelope.PHASES:
                expected_names.add(candidate_envelope._phase_report_filename(phase))
                commands, _digest = candidate_envelope._expanded_phase_commands(
                    candidate, phase
                )
                expected_names.update(
                    candidate_envelope._phase_log_filename(
                        phase, ordinal, command["id"], stream
                    )
                    for ordinal, command in enumerate(commands, start=1)
                    for stream in ("stdout", "stderr")
                )
            self.assertEqual(
                {path.name for path in harness.evidence_directory.iterdir()},
                expected_names,
            )
            self.assertEqual(
                stat.S_IMODE(harness.evidence_directory.stat().st_mode), 0o700
            )
            for path in harness.evidence_directory.iterdir():
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                self.assertEqual(path.stat().st_nlink, 1)

    def test_typescript_stage_is_frozen_at_verifier_and_bound_after_gate_a(
        self,
    ) -> None:
        with CandidateHarness() as harness:
            candidate = harness.capture()
            harness.checkpoint("gate_a_before")
            report = harness.phase("gate_a")
            stage = report["frozen_outputs"]["typescript_stage"]
            self.assertEqual(
                stage["tree_manifest"]["file_count"],
                len(candidate_envelope.TYPESCRIPT_STAGE_FILES),
            )
            self.assertEqual(stage["tree_manifest"]["directory_count"], 0)
            self.assertEqual(
                [entry["path"] for entry in stage["tree_manifest"]["files"]],
                list(candidate_envelope.TYPESCRIPT_STAGE_FILES),
            )
            commands, _digest = candidate_envelope._expanded_phase_commands(
                candidate, "gate_a"
            )
            verifier_index = next(
                index
                for index, command in enumerate(commands)
                if command["id"] == "typescript_stage_verify"
            )
            base_digest = evidence.canonical_json_sha256(
                candidate_envelope._expected_observation(candidate),
                domain=candidate_envelope.HASH_DOMAINS["observation"],
            )
            staged_digest = evidence.canonical_json_sha256(
                candidate_envelope._expected_observation(candidate, stage),
                domain=candidate_envelope.HASH_DOMAINS["observation"],
            )
            self.assertTrue(
                all(
                    record["candidate_observation_sha256"] == base_digest
                    for record in report["commands"][:verifier_index]
                )
            )
            self.assertTrue(
                all(
                    record["candidate_observation_sha256"] == staged_digest
                    for record in report["commands"][verifier_index:]
                )
            )
            harness.checkpoint("gate_a_after")
            (harness.validation_root / "typescript-dist" / "index.js").write_bytes(
                b"mutated"
            )
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "tree identity changed"
            ):
                harness.checkpoint("gate_b_before")

    def test_typescript_stage_digest_and_exact_private_tree_fail_closed(self) -> None:
        for mutation in ("digest", "mode", "extra"):
            with self.subTest(mutation=mutation), CandidateHarness() as harness:
                harness.capture()
                harness.checkpoint("gate_a_before")

                def runner(
                    workspace: pathlib.Path,
                    argv: list[str] | tuple[str, ...],
                    environment: dict[str, str],
                    timeout_seconds: int,
                    maximum_output_bytes: int,
                ) -> candidate_envelope.CommandExecution:
                    execution = harness.command(
                        workspace,
                        argv,
                        environment,
                        timeout_seconds,
                        maximum_output_bytes,
                    )
                    if not (
                        "verify" in argv
                        and any(
                            str(value).endswith("/dist-stage.mjs") for value in argv
                        )
                    ):
                        return execution
                    stage = harness.validation_root / "typescript-dist"
                    if mutation == "digest":
                        return candidate_envelope.CommandExecution(
                            exit_code=0, stdout=b"0" * 64 + b"\n"
                        )
                    if mutation == "mode":
                        (stage / "index.js").chmod(0o644)
                    else:
                        extra = stage / "unexpected.js"
                        extra.write_bytes(b"unexpected")
                        extra.chmod(0o600)
                    return execution

                assert harness.anchor is not None
                with self.assertRaises(candidate_envelope.CandidateEnvelopeError):
                    candidate_envelope.run_phase(
                        harness.workspace,
                        harness.evidence_directory,
                        "gate_a",
                        harness.digest,
                        harness.anchor,
                        metadata_loader=harness.metadata,
                        runtime_identity_loader=harness.runtime_identity,
                        command_runner=runner,
                    )

    def test_external_digest_rejects_replaced_self_consistent_baseline(self) -> None:
        with CandidateHarness() as harness:
            harness.capture()
            original_digest = harness.digest
            original = harness.root / "candidate-original"
            harness.evidence_directory.rename(original)
            replacement = candidate_envelope.capture_candidate(
                harness.workspace,
                harness.evidence_directory,
                harness.build_receipt(),
                harness.build_context(),
                metadata_loader=harness.metadata,
            )
            self.assertNotEqual(
                replacement["candidate_manifest_sha256"], original_digest
            )
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "externally carried"
            ):
                candidate_envelope.record_checkpoint(
                    harness.workspace,
                    harness.evidence_directory,
                    "gate_a_before",
                    original_digest,
                    original_digest,
                    metadata_loader=harness.metadata,
                )

    def test_wrong_or_malformed_external_digest_fails(self) -> None:
        with CandidateHarness() as harness:
            harness.capture()
            for value, pattern in (("0" * 64, "externally carried"), ("bad", "64")):
                with self.subTest(value=value):
                    with self.assertRaisesRegex(
                        candidate_envelope.CandidateEnvelopeError, pattern
                    ):
                        candidate_envelope.record_checkpoint(
                            harness.workspace,
                            harness.evidence_directory,
                            "gate_a_before",
                            value,
                            harness.anchor,
                            metadata_loader=harness.metadata,
                        )

    def test_capture_refuses_existing_or_nonempty_evidence(self) -> None:
        with CandidateHarness() as harness:
            harness.capture()
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "non-empty"
            ):
                candidate_envelope.capture_candidate(
                    harness.workspace,
                    harness.evidence_directory,
                    harness.build_receipt(),
                    harness.build_context(),
                    metadata_loader=harness.metadata,
                )

    def test_ambient_target_is_ignored_and_metadata_redirection_fails_closed(
        self,
    ) -> None:
        environment_cases = (None, "relative", "/tmp/redirected-target")
        for value in environment_cases:
            with self.subTest(environment=value), CandidateHarness() as harness:
                if value is None:
                    os.environ.pop("CARGO_TARGET_DIR", None)
                else:
                    os.environ["CARGO_TARGET_DIR"] = value
                candidate = harness.capture()
                self.assertEqual(
                    candidate["cargo_layout"]["target_directory"],
                    str(harness.target),
                )

        with CandidateHarness() as harness:
            harness.metadata_target = harness.root / "ambient-config-target"
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "metadata target_directory"
            ):
                harness.capture()
        with CandidateHarness() as harness:
            harness.metadata_workspace = harness.root / "other-workspace"
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "metadata workspace_root"
            ):
                harness.capture()

    def test_workspace_pwd_and_target_aliases_fail_closed(self) -> None:
        with CandidateHarness() as harness:
            os.environ["PWD"] = str(harness.workspace / ".") + "/"
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "PWD"
            ):
                harness.capture()
        with CandidateHarness() as harness:
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "absolute"
            ):
                candidate_envelope.capture_candidate(
                    pathlib.Path("."),
                    harness.evidence_directory,
                    harness.build_receipt(),
                    harness.build_context(),
                    metadata_loader=harness.metadata,
                )
        with CandidateHarness() as harness:
            original_target = harness.workspace / ".context" / "target-original"
            harness.target.rename(original_target)
            harness.target.symlink_to(original_target, target_is_directory=True)
            with self.assertRaisesRegex(
                (
                    candidate_envelope.CandidateEnvelopeError,
                    candidate_envelope.source_digest.SourceIdentityError,
                ),
                "symlink|real directory",
            ):
                harness.capture()

    def test_source_content_mode_and_path_changes_fail(self) -> None:
        mutations = ("content", "mode", "path")
        for mutation in mutations:
            with self.subTest(mutation=mutation), CandidateHarness() as harness:
                harness.capture()
                cargo = harness.workspace / "Cargo.toml"
                if mutation == "content":
                    cargo.write_text(
                        "[workspace]\nmembers=[]\n# changed\n", encoding="utf-8"
                    )
                elif mutation == "mode":
                    cargo.chmod(0o600)
                else:
                    (harness.workspace / "README.md").write_text(
                        "new tracked path\n", encoding="utf-8"
                    )
                with self.assertRaisesRegex(
                    candidate_envelope.CandidateEnvelopeError, "source identity"
                ):
                    harness.checkpoint("gate_a_before")

    def test_every_named_binary_content_mutation_fails(self) -> None:
        for name in evidence.REQUIRED_RELEASE_BINARIES:
            with self.subTest(name=name), CandidateHarness() as harness:
                harness.capture()
                (harness.release / name).write_bytes(b"mutated")
                with self.assertRaisesRegex(
                    candidate_envelope.CandidateEnvelopeError, "changed"
                ):
                    harness.checkpoint("gate_a_before")

    def test_binary_missing_mode_alias_and_replacement_fail(self) -> None:
        mutations = ("missing", "mode", "symlink", "hardlink", "replacement")
        for mutation in mutations:
            with self.subTest(mutation=mutation), CandidateHarness() as harness:
                harness.capture()
                target = harness.release / "pmux"
                original = target.read_bytes()
                if mutation == "missing":
                    target.unlink()
                elif mutation == "mode":
                    target.chmod(0o644)
                elif mutation == "symlink":
                    moved = harness.workspace / ".context" / "pmux-original"
                    target.rename(moved)
                    target.symlink_to(moved)
                elif mutation == "hardlink":
                    os.link(target, harness.workspace / ".context" / "pmux-alias")
                else:
                    target.unlink()
                    target.write_bytes(original)
                    target.chmod(0o755)
                with self.assertRaises(candidate_envelope.CandidateEnvelopeError):
                    harness.checkpoint("gate_a_before")

    def test_same_content_rewrite_is_detected_by_full_binary_stat(self) -> None:
        with CandidateHarness() as harness:
            harness.capture()
            target = harness.release / "pmux"
            metadata = target.stat()
            os.utime(
                target,
                ns=(metadata.st_atime_ns, metadata.st_mtime_ns + 1_000_000_000),
            )
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "changed"
            ):
                harness.checkpoint("gate_a_before")

    def test_target_release_directory_identity_and_mode_changes_fail(self) -> None:
        for mutation in (
            "target_replace",
            "target_mode",
            "release_replace",
            "release_mode",
        ):
            with self.subTest(mutation=mutation), CandidateHarness() as harness:
                harness.capture()
                if mutation == "target_replace":
                    saved = harness.workspace / ".context" / "target-saved"
                    harness.target.rename(saved)
                    shutil.copytree(saved, harness.target)
                elif mutation == "target_mode":
                    harness.target.chmod(0o777)
                elif mutation == "release_replace":
                    saved = harness.workspace / ".context" / "release-saved"
                    harness.release.rename(saved)
                    shutil.copytree(saved, harness.release)
                else:
                    harness.release.chmod(0o700)
                with self.assertRaises(candidate_envelope.CandidateEnvelopeError):
                    harness.checkpoint("gate_a_before")

    def test_evidence_directory_file_mode_alias_and_content_changes_fail(self) -> None:
        mutations = ("directory_mode", "file_mode", "hardlink", "content")
        for mutation in mutations:
            with self.subTest(mutation=mutation), CandidateHarness() as harness:
                harness.capture()
                candidate_path = (
                    harness.evidence_directory / candidate_envelope.CANDIDATE_FILE
                )
                if mutation == "directory_mode":
                    harness.evidence_directory.chmod(0o755)
                elif mutation == "file_mode":
                    candidate_path.chmod(0o644)
                elif mutation == "hardlink":
                    os.link(
                        candidate_path,
                        harness.evidence_directory / "candidate-alias.json",
                    )
                else:
                    payload = json.loads(candidate_path.read_text(encoding="utf-8"))
                    payload["kind"] = "mutated"
                    rewrite_private_json(candidate_path, payload)
                with self.assertRaises(candidate_envelope.CandidateEnvelopeError):
                    harness.checkpoint("gate_a_before")

    def test_unknown_duplicate_out_of_order_and_missing_checkpoints_fail(self) -> None:
        with CandidateHarness() as harness:
            harness.capture()
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "out of order"
            ):
                harness.checkpoint("gate_a_after")
            harness.checkpoint("gate_a_before")
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "out of order"
            ):
                harness.checkpoint("gate_a_before")

        with CandidateHarness() as harness:
            harness.capture()
            (harness.evidence_directory / "unknown.json").write_text(
                "{}\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "unknown artifacts"
            ):
                harness.checkpoint("gate_a_before")

        with CandidateHarness() as harness:
            harness.capture()
            harness.checkpoint("gate_a_before")
            harness.phase("gate_a")
            harness.checkpoint("gate_a_after")
            first = (
                harness.evidence_directory
                / candidate_envelope._checkpoint_filename(1, "gate_a_before")
            )
            first.unlink()
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "gap"
            ):
                harness.checkpoint("gate_b_before")

    def test_receipt_mutation_mode_alias_and_incomplete_audit_fail(self) -> None:
        for mutation in ("content", "mode", "hardlink"):
            with self.subTest(mutation=mutation), CandidateHarness() as harness:
                harness.capture()
                harness.checkpoint("gate_a_before")
                receipt = (
                    harness.evidence_directory
                    / candidate_envelope._checkpoint_filename(1, "gate_a_before")
                )
                if mutation == "content":
                    payload = json.loads(receipt.read_text(encoding="utf-8"))
                    payload["label"] = "gate_a_after"
                    rewrite_private_json(receipt, payload)
                elif mutation == "mode":
                    receipt.chmod(0o644)
                else:
                    os.link(
                        receipt,
                        harness.workspace / ".context" / "receipt-alias.json",
                    )
                with self.assertRaises(candidate_envelope.CandidateEnvelopeError):
                    harness.phase("gate_a")

        with CandidateHarness() as harness:
            harness.capture()
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "incomplete"
            ):
                harness.audit()

    def test_final_audit_is_nonoverwritable_and_mutation_is_detected(self) -> None:
        with CandidateHarness() as harness:
            harness.capture()
            harness.all_checkpoints()
            harness.audit()
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "replace"
            ):
                harness.audit()
            final = harness.evidence_directory / candidate_envelope.FINAL_AUDIT_FILE
            payload = json.loads(final.read_text(encoding="utf-8"))
            payload["verdict"] = "changed"
            rewrite_private_json(final, payload)
            with self.assertRaises(candidate_envelope.CandidateEnvelopeError):
                harness.verify()

    def test_cli_build_capture_emits_only_externally_carried_digest(self) -> None:
        with CandidateHarness() as harness:
            shutil.rmtree(harness.target)
            shutil.rmtree(harness.validation_parent)

            def fake_build(
                workspace: pathlib.Path,
                target: pathlib.Path,
                command: list[str] | tuple[str, ...],
                environment: dict[str, str],
            ) -> dict[str, object]:
                self.assertEqual(workspace, harness.workspace)
                self.assertEqual(target, harness.target)
                self.assertEqual(environment["CARGO_TARGET_DIR"], str(harness.target))
                harness.release.mkdir(parents=True)
                for index, name in enumerate(evidence.REQUIRED_RELEASE_BINARIES):
                    path = harness.release / name
                    path.write_bytes(f"candidate-{index}".encode())
                    path.chmod(0o755)
                return harness.build_receipt(workspace, target, command, environment)

            patches = (
                mock.patch.object(
                    candidate_envelope, "_run_cargo_metadata", harness.metadata
                ),
                mock.patch.object(candidate_envelope, "_run_release_build", fake_build),
                mock.patch.object(
                    candidate_envelope,
                    "_runtime_identity",
                    harness.runtime_identity,
                ),
            )
            for patch in patches:
                patch.start()
            self.addCleanup(lambda: [patch.stop() for patch in reversed(patches)])
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                status = candidate_envelope.main(
                    [
                        "build-capture",
                        "--workspace",
                        str(harness.workspace),
                        "--evidence-dir",
                        str(harness.evidence_directory),
                        "--validation-root",
                        str(harness.validation_root),
                    ]
                )
            self.assertEqual(status, 0)
            self.assertRegex(output.getvalue().strip(), r"^[0-9a-f]{64}$")


class PrivateCargoClosureTests(unittest.TestCase):
    @staticmethod
    def archive_bytes(
        package_directory: str,
        files: dict[str, bytes],
        *,
        unsafe_name: str | None = None,
    ) -> bytes:
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            for relative, payload in sorted(files.items()):
                member = tarfile.TarInfo(
                    unsafe_name or f"{package_directory}/{relative}"
                )
                member.size = len(payload)
                member.mode = 0o644
                member.mtime = 0
                archive.addfile(member, io.BytesIO(payload))
                unsafe_name = None
        return output.getvalue()

    @staticmethod
    def make_writable(root: pathlib.Path) -> None:
        if not root.exists():
            return
        for current, directories, files in os.walk(root):
            pathlib.Path(current).chmod(0o700)
            for name in directories:
                (pathlib.Path(current) / name).chmod(0o700)
            for name in files:
                (pathlib.Path(current) / name).chmod(0o600)

    def fixture(
        self, root: pathlib.Path
    ) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path, bytes]:
        workspace = root / "workspace"
        cargo_home = root / "ambient-cargo"
        private_cargo_home = root / "private-cargo"
        workspace.mkdir()
        cargo_home.mkdir()
        private_cargo_home.mkdir(mode=0o700)
        private_cargo_home.chmod(0o700)
        package_directory = "tiny-crate-1.0.0"
        files = {
            "Cargo.toml": b'[package]\nname="tiny-crate"\nversion="1.0.0"\n',
            "src/lib.rs": b"pub fn value() -> u8 { 7 }\n",
        }
        archive = self.archive_bytes(package_directory, files)
        checksum = hashlib.sha256(archive).hexdigest()
        lock = (
            "version = 4\n\n[[package]]\n"
            'name = "tiny-crate"\n'
            'version = "1.0.0"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
            f'checksum = "{checksum}"\n'
        )
        for lock_relative, manifest_relative in candidate_envelope.CARGO_LOCK_MANIFESTS:
            lock_path = workspace / lock_relative
            manifest_path = workspace / manifest_relative
            lock_path.parent.mkdir(parents=True, exist_ok=True)
            manifest_path.parent.mkdir(parents=True, exist_ok=True)
            lock_path.write_text(lock, encoding="utf-8")
            manifest_path.write_text(
                '[package]\nname="fixture"\nversion="0.0.0"\n',
                encoding="utf-8",
            )
        cache = cargo_home / "registry" / "cache" / "index.test"
        source = cargo_home / "registry" / "src" / "index.test" / package_directory
        cache.mkdir(parents=True)
        (source / "src").mkdir(parents=True)
        (cache / f"{package_directory}.crate").write_bytes(archive)
        for relative, payload in files.items():
            path = source / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(payload)
        (source / ".cargo-ok").write_text("v1\n", encoding="utf-8")
        return workspace, cargo_home, private_cargo_home, archive

    def test_private_cargo_union_is_archive_bound_read_only_and_repeatable(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            workspace, cargo_home, private_cargo_home, archive = self.fixture(root)
            try:
                closure = candidate_envelope._stage_private_cargo_home(
                    workspace, cargo_home, private_cargo_home
                )
                package = private_cargo_home / "vendor" / "tiny-crate-1.0.0"
                self.assertFalse((package / ".cargo-ok").exists())
                checksum = evidence.strict_json_loads(
                    (package / ".cargo-checksum.json").read_bytes(),
                    description="test checksum",
                )
                self.assertEqual(
                    checksum["package"], hashlib.sha256(archive).hexdigest()
                )
                self.assertEqual(set(checksum["files"]), {"Cargo.toml", "src/lib.rs"})
                self.assertEqual(
                    stat.S_IMODE((private_cargo_home / "config.toml").stat().st_mode),
                    0o400,
                )
                self.assertEqual(
                    closure,
                    candidate_envelope._stage_private_cargo_home(
                        workspace, cargo_home, private_cargo_home
                    ),
                )
                (package / "src/lib.rs").chmod(0o600)
                (package / "src/lib.rs").write_text("mutated\n", encoding="utf-8")
                with self.assertRaisesRegex(
                    candidate_envelope.CandidateEnvelopeError,
                    "checksum differs",
                ):
                    candidate_envelope._stage_private_cargo_home(
                        workspace, cargo_home, private_cargo_home
                    )
            finally:
                self.make_writable(private_cargo_home)

    def test_archive_and_ambient_source_membership_fail_closed(self) -> None:
        package = "tiny-crate-1.0.0"
        unsafe = self.archive_bytes(
            package,
            {"src/lib.rs": b"safe"},
            unsafe_name=f"{package}/../escape",
        )
        with self.assertRaisesRegex(
            candidate_envelope.CandidateEnvelopeError, "unsafe"
        ):
            candidate_envelope._cargo_archive_manifest(
                unsafe, package_directory=package
            )

        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            _workspace, cargo_home, _private, archive = self.fixture(root)
            source = cargo_home / "registry" / "src" / "index.test" / package
            manifest = candidate_envelope._cargo_archive_manifest(
                archive, package_directory=package
            )
            (source / "extra").write_text("extra", encoding="utf-8")
            with self.assertRaisesRegex(
                candidate_envelope.CandidateEnvelopeError, "differs"
            ):
                candidate_envelope._cargo_source_matches_archive(source, manifest)


if __name__ == "__main__":
    unittest.main()
