from __future__ import annotations

import copy
import importlib.util
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
import uuid
from unittest import mock

TOOLS = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))

import source_digest  # noqa: E402


class SourceDigestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.workspace = pathlib.Path(self.temporary.name).resolve()
        (self.workspace / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        tools = self.workspace / "tools"
        tools.mkdir()
        self.script = tools / "check.sh"
        self.script.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.script.chmod(0o755)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def manifest(self) -> dict[str, object]:
        return source_digest.workspace_source_manifest(self.workspace)

    def test_preloaded_bounded_process_module_cannot_substitute_for_sibling(
        self,
    ) -> None:
        sentinel = object()
        previous = sys.modules.get("bounded_process", sentinel)
        hostile = type(sys)("bounded_process")
        hostile.run = lambda *args, **kwargs: None
        sys.modules["bounded_process"] = hostile
        module_name = f"_pmux_source_digest_substitution_test_{uuid.uuid4().hex}"
        spec = importlib.util.spec_from_file_location(
            module_name, source_digest.__file__
        )
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        sys.modules[module_name] = module
        try:
            spec.loader.exec_module(module)
            self.assertIsNot(module.bounded_process, hostile)
            self.assertEqual(
                pathlib.Path(module.bounded_process.__file__).resolve(strict=True),
                TOOLS.parent / "evidence_common" / "bounded_process.py",
            )
            self.assertEqual(
                module._revalidate_bounded_process_authority(),
                module._BOUNDED_PROCESS_IDENTITY,
            )
        finally:
            sys.modules.pop(module_name, None)
            if previous is sentinel:
                sys.modules.pop("bounded_process", None)
            else:
                sys.modules["bounded_process"] = previous

    def initialize_revision_repository(self) -> None:
        implementation = self.workspace / "tools" / "linux-docker" / "source_digest.py"
        implementation.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source_digest.__file__, implementation)
        subprocess.run(
            ["git", "init", "-q", str(self.workspace)], check=True, timeout=10
        )
        subprocess.run(
            ["git", "-C", str(self.workspace), "add", "-A"], check=True, timeout=10
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(self.workspace),
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

    def git_control_path(self, argument: str) -> pathlib.Path:
        raw = (
            subprocess.run(
                ["git", "-C", str(self.workspace), "rev-parse", argument],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=10,
            )
            .stdout.decode("utf-8")
            .strip()
        )
        path = pathlib.Path(raw)
        if not path.is_absolute():
            path = self.workspace / path
        return path.resolve(strict=True)

    def enable_worktree_and_sparse_config(self) -> None:
        subprocess.run(
            [
                "git",
                "-C",
                str(self.workspace),
                "config",
                "extensions.worktreeConfig",
                "true",
            ],
            check=True,
            timeout=10,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(self.workspace),
                "config",
                "--worktree",
                "core.sparseCheckout",
                "true",
            ],
            check=True,
            timeout=10,
        )
        git_dir = self.git_control_path("--git-dir")
        sparse_path = git_dir / "info" / "sparse-checkout"
        sparse_path.parent.mkdir(parents=True, exist_ok=True)
        sparse_path.write_text("/*\n", encoding="utf-8")

    @staticmethod
    def resign_execution_receipt(receipt: dict[str, object]) -> None:
        body = dict(receipt)
        body.pop("receipt_sha256")
        receipt["receipt_sha256"] = (
            source_digest.bounded_process._canonical_json_sha256(
                body,
                domain=source_digest.bounded_process.EXECUTION_RECEIPT_DOMAIN,
            )
        )

    @staticmethod
    def resign_revision_capture(capture: dict[str, object]) -> None:
        body = dict(capture)
        body.pop("capture_sha256")
        capture["capture_sha256"] = source_digest._revision_capture_sha256(body)

    def test_digest_is_deterministic_and_records_path_mode_size_and_content(
        self,
    ) -> None:
        first = self.manifest()
        second = self.manifest()
        self.assertEqual(first, second)
        self.assertEqual(first["algorithm"], source_digest.SOURCE_ALGORITHM)
        files = {entry["path"]: entry for entry in first["files"]}
        self.assertEqual(files["tools/check.sh"]["mode"], "0755")
        self.assertEqual(files["tools/check.sh"]["size"], self.script.stat().st_size)
        self.assertRegex(files["tools/check.sh"]["sha256"], r"^[0-9a-f]{64}$")

    def test_content_mode_and_path_drift_each_change_the_digest(self) -> None:
        original = self.manifest()["workspace_source_sha256"]
        self.script.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
        content = self.manifest()["workspace_source_sha256"]
        self.assertNotEqual(content, original)
        self.script.chmod(0o700)
        mode = self.manifest()["workspace_source_sha256"]
        self.assertNotEqual(mode, content)
        renamed = self.script.with_name("renamed.sh")
        self.script.rename(renamed)
        path = self.manifest()["workspace_source_sha256"]
        self.assertNotEqual(path, mode)

    def test_directory_mode_drift_changes_the_digest(self) -> None:
        """The drifted mode is DERIVED from the current one, not hand-written.

        This used to `chmod(0o700)` a directory `setUp` had just created. Under
        an ordinary 0022 umask that is a real change and the test passed. Under
        `umask 077` -- which `docs/testing.md:124` requires of EVERY gate
        command and which `tools/gate-a/run_gate.py:914` sets for every cell --
        `mkdir` already yields 0700, so the chmod was a no-op, the digest did
        not move, and this test FAILED inside the only environment the docs
        require. Its outcome was decided by the ambient umask rather than by
        the digest it names, in both directions: a `source_digest` that stopped
        hashing directory modes altogether would still have passed it under
        0077's opposite, because it never asserted the mode it produced.

        Flipping the group bits of whatever mode is actually there is a real
        change under every umask, and the recorded mode is now asserted to be
        the derived value rather than a constant that happened to match.
        """

        tools = self.workspace / "tools"
        before = self.manifest()["workspace_source_sha256"]
        current = stat.S_IMODE(tools.stat().st_mode)
        drifted = current ^ 0o070
        self.assertNotEqual(drifted, current, "the drift must change the mode")
        tools.chmod(drifted)
        self.assertEqual(stat.S_IMODE(tools.stat().st_mode), drifted)
        after = self.manifest()["workspace_source_sha256"]
        self.assertNotEqual(before, after)
        directories = {entry["path"]: entry for entry in self.manifest()["directories"]}
        self.assertEqual(directories["tools"]["mode"], f"{drifted:04o}")

    def test_excluded_runtime_content_does_not_change_declared_source(self) -> None:
        before = self.manifest()
        context = self.workspace / ".context"
        context.mkdir()
        (context / "volatile.log").write_text("volatile", encoding="utf-8")
        after = self.manifest()
        self.assertEqual(before, after)

    def test_git_worktree_pointer_file_is_excluded_but_a_git_symlink_is_rejected(
        self,
    ) -> None:
        git = self.workspace / ".git"
        git.write_text("gitdir: /private/worktrees/test\n", encoding="utf-8")
        before = self.manifest()
        git.write_text("gitdir: /private/worktrees/other\n", encoding="utf-8")
        self.assertEqual(before, self.manifest())
        git.unlink()
        git.symlink_to(self.script)
        with self.assertRaisesRegex(source_digest.SourceIdentityError, "symlink"):
            self.manifest()

    def test_unknown_top_level_input_fails_closed(self) -> None:
        (self.workspace / "unreviewed.txt").write_text("x", encoding="utf-8")
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "unknown top-level"
        ):
            self.manifest()

    def test_source_symlink_fails_closed(self) -> None:
        (self.workspace / "tools" / "alias").symlink_to(self.script)
        with self.assertRaisesRegex(source_digest.SourceIdentityError, "symlink"):
            self.manifest()

    def test_source_hard_link_fails_closed(self) -> None:
        alias = self.workspace / "tools" / "hard-link"
        os.link(self.script, alias)
        with self.assertRaisesRegex(source_digest.SourceIdentityError, "hard-link"):
            self.manifest()

    def test_top_level_symlink_fails_even_when_named_like_an_excluded_directory(
        self,
    ) -> None:
        outside = pathlib.Path(self.temporary.name) / "outside"
        outside.mkdir()
        (self.workspace / ".context").symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(source_digest.SourceIdentityError, "symlink"):
            self.manifest()

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO creation requires POSIX")
    def test_special_source_node_fails_closed(self) -> None:
        fifo = self.workspace / "tools" / "unexpected.fifo"
        os.mkfifo(fifo)
        self.assertTrue(stat.S_ISFIFO(fifo.lstat().st_mode))
        with self.assertRaisesRegex(source_digest.SourceIdentityError, "special file"):
            self.manifest()

    def test_stale_metadata_and_path_replacement_are_rejected(self) -> None:
        before = self.script.lstat()
        self.script.write_text("changed", encoding="utf-8")
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "changed before read"
        ):
            source_digest._read_stable_file(self.script, before)

        current = self.script.lstat()
        old = self.script.with_name("old")
        self.script.rename(old)
        self.script.write_text("replacement", encoding="utf-8")
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "changed before read"
        ):
            source_digest._read_stable_file(self.script, current)

    def test_injected_nested_addition_after_snapshot_fails_membership_check(
        self,
    ) -> None:
        original = source_digest._read_stable_file
        injected = False

        def add_after_read(
            path: pathlib.Path, before: os.stat_result
        ) -> tuple[bytes, os.stat_result]:
            nonlocal injected
            result = original(path, before)
            if not injected:
                injected = True
                (self.workspace / "tools" / "added.rs").write_text(
                    "// raced addition\n", encoding="utf-8"
                )
            return result

        with mock.patch.object(source_digest, "_read_stable_file", add_after_read):
            with self.assertRaisesRegex(
                source_digest.SourceIdentityError, "membership changed"
            ):
                self.manifest()

    def test_injected_nested_removal_after_snapshot_fails_membership_check(
        self,
    ) -> None:
        original = source_digest._read_stable_file
        injected = False

        def remove_after_read(
            path: pathlib.Path, before: os.stat_result
        ) -> tuple[bytes, os.stat_result]:
            nonlocal injected
            result = original(path, before)
            if not injected:
                injected = True
                path.unlink()
            return result

        with mock.patch.object(source_digest, "_read_stable_file", remove_after_read):
            with self.assertRaisesRegex(
                source_digest.SourceIdentityError, "membership changed"
            ):
                self.manifest()

    def test_injected_directory_replacement_fails_metadata_check(self) -> None:
        original = source_digest._read_stable_file
        replaced = False
        old_tools = self.workspace.with_name(f"{self.workspace.name}-old-tools")
        self.addCleanup(shutil.rmtree, old_tools, True)

        def replace_after_read(
            path: pathlib.Path, before: os.stat_result
        ) -> tuple[bytes, os.stat_result]:
            nonlocal replaced
            result = original(path, before)
            if path == self.script and not replaced:
                replaced = True
                tools = self.workspace / "tools"
                tools.rename(old_tools)
                tools.mkdir()
                replacement = tools / "check.sh"
                replacement.write_bytes(result[0])
                replacement.chmod(0o755)
            return result

        with mock.patch.object(source_digest, "_read_stable_file", replace_after_read):
            with self.assertRaisesRegex(
                source_digest.SourceIdentityError, "metadata changed"
            ):
                self.manifest()

    def test_only_canonical_fixture_jsonl_paths_are_included(self) -> None:
        fixtures = self.workspace / "crates" / "claude" / "tests" / "fixtures"
        fixtures.mkdir(parents=True)
        canonical = fixtures / "valid.jsonl"
        canonical.write_text("{}\n", encoding="utf-8")
        generated = self.workspace / "tools" / "generated.jsonl"
        generated.write_text("secret runtime row\n", encoding="utf-8")
        files = {entry["path"] for entry in self.manifest()["files"]}
        self.assertIn("crates/claude/tests/fixtures/valid.jsonl", files)
        self.assertNotIn("tools/generated.jsonl", files)

    def test_tracked_vendor_source_is_included_in_canonical_identity(self) -> None:
        vendored = (
            self.workspace / "vendor" / "rmux-client" / "src" / "attach.rs",
            self.workspace / "vendor" / "rmux-server" / "src" / "pane_io.rs",
        )
        for path in vendored:
            path.parent.mkdir(parents=True)
            path.write_text("// tracked dependency patch\n", encoding="utf-8")

        before = self.manifest()
        files = {entry["path"] for entry in before["files"]}
        self.assertIn("vendor/rmux-client/src/attach.rs", files)
        self.assertIn("vendor/rmux-server/src/pane_io.rs", files)

        for path in vendored:
            with self.subTest(path=path):
                path.write_text("// changed dependency patch\n", encoding="utf-8")
                after = self.manifest()
                self.assertNotEqual(
                    before["workspace_source_sha256"],
                    after["workspace_source_sha256"],
                )
                before = after

    def test_expected_digest_validation_is_exact(self) -> None:
        valid = "a" * 64
        self.assertEqual(source_digest.validate_expected_digest(valid), valid)
        self.assertEqual(source_digest.validate_expected_digest("A" * 64), valid)
        for invalid in ("", "a" * 63, "g" * 64, "0x" + "a" * 64):
            with self.subTest(invalid=invalid):
                with self.assertRaises(source_digest.SourceIdentityError):
                    source_digest.validate_expected_digest(invalid)

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_identity_is_separate_strict_and_revalidates_exact_state(
        self,
    ) -> None:
        self.initialize_revision_repository()

        portable_before = self.manifest()
        revision = source_digest.workspace_revision_identity(self.workspace)
        self.assertEqual(revision["algorithm"], source_digest.REVISION_ALGORITHM)
        self.assertRegex(revision["head"], r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
        self.assertFalse(revision["detached"])
        self.assertTrue(str(revision["head_ref"]).startswith("refs/heads/"))
        self.assertEqual(
            source_digest.validate_workspace_revision_identity(
                revision, workspace=self.workspace
            ),
            revision,
        )

        self.script.write_text("#!/bin/sh\nexit 7\n", encoding="utf-8")
        dirty = source_digest.workspace_revision_identity(self.workspace)
        self.assertEqual(dirty["head"], revision["head"])
        self.assertNotEqual(
            dirty["status_porcelain_v1_z_sha256"],
            revision["status_porcelain_v1_z_sha256"],
        )
        self.assertNotEqual(
            dirty["tracked_binary_diff_sha256"],
            revision["tracked_binary_diff_sha256"],
        )
        self.assertNotEqual(
            portable_before["workspace_source_sha256"],
            self.manifest()["workspace_source_sha256"],
        )
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "revision identity changed"
        ):
            source_digest.validate_workspace_revision_identity(
                revision, workspace=self.workspace
            )

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_default_git_queries_use_the_shared_bounded_process_core(self) -> None:
        self.initialize_revision_repository()
        observed_receipts: list[dict[str, object]] = []
        original_run = source_digest.bounded_process.run

        def observe(*args: object, **kwargs: object) -> object:
            result = original_run(*args, **kwargs)
            receipt = source_digest.bounded_process.validate_execution_receipt(
                result.receipt
            )
            observed_receipts.append(receipt)
            return result

        with mock.patch.object(source_digest.bounded_process, "run", observe):
            source_digest.workspace_revision_identity(self.workspace)

        self.assertEqual(len(observed_receipts), 5)
        git = str(pathlib.Path(shutil.which("git") or "").resolve(strict=True))
        for receipt in observed_receipts:
            self.assertEqual(receipt["executable"]["path"], git)
            self.assertEqual(receipt["cwd"], str(self.workspace))
            self.assertTrue(receipt["process_ledger"])
            self.assertTrue(
                all(row["reaped"] is True for row in receipt["process_ledger"])
            )

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_capture_retains_five_exact_causal_receipts(self) -> None:
        self.initialize_revision_repository()
        capture = source_digest.workspace_revision_capture(self.workspace)

        self.assertEqual(
            [row["label"] for row in capture["commands"]],
            [query.label for query in source_digest._GIT_QUERY_SPECS],
        )
        self.assertEqual(
            source_digest.validate_workspace_revision_capture(
                capture, workspace=self.workspace
            ),
            capture,
        )
        self.assertEqual(
            source_digest.workspace_revision_identity(self.workspace),
            capture["identity"],
        )
        for row in capture["commands"]:
            receipt = row["receipt"]
            self.assertEqual(receipt["cwd"], str(self.workspace))
            self.assertEqual(receipt["stderr_size"], 0)
            self.assertTrue(receipt["process_ledger"])
            self.assertTrue(all(item["reaped"] for item in receipt["process_ledger"]))

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_capture_rejects_hostile_schema_and_order_substitution(
        self,
    ) -> None:
        self.initialize_revision_repository()
        capture = source_digest.workspace_revision_capture(self.workspace)
        mutations: list[tuple[str, dict[str, object], bool]] = []

        mutations.append(("extra", capture | {"extra": True}, False))
        mutations.append(("boolean-version", capture | {"schema_version": True}, False))

        reversed_commands = copy.deepcopy(capture)
        reversed_commands["commands"].reverse()
        mutations.append(("reordered", reversed_commands, True))

        duplicate_label = copy.deepcopy(capture)
        duplicate_label["commands"][1]["label"] = duplicate_label["commands"][0][
            "label"
        ]
        mutations.append(("duplicate-label", duplicate_label, True))

        extra_row_field = copy.deepcopy(capture)
        extra_row_field["commands"][0]["extra"] = True
        mutations.append(("extra-row-field", extra_row_field, True))

        wrong_digest = copy.deepcopy(capture)
        wrong_digest["capture_sha256"] = "0" * 64
        mutations.append(("wrong-digest", wrong_digest, False))

        for name, mutation, resign in mutations:
            with self.subTest(name=name):
                if resign:
                    self.resign_revision_capture(mutation)
                with self.assertRaises(source_digest.SourceIdentityError):
                    source_digest.validate_workspace_revision_capture(mutation)

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_capture_rejects_self_consistent_receipt_substitution(
        self,
    ) -> None:
        self.initialize_revision_repository()
        capture = source_digest.workspace_revision_capture(self.workspace)
        mutations: list[tuple[str, dict[str, object]]] = []

        wrong_argv = copy.deepcopy(capture)
        wrong_argv["commands"][0]["receipt"]["argv"][-1] = "HEAD~1"
        mutations.append(("argv", wrong_argv))

        wrong_environment = copy.deepcopy(capture)
        wrong_environment["commands"][0]["receipt"]["environment"][
            "environment_sha256"
        ] = "0" * 64
        mutations.append(("environment", wrong_environment))

        changed_executable_witness = copy.deepcopy(capture)
        changed_executable_witness["commands"][0]["receipt"]["executable"][
            "ctime_ns"
        ] += 1
        mutations.append(("executable-witness", changed_executable_witness))

        substituted_receipt = copy.deepcopy(capture)
        substituted_receipt["commands"][0]["receipt"] = copy.deepcopy(
            substituted_receipt["commands"][1]["receipt"]
        )
        mutations.append(("cross-label-receipt", substituted_receipt))

        nonempty_stderr = copy.deepcopy(capture)
        nonempty_stderr["commands"][0]["receipt"]["stderr_size"] = 1
        nonempty_stderr["commands"][0]["receipt"]["stderr_sha256"] = (
            source_digest.hashlib.sha256(b"x").hexdigest()
        )
        mutations.append(("stderr", nonempty_stderr))

        mismatched_status = copy.deepcopy(capture)
        mismatched_status["identity"]["status_porcelain_v1_z_sha256"] = "0" * 64
        mutations.append(("status-identity", mismatched_status))

        noncanonical_head_line = copy.deepcopy(capture)
        head_payload = capture["identity"]["head"].encode("ascii") + b" \n"
        noncanonical_head_line["commands"][0]["receipt"]["stdout_size"] = len(
            head_payload
        )
        noncanonical_head_line["commands"][0]["receipt"]["stdout_sha256"] = (
            source_digest.hashlib.sha256(head_payload).hexdigest()
        )
        mutations.append(("head-text", noncanonical_head_line))

        wrong_timeout = copy.deepcopy(capture)
        wrong_timeout["commands"][0]["receipt"]["timeout_seconds"] += 1
        mutations.append(("timeout", wrong_timeout))

        wrong_cwd = copy.deepcopy(capture)
        wrong_cwd["commands"][0]["receipt"]["cwd"] = "/tmp"
        wrong_cwd["commands"][0]["receipt"]["cwd_witness"]["path"] = "/tmp"
        mutations.append(("cwd", wrong_cwd))

        for name, mutation in mutations:
            with self.subTest(name=name):
                receipt = mutation["commands"][0]["receipt"]
                if name == "cross-label-receipt":
                    pass
                elif name == "status-identity":
                    receipt = None
                if receipt is not None:
                    self.resign_execution_receipt(receipt)
                self.resign_revision_capture(mutation)
                with self.assertRaises(source_digest.SourceIdentityError):
                    source_digest.validate_workspace_revision_capture(mutation)

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_schema_rejects_additions_boolean_integers_and_bad_refs(
        self,
    ) -> None:
        self.initialize_revision_repository()
        revision = source_digest.workspace_revision_identity(self.workspace)
        mutations = (
            revision | {"extra": True},
            revision | {"schema_version": True},
            revision | {"status_porcelain_v1_z_bytes": False},
            revision | {"head_ref": "refs/heads/bad\nref"},
            revision | {"detached": True},
            revision
            | {
                "source_digest_implementation": {
                    "path": "tools/linux-docker/source_digest.py",
                    "sha256": 7,
                }
            },
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                with self.assertRaises(source_digest.SourceIdentityError):
                    source_digest.validate_workspace_revision_identity(mutation)

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_validator_rejects_every_boolean_integer_and_path_tool_drift(
        self,
    ) -> None:
        self.initialize_revision_repository()
        revision = source_digest.workspace_revision_identity(self.workspace)

        integer_paths: list[tuple[object, ...]] = []

        def collect(value: object, path: tuple[object, ...] = ()) -> None:
            if type(value) is int:
                integer_paths.append(path)
            elif isinstance(value, dict):
                for key, child in value.items():
                    collect(child, (*path, key))
            elif isinstance(value, list):
                for index, child in enumerate(value):
                    collect(child, (*path, index))

        def replace(root: object, path: tuple[object, ...], value: object) -> None:
            target = root
            for component in path[:-1]:
                target = target[component]  # type: ignore[index]
            target[path[-1]] = value  # type: ignore[index]

        collect(revision)
        self.assertGreater(len(integer_paths), 10)
        for path in integer_paths:
            with self.subTest(boolean_integer_path=path):
                mutation = copy.deepcopy(revision)
                replace(mutation, path, True)
                with self.assertRaises(source_digest.SourceIdentityError):
                    source_digest.validate_workspace_revision_identity(mutation)

        mutations = []
        for workspace_path in (
            str(self.workspace / ".." / self.workspace.name),
            f"{self.workspace}/./child",
        ):
            mutations.append(revision | {"workspace": workspace_path})
        bad_ref = revision | {
            "head_ref": "refs/heads/good/../bad",
            "branch": "good/../bad",
        }
        mutations.append(bad_ref)
        for field, value in (
            ("mode", "0644"),
            ("size", source_digest.MAX_GIT_EXECUTABLE_BYTES + 1),
            ("path", str(pathlib.Path(revision["git"]["path"]) / ".." / "git")),
            ("version", "git version 2.0\nforged"),
            ("version", "git version 2.0\x00forged"),
        ):
            mutation = copy.deepcopy(revision)
            mutation["git"][field] = value
            mutations.append(mutation)
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                with self.assertRaises(source_digest.SourceIdentityError):
                    source_digest.validate_workspace_revision_identity(mutation)

        missing_git = copy.deepcopy(revision)
        missing_git["git"]["path"] = str(self.workspace / "missing-git")
        with self.assertRaises(source_digest.SourceIdentityError):
            source_digest.validate_workspace_revision_identity(
                missing_git, workspace=self.workspace
            )

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_bracket_rejects_git_control_mutate_restore_and_replacement(
        self,
    ) -> None:
        self.initialize_revision_repository()
        repository = self.workspace
        worktree = pathlib.Path(self.temporary.name) / "linked-worktree"
        subprocess.run(
            [
                "git",
                "-C",
                str(repository),
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-test",
                str(worktree),
            ],
            check=True,
            timeout=10,
        )
        self.addCleanup(
            subprocess.run,
            [
                "git",
                "-C",
                str(repository),
                "worktree",
                "remove",
                "--force",
                str(worktree),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
        gitfile = worktree / ".git"
        self.assertTrue(gitfile.is_file())
        source_digest.workspace_revision_identity(worktree)

        for mode in ("mutate_restore", "replace_restore"):
            mutated = False

            def runner(
                git: pathlib.Path,
                workspace: pathlib.Path,
                arguments: tuple[str, ...],
                maximum_stdout_bytes: int,
            ) -> source_digest.GitCommandOutcome:
                nonlocal mutated
                if not mutated:
                    mutated = True
                    original = gitfile.read_bytes()
                    if mode == "mutate_restore":
                        gitfile.write_bytes(original + b"# temporary\n")
                        gitfile.write_bytes(original)
                    else:
                        saved = gitfile.with_name(".git.saved")
                        gitfile.rename(saved)
                        gitfile.write_bytes(original)
                        gitfile.unlink()
                        saved.rename(gitfile)
                return source_digest._default_git_command_runner(
                    git, workspace, arguments, maximum_stdout_bytes
                )

            with self.subTest(mode=mode):
                with self.assertRaisesRegex(
                    source_digest.SourceIdentityError, "repository control identity"
                ):
                    source_digest.workspace_revision_identity(
                        worktree, command_runner=runner
                    )

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_control_directory_identity_survives_its_own_entry_set_moving(
        self,
    ) -> None:
        """A directory's mtime is not its identity, and every Git command moves it.

        `git status` creates `.git/index.lock` and unlinks it again even when it
        writes no index, which moves the Git directory's `st_mtime_ns` and
        `st_ctime_ns`. Recording those as identity made
        `workspace_revision_capture` abort with `Git repository control identity
        changed during capture` on a change it had not caused and that changed
        nothing about the repository -- MEASURED on this project's own checkout
        at 1 capture in 20 against an external ~6 s workspace poller, which took
        `gate_f/phase0_self_tests` red in 2 of 12 isolated runs.

        This is deliberately a PROPERTY over a real directory rather than an
        assertion about the field list: it fails against the pre-fix producer
        for the actual reason, and it keeps holding if the identity is ever
        spelled some other way.
        """

        self.initialize_revision_repository()
        git_dir = self.git_control_path("--git-dir")

        before = source_digest._repository_control_snapshot(self.workspace)
        # Exactly what a concurrent `git status` does to that directory: add an
        # entry, then remove it. Nothing else about the repository changes.
        lock = git_dir / "index.lock"
        lock.write_bytes(b"")
        lock.unlink()
        after = source_digest._repository_control_snapshot(self.workspace)

        self.assertEqual(before, after)
        # The entry set really did move, so the equality above is not vacuous.
        self.assertNotEqual(before["git_dir"]["path"], "")
        for node in (before["git_dir"], before["directories"]["objects"]):
            self.assertEqual(node["kind"], "directory")
            self.assertNotIn("mtime_ns", node)
            self.assertNotIn("ctime_ns", node)
        # A regular file keeps them: its bytes are bound by `sha256`, and its
        # timestamps do not move when a sibling entry appears.
        self.assertEqual(before["files"]["head"]["kind"], "file")
        for field in ("mtime_ns", "ctime_ns", "sha256"):
            self.assertIn(field, before["files"]["head"])
        # The validator agrees with the producer in BOTH directions. Accepting
        # what the producer emits is half of it; a validator that still expected
        # the timestamps would refuse every capture instead.
        self.assertEqual(
            source_digest._validate_control_node(
                before["git_dir"], description="Git directory"
            ),
            dict(before["git_dir"]),
        )
        # And refusing a directory node that carries one, so a producer that
        # started emitting them again would be caught here rather than silently
        # reintroducing the abort.
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "identity schema is not exact"
        ):
            source_digest._validate_control_node(
                {**before["git_dir"], "mtime_ns": "1", "ctime_ns": "1"},
                description="Git directory",
            )

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_control_binds_excludes_worktree_and_sparse_inputs(
        self,
    ) -> None:
        self.initialize_revision_repository()
        self.enable_worktree_and_sparse_config()

        capture = source_digest.workspace_revision_capture(self.workspace)
        control = capture["identity"]["repository_control"]
        git_dir = self.git_control_path("--git-dir")
        common_dir = self.git_control_path("--git-common-dir")
        files = control["files"]

        self.assertEqual(
            files["info_exclude"]["path"], str(common_dir / "info" / "exclude")
        )
        self.assertEqual(
            files["config_worktree"]["path"], str(git_dir / "config.worktree")
        )
        self.assertEqual(
            files["sparse_checkout"]["path"], str(git_dir / "info" / "sparse-checkout")
        )
        self.assertEqual(control["shared_indexes"], {})
        for row in capture["commands"]:
            argv = row["receipt"]["argv"]
            self.assertIn("core.excludesfile=/dev/null", argv)

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_split_index_is_rejected_before_any_revision_query(self) -> None:
        self.initialize_revision_repository()
        subprocess.run(
            ["git", "-C", str(self.workspace), "update-index", "--split-index"],
            check=True,
            timeout=10,
        )
        runner = mock.Mock()
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "split-index mode is unsupported"
        ):
            source_digest.workspace_revision_identity(
                self.workspace, command_runner=runner
            )
        runner.assert_not_called()

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_bracket_rejects_new_control_mutate_restore_and_replacement(
        self,
    ) -> None:
        self.initialize_revision_repository()
        self.enable_worktree_and_sparse_config()
        baseline = source_digest.workspace_revision_identity(self.workspace)
        control = baseline["repository_control"]
        paths = {
            "info-exclude": pathlib.Path(control["files"]["info_exclude"]["path"]),
            "config-worktree": pathlib.Path(
                control["files"]["config_worktree"]["path"]
            ),
            "sparse-checkout": pathlib.Path(
                control["files"]["sparse_checkout"]["path"]
            ),
        }

        for name, path in paths.items():
            for mode in ("mutate_restore", "replace_restore"):
                mutated = False

                def runner(
                    git: pathlib.Path,
                    workspace: pathlib.Path,
                    arguments: tuple[str, ...],
                    maximum_stdout_bytes: int,
                ) -> source_digest.GitCommandOutcome:
                    nonlocal mutated
                    if not mutated:
                        mutated = True
                        original = path.read_bytes()
                        if mode == "mutate_restore":
                            path.write_bytes(original + b"pmux-temporary-mutation")
                            path.write_bytes(original)
                        else:
                            saved = path.with_name(f"{path.name}.pmux-saved")
                            path.rename(saved)
                            path.write_bytes(original)
                            path.unlink()
                            saved.rename(path)
                    return source_digest._default_git_command_runner(
                        git, workspace, arguments, maximum_stdout_bytes
                    )

                with self.subTest(control=name, mode=mode):
                    with self.assertRaisesRegex(
                        source_digest.SourceIdentityError,
                        "repository control identity",
                    ):
                        source_digest.workspace_revision_identity(
                            self.workspace, command_runner=runner
                        )

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_repository_and_worktree_external_config_includes_are_rejected(
        self,
    ) -> None:
        self.initialize_revision_repository()
        common_dir = self.git_control_path("--git-common-dir")
        common_config = common_dir / "config"
        common_original = common_config.read_bytes()
        with common_config.open("a", encoding="utf-8") as handle:
            handle.write("\n[include]\n\tpath = /tmp/unbound-pmux-config\n")
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "unsupported external include"
        ):
            source_digest.workspace_revision_identity(self.workspace)

        common_config.write_bytes(common_original)
        self.enable_worktree_and_sparse_config()
        git_dir = self.git_control_path("--git-dir")
        with (git_dir / "config.worktree").open("a", encoding="utf-8") as handle:
            handle.write(
                '\n[includeIf "gitdir:**"]\n\tpath = /tmp/unbound-pmux-config\n'
            )
        with self.assertRaisesRegex(
            source_digest.SourceIdentityError, "unsupported external include"
        ):
            source_digest.workspace_revision_identity(self.workspace)

    @unittest.skipUnless(shutil.which("git"), "Git is required")
    def test_revision_control_schema_rejects_shared_index_and_fixed_path_substitution(
        self,
    ) -> None:
        self.initialize_revision_repository()
        self.enable_worktree_and_sparse_config()
        capture = source_digest.workspace_revision_capture(self.workspace)
        mutations: list[dict[str, object]] = []

        extra = copy.deepcopy(capture)
        extra["identity"]["repository_control"]["shared_indexes"][
            "sharedindex." + "0" * 40
        ] = copy.deepcopy(extra["identity"]["repository_control"]["files"]["index"])
        extra["identity"]["repository_control"]["shared_indexes"][
            "sharedindex." + "0" * 40
        ]["path"] = str(
            self.git_control_path("--git-dir") / ("sharedindex." + "0" * 40)
        )
        mutations.append(extra)

        wrong_path = copy.deepcopy(capture)
        wrong_path["identity"]["repository_control"]["files"]["info_exclude"][
            "path"
        ] = str(self.workspace / "forged-exclude")
        mutations.append(wrong_path)

        missing_sparse = copy.deepcopy(capture)
        del missing_sparse["identity"]["repository_control"]["files"]["sparse_checkout"]
        mutations.append(missing_sparse)

        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.resign_revision_capture(mutation)
                with self.assertRaises(source_digest.SourceIdentityError):
                    source_digest.validate_workspace_revision_capture(mutation)


if __name__ == "__main__":
    unittest.main()
