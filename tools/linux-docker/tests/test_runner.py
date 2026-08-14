from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

TOOLS = pathlib.Path(__file__).resolve().parents[1]
RUNNER = TOOLS / "run.sh"
WORKSPACE = TOOLS.parents[1]
sys.path.insert(0, str(TOOLS))

import source_digest  # noqa: E402


BASE_IMAGE = "docker.io/library/rust:1.88.0-bookworm@sha256:" + "b" * 64

# The gates that exist ONLY inside the container: image and cross-UID identity,
# the release build and its reproduction, the TypeScript stage identity carried
# across the gates that consume it, and the post-run source/artifact checks.
# There is nothing to derive these from -- they have no candidate cell by
# definition -- so they are declared once, here, and every other statement about
# the size or membership of the Linux projection is computed from this set plus
# the candidate manifest. `docs/gate-c-linux-handoff.md` §3.2 names the same
# fifteen.
CONTAINER_ONLY_GATES = frozenset(
    """
    system_identity image_release_binary_identity cross_uid_uds_report
    typescript_stage_identity_capture release_candidate_binding release_build
    release_repro_stage release_repro_binary_equivalence
    typescript_stage_preconsume_unchanged release_binary_unchanged
    typescript_stage_postconsume_unchanged validation_output_cleanup
    container_source_after container_source_stability artifact_privacy
    """.split()
)


class RunnerFailureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name).resolve()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.workspace = self.root / "workspace"
        isolated_tools = self.workspace / "tools" / "linux-docker"
        isolated_tools.mkdir(parents=True)
        for name in (
            "Dockerfile",
            "bounded_runner.py",
            "evidence.py",
            "run.sh",
            "source_digest.py",
        ):
            shutil.copy2(TOOLS / name, isolated_tools / name)
        isolated_common = self.workspace / "tools" / "evidence_common"
        isolated_common.mkdir()
        shutil.copy2(
            WORKSPACE / "tools" / "evidence_common" / "bounded_process.py",
            isolated_common / "bounded_process.py",
        )
        shutil.copy2(
            WORKSPACE / "tools" / "evidence_common" / "managed_process.py",
            isolated_common / "managed_process.py",
        )
        self.runner = isolated_tools / "run.sh"
        self.marker = self.root / "docker-called"
        docker = self.bin / "docker"
        docker.write_text(
            f"#!/bin/sh\nprintf called > '{self.marker}'\nexit 99\n",
            encoding="utf-8",
        )
        docker.chmod(0o755)
        self.environment = os.environ.copy()
        self.environment["PATH"] = f"{self.bin}:{self.environment['PATH']}"
        self.environment["PYTHONDONTWRITEBYTECODE"] = "1"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def invoke(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(self.runner), *arguments],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=20,
            check=False,
            env=self.environment,
        )

    def assert_no_docker(self) -> None:
        self.assertFalse(self.marker.exists(), "an early runner failure invoked Docker")

    def test_help_is_side_effect_free(self) -> None:
        result = self.invoke("--help")
        self.assertEqual(result.returncode, 0)
        self.assertIn("--acknowledge-docker", result.stderr)
        self.assert_no_docker()

    def test_missing_consent_fails_before_docker(self) -> None:
        result = self.invoke("--source-sha256", "a" * 64)
        self.assertEqual(result.returncode, 2)
        self.assertIn("acknowledge", result.stderr)
        self.assert_no_docker()

    def test_missing_or_mutable_base_image_fails_before_docker(self) -> None:
        current_digest, _ = source_digest.workspace_source_digest(self.workspace)
        for base_arguments in (
            (),
            ("--base-image", "rust:1.88.0-bookworm"),
            (
                "--base-image",
                "docker.io/library/rust:1.88.0-bookworm@sha256:" + "B" * 64,
            ),
        ):
            with self.subTest(base_arguments=base_arguments):
                self.marker.unlink(missing_ok=True)
                result = self.invoke(
                    "--source-sha256",
                    current_digest,
                    "--acknowledge-docker",
                    *base_arguments,
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("base image", result.stderr)
                self.assert_no_docker()

    def test_malformed_digest_unknown_platform_and_relative_output_fail_early(
        self,
    ) -> None:
        cases = (
            ("--source-sha256", "bad", "--acknowledge-docker"),
            (
                "--source-sha256",
                "a" * 64,
                "--acknowledge-docker",
                "--platform",
                "s390x",
            ),
            (
                "--source-sha256",
                "a" * 64,
                "--acknowledge-docker",
                "--output",
                "relative",
                "--base-image",
                BASE_IMAGE,
            ),
        )
        for arguments in cases:
            with self.subTest(arguments=arguments):
                self.marker.unlink(missing_ok=True)
                result = self.invoke(*arguments)
                self.assertEqual(result.returncode, 2)
                self.assert_no_docker()

    def test_nonempty_evidence_directory_fails_before_docker(self) -> None:
        output = self.root / "evidence"
        output.mkdir()
        (output / "existing").write_text("preserve", encoding="utf-8")
        current_digest, _ = source_digest.workspace_source_digest(self.workspace)
        result = self.invoke(
            "--source-sha256",
            current_digest,
            "--acknowledge-docker",
            "--base-image",
            BASE_IMAGE,
            "--output",
            str(output),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((output / "existing").read_text(encoding="utf-8"), "preserve")
        self.assert_no_docker()

    def test_wrong_frozen_digest_fails_before_docker(self) -> None:
        output = self.root / "evidence"
        result = self.invoke(
            "--source-sha256",
            "0" * 64,
            "--acknowledge-docker",
            "--base-image",
            BASE_IMAGE,
            "--output",
            str(output),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(output.exists())
        self.assert_no_docker()

    def test_runner_contains_no_broad_docker_cleanup_or_host_mount(self) -> None:
        source = RUNNER.read_text(encoding="utf-8")
        for forbidden in (
            "docker system prune",
            "docker image prune",
            "docker builder prune",
            "docker container prune",
            "docker volume prune",
            "docker ps -q",
            "docker images -q",
            "--mount",
        ):
            with self.subTest(forbidden=forbidden):
                self.assertNotIn(forbidden, source)
        self.assertFalse((TOOLS / "blackbox.py").exists())

    def test_build_identity_is_digest_bound_and_records_package_resolution(
        self,
    ) -> None:
        runner = RUNNER.read_text(encoding="utf-8")
        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn('--build-arg "PMUX_RUST_BASE=$base_image"', runner)
        self.assertIn('--iidfile "$iid_file"', runner)
        self.assertFalse(
            any(line.startswith("# syntax=") for line in dockerfile.splitlines())
        )
        self.assertIn("FROM ${PMUX_RUST_BASE}", dockerfile)
        self.assertIn("pmux-system-packages.tsv", dockerfile)
        self.assertIn("pmux-debian-snapshot.txt", dockerfile)
        self.assertIn("snapshot=20250725T000000Z", dockerfile)
        self.assertNotIn("resolved-versions-recorded-not-snapshot-pinned", dockerfile)

    def test_canonical_vendor_source_is_reincluded_and_rehashed_in_build_context(
        self,
    ) -> None:
        self.assertIn("vendor", source_digest.INCLUDED_ROOT_DIRECTORIES)
        expected = {f"!{name}" for name in source_digest.INCLUDED_ROOT_FILES}
        for name in source_digest.INCLUDED_ROOT_DIRECTORIES:
            expected.update((f"!{name}", f"!{name}/**"))
        for ignore_path in (
            WORKSPACE / ".dockerignore",
            TOOLS / "Dockerfile.dockerignore",
        ):
            with self.subTest(ignore_path=ignore_path):
                directives = {
                    line.strip()
                    for line in ignore_path.read_text(encoding="utf-8").splitlines()
                    if line.strip() and not line.lstrip().startswith("#")
                }
                self.assertEqual(expected - directives, set())

        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")
        copy_offset = dockerfile.index("COPY . /workspace")
        digest_offset = dockerfile.index(
            "python3 tools/linux-docker/source_digest.py /workspace"
        )
        self.assertLess(copy_offset, digest_offset)
        self.assertNotIn("COPY --chown=pmux:pmux . /workspace", dockerfile)

    def test_docker_context_filters_are_exact_and_exclude_credential_files(
        self,
    ) -> None:
        root_ignore = (WORKSPACE / ".dockerignore").read_text(encoding="utf-8")
        dockerfile_ignore = (TOOLS / "Dockerfile.dockerignore").read_text(
            encoding="utf-8"
        )
        self.assertEqual(root_ignore, dockerfile_ignore)
        directives = {
            line.strip()
            for line in root_ignore.splitlines()
            if line.strip() and not line.lstrip().startswith("#")
        }
        for pattern in (
            ".claude",
            "**/.claude",
            ".env",
            "**/.env",
            ".npmrc",
            "**/.npmrc",
            ".netrc",
            "**/.netrc",
            ".cargo/credentials",
            "**/.cargo/credentials",
            ".cargo/credentials.toml",
            "**/.cargo/credentials.toml",
            "credentials",
            "**/credentials",
            "credentials.toml",
            "**/credentials.toml",
            "*.pem",
            "**/*.pem",
            "*.key",
            "**/*.key",
            "id_rsa*",
            "**/id_rsa*",
            "id_ed25519*",
            "**/id_ed25519*",
        ):
            with self.subTest(pattern=pattern):
                self.assertIn(pattern, directives)

    def test_linux_manifest_is_the_exact_ordered_candidate_projection(self) -> None:
        candidate = json.loads(
            (WORKSPACE / "tools/gate-a-candidate/phase-manifest.json").read_text(
                encoding="utf-8"
            )
        )
        linux = json.loads((TOOLS / "gate-a-manifest.json").read_text(encoding="utf-8"))
        # The candidate phase counts used to be a literal dict here, three lines
        # below the file it restated, and it went stale the instant the candidate
        # was trimmed: it read `gate_a: 42, gate_d: 11` against 41 and 10 on
        # disk. That made this test red with a message about ARITHMETIC while the
        # thing it exists to detect -- the projection drift of debt row C6 -- was
        # never reached. The counts are not a fact about this file. What is
        # required of the candidate is only that it declares the seven phases the
        # projection below walks, and that none of them is empty.
        self.assertEqual(
            sorted(candidate["phases"]),
            ["gate_a", "gate_b", "gate_c", "gate_d", "gate_e", "gate_f", "residue"],
        )
        for phase, commands in candidate["phases"].items():
            self.assertTrue(commands, f"candidate phase {phase!r} declares no cell")

        expected: list[tuple[str, str]] = [
            ("P", "system_identity"),
            ("P", "image_release_binary_identity"),
            ("P", "cross_uid_uds_report"),
        ]
        candidate_a = [command["id"] for command in candidate["phases"]["gate_a"]]
        stage_verify_index = candidate_a.index("typescript_stage_verify") + 1
        expected.extend(("A", name) for name in candidate_a[:stage_verify_index])
        expected.append(("A", "typescript_stage_identity_capture"))
        expected.extend(("A", name) for name in candidate_a[stage_verify_index:])
        for candidate_phase, linux_phase in (("gate_b", "B"), ("gate_c", "C")):
            expected.extend(
                (linux_phase, command["id"])
                for command in candidate["phases"][candidate_phase]
            )
        expected.extend(
            (
                ("D", "release_candidate_binding"),
                ("D", "release_build"),
                ("D", "release_repro_stage"),
                ("D", "release_repro_binary_equivalence"),
                ("D", "typescript_stage_preconsume_unchanged"),
            )
        )
        expected.extend(
            ("D", command["id"]) for command in candidate["phases"]["gate_d"]
        )
        expected.extend(
            ("E", command["id"]) for command in candidate["phases"]["gate_e"]
        )
        expected.append(("E", "release_binary_unchanged"))

        candidate_f = [command["id"] for command in candidate["phases"]["gate_f"]]
        residue_index = candidate_f.index("residue_script_self_test")
        expected.extend(("F", name) for name in candidate_f[:residue_index])
        expected.append(("F", "typescript_stage_postconsume_unchanged"))
        expected.append(("F", "validation_output_cleanup"))
        expected.extend(("F", name) for name in candidate_f[residue_index:])
        candidate_residue = candidate["phases"]["residue"]
        self.assertEqual(
            [command["id"] for command in candidate_residue], ["gate_a_residue"]
        )
        expected.append(("F", "gate_a_residue"))
        expected.extend(
            (
                ("Z", "container_source_after"),
                ("Z", "container_source_stability"),
                ("Z", "artifact_privacy"),
            )
        )

        candidate_ids = [
            command["id"] for phase in candidate["phases"].values() for command in phase
        ]
        # The container-only names built into `expected` above and the declared
        # set are cross-checked against each other, so neither can drift alone.
        self.assertEqual(
            {name for _phase, name in expected} - set(candidate_ids),
            set(CONTAINER_ONLY_GATES),
        )

        observed = [(gate["phase"], gate["name"]) for gate in linux["gates"]]
        self.assertEqual(len(observed), len(set(observed)))
        # Membership and size BEFORE order, because "which cells" is the
        # diagnosis and "in what order" is the detail -- and because the ordered
        # comparison below is currently red on debt row C6, which would otherwise
        # make these two unreachable. Was `len(observed) == 97`: a number this
        # test remembered rather than a property of a projection. The size of an
        # exact projection is the candidate's cells plus the cells that exist
        # only inside the container; a name that is neither IS the drift.
        self.assertEqual(
            {name for _phase, name in observed},
            set(candidate_ids) | set(CONTAINER_ONLY_GATES),
        )
        self.assertEqual(len(observed), len(candidate_ids) + len(CONTAINER_ONLY_GATES))
        self.assertEqual(observed, expected)

    def test_high_risk_mirrored_commands_bind_cwd_and_nightly_plugins(self) -> None:
        candidate = json.loads(
            (WORKSPACE / "tools/gate-a-candidate/phase-manifest.json").read_text(
                encoding="utf-8"
            )
        )
        commands = {
            command["id"]: command
            for phase in candidate["phases"].values()
            for command in phase
        }
        python = commands["python_client"]
        self.assertEqual(python["cwd"], "{workspace}/clients/python")
        self.assertEqual(
            python["argv"],
            ["{python}", "-m", "unittest", "discover", "-s", "tests", "-v"],
        )
        production_fuzz = commands["production_fuzz"]
        self.assertEqual(
            production_fuzz["env"],
            {
                "PMUX_CARGO_FUZZ_BIN": "{cargo_fuzz}",
                "PMUX_FUZZ_EVIDENCE_ROOT": "{validation}/fuzz-evidence",
                "PMUX_FUZZ_RUNS": "50000",
                "PMUX_FUZZ_TARGET_DIR": "{validation}/fuzz",
                "PMUX_NIGHTLY_BIN_DIR": "{nightly_bin}",
                "PMUX_NIGHTLY_CARGO": "{nightly_cargo}",
                "PMUX_NIGHTLY_RUSTC": "{nightly_rustc}",
            },
        )

        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        self.assertIn(
            "bash -c 'cd clients/python && exec python3 -m unittest discover -s tests -v'",
            suite,
        )
        self.assertNotIn("unittest discover -s clients/python/tests", suite)
        for value in (
            "readonly nightly_toolchain=nightly-2026-03-26",
            'rustup which --toolchain "$nightly_toolchain" cargo',
            'rustup which --toolchain "$nightly_toolchain" rustc',
            'PMUX_NIGHTLY_BIN_DIR="$nightly_bin"',
            'PMUX_NIGHTLY_CARGO="$nightly_cargo"',
            'PMUX_NIGHTLY_RUSTC="$nightly_rustc"',
        ):
            with self.subTest(value=value):
                self.assertIn(value, suite)

    def test_typescript_stage_binding_rejects_mutation_between_gates(self) -> None:
        files = (
            "client.d.ts",
            "client.d.ts.map",
            "client.js",
            "client.js.map",
            "index.d.ts",
            "index.d.ts.map",
            "index.js",
            "index.js.map",
            "package.json",
            "protocol.d.ts",
            "protocol.d.ts.map",
            "protocol.js",
            "protocol.js.map",
            "smithers.d.ts",
            "smithers.d.ts.map",
            "smithers.js",
            "smithers.js.map",
        )
        node = shutil.which("node")
        self.assertIsNotNone(node)
        assert node is not None
        verifier = WORKSPACE / "clients/typescript/tests/dist-stage.mjs"
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            outside_root = root / "workspace"
            stage = root / "stage"
            outside_root.mkdir(mode=0o700)
            stage.mkdir(mode=0o700)
            for name in files:
                payload = (
                    b'{"type":"module"}\n' if name == "package.json" else name.encode()
                )
                path = stage / name
                path.write_bytes(payload)
                path.chmod(0o600)

            verified = subprocess.run(
                [
                    node,
                    str(verifier),
                    "verify",
                    str(stage),
                    "--outside-root",
                    str(outside_root),
                ],
                check=False,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=10,
            )
            self.assertEqual(verified.returncode, 0, verified.stderr)
            digest = verified.stdout.strip()
            self.assertRegex(digest, r"^[0-9a-f]{64}$")
            command = [
                "bash",
                str(TOOLS / "suite.sh"),
                "--verify-typescript-stage-identity",
                digest,
                node,
                str(verifier),
                str(stage),
                str(outside_root),
            ]
            unchanged = subprocess.run(
                command,
                check=False,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=10,
            )
            self.assertEqual(unchanged.returncode, 0, unchanged.stderr)
            self.assertEqual(unchanged.stdout, f"{digest}\n")

            (stage / "index.js").write_bytes(b"mutated between gate phases\n")
            mutated = subprocess.run(
                command,
                check=False,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=10,
            )
            self.assertNotEqual(mutated.returncode, 0)
            self.assertIn("changed after its Gate A freeze", mutated.stderr)

    def test_standalone_vendor_gates_are_prefetched_built_and_ordered(self) -> None:
        gate_names = [
            "rmux_vendor_standalone_fmt",
            "rmux_vendor_standalone_check",
            "rmux_vendor_standalone_clippy",
            "rmux_vendor_standalone_rustdoc",
            "rmux_vendor_standalone_tests",
            "rmux_vendor_patch",
            "rmux_attach_fragmentation",
            "rmux_server_vendor_fmt",
            "rmux_server_vendor_product_check",
            "rmux_server_vendor_strict_clippy",
            "rmux_server_vendor_strict_rustdoc",
            "rmux_server_vendor_patch_regressions",
            "rmux_server_vendor_patch",
        ]
        manifest = json.loads(
            (TOOLS / "gate-a-manifest.json").read_text(encoding="utf-8")
        )
        declared = [gate["name"] for gate in manifest["gates"] if gate["phase"] == "A"]
        offsets = [declared.index(name) for name in gate_names]
        self.assertEqual(offsets, sorted(offsets))

        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        for name in gate_names:
            with self.subTest(name=name):
                self.assertIn(f"run_gate {name}", suite)
        self.assertIn(
            'readonly vendor_client_target="$cargo_target_root/vendor-rmux-client"',
            suite,
        )
        self.assertIn(
            'readonly vendor_server_target="$cargo_target_root/vendor-rmux-server"',
            suite,
        )
        self.assertIn("--manifest-path vendor/rmux-client/Cargo.toml", suite)
        self.assertIn("--manifest-path vendor/rmux-server/Cargo.toml", suite)
        self.assertIn(
            "rustfmt +1.88 --edition 2021 --check \\\n"
            "  vendor/rmux-server/src/lib.rs vendor/rmux-server/build.rs",
            suite,
        )
        self.assertIn("--all-targets --no-default-features", suite)
        self.assertIn("--lib --no-default-features", suite)
        self.assertIn("--all-targets --all-features \\\n  -- -D warnings", suite)
        self.assertIn(
            "-A clippy::collapsible-else-if -A clippy::uninlined-format-args",
            suite,
        )

        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn(
            "cargo +1.88 fetch --locked --manifest-path vendor/rmux-client/Cargo.toml",
            dockerfile,
        )
        self.assertIn(
            "CARGO_TARGET_DIR=/opt/pmux-prefetch/vendor-rmux-client", dockerfile
        )
        self.assertIn(
            "cargo +1.88 fetch --locked --manifest-path vendor/rmux-server/Cargo.toml",
            dockerfile,
        )
        self.assertIn(
            "CARGO_TARGET_DIR=/opt/pmux-prefetch/vendor-rmux-server", dockerfile
        )
        self.assertIn("--lib --no-default-features --no-run", dockerfile)
        self.assertIn("--all-targets --all-features --no-run", dockerfile)

    def test_the_linux_lane_runs_the_patch_regressions_by_module_never_by_name(
        self,
    ) -> None:
        """The names come from the patch document, so this grows by itself.

        This suite used to carry the fourteen patch-owned regression names as a
        literal tuple -- the fourth of six hand-kept copies -- and assert that
        `suite.sh` carried them too. Both copies were `--exact`, which runs zero
        tests and exits zero for a name nobody wrote, so the pair agreed
        perfectly about a set neither derived and a fifteenth regression would
        have executed in no lane at all.

        `vendor/rmux-server/PMUX-PATCH.md` publishes the set, and
        `crates/rmux/tests/vendor_server_patch.rs` proves that list is exactly
        the tests the patch adds to the vendored source. Read from there, the
        assertion below is the inverse of the old one: the lane must run the
        MODULE and must not name a single member of it.
        """

        published = [
            line.split("`")[1]
            for line in (
                (WORKSPACE / "vendor" / "rmux-server" / "PMUX-PATCH.md")
                .read_text(encoding="utf-8")
                .split("The exact regression names are:")[1]
                .splitlines()
            )
            if line.startswith("- `")
        ]
        self.assertTrue(published, "the patch document published no regression")

        module_gate = (
            "  --manifest-path vendor/rmux-server/Cargo.toml"
            " --lib --no-default-features \\\n"
            "  pane_io::tests:: \\\n"
            "  -- --test-threads=1\n"
        )
        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        # assertTrue and not assertIn, here and below: the haystack is the whole
        # suite script, and dumping it buries the one fact that matters.
        self.assertTrue(
            module_gate in suite,
            f"suite.sh does not run the vendored patch regressions by module:\n"
            f"{module_gate}",
        )
        for regression in published:
            with self.subTest(regression=regression):
                self.assertFalse(
                    regression in suite,
                    f"suite.sh names the patch regression {regression}; this "
                    f"lane runs the module and the names live in PMUX-PATCH.md",
                )

    def test_package_framing_property_and_shellcheck_gates_are_exact(self) -> None:
        manifest = json.loads(
            (TOOLS / "gate-a-manifest.json").read_text(encoding="utf-8")
        )
        names = [gate["name"] for gate in manifest["gates"]]
        required = [
            "typescript_package_artifact",
            "python_package_artifact",
            "python_ruff_format",
            "protocol_framing_properties",
            "package_smoke_self_tests",
            "candidate_envelope_tests",
            "shellcheck",
            "release_native_service",
            "residue_script_self_test",
            "gate_a_residue",
        ]
        self.assertTrue(set(required).issubset(names))

        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        for name in required:
            with self.subTest(name=name):
                self.assertIn(f"run_gate {name}", suite)
        self.assertIn("tools/package-smoke/package_smoke.py typescript", suite)
        self.assertIn("tools/package-smoke/package_smoke.py python", suite)
        self.assertIn("PROPTEST_CASES=2048 PROPTEST_MAX_SHRINK_ITERS=10000", suite)
        self.assertIn(
            "handler::tests::arbitrary_admitted_payloads_have_bounded_decode_recovery_and_responses",
            suite,
        )
        self.assertIn("python3 -m ruff check --no-cache", suite)
        self.assertIn("python3 -m ruff format --check --no-cache", suite)
        client_source = (WORKSPACE / "crates/client/src/lib.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("RngSeed::Fixed(0x504d_5558_434c_4e54)", client_source)
        self.assertIn("scripts/gate-a-fuzz.sh", suite)
        self.assertIn("scripts/gate-a-residue.sh", suite)
        self.assertIn('PMUX_E2E_BIN_DIR="$release_dir"', suite)
        self.assertNotIn("pack --dry-run", suite)
        self.assertNotIn("python_package_stage", suite)

        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("        shellcheck \\\n", dockerfile)

    def test_external_typescript_stage_is_exact_and_cleaned(self) -> None:
        manifest = json.loads(
            (TOOLS / "gate-a-manifest.json").read_text(encoding="utf-8")
        )
        declared = [gate["name"] for gate in manifest["gates"]]
        stage_gates = [
            "typescript_typecheck",
            "typescript_stage_prepare",
            "typescript_external_build",
            "typescript_stage_verify",
            "typescript_stage_identity_capture",
            "typescript_tests",
            "typescript_actual_daemon_syntax",
            "typescript_stage_preconsume_unchanged",
            "typescript_stage_postconsume_unchanged",
        ]
        offsets = [declared.index(name) for name in stage_gates]
        self.assertEqual(offsets, sorted(offsets))
        self.assertLess(
            declared.index("release_full_stack_e2e"),
            declared.index("typescript_stage_postconsume_unchanged"),
        )
        self.assertLess(
            declared.index("typescript_stage_postconsume_unchanged"),
            declared.index("validation_output_cleanup"),
        )
        self.assertLess(
            declared.index("validation_output_cleanup"),
            declared.index("gate_a_residue"),
        )

        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        self.assertIn(
            "readonly validation_root=/var/tmp/pmux-linux-suite/validation",
            suite,
        )
        self.assertIn(
            'readonly typescript_dist="$validation_root/typescript-dist"', suite
        )
        self.assertIn('readonly fuzz_target="$validation_root/fuzz"', suite)
        self.assertIn("install -d -m 0700 \\\n", suite)
        self.assertIn('  "$validation_root" \\\n', suite)
        self.assertIn('  "$typescript_dist" \\\n', suite)
        self.assertIn('  "$fuzz_target"', suite)
        for name in stage_gates:
            with self.subTest(name=name):
                self.assertIn(f"run_gate {name}", suite)
        self.assertIn(
            "node clients/typescript/tests/dist-stage.mjs prepare \\\n"
            '  "$typescript_dist" --outside-root "$workspace"',
            suite,
        )
        self.assertIn('--outDir "$typescript_dist"', suite)
        self.assertIn(
            "node clients/typescript/tests/dist-stage.mjs verify \\\n"
            '  "$typescript_dist" --outside-root "$workspace"',
            suite,
        )
        self.assertIn('PMUX_TYPESCRIPT_DIST_DIR="$typescript_dist"', suite)
        self.assertIn('PMUX_E2E_TYPESCRIPT_DIST_DIR="$typescript_dist"', suite)
        self.assertIn(
            "run_gate validation_output_cleanup \\\n"
            '  bash "$suite_script" --cleanup-validation-outputs "$validation_root"',
            suite,
        )
        self.assertNotIn("clients/typescript/dist", suite)
        self.assertNotIn("fuzz/target", suite)

    def test_the_typescript_gate_runs_every_test_file_the_package_globs(self) -> None:
        """DERIVED from the directory, not restated here.

        `clients/typescript/package.json` runs `node --test tests/*.test.mjs`;
        both this suite and `tools/gate-a-candidate/phase-manifest.json` hand-list
        the files instead, because a gate argv is literal by design. PROVEN
        vacuous by mutation on the candidate side: with a deliberately failing
        `zz-mutation.test.mjs` added beside them the glob exited 1 (50 pass,
        1 fail) and the hand-listed cell exited 0 (50 pass, 0 fail). This is the
        Linux half of that derivation;
        `tools/gate-a/tests/test_run_gate.py::test_the_typescript_cell_runs_every_test_file_the_package_globs`
        is the candidate half.
        """

        directory = WORKSPACE / "clients" / "typescript" / "tests"
        globbed = sorted(
            str(path.relative_to(WORKSPACE)) for path in directory.glob("*.test.mjs")
        )
        self.assertTrue(globbed, "found no TypeScript test file; derivation broken")
        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        _, _, after = suite.partition("run_gate typescript_tests")
        invocation, _, _ = after.partition("\nrun_gate ")
        named = sorted(
            word
            for word in invocation.split()
            if word.endswith(".test.mjs") or word.endswith(".mjs")
        )
        self.assertEqual(named, globbed)

    def test_source_candidate_and_validation_are_three_distinct_planes(self) -> None:
        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")
        inside = (TOOLS / "inside.sh").read_text(encoding="utf-8")
        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        manifest = json.loads(
            (TOOLS / "gate-a-manifest.json").read_text(encoding="utf-8")
        )
        names = [gate["name"] for gate in manifest["gates"]]

        self.assertIn("CARGO_TARGET_DIR=/opt/pmux-candidate/build", dockerfile)
        self.assertIn("/opt/pmux-candidate/bin/$binary", dockerfile)
        self.assertNotIn("CARGO_TARGET_DIR=/workspace/target", dockerfile)
        self.assertIn("readonly release_dir=/opt/pmux-candidate/bin", inside)
        self.assertIn(
            'PMUX_LINUX_CANDIDATE_DIR="$release_dir"',
            inside,
        )

        self.assertIn('readonly release_dir="${PMUX_LINUX_CANDIDATE_DIR:', suite)
        self.assertIn('readonly workspace_target="$cargo_target_root/workspace"', suite)
        self.assertIn(
            'readonly repro_release_target="$cargo_target_root/repro-release"',
            suite,
        )
        self.assertIn('export CARGO_TARGET_DIR="$workspace_target"', suite)
        self.assertIn(
            'env CARGO_TARGET_DIR="$repro_release_target"',
            suite,
        )
        self.assertIn('"$evidence" binary-repro-compare', suite)
        self.assertIn('"$candidate_before" "$repro_bin" "$repro_comparison"', suite)
        self.assertNotIn('readonly release_dir="$workspace/target/release"', suite)

        required = [
            "release_candidate_binding",
            "release_build",
            "release_repro_stage",
            "release_repro_binary_equivalence",
            "typescript_stage_preconsume_unchanged",
            "release_full_stack_e2e",
            "release_binary_unchanged",
            "typescript_stage_postconsume_unchanged",
            "validation_output_cleanup",
        ]
        offsets = [names.index(name) for name in required]
        self.assertEqual(offsets, sorted(offsets))

    def test_host_export_binds_control_plane_and_complete_candidate_chain(self) -> None:
        runner = (TOOLS / "run.sh").read_text(encoding="utf-8")
        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        for value in (
            "host.client-plugin-inventory",
            "{{json .ClientInfo.Plugins}}",
            '"$evidence" docker-transport "$docker_socket_path"',
            '"$evidence" docker-transport-stability',
            '"$evidence" docker-control-plane',
            '"$copy_stage/image-release-binaries.json"',
            '"$copy_stage/release-binaries-before.json"',
            '"$copy_stage/release-binaries-after.json"',
            '"$copy_stage/repro-release-staged.json"',
            '"$copy_stage/repro-release-comparison.json"',
            '"$copy_stage/uds-binary-binding.json"',
            '"$copy_stage/result.json"',
            '"$copy_stage/platform-gate-a-manifest.json"',
        ):
            with self.subTest(value=value):
                self.assertIn(value, runner)
        self.assertIn('"$evidence" binary-repro-stage', suite)
        self.assertNotIn("install -m 0500", suite)
        revision_after = runner.index("host-revision-capture-after.json")
        revision_stability = runner.index(
            '"$evidence" revision-stability', revision_after
        )
        cell_binding = runner.index('"$evidence" cell-binding', revision_stability)
        self.assertLess(revision_after, revision_stability)
        self.assertLess(revision_stability, cell_binding)
        binding_slice = runner[cell_binding : cell_binding + 1800]
        self.assertIn('"$output_root/host-revision-capture-before.json"', binding_slice)
        self.assertIn('"$output_root/host-revision-capture-after.json"', binding_slice)
        self.assertIn('"$output_root/host-revision-stability.json"', binding_slice)

    def test_cross_uid_probe_binds_six_paths_and_candidate_mutation_denial(
        self,
    ) -> None:
        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        inside = (TOOLS / "inside.sh").read_text(encoding="utf-8")
        probe = (TOOLS / "permissions_probe.py").read_text(encoding="utf-8")
        dockerfile = (TOOLS / "Dockerfile").read_text(encoding="utf-8")

        self.assertIn(
            "run_gate cross_uid_uds_report \\\n"
            '  "$evidence" uds-binding \\\n'
            '  "$artifacts/uds-permissions.json" "$initial_binaries" \\\n'
            '  "$artifacts/uds-probe.receipt.json" \\\n'
            '  "$artifacts/uds-probe.stdout" "$artifacts/uds-probe.stderr" \\\n'
            '  "$artifacts/uds-binary-binding.json"',
            suite,
        )
        for value in (
            'root_evidence="$(mktemp -d /var/tmp/pmux-root-evidence.XXXXXXXX)"',
            "--timeout-seconds 90",
            "--drain-timeout-seconds 10",
            "--maximum-output-bytes 16777216",
            '"$root_report:$artifacts/uds-permissions.json:67108864"',
            '"$root_stdout:$artifacts/uds-probe.stdout:16777216"',
            '"$root_stderr:$artifacts/uds-probe.stderr:16777216"',
            '"$root_receipt:$artifacts/uds-probe.receipt.json:4194304"',
            "exec runuser -u pmux -- env -i",
        ):
            with self.subTest(value=value):
                self.assertIn(value, inside)
        self.assertIn('"--write-denied"', probe)
        self.assertIn('domain="pmux.evidence.uds-permissions-report.v3"', probe)
        self.assertIn('print(payload["report_sha256"], flush=True)', probe)
        self.assertIn("COPY . /workspace", dockerfile)
        self.assertNotIn("COPY --chown=pmux:pmux . /workspace", dockerfile)
        self.assertIn("chown -R root:root /opt/pmux-candidate", dockerfile)
        self.assertIn("chmod 0555 /opt/pmux-candidate/bin/*", dockerfile)

    def test_every_inside_gate_uses_bounded_private_receipts(self) -> None:
        suite = (TOOLS / "suite.sh").read_text(encoding="utf-8")
        inside = (TOOLS / "inside.sh").read_text(encoding="utf-8")
        self.assertNotIn("> >(tee", suite)
        self.assertNotIn("command_identity", suite)
        for value in (
            'readonly bounded_runner="$workspace/tools/linux-docker/bounded_runner.py"',
            "--timeout-seconds 3600",
            "--drain-timeout-seconds 30",
            "--maximum-output-bytes 8388608",
            'stdout_path="$artifacts/$name.log"',
            'stderr_path="$artifacts/$name.stderr"',
            'receipt_path="$artifacts/$name.receipt.json"',
            '"${gate_environment_arguments[@]}"',
            'printf \'%s\\t%s\\t%s\\t%s\\n\' "$name" "$outcome" "$elapsed" "$receipt_sha"',
            'readonly gate_evidence_ledger="$artifacts/gate-evidence-ledger.ndjson"',
            '"$evidence" append-gate',
            '"$evidence" append-gate-skip',
        ):
            with self.subTest(value=value):
                self.assertIn(value, suite)
        self.assertIn("PYTHONDONTWRITEBYTECODE=1", inside)
        self.assertIn("PYTHONDONTWRITEBYTECODE \\", suite)


if __name__ == "__main__":
    unittest.main()
