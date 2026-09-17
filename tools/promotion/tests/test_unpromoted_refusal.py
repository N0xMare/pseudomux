"""A promotion tool pointed at an unpromoted daemon's output must refuse.

`pmuxd --allow-unpromoted-claude` admits a Claude compatibility tuple nothing
measured, and labels everything it emits. These tests are the other half of that
bargain: the label is only worth writing if a tool that could turn it into a
promoted number stops when it sees one.

Nothing here starts a daemon, opens a socket or spends a token. The corpus is a
temporary directory and the `doctor` report is a dict.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

WORKSPACE = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(WORKSPACE / "tools" / "evidence_common"))

import unpromoted  # noqa: E402 -- tools/evidence_common, resolved above


def doctor_report(layers: list[dict]) -> dict:
    return {"status": "healthy", "diagnosis": {"layers": layers}}


class MarkerTests(unittest.TestCase):
    def test_a_corpus_without_the_marker_is_measurable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            (root / "a.jsonl").write_text("{}\n", encoding="utf-8")
            self.assertIsNone(unpromoted.marker_in([root]))
            unpromoted.refuse_unpromoted_corpus([root])

    def test_a_marker_anywhere_under_a_corpus_root_refuses_the_whole_run(self):
        """Nested, because a corpus root is normally a parent of several
        evidence directories and one unpromoted daemon contaminates the pool."""

        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            nested = root / "host" / "pool-evidence"
            nested.mkdir(parents=True)
            (nested / unpromoted.UNPROMOTED_MARKER).write_text(
                json.dumps({"unpromoted": True, "why": "started with the flag"}),
                encoding="utf-8",
            )
            self.assertIsNotNone(unpromoted.marker_in([root]))
            with self.assertRaises(unpromoted.UnpromotedArtifact) as refusal:
                unpromoted.refuse_unpromoted_corpus([root])
            self.assertIn("cannot back a promotion", str(refusal.exception))


class DoctorTests(unittest.TestCase):
    def test_an_ordinary_doctor_report_is_not_refused(self):
        report = doctor_report(
            [
                {"layer": "configuration", "evidence": {"unpromoted_claude_opt_in": False}},
                {"layer": "compatibility_profile", "evidence": {"pool_claude_unpromoted": False}},
            ]
        )
        self.assertFalse(unpromoted.doctor_opt_in(report))
        unpromoted.refuse_unpromoted_daemon(report)

    def test_a_report_with_no_layers_at_all_is_not_refused(self):
        """A daemon older than the flag cannot have it on, and a refusal that
        fired on a missing key would refuse every such host."""

        self.assertFalse(unpromoted.doctor_opt_in({}))
        unpromoted.refuse_unpromoted_daemon({})

    def test_either_layer_saying_so_refuses_the_daemon(self):
        for layer, key in (
            ("configuration", "unpromoted_claude_opt_in"),
            ("compatibility_profile", "pool_claude_unpromoted"),
        ):
            with self.subTest(layer=layer):
                report = doctor_report([{"layer": layer, "evidence": {key: True}}])
                self.assertTrue(unpromoted.doctor_opt_in(report))
                with self.assertRaises(unpromoted.UnpromotedArtifact) as refusal:
                    unpromoted.refuse_unpromoted_daemon(report)
                self.assertIn("--allow-unpromoted-claude", str(refusal.exception))


class DrainToolTests(unittest.TestCase):
    """The refusal is wired into the tool, not just available to it."""

    def test_measure_transcript_drain_exits_on_an_unpromoted_corpus(self):
        tool = WORKSPACE / "tools" / "promotion" / "measure_transcript_drain.py"
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            (root / unpromoted.UNPROMOTED_MARKER).write_text(
                json.dumps({"unpromoted": True}), encoding="utf-8"
            )
            done = subprocess.run(
                [
                    sys.executable,
                    str(tool),
                    "--corpus",
                    str(root),
                    "--version",
                    "2.1.272",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
        # Exit 6, and NOT exit 5 ("nothing to check"): an operator who is told
        # their corpus was empty goes looking for transcripts, and an operator
        # who is told it was unpromoted restarts the daemon.
        self.assertEqual(done.returncode, 6, done.stderr)
        self.assertIn("--allow-unpromoted-claude", done.stderr)

    def test_the_exit_code_is_the_one_the_tool_publishes(self):
        """Derived from the tool's own constant rather than restated here."""

        source = (
            WORKSPACE / "tools" / "promotion" / "measure_transcript_drain.py"
        ).read_text(encoding="utf-8")
        self.assertIn("EXIT_UNPROMOTED_CORPUS = 6", source)


if __name__ == "__main__":
    unittest.main()
