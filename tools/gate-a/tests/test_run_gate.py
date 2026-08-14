"""Unit tests for the minimal Gate A driver.

Every cell executed here is synthetic (`sys.executable`, `/bin/sh`): no test
invokes a real gate cell and no test runs cargo, and each builds a throwaway
workspace, release directory and validation root under a temporary one. The
derivations below additionally READ the tracked tree; none of them writes it.
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import pathlib
import re
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

TOOLS = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(TOOLS / "gate-a"))

import run_gate  # noqa: E402

REAL_MANIFEST = TOOLS / "gate-a-candidate" / "phase-manifest.json"
SHELL = "/bin/sh"


def cell(identifier, argv, **extra):
    """Build one synthetic manifest cell."""

    return {
        "id": identifier,
        "cwd": extra.pop("cwd", "{workspace}"),
        "argv": argv,
        "env": extra.pop("env", {}),
        "stdout_equals": extra.pop("stdout_equals", None),
        **extra,
    }


def python_cell(identifier, source, **extra):
    return cell(identifier, ["{python}", "-c", source], **extra)


class Harness:
    """A disposable workspace, release directory, and receipt path."""

    def __init__(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name).resolve()
        self.workspace = self.root / "workspace"
        (self.workspace / "docs").mkdir(parents=True)
        (self.workspace / "docs" / "spec.md").write_text("spec\n", encoding="utf-8")
        (self.workspace / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        # The digest derives its file set from this file, so a workspace
        # without one is not a workspace the driver will hash.
        (self.workspace / ".gitignore").write_text(
            "/target\n**/target/\n.DS_Store\n", encoding="utf-8"
        )
        self.release = self.root / "release"
        self.release.mkdir()
        self.binary = self.release / "pmuxd"
        self.binary.write_bytes(b"not really a binary\n")
        self.binary.chmod(0o755)
        # The Gate A release directory is cargo's own `target/release`, so every
        # candidate ships the depinfo `pool_concurrency.rs` reads to prove it is
        # not stale. The driver refuses a directory that does not.
        self.depinfo = self.release / "pmuxd.d"
        self.depinfo.write_text("pmuxd: src/main.rs\n", encoding="utf-8")
        self.validation = self.root / "validation"
        self.receipt = self.root / "receipt.json"
        self.manifests = 0
        self.stderr = ""

    def close(self):
        self.temporary.cleanup()

    def manifest(self, phases, **extra):
        self.manifests += 1
        path = self.root / f"manifest-{self.manifests}.json"
        document = {"schema_version": 1, "phases": phases, **extra}
        path.write_text(json.dumps(document), encoding="utf-8")
        return path

    def drive(self, manifest, *extra):
        argv = [
            "--manifest", str(manifest),
            "--workspace", str(self.workspace),
            "--release-dir", str(self.release),
            "--validation-root", str(self.validation),
            "--receipt", str(self.receipt),
            *extra,
        ]  # fmt: skip
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = run_gate.main(argv)
        self.stderr = err.getvalue()
        return code, out.getvalue()

    def receipt_document(self):
        return json.loads(self.receipt.read_text(encoding="utf-8"))


class GateDriverTest(unittest.TestCase):
    def setUp(self):
        self.harness = Harness()
        self.addCleanup(self.harness.close)

    def cells_by_id(self, receipt):
        return {record["id"]: record for record in receipt["cells"]}

    # -- placeholders ----------------------------------------------------

    def test_placeholders_are_substituted_in_cwd_argv_and_env(self):
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    python_cell(
                        "echo_env",
                        "import os,sys;sys.stdout.write(os.environ['TARGET'])",
                        env={"TARGET": "{release}/pmuxd", "ROOT": "{validation}/x"},
                    )
                ]
            },
            phase_timeouts_seconds={"gate_x": 60},
        )
        code, _ = self.harness.drive(manifest)
        receipt = self.harness.receipt_document()
        record = receipt["cells"][0]
        self.assertEqual(code, 0, record)
        self.assertEqual(record["cwd"], str(self.harness.workspace))
        # The driver absolutises the parent but preserves the invoked NAME:
        # rustup shims dispatch on argv[0], so collapsing `cargo` -> `rustup`
        # breaks every cargo cell. See run_gate.py Replacements.__missing__.
        expected = str(
            pathlib.Path(sys.executable).parent.resolve()
            / pathlib.Path(sys.executable).name
        )
        self.assertEqual(record["argv"][0], expected)
        self.assertEqual(record["env"]["TARGET"], f"{self.harness.release}/pmuxd")
        self.assertEqual(record["env"]["ROOT"], f"{self.harness.validation}/x")
        self.assertEqual(record["stdout"]["head"], f"{self.harness.release}/pmuxd")
        self.assertEqual(record["timeout_seconds"], 60.0)
        self.assertEqual(receipt["workspace"], str(self.harness.workspace))

    def test_an_unresolved_placeholder_is_fatal_before_any_cell_runs(self):
        marker = self.harness.root / "must-not-exist"
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("writes", [SHELL, "-c", f"touch {marker}"]),
                    cell("unresolved", ["{nightly_cargo}", "--version"]),
                ]
            }
        )
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertFalse(marker.exists(), "a cell ran despite a fatal placeholder")
        self.assertFalse(self.harness.receipt.exists())

    def test_every_unresolved_placeholder_is_named_by_the_one_refusal(self):
        """`gate_b` needs four placeholders and used to name one per run.

        The phase is budgeted at four hours (`phase_timeouts_seconds.gate_b`), so
        an operator who learnt about `{cargo_fuzz}`, then `{nightly_cargo}`, then
        `{nightly_rustc}` spent three gate attempts on one missing `--tool`
        argument. Every fault in every selected cell, in one message.
        """

        marker = self.harness.root / "must-not-exist"
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("writes", [SHELL, "-c", f"touch {marker}"]),
                    cell(
                        "nightly",
                        ["{nightly_cargo}", "--version"],
                        env={"RUSTC": "{nightly_rustc}"},
                    ),  # fmt: skip
                    # Two in ONE value: scanning must not stop at the first.
                    cell("elsewhere", ["{no_such_tool}", "{other_tool}/{third_tool}"]),
                ]
            }
        )
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertFalse(marker.exists(), "a cell ran despite a fatal placeholder")
        self.assertFalse(self.harness.receipt.exists())
        self.assertIn("2 of 3 selected cells cannot be expanded", self.harness.stderr)
        for name in ("nightly_cargo", "nightly_rustc", "no_such_tool",
                     "other_tool", "third_tool"):  # fmt: skip
            with self.subTest(placeholder=name):
                self.assertIn(
                    f"placeholder {{{name}}} is unresolved", self.harness.stderr
                )

    def test_a_workspace_pinned_tool_resolves_without_being_on_path(self):
        """`{cargo_fuzz}` lives under the workspace, not on PATH.

        The gate pins the exact version (`cargo_fuzz_version` asserts
        `cargo-fuzz 0.13.2`), so `scripts/gate-a-fuzz.sh:114` and
        `tools/gate-a-candidate/candidate_envelope.py:1605` both read it from
        `.context/tools/cargo-fuzz/bin/cargo-fuzz` under the workspace. This
        driver was the third reader of that path and the only one that did not
        know it, so `--phase gate_b` -- six cells, four hours -- aborted with
        `placeholder {cargo_fuzz} is unresolved` on a host that had the pinned
        binary sitting right there.
        """

        base = {"workspace": str(self.harness.workspace)}
        beside = self.harness.workspace / run_gate.WORKSPACE_TOOLS["cargo_fuzz"]
        beside.parent.mkdir(parents=True)
        beside.write_text("#!/bin/sh\necho 'cargo-fuzz 0.13.2'\n", encoding="utf-8")
        decoy = self.harness.root / "cargo-fuzz"
        decoy.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        decoy.chmod(0o700)
        with mock.patch.object(run_gate.shutil, "which", return_value=None):
            with self.assertRaises(run_gate.UnresolvedPlaceholder):
                run_gate.Replacements(base, {})["cargo_fuzz"]  # not executable
            beside.chmod(0o700)
            self.assertEqual(run_gate.Replacements(base, {})["cargo_fuzz"], str(beside))
        # The version is PINNED, so the workspace copy outranks whatever the host
        # carries: otherwise `cargo_fuzz_version` and `scripts/gate-a-fuzz.sh`
        # (whose own default is this same path) would measure different binaries.
        with mock.patch.object(run_gate.shutil, "which", return_value=str(decoy)):
            self.assertEqual(run_gate.Replacements(base, {})["cargo_fuzz"], str(beside))
        # An explicit --tool still outranks the workspace copy.
        exe = pathlib.Path(sys.executable)
        override = run_gate.Replacements(base, {"cargo_fuzz": sys.executable})
        self.assertEqual(override["cargo_fuzz"], str(exe.parent.resolve() / exe.name))

    def test_an_unbalanced_placeholder_is_fatal(self):
        table = run_gate.Replacements({"workspace": "/w"}, {})
        with self.assertRaises(run_gate.GateDriverError):
            run_gate.expand("{workspace", table, "where")
        with self.assertRaises(run_gate.GateDriverError):
            run_gate.expand("a}b", table, "where")
        self.assertEqual(run_gate.expand("{workspace}/x", table, "w"), "/w/x")

    def test_tool_overrides_resolve_placeholders_and_derive_nightly_bin(self):
        table = run_gate.Replacements(
            {}, {"nightly_cargo": sys.executable, "cargo": sys.executable}
        )
        # Parent absolutised, final component preserved (rustup shims dispatch
        # on argv[0]).
        exe = pathlib.Path(sys.executable)
        expected = str(exe.parent.resolve() / exe.name)
        self.assertEqual(table["nightly_bin"], str(exe.parent.resolve()))
        self.assertEqual(table["nightly_cargo"], expected)
        self.assertEqual(table["cargo"], expected)
        self.assertEqual(sorted(table.tools), ["cargo", "nightly_bin", "nightly_cargo"])
        with self.assertRaises(run_gate.GateDriverError):
            table["shellcheck_missing"]

    # -- per-cell environment --------------------------------------------

    def test_per_cell_environment_is_applied_over_a_sanitized_base(self):
        source = (
            "import os,sys;"
            "sys.stdout.write('%s %s %s %s' % (sys.dont_write_bytecode,"
            " os.environ.get('PMUX_CELL'), os.environ.get('PMUX_LEAKED'),"
            " 'PATH' in os.environ))"
        )
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    python_cell(
                        "env_cell",
                        source,
                        env={"PYTHONDONTWRITEBYTECODE": "1", "PMUX_CELL": "value"},
                        stdout_equals="True value None True",
                    )
                ]
            }
        )
        with mock.patch.dict(os.environ, {"PMUX_LEAKED": "leaked"}):
            code, _ = self.harness.drive(manifest)
        record = self.harness.receipt_document()["cells"][0]
        self.assertEqual(code, 0, record)
        self.assertEqual(record["stdout"]["head"], "True value None True")
        self.assertTrue(record["assertions"][0]["ok"])
        self.assertEqual(record["env"]["PYTHONDONTWRITEBYTECODE"], "1")
        self.assertNotIn(
            "PMUX_LEAKED", self.harness.receipt_document()["cells"][0]["env"]
        )

    def real_manifest(self):
        return run_gate.load_manifest(REAL_MANIFEST.read_bytes(), str(REAL_MANIFEST))

    def real_cells(self):
        return [c for phase in self.real_manifest()["phases"].values() for c in phase]

    def test_every_validation_child_the_real_manifest_uses_is_prepared(self):
        """The child set is DERIVED from the manifest, not restated in the driver.

        `VALIDATION_CHILDREN` named three of the four. Twenty-one `gate_a`
        vendor cells set `CARGO_TARGET_DIR={validation}/cargo-target/<name>`,
        `docs/testing.md:391-396` documents that child alongside the other
        three, and `prepare_validation_root` -- whose docstring says it creates
        "the documented validation tree owner-private, or refuse" -- never
        looked at it. An operator who pre-created it 0755 got no refusal, and
        every vendor build wrote into a directory somebody else could read.
        """

        manifest = self.real_manifest()
        referenced = set()
        for cells in manifest["phases"].values():
            for entry in cells:
                for value in [entry["cwd"], *entry["argv"], *entry["env"].values()]:
                    for piece in str(value).split("{validation}")[1:]:
                        child = piece.lstrip("/").split("/", 1)[0]
                        if child and not child.startswith("{"):
                            referenced.add(child)
        self.assertTrue(
            referenced, "no cell names a validation child; derivation broken"
        )
        prepared = set(run_gate.validation_children(manifest))
        self.assertEqual(referenced - prepared, set())
        # And the documented floor is still in it, so a derivation that stops
        # matching cannot quietly shrink the tree back to nothing.
        self.assertEqual(set(run_gate.VALIDATION_CHILDREN) - prepared, set())

    def test_the_real_manifest_supplies_pythondontwritebytecode_per_cell(self):
        # This was `len(carriers) == 1`, and that predicate passed in exactly
        # the dangerous case and failed in the safe one: a new python cell added
        # WITHOUT the guard leaves the count at one and passes, while adding a
        # cell WITH the guard trips "the bytecode-residue guard moved". It was
        # forbidding the protection rather than requiring it. The rule
        # `scripts/gate-a-residue.sh` actually needs is derived from the cells --
        # every cell that runs python is guarded, and only those.
        cells = self.real_cells()
        python_cells = {c["id"] for c in cells if c["argv"][0] == "{python}"}
        carriers = {
            c["id"] for c in cells if c["env"].get("PYTHONDONTWRITEBYTECODE") == "1"
        }
        self.assertTrue(python_cells, "no python cell; the derivation is broken")
        self.assertEqual(python_cells, carriers)
        self.assertEqual(
            {c["id"] for c in cells if "PYTHONDONTWRITEBYTECODE" in c["env"]},
            carriers,
            "a cell sets the guard to something other than '1'",
        )
        self.assertEqual(
            sorted(self.real_manifest()["phases"]),
            ["gate_a", "gate_b", "gate_c", "gate_d", "gate_e", "gate_f", "residue"],
        )

    def test_the_mutation_gate_probes_every_profile_property_it_names(self):
        """A guard may not name a build property its predicate never reads.

        `scripts/gate-a-mutants.sh` refused any `[profile.mutants]` key beyond
        `{inherits = "dev", debug = false}` and said so with: "with
        debug-assertions or overflow-checks off, every assertion in the tree
        stops being a test." The predicate never checked either one, anywhere.
        `mutants` inherits `dev`, `Cargo.toml` declares no `[profile.dev]`, and
        both settings came from cargo defaults that nothing pinned -- so
        `[profile.dev] debug-assertions = false` would have left the guard green
        and compiled every `debug_assert!` in the tree out of the very run whose
        score was being published.

        The claim now lives in ONE array, `PROFILE_PROPERTIES`. The refusal
        message is built from that array rather than written out, and
        `crates/protocol/tests/mutation_profile.rs` reports back the properties
        it actually observed live so the guard can compare them. This test is
        the static half: the array, the probe's declared literals, and the
        properties the probe truly asserts on must be one set, and the guard's
        own text must spell none of them.

        It sits here rather than beside the probe because everything under
        `crates/service/tests/` runs once per mutant inside the mutation gate.
        """

        root = TOOLS.parent
        script = (root / "scripts" / "gate-a-mutants.sh").read_text(encoding="utf-8")

        def declared(name):
            match = re.search(rf"^readonly {name}=(.+)$", script, re.MULTILINE)
            self.assertIsNotNone(match, f"{name} is gone from the mutation gate")
            return match.group(1)

        properties = declared("PROFILE_PROPERTIES").strip("()").split()
        package = declared("PROFILE_PROBE_PACKAGE")
        target = declared("PROFILE_PROBE_TARGET")
        self.assertTrue(properties, "the mutation gate asserts no profile property")

        # The probe is a real test target of a real package, named by the guard.
        probe_path = root / "crates" / "protocol" / "tests" / f"{target}.rs"
        self.assertTrue(probe_path.is_file(), f"{probe_path} is not a test target")
        self.assertIn(
            f'name = "{package}"',
            (root / "crates" / "protocol" / "Cargo.toml").read_text(encoding="utf-8"),
        )
        probe = probe_path.read_text(encoding="utf-8")

        # Declared, and actually asserted on: a constant that is defined and
        # never reached is a property the report would silently omit.
        constants = dict(
            re.findall(r'^const ([A-Z_]+): &str = "([^"]+)";$', probe, re.MULTILINE)
        )
        reached = set(re.findall(r"live\.contains\(&([A-Z_]+)\)", probe))
        self.assertEqual(
            set(constants),
            reached,
            "every property the probe declares must be one it asserts on",
        )
        self.assertEqual(
            sorted(constants.values()),
            sorted(properties),
            f"{probe_path.relative_to(root)} observes {sorted(constants.values())} "
            f"but the gate asserts {sorted(properties)}; the message and the "
            f"measurement have come apart again",
        )

        # The guard's own body spells no property, so its refusal cannot name
        # one the probe does not cover -- it can only interpolate the array.
        guard = script.split("assert_profile_properties_are_live() {", 1)
        self.assertEqual(len(guard), 2, "the profile probe guard is gone")
        body = guard[1].split("\n}\n", 1)[0]
        for name in properties:
            with self.subTest(property=name):
                self.assertNotIn(
                    name,
                    body,
                    f"assert_profile_properties_are_live writes {name!r} out "
                    f"instead of interpolating PROFILE_PROPERTIES; a message "
                    f"written by hand is a message that can outrun its predicate",
                )
        self.assertIn(
            "run_logged profile-properties assert_profile_properties_are_live",
            script,
            "the profile probe is defined but never run",
        )

        # And it costs the score nothing: the probe is a `tests/` target, and no
        # mutation glob in this script reaches `tests/`, so the denominator is
        # the same set of mutants it was without it.
        globs = re.search(
            r"^readonly FULL_GLOBS=\(\n(.*?)^\)$", script, re.MULTILINE | re.DOTALL
        )
        self.assertIsNotNone(globs, "FULL_GLOBS is gone from the mutation gate")
        covered = [line.strip().strip("'") for line in globs.group(1).splitlines()]
        self.assertTrue(covered)
        probed = str(probe_path.relative_to(root))
        for glob in covered:
            with self.subTest(glob=glob):
                self.assertFalse(
                    probed.startswith(glob.split("*", 1)[0]),
                    f"{probed} falls inside the mutation glob {glob}, so adding "
                    f"it moved the score's denominator",
                )

    def test_both_shell_cells_cover_every_shell_script_in_the_source_tree(self):
        """The script set is DERIVED from the tree, not restated here.

        Both cells listed three of the seven `.sh` files in the tree. The three
        `tools/linux-docker` scripts that `docs/testing.md` names in Gate F and
        `tools/screen-corpus/per_binary_tests.sh`, added this week, were never
        parsed or linted by the gate that reports "shell_syntax ok".
        """

        root = TOOLS.parent
        scripts = sorted(
            str(path.relative_to(root))
            for path in run_gate.source_files(root)
            if path.suffix == ".sh"
        )
        self.assertTrue(scripts, "found no shell script; the derivation is broken")
        by_id = {c["id"]: c for c in self.real_cells()}
        for identifier in ("shell_syntax", "shellcheck"):
            with self.subTest(cell=identifier):
                named = sorted(
                    a for a in by_id[identifier]["argv"] if a.endswith(".sh")
                )
                self.assertEqual(named, scripts)

    def test_the_source_digest_is_exactly_what_the_repository_calls_source(self):
        """Two independent derivations of one set, asserted equal.

        The driver derives it from `.gitignore` because it must run against a
        `git archive` export with no repository. Git derives it from the index
        and the same file. They agree here, over 953 files, or this test says
        where they stopped agreeing -- which is the only way a hand-written
        `.gitignore` parser stays honest.

        Before this, the set was two hand-written lists: 10 gitignored
        `.DS_Store` files were IN the digest and every one of `evidence/`'s eight
        was out, including the model-attempt ledger a `gate_f` cell reads.
        """

        root = TOOLS.parent
        try:
            listed = subprocess.run(
                ["git", "-C", str(root), "ls-files", "--cached", "--others",
                 "--exclude-standard"],
                capture_output=True, text=True, check=True, timeout=60,
            )  # fmt: skip
        except (OSError, subprocess.SubprocessError) as error:
            self.skipTest(f"no usable git checkout to cross-check against: {error}")
        expected = {line for line in listed.stdout.splitlines() if line}
        self.assertGreater(len(expected), 500, "git listed almost nothing")
        derived = {str(p.relative_to(root)) for p in run_gate.source_files(root)}
        self.assertEqual(derived - expected, set(), "hashed, and not source")
        self.assertEqual(expected - derived, set(), "source, and not hashed")

    def test_the_digest_hashes_committed_evidence_and_not_ignored_noise(self):
        """The two halves of the defect, without needing a repository."""

        with tempfile.TemporaryDirectory() as raw:
            root = pathlib.Path(raw).resolve()
            (root / ".gitignore").write_text(
                "/target\n**/target/\n.DS_Store\n*.log\n.context/\n", encoding="utf-8"
            )
            for relative in (
                "evidence/model-attempt-ledger.ndjson",
                "crates/lib.rs",
                "crates/.DS_Store",
                "target/debug/binary",
                "crates/inner/target/artifact",
                "run.log",
                ".context/notes.md",
            ):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("x\n", encoding="utf-8")
            hashed = sorted(
                str(p.relative_to(root)) for p in run_gate.source_files(root)
            )
            self.assertEqual(
                hashed,
                [".gitignore", "crates/lib.rs", "evidence/model-attempt-ledger.ndjson"],
            )
            self.assertEqual(run_gate.source_digest(root)["file_count"], 3)

    def test_an_ignore_pattern_the_parser_does_not_implement_is_fatal(self):
        """Fail closed. A pattern nobody parsed is a file nobody decided about."""

        with tempfile.TemporaryDirectory() as raw:
            root = pathlib.Path(raw).resolve()
            for pattern in ("!keep-me", "build[0-9]/", "temp?.txt"):
                with self.subTest(pattern=pattern):
                    (root / ".gitignore").write_text(
                        f"/target\n{pattern}\n", encoding="utf-8"
                    )
                    with self.assertRaises(run_gate.GateDriverError):
                        run_gate.source_files(root)
            (root / ".gitignore").unlink()
            with self.assertRaises(run_gate.GateDriverError):
                run_gate.source_files(root)

    def test_the_testing_document_names_the_shell_scripts_the_gate_lints(self):
        """`docs/testing.md` §F is a copy of two cells, checked against them.

        The document listed SEVEN scripts where the manifest lints eight: it was
        missing `scripts/gate-a-mutants.sh`, which both cells have carried since
        the mutation gate landed. Nothing compared the two, so the document read
        as the gate's contents and was not.
        """

        block = (TOOLS.parent / "docs" / "testing.md").read_text(encoding="utf-8")
        by_id = {c["id"]: c for c in self.real_cells()}
        published = {
            "shell_syntax": "bash -n ",
            "shellcheck": "shellcheck ",
        }
        for identifier, opener in published.items():
            with self.subTest(cell=identifier):
                lines = [
                    line
                    for line in block.splitlines()
                    if line.startswith(opener) and ".sh" in line
                ]
                self.assertEqual(
                    len(lines), 1, f"docs/testing.md has {len(lines)} {opener!r} lines"
                )
                self.assertEqual(
                    sorted(a for a in lines[0].split() if a.endswith(".sh")),
                    sorted(a for a in by_id[identifier]["argv"] if a.endswith(".sh")),
                )

    def test_the_driver_readme_publishes_the_cell_census_the_manifest_has(self):
        """The README's first sentence is a census, so it is derived from one.

        It also published `gate_b`, *"passing 6/6 in 138 s"*, two lines under its
        own statement that `gate_b` is eight cells. A receipt for a phase that
        had six cells is not a receipt for the phase that has eight, and the
        number nobody recomputes is the one that goes stale first.
        """

        readme = (TOOLS / "gate-a" / "README.md").read_text(encoding="utf-8")
        census = re.search(r"\((\d+) cells: (.+?)\)", readme, re.DOTALL)
        self.assertIsNotNone(census, "the README no longer publishes a cell census")
        published = {
            name: int(count)
            for name, count in re.findall(r"`([a-z_]+)` (\d+)", census.group(2))
        }
        manifest = {
            phase: len(cells) for phase, cells in self.real_manifest()["phases"].items()
        }
        self.assertEqual(published, manifest)
        self.assertEqual(int(census.group(1)), sum(manifest.values()))

    def test_the_typescript_cell_runs_every_test_file_the_package_globs(self):
        """The file set is DERIVED from the directory, not restated here.

        `clients/typescript/package.json` runs `node --test tests/*.test.mjs`;
        the cell hand-listed three of them. PROVEN vacuous by mutation: with a
        deliberately failing `zz-mutation.test.mjs` added beside them the glob
        exited 1 (50 pass, 1 fail) and the gate cell exited 0 (50 pass, 0 fail).
        The cell cannot use the glob -- `argv` is literal by design, and `npm
        test` would build into `clients/typescript/dist`, which
        `scripts/gate-a-residue.sh:237` forbids -- so the list stays and this
        derivation is what makes it complete, exactly as
        `test_both_shell_cells_cover_every_shell_script_in_the_source_tree` does
        for the two list-based shell cells.
        """

        root = TOOLS.parent
        directory = root / "clients" / "typescript" / "tests"
        globbed = sorted(
            str(path.relative_to(root)) for path in directory.glob("*.test.mjs")
        )
        self.assertTrue(globbed, "found no TypeScript test file; derivation broken")
        cell = {c["id"]: c for c in self.real_cells()}["typescript_tests"]
        named = sorted(a for a in cell["argv"] if a.endswith(".test.mjs"))
        self.assertEqual(named, globbed)
        # The cell must name only test files: a listed non-test `.mjs` would run
        # as a test target and a listed directory would silently expand to
        # nothing.
        self.assertEqual([a for a in cell["argv"] if a.endswith(".mjs")], sorted(named))

    def gate_f_documented_directories(self):
        """Every unittest directory the Gate F section of docs/testing.md names.

        Parsed, not restated: the doc IS the requirement, and the one cell that
        went missing (`tools/linux-docker/tests`, docs/testing.md:765) went
        missing precisely because nothing compared the two.
        """

        text = (TOOLS.parent / "docs" / "testing.md").read_text(encoding="utf-8")
        _, _, after = text.partition("### F. Tooling and evidence-envelope self-tests")
        block = after.split("```")[1]
        return sorted(
            line.split("-s", 1)[1].split()[0]
            for line in block.splitlines()
            if "unittest discover -s" in line
        )

    def test_gate_f_runs_every_unittest_directory_the_doc_requires(self):
        documented = self.gate_f_documented_directories()
        self.assertTrue(
            documented, "parsed no Gate F command; the derivation is broken"
        )
        cells = self.real_manifest()["phases"]["gate_f"]
        run = sorted(
            c["argv"][c["argv"].index("-s") + 1]
            for c in cells
            if "unittest" in c["argv"] and "-s" in c["argv"]
        )
        self.assertEqual(run, documented)

    def test_the_real_manifest_cell_count_is_the_one_the_readme_publishes(self):
        """The published shape is READ from the README, not restated here.

        A tripwire on shrinkage: `tools/linux-docker/gate-a-manifest.json` is a
        projection of this manifest and has already been left behind by one
        trim. The counts used to be a literal dict in this file, which meant a
        cell added or removed needed the same number edited in two places and
        the README was the one that got forgotten -- so the tripwire measured
        the test's own memory, not the document a reader trusts.
        """

        published = (TOOLS / "gate-a" / "README.md").read_text(encoding="utf-8")
        line = next(ln for ln in published.splitlines() if " cells: `gate_a` " in ln)
        counts = {
            name: int(number) for name, number in re.findall(r"`(\w+)` (\d+)", line)
        }
        total = int(re.search(r"\((\d+) cells:", line).group(1))
        self.assertEqual(total, sum(counts.values()), "the README total is wrong")
        self.assertEqual(
            {k: len(v) for k, v in self.real_manifest()["phases"].items()}, counts
        )

    def test_every_reader_of_the_workspace_tool_root_derives_the_same_path(self):
        """No fourth reader, and no second spelling.

        `.context/tools/cargo-fuzz/bin/cargo-fuzz` was written out as a literal
        in three places, one of which did not know about it, and `--phase
        gate_b` aborted on a host that had the pinned binary sitting right
        there. `cargo-mutants` would have made it four readers of two literals,
        so the two Python readers now compute the path from the tool's own name
        and this derivation refuses any occurrence anywhere in the tree that
        does not match the same rule -- scripts and documentation included,
        because a shell default that drifts is exactly as wrong as a Python one.
        """

        expected = {
            run_gate.workspace_tool_path(name) for name in run_gate.WORKSPACE_TOOLS
        }
        self.assertEqual(set(run_gate.WORKSPACE_TOOLS.values()), expected)
        self.assertTrue(expected, "no pinned workspace tool; the derivation is broken")
        # The install roots are the SECOND derived spelling, and admitting them
        # is not a loophole: `docs/testing.md` documents the
        # `cargo install --root <dir>` that produces each binary, and `<dir>` is
        # by construction the parent of `workspace_tool_path`. Both come from
        # `workspace_tool_root`, so the install command and the lookup cannot
        # name different directories. Anything neither function produces still
        # fails below.
        roots = {
            run_gate.workspace_tool_root(name) for name in run_gate.WORKSPACE_TOOLS
        }
        for name in run_gate.WORKSPACE_TOOLS:
            self.assertTrue(
                run_gate.workspace_tool_path(name).startswith(
                    run_gate.workspace_tool_root(name) + "/"
                ),
                f"{name}: the install root must be a prefix of the binary path, "
                f"or the documented install puts it somewhere the gate does not look",
            )
        # Every pinned tool must also be resolvable from PATH by a plain name,
        # so a host without the workspace copy gets a NAMED refusal from the
        # version cell rather than an unresolved placeholder.
        for name in run_gate.WORKSPACE_TOOLS:
            self.assertIn(name, run_gate.TOOL_EXECUTABLES)

        root = TOOLS.parent
        skip = set(run_gate.SOURCE_SKIP) | {".context", ".git", "target"}
        occurrence = re.compile(r"\.context/tools/[A-Za-z0-9._/-]*")
        readers = {}
        for path in sorted(root.rglob("*")):
            if not path.is_file() or skip & set(path.relative_to(root).parts):
                continue
            if path.suffix not in {".py", ".sh", ".md", ".json", ".rs", ".toml"}:
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            for found in occurrence.findall(text):
                # `.../` with nothing after it is prose naming the root itself,
                # e.g. the README's `.context/tools/<binary>/bin/<binary>`: the
                # template placeholder stops the match, and the rule it states
                # is the one under test rather than a competing spelling.
                readers.setdefault(found.rstrip("/"), set()).add(
                    str(path.relative_to(root))
                )
        self.assertTrue(readers, "found no reader; the derivation is broken")
        allowed = expected | roots | {run_gate.WORKSPACE_TOOLS_ROOT}
        for spelling, files in sorted(readers.items()):
            with self.subTest(spelling=spelling):
                self.assertIn(
                    spelling,
                    allowed,
                    f"{sorted(files)} spells the pinned-tool path a way "
                    f"`workspace_tool_path` does not produce; the readers of it "
                    f"must agree or one of them silently reads nothing",
                )

    def test_every_statement_of_the_bug_class_counter_spells_the_same_ordinal(self):
        """The bug-class counter is ONE number restated in four Rust files.

        `crates/service/tests/agent_resource.rs` has said since instance
        twenty-nine that "a count restated in three files is a count that is
        wrong in at least one of them" -- and then left the three to be kept in
        step by hand, which is that sentence one level up. It went wrong again:
        the brief for instance thirty-two opened by asserting every site read
        "thirty-two" when every one read "thirty-one".

        THE TRUTH IS THE DOCUMENT, not this test: the ordinal spelled by the
        LAST `### … THE BUG CLASS, instance …` heading in
        `docs/current-state.md`. A HEADING and not a mention -- the section that
        describes this derivation quotes the phrase in prose, and matching
        anywhere took the ordinal from that sentence instead.

        It lives here rather than beside the counter because every test target
        of `pseudomux-service` runs once per mutant inside
        `scripts/gate-a-mutants.sh`, in a copy of the tree `cargo-mutants`
        makes, and whether that copy carries `docs/` is a claim about a tool
        nobody had checked -- with an aborted baseline of the 88-minute
        `gate_b` cell as the price of guessing wrong.

        `FLOOR` names the files known to carry the sentence, so a phrasing
        change that makes the scan stop finding one fails here rather than
        reporting agreement over a smaller set. It earned its keep on the first
        run: the scan found `v1.rs` and lost the other three, because the
        sentence ends "times." at one site and "times:" at two more.
        """

        phrase, heading = "has now found ", "THE BUG CLASS, instance "
        floor = {
            "crates/protocol/src/v1.rs",
            "crates/service/src/pool/mod.rs",
            "crates/service/tests/agent_resource.rs",
        }
        root = TOOLS.parent
        document = (root / "docs" / "current-state.md").read_text(encoding="utf-8")
        headings = [
            line.split(heading, 1)[1].split()[0]
            for line in document.splitlines()
            if line.startswith("###") and heading in line
        ]
        self.assertTrue(headings, "docs/current-state.md declares no instance")
        published = headings[-1]

        statements = {}
        for area in ("crates", "bin"):
            for path in sorted((root / area).rglob("*.rs")):
                # Flattened, because `rustfmt` breaks the sentence across doc
                # comment lines in three of the four places it appears.
                flat = " ".join(
                    line.lstrip().lstrip("/!").strip()
                    for line in path.read_text(encoding="utf-8").splitlines()
                )
                for piece in flat.split(phrase)[1:]:
                    words = [word.rstrip(".,:;") for word in piece.split(None, 2)[:2]]
                    if len(words) == 2 and words[1] == "times":
                        name = str(path.relative_to(root))
                        statements.setdefault(name, set()).add(words[0])

        self.assertLessEqual(
            floor,
            set(statements),
            f"the counter scan lost a known site, so agreement over "
            f"{sorted(statements)} says nothing",
        )
        for name, ordinals in sorted(statements.items()):
            with self.subTest(file=name):
                self.assertEqual(
                    ordinals,
                    {published},
                    f"{name} spells the bug-class counter differently from the "
                    f"last `{heading}` heading in docs/current-state.md, which "
                    f"is {published}",
                )

    # The budget figures `evidence/README.md` is forbidden to write, as the
    # shapes they were actually written in. Each entry is a VERBATIM sentence
    # from the revision that shipped the falsified budget, so the scan below is
    # known to catch the defect it exists for rather than merely to pass.
    ROTTED_BUDGET_CLAIMS = (
        "- 39 records, global ordinals **5 through 43**",
        "the 14 reservation records carry `schema` =",
        "**47 of the authorized 100 global attempts are consumed; 53 remain.**",
        "the ledger's own last ordinal (43) plus the four detached reservations",
        "    # 39 records, 5 through 43   ->   43 + 4 detached = 47 consumed, 53 remain",
    )
    # Every shape a count of THIS file's current extent can take. Closed
    # historical ranges -- "Ordinals 5-29", "all numbered 31", "17 attempt rows
    # over global ordinals 31 through 43" -- are deliberately NOT matched: they
    # are statements about the past, and nothing appends to the past.
    BUDGET_CLAIM_SHAPES = (
        r"\b\d+\s+(?:\w+\s+)?records\b",
        r"\b\d+\s+remain(?:ing)?\b",
        r"\b\d+\s+of\s+(?:the\s+)?(?:authorized\s+)?\d+\b",
        r"\b\d+\s+consumed\b",
        r"\blast\s+ordinal\s*\(\s*\d+",
    )

    def test_the_evidence_readme_states_no_budget_figure_and_its_command_derives_one(
        self,
    ):
        """The global attempt budget must be asked of the file, never read here.

        `evidence/README.md` published "47 of the authorized 100 global attempts
        are consumed; 53 remain" while its own ledger had reached ordinal 81 --
        85 consumed and 15 left, a denominator wrong by 3.5x against a ceiling
        that cannot be raised. The same document, two paragraphs above, refuses
        to pin a SHA-256 because "a stale digest that looks authoritative is
        worse than none", and then pinned a budget.

        So the fix is not a corrected number. It is that the document states no
        number at all: `phase0.py budget` computes every field from the file on
        the call. This test enforces both halves -- that no budget figure has
        crept back into the prose, and that the command the prose tells you to
        run still exists, still runs, and still agrees with a count taken here.

        It reads the tracked tree and spawns one subprocess; it writes nothing.
        """

        root = TOOLS.parent
        readme_path = root / "evidence" / "README.md"
        ledger = root / "evidence" / "model-attempt-ledger.ndjson"
        readme = readme_path.read_text(encoding="utf-8")

        for pattern in self.BUDGET_CLAIM_SHAPES:
            with self.subTest(shape=pattern):
                found = re.compile(pattern).findall(readme)
                self.assertEqual(
                    found,
                    [],
                    f"{readme_path.relative_to(root)} states a ledger figure "
                    f"({found}) that the next reservation makes false; delete it "
                    f"and let `phase0.py budget` print it",
                )
                # A shape is only worth running if it would have caught at least
                # one sentence this file actually published.
                self.assertTrue(
                    any(
                        re.search(pattern, claim) for claim in self.ROTTED_BUDGET_CLAIMS
                    ),
                    f"no known stale claim matches {pattern}; the shape covers "
                    f"nothing that has ever gone wrong here",
                )
        for claim in self.ROTTED_BUDGET_CLAIMS:
            with self.subTest(claim=claim):
                self.assertTrue(
                    any(re.search(shape, claim) for shape in self.BUDGET_CLAIM_SHAPES),
                    f"the scan no longer catches {claim!r}, which is a sentence "
                    f"this file actually published",
                )

        # The command is taken FROM the document, so a README that starts
        # advertising a tool that does not exist fails here rather than sending
        # the next reader to recount by hand.
        printed = [
            line.strip()
            for line in readme.splitlines()
            if "phase0.py budget" in line and line.startswith("    ")
        ]
        self.assertEqual(
            len(printed),
            1,
            "evidence/README.md must print exactly one recount command",
        )
        argv = printed[0].split()
        self.assertEqual(argv[0], "python3")
        completed = subprocess.run(
            [sys.executable, *argv[1:]],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(
            completed.returncode,
            0,
            f"the recount command the README prints failed: {completed.stderr}",
        )
        budget = json.loads(completed.stdout)

        # Counted here, not imported: the line count needs no schema at all, and
        # the last ordinal is read through the ONE spelling tuple rather than a
        # second copy of it -- a second copy is what reads the budget cheap.
        sys.path.insert(0, str(TOOLS / "phase0"))
        try:
            from phase0_lib import ORDINAL_SPELLINGS
        finally:
            sys.path.pop(0)
        lines = [
            line for line in ledger.read_text(encoding="utf-8").splitlines() if line
        ]
        last = json.loads(lines[-1])
        last_ordinal = next(
            last[key] for key in ORDINAL_SPELLINGS if isinstance(last.get(key), int)
        )
        self.assertEqual(budget["records"], len(lines))
        self.assertEqual(budget["last_ordinal"], last_ordinal)
        self.assertEqual(
            budget["records"], budget["last_ordinal"] - budget["first_ordinal"] + 1
        )
        self.assertEqual(
            budget["consumed"], budget["last_ordinal"] + budget["detached"]
        )
        self.assertEqual(budget["remaining"], budget["ceiling"] - budget["consumed"])
        self.assertGreaterEqual(budget["remaining"], 0)

    def test_every_real_manifest_cell_expands_with_the_documented_placeholders(self):
        manifest = run_gate.load_manifest(
            REAL_MANIFEST.read_bytes(), str(REAL_MANIFEST)
        )
        # DERIVED from the driver's own tables rather than restated, so a
        # manifest cell that starts using a newly pinned tool placeholder is
        # expanded here the day it lands instead of erroring on a name this
        # fixture was never told about.
        table = run_gate.Replacements(
            {"workspace": "/w", "release": "/r", "validation": "/v"},
            {
                name: sys.executable
                for name in (
                    *run_gate.TOOL_EXECUTABLES,
                    "nightly_cargo",
                    "nightly_rustc",
                )
                if name != "python"
            },  # fmt: skip
        )
        for phase, cells in manifest["phases"].items():
            for index, raw in enumerate(cells):
                plan = run_gate.expand_cell(raw, phase, index, table)
                joined = " ".join([plan["cwd"], *plan["argv"], *plan["env"].values()])
                self.assertNotIn("{", joined)
                self.assertNotIn("}", joined)

    # -- failure recording ------------------------------------------------

    def test_a_failing_cell_is_recorded_and_the_run_continues(self):
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("first", [SHELL, "-c", "echo one"]),
                    cell("boom", [SHELL, "-c", "echo bad >&2; exit 3"]),
                    cell("third", [SHELL, "-c", "echo three"]),
                ]
            }
        )
        code, output = self.harness.drive(manifest)
        receipt = self.harness.receipt_document()
        records = self.cells_by_id(receipt)
        self.assertEqual(code, 1)
        self.assertEqual(
            [r["id"] for r in receipt["cells"]], ["first", "boom", "third"]
        )
        self.assertEqual(receipt["summary"]["failed"], 1)
        self.assertEqual(receipt["summary"]["failed_ids"], ["boom"])
        self.assertEqual(receipt["summary"]["passed"], 2)
        self.assertEqual(receipt["summary"]["executed"], 3)
        self.assertEqual(records["boom"]["exit_status"]["code"], 3)
        self.assertEqual(records["boom"]["failures"], ["exit_status"])
        self.assertEqual(records["boom"]["stderr"]["head"], "bad\n")
        self.assertTrue(records["third"]["passed"])
        self.assertIn("FAIL", output)
        self.assertIn("boom", output)

    def test_continue_on_failure_is_the_default_and_may_be_stated(self):
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("boom", [SHELL, "-c", "exit 1"]),
                    cell("next", ["/bin/echo"]),
                ]
            }
        )
        code, _ = self.harness.drive(manifest, "--continue-on-failure")
        self.assertEqual(code, 1)
        self.assertEqual(self.harness.receipt_document()["summary"]["executed"], 2)

    def test_stop_on_failure_is_an_opt_in(self):
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("boom", [SHELL, "-c", "exit 1"]),
                    cell("next", ["/bin/echo"]),
                ]
            }
        )
        code, _ = self.harness.drive(manifest, "--stop-on-failure")
        receipt = self.harness.receipt_document()
        self.assertEqual(code, 1)
        self.assertEqual(receipt["summary"]["planned"], 2)
        self.assertEqual(receipt["summary"]["executed"], 1)

    # -- bounded time and bounded output ----------------------------------

    def test_a_timeout_is_bounded_and_kills_the_process_group(self):
        marker = self.harness.root / "escaped"
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell(
                        "hangs", [SHELL, "-c", f"(sleep 3; touch {marker}) & sleep 300"]
                    ),
                    cell("after", [SHELL, "-c", "echo after"]),
                ]
            }
        )
        started = time.monotonic()
        code, _ = self.harness.drive(manifest, "--cell-timeout-seconds", "1")
        elapsed = time.monotonic() - started
        records = self.cells_by_id(self.harness.receipt_document())
        self.assertEqual(code, 1)
        self.assertLess(elapsed, 30.0)
        self.assertTrue(records["hangs"]["exit_status"]["timed_out"])
        self.assertIn("timeout", records["hangs"]["failures"])
        self.assertLess(records["hangs"]["wall_ms"], 15000)
        self.assertTrue(records["after"]["passed"], "the run continued past a timeout")
        time.sleep(4)
        self.assertFalse(marker.exists(), "a descendant survived the group kill")

    def test_output_beyond_the_excerpt_is_truncated_but_fully_hashed(self):
        size = 20000
        source = f"import sys;sys.stdout.write('a' * {size})"
        manifest = self.harness.manifest(
            {"gate_x": [python_cell("loud", source)]},
            max_command_output_bytes=1024 * 1024,
        )
        code, _ = self.harness.drive(manifest)
        record = self.harness.receipt_document()["cells"][0]
        self.assertEqual(code, 0)
        self.assertEqual(record["stdout"]["bytes"], size)
        self.assertEqual(
            record["stdout"]["sha256"], hashlib.sha256(b"a" * size).hexdigest()
        )
        self.assertEqual(len(record["stdout"]["head"]), run_gate.EXCERPT_BYTES)
        self.assertEqual(len(record["stdout"]["tail"]), run_gate.EXCERPT_BYTES)
        self.assertFalse(record["stdout"]["over_limit"])
        self.assertLess(self.harness.receipt.stat().st_size, 64 * 1024)

    def test_output_past_the_manifest_cap_is_recorded_and_the_cell_is_stopped(self):
        source = "import sys\nwhile True:\n    sys.stdout.write('x' * 4096)\n"
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    python_cell("flood", source),
                    cell("after", [SHELL, "-c", "echo after"]),
                ]
            },
            max_command_output_bytes=8192,
        )
        started = time.monotonic()
        code, _ = self.harness.drive(manifest, "--cell-timeout-seconds", "120")
        elapsed = time.monotonic() - started
        records = self.cells_by_id(self.harness.receipt_document())
        self.assertEqual(code, 1)
        self.assertLess(elapsed, 60.0, "a flooding cell was not stopped promptly")
        self.assertTrue(records["flood"]["stdout"]["over_limit"])
        self.assertIn("output_limit", records["flood"]["failures"])
        self.assertNotIn("timeout", records["flood"]["failures"])
        self.assertLessEqual(len(records["flood"]["stdout"]["head"]), 4096)
        self.assertLessEqual(len(records["flood"]["stdout"]["tail"]), 4096)
        self.assertTrue(records["after"]["passed"])

    # -- manifest assertions ----------------------------------------------

    def test_stdout_equals_mismatch_is_recorded_as_a_failure(self):
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("match", ["/bin/echo", "yes"], stdout_equals="yes\n"),
                    cell("mismatch", ["/bin/echo", "no"], stdout_equals="yes\n"),
                ]
            }
        )
        code, _ = self.harness.drive(manifest)
        records = self.cells_by_id(self.harness.receipt_document())
        self.assertEqual(code, 1)
        self.assertTrue(records["match"]["passed"])
        self.assertTrue(records["match"]["assertions"][0]["ok"])
        self.assertFalse(records["mismatch"]["passed"])
        self.assertEqual(records["mismatch"]["exit_status"]["code"], 0)
        self.assertEqual(records["mismatch"]["failures"], ["assertion:stdout_equals"])
        assertion = records["mismatch"]["assertions"][0]
        self.assertEqual(assertion["kind"], "stdout_equals")
        self.assertNotEqual(assertion["expected_sha256"], assertion["observed_sha256"])

    def test_stdout_sha256_line_assertion_is_recorded_both_ways(self):
        digest = hashlib.sha256(b"x").hexdigest()
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("good", ["/bin/echo", digest], stdout_sha256_line=True),
                    cell("bad", ["/bin/echo", "not-a-digest"], stdout_sha256_line=True),
                ]
            }
        )
        code, _ = self.harness.drive(manifest)
        records = self.cells_by_id(self.harness.receipt_document())
        self.assertEqual(code, 1)
        self.assertTrue(records["good"]["assertions"][0]["ok"])
        self.assertFalse(records["bad"]["assertions"][0]["ok"])
        self.assertEqual(records["bad"]["failures"], ["assertion:stdout_sha256_line"])

    # -- receipt ----------------------------------------------------------

    def test_the_receipt_validates_against_its_own_schema(self):
        manifest = self.harness.manifest({"gate_x": [cell("ok", ["/bin/echo", "hi"])]})
        self.harness.drive(manifest)
        receipt = self.harness.receipt_document()
        self.assertIs(run_gate.validate_receipt(receipt), receipt)
        for mutate in (
            lambda r: r.pop("source_digest_after"),
            lambda r: r.update(schema_version=99),
            lambda r: r["cells"][0].pop("failures"),
            lambda r: r["cells"][0]["stdout"].pop("sha256"),
            lambda r: r["cells"][0].update(passed="yes"),
        ):
            broken = json.loads(self.harness.receipt.read_text(encoding="utf-8"))
            mutate(broken)
            with self.assertRaises(run_gate.GateDriverError):
                run_gate.validate_receipt(broken)

    def test_the_receipt_records_host_tool_release_and_owner_only_mode(self):
        manifest = self.harness.manifest({"gate_x": [cell("ok", ["/bin/echo", "hi"])]})
        code, output = self.harness.drive(manifest)
        receipt = self.harness.receipt_document()
        self.assertEqual(code, 0)
        self.assertIn(str(self.harness.receipt), output)
        self.assertIn("PASS 1/1 cells passed", output)
        self.assertEqual(receipt["host"]["os"], os.uname().sysname)
        self.assertEqual(receipt["host"]["arch"], os.uname().machine)
        self.assertEqual(receipt["host"]["kernel"], os.uname().release)
        self.assertTrue(receipt["tools"]["python"]["version"].startswith("Python"))
        for name in ("cargo", "node", "python", "ruff", "rustc"):
            self.assertIn(name, receipt["tools"])
        binaries = receipt["release"]["binaries"]
        # Every regular file, not just the executables: the depinfo cargo writes
        # beside each candidate is part of what the E2E harnesses read.
        self.assertEqual([entry["name"] for entry in binaries], ["pmuxd", "pmuxd.d"])
        self.assertEqual(
            binaries[0]["sha256"],
            hashlib.sha256(self.harness.binary.read_bytes()).hexdigest(),
        )
        self.assertEqual(binaries[0]["mode"], "0755")
        self.assertEqual(receipt["release"]["path"], str(self.harness.release))
        self.assertEqual(stat.S_IMODE(self.harness.receipt.stat().st_mode), 0o600)
        self.assertEqual(receipt["manifest"]["sha256"],
                         hashlib.sha256(manifest.read_bytes()).hexdigest())  # fmt: skip

    def test_source_digest_before_and_after_are_both_recorded(self):
        manifest = self.harness.manifest({"gate_x": [cell("ok", ["/bin/echo", "hi"])]})
        self.harness.drive(manifest)
        receipt = self.harness.receipt_document()
        before, after = receipt["source_digest_before"], receipt["source_digest_after"]
        self.assertEqual(before["algorithm"], run_gate.SOURCE_ALGORITHM)
        self.assertEqual(before["sha256"], after["sha256"])
        self.assertEqual(before["file_count"], 3)
        self.assertTrue(receipt["source_unchanged"])

    def test_a_cell_that_mutates_the_source_tree_changes_the_after_digest(self):
        written = self.harness.workspace / "docs" / "residue.md"
        manifest = self.harness.manifest(
            {"gate_x": [cell("writes", [SHELL, "-c", f"echo residue > {written}"])]}
        )
        self.harness.drive(manifest)
        receipt = self.harness.receipt_document()
        self.assertNotEqual(
            receipt["source_digest_before"]["sha256"],
            receipt["source_digest_after"]["sha256"],
        )
        self.assertFalse(receipt["source_unchanged"])
        self.assertEqual(receipt["source_digest_after"]["file_count"], 4)

    # -- selection and malformed input -------------------------------------

    def test_phase_selection_runs_only_the_named_phases_in_manifest_order(self):
        manifest = self.harness.manifest(
            {
                "gate_a": [cell("a1", ["/bin/echo", "a"])],
                "gate_b": [cell("b1", ["/bin/echo", "b"])],
                "gate_c": [cell("c1", ["/bin/echo", "c"])],
            }
        )
        code, _ = self.harness.drive(manifest, "--phase", "gate_c", "--phase", "gate_a")
        receipt = self.harness.receipt_document()
        self.assertEqual(code, 0)
        self.assertEqual(receipt["phases"], ["gate_a", "gate_c"])
        self.assertEqual([r["id"] for r in receipt["cells"]], ["a1", "c1"])

    def test_an_unknown_phase_is_fatal(self):
        manifest = self.harness.manifest({"gate_a": [cell("a1", ["/bin/echo"])]})
        code, _ = self.harness.drive(manifest, "--phase", "gate_z")
        self.assertEqual(code, 2)
        self.assertFalse(self.harness.receipt.exists())

    def test_a_malformed_manifest_is_fatal(self):
        broken = [
            b"{",
            b"{}",
            b'{"phases": {}}',
            b'{"phases": {"gate_a": []}}',
            b'{"phases": {"gate_a": [{"id": "x", "cwd": "{workspace}", "env": {}}]}}',
            b'{"phases": {"gate_a": [{"id": "x", "cwd": 1, "argv": ["a"], "env": {},'
            b' "stdout_equals": null}]}}',
            b'{"phases": {"gate_a": [{"id": "x", "cwd": "w", "argv": [], "env": {},'
            b' "stdout_equals": null}]}}',
            b'{"phases": {"gate_a": [{"id": "x", "cwd": "w", "argv": [3], "env": {},'
            b' "stdout_equals": null}]}}',
            b'{"phases": {"gate_a": [{"id": "x", "cwd": "w", "argv": ["a"],'
            b' "env": {"A": 1}, "stdout_equals": null}]}}',
        ]
        for payload in broken:
            with self.assertRaises(run_gate.GateDriverError, msg=payload):
                run_gate.load_manifest(payload, "manifest")
        path = self.harness.root / "broken.json"
        path.write_bytes(b'{"phases": {"gate_a": []}}')
        code, _ = self.harness.drive(path)
        self.assertEqual(code, 2)

    def test_a_missing_release_directory_is_fatal(self):
        manifest = self.harness.manifest({"gate_x": [cell("ok", ["/bin/echo"])]})
        self.harness.release.rename(self.harness.root / "moved")
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertFalse(self.harness.receipt.exists())

    def test_the_documented_validation_children_are_created_owner_private(self):
        manifest = self.harness.manifest({"gate_x": [cell("ok", ["/bin/echo"])]})
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 0)
        for name in ("", *run_gate.VALIDATION_CHILDREN):
            path = self.harness.validation / name if name else self.harness.validation
            with self.subTest(child=name or "<root>"):
                self.assertTrue(path.is_dir(), path)
                self.assertEqual(stat.S_IMODE(path.lstat().st_mode), 0o700)

    def test_a_group_readable_validation_child_is_fatal_before_any_cell_runs(self):
        # The whole point: an operator who pre-created the tree under an
        # ordinary umask used to get four red cells that looked like product
        # failures (`typescript_stage_prepare`, `typescript_stage_verify`,
        # `typescript_tests`, `release_full_stack_e2e`). One refusal, naming
        # the directory, before anything runs.
        stage = self.harness.validation / "typescript-dist"
        stage.mkdir(parents=True)
        stage.chmod(0o755)
        marker = self.harness.root / "ran"
        manifest = self.harness.manifest(
            {"gate_x": [cell("touch", ["/usr/bin/touch", str(marker)])]}
        )
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertFalse(marker.exists(), "a cell ran despite the refusal")
        self.assertFalse(self.harness.receipt.exists())

    def test_a_group_readable_derived_validation_child_is_fatal_too(self):
        """The refusal covers the children the MANIFEST names, not a literal list.

        `cargo-target` is not in `VALIDATION_CHILDREN` and never was, yet
        twenty-one real `gate_a` cells build into it. A pre-created 0755
        `cargo-target` used to pass this guard silently and then take every
        vendor build with it, which is the precise case the guard's docstring
        says it exists to prevent.
        """

        # The ROOT is created owner-private on purpose: a 0755 root would be
        # refused first and this test would pass without ever reaching the
        # derived child, which is the vacuity it exists to avoid.
        self.harness.validation.mkdir(parents=True)
        self.harness.validation.chmod(0o700)
        target = self.harness.validation / "cargo-target"
        target.mkdir()
        target.chmod(0o755)
        marker = self.harness.root / "ran"
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell(
                        "builds",
                        ["/usr/bin/touch", str(marker)],
                        env={"CARGO_TARGET_DIR": "{validation}/cargo-target/vendor"},
                    )
                ]
            }
        )
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertIn("cargo-target", self.harness.stderr)
        self.assertFalse(marker.exists(), "a cell ran despite the refusal")
        self.assertFalse(self.harness.receipt.exists())

    def test_a_release_directory_without_cargo_depinfo_is_fatal(self):
        # `pool_concurrency.rs:237` refuses to run without `<binary>.d`, so a
        # release directory assembled by copying only the executables fails all
        # nineteen pool tests six minutes into `release_full_stack_e2e`. One
        # refusal, before anything runs, naming the file.
        self.harness.depinfo.unlink()
        marker = self.harness.root / "ran"
        manifest = self.harness.manifest(
            {"gate_x": [cell("touch", ["/usr/bin/touch", str(marker)])]}
        )
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertFalse(marker.exists(), "a cell ran despite the refusal")
        self.assertFalse(self.harness.receipt.exists())

    # -- release freshness -----------------------------------------------

    def _stale_binary(self, name, source_name):
        """One release executable whose cargo depinfo names a NEWER source."""

        source = self.harness.workspace / source_name
        source.write_text("fn main() {}\n", encoding="utf-8")
        binary = self.harness.release / name
        binary.write_bytes(b"not really a binary\n")
        binary.chmod(0o755)
        binary.with_suffix(".d").write_text(f"{binary}: {source}\n", encoding="utf-8")
        older = source.lstat().st_mtime - 60
        os.utime(binary, (older, older))
        return binary, source

    def test_a_release_binary_older_than_its_own_sources_is_fatal(self):
        # The measured case: one stale `target/release` produced three red
        # cells across two phases -- nine MCP tools where the source defines
        # thirteen, `unexpected argument '--agent-version'`, and fourteen pool
        # failures six minutes into `release_full_stack_e2e` -- and not one of
        # them named a stale binary. One refusal, before anything runs.
        binary, source = self._stale_binary("pmux-mcp", "tools.rs")
        marker = self.harness.root / "ran"
        manifest = self.harness.manifest(
            {"gate_x": [cell("touch", ["/usr/bin/touch", str(marker)])]}
        )
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertIn(str(binary), self.harness.stderr)
        self.assertIn(str(source), self.harness.stderr)
        self.assertIn("cargo build --locked --release --workspace", self.harness.stderr)
        self.assertFalse(marker.exists(), "a cell ran despite the refusal")
        self.assertFalse(self.harness.receipt.exists())

    def test_every_stale_release_binary_is_named_by_the_one_refusal(self):
        # A gate attempt costs two hours on this host, so a refusal that names
        # the first stale binary and stops costs one attempt per binary.
        first, _ = self._stale_binary("pmux-mcp", "tools.rs")
        second, _ = self._stale_binary("pmux", "cli.rs")
        manifest = self.harness.manifest({"gate_x": [cell("noop", ["/bin/echo", "x"])]})
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertIn(str(first), self.harness.stderr)
        self.assertIn(str(second), self.harness.stderr)
        self.assertIn("2 release binaries are older", self.harness.stderr)

    def test_a_source_cargo_listed_and_that_no_longer_exists_is_not_stale(self):
        # A source DELETED since the build has no mtime to compare against.
        # Refusing there would wedge the gate on a tree the next compile
        # answers for anyway -- and would make every fixture in this file that
        # writes a depinfo naming a path it never created fail as "stale".
        binary, source = self._stale_binary("pmux-hook", "hook.rs")
        source.unlink()
        manifest = self.harness.manifest({"gate_x": [cell("noop", ["/bin/echo", "x"])]})
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 0)
        self.assertNotIn(str(binary), self.harness.stderr)

    def test_a_depinfo_that_lists_no_source_is_fatal(self):
        # `pool_concurrency.rs:253` refuses an empty dependency set for the
        # same reason: "nothing is stale" over an empty set says nothing, and
        # a truncated `.d` would silently disable this whole check.
        self.harness.depinfo.write_text("pmuxd:\n", encoding="utf-8")
        marker = self.harness.root / "ran"
        manifest = self.harness.manifest(
            {"gate_x": [cell("touch", ["/usr/bin/touch", str(marker)])]}
        )
        code, _ = self.harness.drive(manifest)
        self.assertEqual(code, 2)
        self.assertIn(str(self.harness.depinfo), self.harness.stderr)
        self.assertFalse(marker.exists(), "a cell ran despite the refusal")

    def test_a_cell_whose_program_does_not_exist_is_recorded_not_fatal(self):
        missing = self.harness.root / "no-such-program"
        manifest = self.harness.manifest(
            {
                "gate_x": [
                    cell("absent", [str(missing)]),
                    cell("after", ["/bin/echo", "after"]),
                ]
            }
        )
        code, _ = self.harness.drive(manifest)
        records = self.cells_by_id(self.harness.receipt_document())
        self.assertEqual(code, 1)
        self.assertEqual(records["absent"]["failures"], ["spawn"])
        self.assertIsNone(records["absent"]["exit_status"]["code"])
        self.assertTrue(records["after"]["passed"])


if __name__ == "__main__":
    unittest.main()


class ToolNamePreservationTest(unittest.TestCase):
    """A rustup shim dispatches on argv[0]; resolving the symlink breaks it.

    Regression for the Gate A capture of 2026-07-27, in which `{cargo}` resolved
    through `~/.cargo/bin/cargo` to `.../bin/rustup` and all 59 cargo cells
    failed with `error: unexpected argument '--all' found`.
    """

    def test_symlinked_tool_keeps_the_name_it_must_be_invoked_under(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = pathlib.Path(raw).resolve()
            real = root / "shim"
            real.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            real.chmod(0o755)
            link = root / "cargo"
            link.symlink_to(real)

            table = run_gate.Replacements({}, {"cargo": str(link)})
            self.assertEqual(
                table["cargo"],
                str(link),
                "the tool must be invoked as `cargo`, not as its symlink target",
            )


class UmaskTest(unittest.TestCase):
    """docs/testing.md:124 requires every gate command to run under umask 077.

    Regression for the Gate A capture of 2026-07-27, in which the driver left the
    inherited umask alone, `tsc` emitted 0644 into the validation stage, and
    `dist-stage.mjs verify` rejected the tree, failing three TypeScript cells.
    """

    def test_cells_inherit_umask_077(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = pathlib.Path(raw).resolve()
            release = root / "release"
            release.mkdir()
            (release / "pmuxd").write_bytes(b"candidate\n")
            (release / "pmuxd").chmod(0o755)
            (release / "pmuxd.d").write_text("pmuxd: src/main.rs\n", encoding="utf-8")
            (root / ".gitignore").write_text("/target\n", encoding="utf-8")
            probe = root / "probe.txt"
            manifest = root / "m.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "phases": {
                            "gate_a": [
                                {
                                    "id": "emit",
                                    "cwd": "{workspace}",
                                    "argv": ["/bin/sh", "-c", f"echo hi > {probe}"],
                                    "env": {},
                                    "stdout_equals": None,
                                }
                            ]
                        },
                    }
                ),
                encoding="utf-8",
            )
            code = run_gate.main(
                [
                    "--manifest", str(manifest),
                    "--workspace", str(root),
                    "--release-dir", str(release),
                    "--validation-root", str(root),
                    "--receipt", str(root / "r.json"),
                ]
            )  # fmt: skip
            self.assertEqual(code, 0)
            self.assertEqual(probe.stat().st_mode & 0o777, 0o600)
