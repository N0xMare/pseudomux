"""`drain_n50.keep_corpus`, the one thing standing between a campaign and a
lost corpus.

The function is small, and that is the reason it had no test and the reason it
needs one. A campaign's transcripts live inside `Sandbox.root` and are deleted
in the tool's own `finally`, so a `keep_corpus` that silently copied the wrong
set -- or flattened the tree, or skipped a subdirectory -- would not fail the
run. It would produce a receipt that looks exactly like a good one and a
corpus a later pooled measurement cannot use, after the fifty real turns that
paid for it had already been spent.

So what is asserted here is what a later `measure_transcript_drain.py --corpus
<destination>` depends on: every `*.jsonl` under the source, at the SAME
relative path, nothing else copied, and a count in the receipt that equals the
number of files really written.
"""

from __future__ import annotations

import importlib.util
import pathlib
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[3]
DEV = ROOT / "tools" / "dev"


def load(name: str):
    path = DEV / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


drain_n50 = load("drain_n50")


def fake_sandbox_corpus(root: pathlib.Path) -> None:
    """A Path B evidence mirror's shape: jsonl at the root and one level down.

    The daemon writes its mirror flat today, but `keep_corpus` copies with
    `rglob` and preserves the relative path, so the test states the property
    the function claims rather than the layout one version of pmuxd happens to
    write.
    """

    (root / "nested").mkdir(parents=True)
    (root / "a.jsonl").write_text('{"type":"user"}\n', encoding="utf-8")
    (root / "b.jsonl").write_text('{"type":"assistant"}\n', encoding="utf-8")
    (root / "nested" / "c.jsonl").write_text('{"type":"system"}\n', encoding="utf-8")
    # Everything a real mirror directory also holds, and none of it a corpus.
    (root / "pmux-unpromoted.json").write_text("{}", encoding="utf-8")
    (root / "notes.txt").write_text("not a transcript", encoding="utf-8")
    (root / "nested" / "d.json").write_text("{}", encoding="utf-8")


class KeepCorpusTest(unittest.TestCase):
    def test_every_jsonl_is_copied_at_its_own_relative_path(self):
        with tempfile.TemporaryDirectory() as raw:
            base = pathlib.Path(raw)
            corpus = base / "pool-evidence"
            corpus.mkdir()
            fake_sandbox_corpus(corpus)
            destination = base / "kept" / "2.1.272"

            kept = drain_n50.keep_corpus(corpus, destination)

            self.assertEqual(
                sorted(
                    path.relative_to(destination).as_posix()
                    for path in destination.rglob("*")
                    if path.is_file()
                ),
                ["a.jsonl", "b.jsonl", "nested/c.jsonl"],
                "the destination must be a corpus root, with the source's "
                "relative layout and nothing that is not a transcript",
            )
            self.assertEqual(kept["jsonl_files"], 3)
            self.assertEqual(kept["root"], str(destination))
            self.assertEqual(
                (destination / "nested" / "c.jsonl").read_text(encoding="utf-8"),
                '{"type":"system"}\n',
                "contents are copied, not just paths",
            )

    def test_the_destination_is_created_and_an_empty_corpus_is_not_an_error(self):
        """Exit 5 belongs to the measurement, not to the copy.

        A campaign that produced no transcript is a campaign to look at, and
        `measure_transcript_drain.py` already refuses it with its own
        "nothing to check" exit. `keep_corpus` raising instead would lose the
        receipt for the run that failed, which is the artefact that says what
        happened.
        """

        with tempfile.TemporaryDirectory() as raw:
            base = pathlib.Path(raw)
            corpus = base / "pool-evidence"
            corpus.mkdir()
            destination = base / "kept" / "2.1.258"

            kept = drain_n50.keep_corpus(corpus, destination)

            self.assertTrue(destination.is_dir())
            self.assertEqual(kept["jsonl_files"], 0)

    def test_the_recorded_root_is_the_path_the_tool_saw(self):
        """`corpus_kept.root` is the destination as given, not resolved.

        In the container lane that is `/corpus/<version>`, a container-internal
        path, which is what the lane's README says it is. A test that asserted
        a resolved or host-side path would be asserting the opposite.
        """

        with tempfile.TemporaryDirectory() as raw:
            base = pathlib.Path(raw)
            corpus = base / "pool-evidence"
            corpus.mkdir()
            (corpus / "a.jsonl").write_text("{}\n", encoding="utf-8")
            destination = base / "kept" / "." / "2.1.272"

            kept = drain_n50.keep_corpus(corpus, destination)

            self.assertEqual(kept["root"], str(destination))


if __name__ == "__main__":
    unittest.main()
