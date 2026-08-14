"""The renderer every receipt emitter writes its paths through.

Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/evidence_common/tests -v

The property under test is not "the placeholder is pretty". It is that a
receipt rendered through this module still answers the questions its readers
ask of it: `_validate_campaign_contract` in `tools/phase0/phase0_lib.py` reads a
recorded binary path back and checks the file NAME and the shared parent
directory, and a renderer that returned a digest, an elision or a bare basename
would pass a redaction check and break that one. Structure-preservation is
therefore tested as a contract and not described as a style.

Every needle is asked of the running machine, so these tests plant the machine's
own identifiers into strings they build rather than spelling any. A test that
spelled one would be green on the host that had nothing to find.
"""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

COMMON = pathlib.Path(__file__).resolve().parents[1]
WORKSPACE = COMMON.parents[1]
sys.path.insert(0, str(COMMON))
sys.path.insert(0, str(WORKSPACE / "tools" / "phase0"))

import portable_paths  # noqa: E402
import reseal_ledger  # noqa: E402 -- the one writer allowed to change a sealed file


class DerivationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.identifiers = portable_paths.machine_identifiers()

    def test_the_derivation_finds_identifiers_and_each_one_can_be_caught(self):
        """A checker that derives nothing reports success over nothing."""
        self.assertGreaterEqual(
            len(self.identifiers),
            4,
            f"too few identifiers derived: {sorted(self.identifiers)}",
        )
        for description, (needle, _) in self.identifiers.items():
            self.assertEqual(
                len(
                    portable_paths.offences(
                        f"a line carrying {needle} in the middle",
                        {description: (needle, "")},
                    )
                ),
                1,
                f"the scanner cannot catch the {description} it derived",
            )

    def test_the_map_substitutes_longer_identifiers_before_shorter_ones(self):
        """The checkout path contains the home directory, which contains the user."""
        lengths = [len(needle) for needle, _ in portable_paths.substitutions()]
        self.assertEqual(lengths, sorted(lengths, reverse=True))

    def test_every_placeholder_the_map_can_emit_is_declared(self):
        """`PLACEHOLDERS` is what the checks downstream look for.

        A map that emitted a token absent from that tuple would be invisible to
        the idempotence check, which is the one property a reader can verify
        without re-running the emitter.
        """
        for _, placeholder in portable_paths.substitutions():
            self.assertIn(placeholder, portable_paths.PLACEHOLDERS)

    def test_the_checkout_is_rendered_before_the_home_that_contains_it(self):
        """Order, at the one pair where getting it wrong is silent.

        Shortest-first leaves `<HOME>/...rest of the checkout path...`, which is
        neither the absolute path nor a rooted one, and which every check in
        this file would then pass.
        """
        rendered = portable_paths.render(str(portable_paths.WORKSPACE))
        self.assertEqual(rendered, "<REPO>")

    def test_the_rooted_form_renders_an_absolute_needle_as_an_absolute_path(self):
        """The property, over every needle the map derived, in both directions.

        `Path("<HOME>/x").is_absolute()` is False, and three validators read a
        recorded path back through exactly that call. So the claim is not "the
        rooted map adds a slash" -- it is that rendering preserves
        `is_absolute()`, which means the rootless needles must NOT acquire one:
        `<USER>` is a login name and `<WORKSPACES>` is a relative distance, and
        rooting either would invent a directory.
        """
        rooted = portable_paths.machine_identifiers(absolute_placeholders=True)
        self.assertEqual(sorted(rooted), sorted(portable_paths.machine_identifiers()))
        absolute = 0
        for description, (needle, placeholder) in rooted.items():
            expected = pathlib.Path(needle).is_absolute()
            absolute += expected
            self.assertEqual(
                pathlib.Path(placeholder).is_absolute(),
                expected,
                f"{description} renders {needle!r} as {placeholder!r}",
            )
            self.assertEqual(
                pathlib.Path(
                    portable_paths.render(
                        f"{needle}/x", against=[(needle, placeholder)]
                    )
                ).is_absolute(),
                expected,
            )
        self.assertGreaterEqual(
            absolute, 2, "no absolute needle was derived, so nothing was tested"
        )

    def test_the_temporary_directory_is_taken_only_where_it_differs(self):
        """`/private/tmp` names a platform; the hashed per-user path names a host."""
        default = portable_paths._platform_default_temporary_directory()
        with mock.patch.object(
            portable_paths.tempfile, "gettempdir", return_value=default
        ):
            self.assertIsNone(portable_paths._private_temporary_directory())


class RenderingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.home = str(pathlib.Path.home().resolve())
        self.repo = str(portable_paths.WORKSPACE)

    def test_a_recorded_binary_keeps_the_shape_its_reader_checks(self):
        """`tools/phase0/phase0_lib.py` reads the name and the shared parent.

        A rendered release binary must still be named after the binary and must
        still share one parent with its siblings, because that is what
        `_validate_campaign_contract` asks of it.
        """
        names = ("pmux", "pmuxd", "pmux-rmuxd")
        recorded = [f"{self.repo}/target/release/{name}" for name in names]
        rendered = [portable_paths.render(path) for path in recorded]
        self.assertEqual(
            [pathlib.PurePosixPath(path).name for path in rendered], list(names)
        )
        self.assertEqual(
            {str(pathlib.PurePosixPath(path).parent) for path in rendered},
            {"<REPO>/target/release"},
        )

    def test_rendering_is_idempotent(self):
        once = portable_paths.render(f"{self.repo}/target and {self.home}/.local")
        self.assertEqual(portable_paths.render(once), once)
        self.assertEqual(portable_paths.nested_placeholders(once), [])

    def test_a_document_is_rendered_through_its_keys_as_well_as_its_values(self):
        """A receipt that keys a map by file name keys it by a path."""
        rendered = portable_paths.render_document(
            {f"{self.repo}/a.rs": {"argv": [f"{self.home}/bin/tool", "--flag"]}}
        )
        self.assertEqual(
            rendered, {"<REPO>/a.rs": {"argv": ["<HOME>/bin/tool", "--flag"]}}
        )

    def test_a_document_keeps_everything_that_is_not_a_string(self):
        document = {"n": 1, "f": 1.5, "b": True, "none": None, "l": [1, "x"]}
        self.assertEqual(portable_paths.render_document(document), document)

    def test_a_path_under_no_named_root_is_left_alone(self):
        """`/usr/bin/env` names a platform. Renaming it would lose a fact."""
        self.assertEqual(portable_paths.render("/usr/bin/env"), "/usr/bin/env")

    def test_a_rendered_document_still_serialises(self):
        """The choke point is `json.dumps`; a renderer that broke it is useless."""
        rendered = portable_paths.render_document({"p": f"{self.repo}/x"})
        self.assertEqual(json.loads(json.dumps(rendered)), {"p": "<REPO>/x"})


class CommandLineTests(unittest.TestCase):
    """The entry point the artefacts written before the emitters were fixed use."""

    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = pathlib.Path(self.directory.name) / "receipt.json"
        self.path.write_text(
            json.dumps({"binary": f"{portable_paths.WORKSPACE}/target/release/pmux"}),
            encoding="utf-8",
        )

    def _run(self, *arguments: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(COMMON / "portable_paths.py"), *arguments],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_check_reports_and_fails_on_an_unrendered_file(self):
        done = self._run("--check", str(self.path))
        self.assertEqual(done.returncode, 1, done.stderr)
        self.assertIn("checkout path", done.stdout)

    def test_rewrite_then_check_is_clean_and_running_it_twice_changes_nothing(self):
        self.assertEqual(self._run("--rewrite", str(self.path)).returncode, 0)
        once = self.path.read_bytes()
        self.assertIn(b"<REPO>/target/release/pmux", once)
        self.assertEqual(self._run("--rewrite", str(self.path)).returncode, 0)
        self.assertEqual(self.path.read_bytes(), once)
        self.assertEqual(self._run("--check", str(self.path)).returncode, 0)

    def test_stdin_renders_for_the_emitters_that_are_not_python(self):
        done = subprocess.run(
            [sys.executable, str(COMMON / "portable_paths.py"), "--stdin"],
            input=f"{portable_paths.WORKSPACE}/target\n",
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, "<REPO>/target\n")


class TwoSpellingsTests(unittest.TestCase):
    """One directory, two strings, and the map has to hold both.

    This is not hypothetical and it is not reasoned: the map carried only the
    resolved spelling, `EmitterTests` below was run against a corpus under a
    temporary `HOME`, and the root came back out of the receipt untouched.
    `/var` is a symlink into `/private/var` here, the emitter recorded what it
    was given, and the needle was the other string.
    """

    def test_a_symlinked_root_contributes_both_of_its_spellings(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        real = pathlib.Path(directory.name) / "real"
        real.mkdir()
        link = pathlib.Path(directory.name) / "link"
        link.symlink_to(real)
        spellings = portable_paths._spellings(link)
        self.assertEqual(spellings, [str(real.resolve()), str(link)])

    def test_a_root_that_is_not_a_symlink_contributes_one_spelling(self):
        """So the map does not carry a duplicate needle on every platform."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        real = (pathlib.Path(directory.name) / "real").resolve()
        real.mkdir()
        self.assertEqual(portable_paths._spellings(real), [str(real)])


class TrackedTreeTests(unittest.TestCase):
    """The file set, and the one property that decides where the map stops.

    Both are DERIVED. The set is `git ls-files`, so the file somebody adds next
    week is in scope without anybody remembering; the exemption is a seal the
    predicate verifies against the file, so a file cannot buy it by name. The
    tests below build a throwaway repository and plant this machine's own
    identifiers into it, because a test that spelled one would be green on the
    host that had nothing left to find.
    """

    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name).resolve()
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.home = str(pathlib.Path.home().resolve())

    def _track(self, name: str, body: bytes) -> pathlib.Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)
        subprocess.run(["git", "add", "--", name], cwd=self.root, check=True)
        return path

    def test_the_file_set_is_what_git_tracks_and_not_what_is_on_disk(self):
        """An untracked file is out of scope; adding one puts it in scope."""
        loose = self.root / "untracked.txt"
        loose.write_text("nothing\n", encoding="utf-8")
        self.assertEqual(portable_paths.tracked_files(self.root), [])
        self._track("kept.txt", b"nothing\n")
        self.assertEqual(
            portable_paths.tracked_files(self.root), [self.root / "kept.txt"]
        )

    def test_a_file_that_is_not_utf8_is_scanned_rather_than_skipped(self):
        """The first binary fixture must not become a hole in the scan."""
        planted = f"{self.home}/secret".encode() + b"\xff\xfe not utf-8\n"
        self._track("blob.bin", planted)
        self.assertIn("blob.bin", portable_paths.tree_offences(self.root))
        self.assertEqual(
            portable_paths.read_text(self.root / "blob.bin").encode(
                "utf-8", portable_paths.BYTES_AS_TEXT
            ),
            planted,
        )

    def test_the_scan_reports_a_planted_identifier_and_the_rewrite_removes_it(self):
        self._track("receipt.json", f'{{"p": "{self.home}/x"}}\n'.encode())
        self.assertIn("receipt.json", portable_paths.tree_offences(self.root))
        self._run(
            "--rewrite", "--tracked", "--workspace", str(self.root), cwd=self.root
        )
        self.assertEqual(portable_paths.tree_offences(self.root), {})
        self.assertIn("<HOME>/x", (self.root / "receipt.json").read_text("utf-8"))

    def test_rewriting_the_tree_twice_changes_nothing(self):
        """Idempotence, over the set the rule actually applies to."""
        self._track("a.json", f'{{"p": "{self.home}/x"}}\n'.encode())
        self._track("b.md", f"see {portable_paths.WORKSPACE}/target\n".encode())
        self._run(
            "--rewrite", "--tracked", "--workspace", str(self.root), cwd=self.root
        )
        once = {
            path: path.read_bytes() for path in portable_paths.tracked_files(self.root)
        }
        self._run(
            "--rewrite", "--tracked", "--workspace", str(self.root), cwd=self.root
        )
        self.assertEqual(
            {
                path: path.read_bytes()
                for path in portable_paths.tracked_files(self.root)
            },
            once,
        )
        for body in once.values():
            self.assertEqual(
                portable_paths.nested_placeholders(body.decode("utf-8")), []
            )

    def test_a_sealed_file_is_reported_by_the_scan_and_refused_by_the_rewrite(self):
        """The refusal is the WRITER's, and the scan reads the file anyway.

        The committed ledger is copied in rather than imitated: a synthetic
        stand-in would prove that the predicate accepts what this test built.
        An identifier is planted in a copied record's `artifact_directory` --
        the committed file carries none any more -- because a sealed file with
        nothing to find cannot demonstrate that the scan looks inside one, which
        is the half that used to be missing: `tree_offences` excused the sealed
        file, so a checker that never opened the largest concentration of
        identifiers in the tree reported zero over it.
        """
        sealed = (WORKSPACE / "evidence" / "model-attempt-ledger.ndjson").read_bytes()
        self.assertNotEqual(portable_paths.sealed_records(sealed.decode("utf-8")), [])
        # Planted THROUGH the re-sealer, because a hand-planted identifier would
        # break the seal it is meant to be hiding behind and the file would stop
        # being the thing under test. The substitution runs backwards here --
        # the committed ledger spells `/<HOME>` and this puts a real home back.
        planted = reseal_ledger.reseal(sealed, against=[("/<HOME>", str(self.home))])
        self.assertNotEqual(planted, sealed)
        self.assertNotEqual(portable_paths.sealed_records(planted.decode("utf-8")), [])
        self._track("ledger.ndjson", planted)
        self.assertEqual(
            list(portable_paths.tree_offences(self.root)), ["ledger.ndjson"]
        )
        done = self._run(
            "--rewrite", "--tracked", "--workspace", str(self.root), cwd=self.root
        )
        self.assertIn("sealed, left alone", done.stdout)
        self.assertIn("reseal_ledger.py", done.stdout)
        self.assertEqual((self.root / "ledger.ndjson").read_bytes(), planted)
        self._run(
            "--check",
            "--tracked",
            "--workspace",
            str(self.root),
            cwd=self.root,
            expect=1,
        )

    def test_a_file_cannot_buy_the_exemption_by_writing_the_field_name(self):
        record = {
            portable_paths.SEAL_FIELD: "0" * 64,
            portable_paths.CHAIN_FIELD: "0" * 64,
            "artifact_directory": f"{self.home}/campaign",
        }
        self.assertFalse(portable_paths.keeps_its_paths(json.dumps(record) + "\n"))
        self.assertFalse(portable_paths.keeps_its_paths("not json at all\n"))
        self.assertFalse(portable_paths.keeps_its_paths(""))

    def test_the_seal_has_to_reach_the_last_record_in_the_file(self):
        """Otherwise the coverage runs out at the end and nobody notices.

        A prefix of the committed ledger that ENDS on a sealed record is itself
        sealed -- the chain digest covers the bytes in front of each record, and
        nothing behind it. Appending one unsealed line makes it not.
        """
        ledger = (WORKSPACE / "evidence" / "model-attempt-ledger.ndjson").read_text(
            encoding="utf-8"
        )
        lines = ledger.splitlines(keepends=True)
        prefix = "".join(
            lines[
                : next(
                    index + 1
                    for index in reversed(range(len(lines)))
                    if portable_paths.SEAL_FIELD in lines[index]
                )
            ]
        )
        self.assertTrue(portable_paths.keeps_its_paths(prefix))
        self.assertFalse(portable_paths.keeps_its_paths(prefix + '{"a": 1}\n'))

    def test_substituting_into_the_sealed_file_forges_it_rather_than_redacts_it(self):
        """The whole argument for the writer's refusal, run rather than asserted.

        The needle is taken from the file and from `PLACEHOLDERS` rather than
        from this machine. It used to be `render(ledger)` with the real map,
        which stopped demonstrating anything the moment the ledger was redacted
        and re-sealed -- the map became a no-op over it and the test would have
        gone green over a substitution that never happened. What is actually
        being claimed is a property of the seal: ANY substitution breaks it.
        """
        ledger = (WORKSPACE / "evidence" / "model-attempt-ledger.ndjson").read_text(
            encoding="utf-8"
        )
        against = [
            (token, token.lower())
            for token in portable_paths.PLACEHOLDERS
            if token in ledger
        ]
        self.assertNotEqual(
            against, [], "the ledger carries no placeholder to substitute for"
        )
        forged = portable_paths.render(ledger, against=against)
        self.assertNotEqual(forged, ledger)
        self.assertNotEqual(portable_paths.sealed_records(ledger), [])
        self.assertEqual(portable_paths.sealed_records(forged), [])

    def test_the_committed_ledger_records_absolute_paths_after_the_reseal(self):
        """The constraint that defeats the obvious substitution, held.

        `<HOME>/x` is not an absolute path, and `_validate_reservation_record`
        reads `artifact_directory` back through `Path(...).is_absolute()`. A
        redaction that used the default placeholder would leave 51 records the
        validator refuses, so the property is asserted over the artefact itself
        rather than over the map that produced it.
        """
        ledger = (WORKSPACE / "evidence" / "model-attempt-ledger.ndjson").read_text(
            encoding="utf-8"
        )
        records = portable_paths.sealed_records(ledger)
        self.assertNotEqual(records, [])
        directories = [record["artifact_directory"] for record in records]
        self.assertEqual(
            [name for name in directories if not pathlib.Path(name).is_absolute()], []
        )
        self.assertNotEqual(
            [name for name in directories if "<" in name],
            [],
            "no artifact directory was rendered, so nothing here is being tested",
        )

    def test_tracked_refuses_to_be_given_a_file_set_as_well(self):
        done = self._run(
            "--check",
            "--tracked",
            "--workspace",
            str(self.root),
            "a.txt",
            cwd=self.root,
            expect=2,
        )
        self.assertIn("--tracked derives the file set", done.stderr)

    def _run(
        self, *arguments: str, cwd: pathlib.Path, expect: int = 0
    ) -> subprocess.CompletedProcess:
        done = subprocess.run(
            [sys.executable, str(COMMON / "portable_paths.py"), *arguments],
            capture_output=True,
            text=True,
            check=False,
            cwd=cwd,
        )
        self.assertEqual(done.returncode, expect, done.stdout + done.stderr)
        return done


class EmitterTests(unittest.TestCase):
    """One emitter driven end to end, and its receipt read back.

    `tools/promotion/measure_transcript_drain.py` is the emitter of
    `evidence/pooled-transcript-drain-<os>-<arch>.json`, and the only one of the
    committed set that reaches a receipt without a live model, a daemon or a
    mutation run. It records the corpus roots it scanned, which live under the
    operator's home directory, so pointing it at a corpus under a temporary
    `HOME` is a whole run of the real tool whose receipt has to come back
    rendered. `HOME` is moved rather than the derivation patched: a test that
    mocked the map would prove the mock renders.
    """

    def test_a_real_receipt_records_its_corpus_against_a_named_root(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        home = pathlib.Path(directory.name)
        corpus = home / "projects"
        corpus.mkdir()
        done = subprocess.run(
            [
                sys.executable,
                str(WORKSPACE / "tools" / "promotion" / "measure_transcript_drain.py"),
                "--corpus",
                str(corpus),
                "--version",
                "2.1.220",
                "--json",
            ],
            capture_output=True,
            text=True,
            check=False,
            env={**os.environ, "HOME": str(home)},
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        receipt = json.loads(done.stdout)
        self.assertEqual(receipt["corpus"]["roots"], ["<HOME>/projects"])
        self.assertEqual(
            portable_paths.offences(done.stdout, portable_paths.machine_identifiers()),
            [],
        )


if __name__ == "__main__":
    unittest.main()
