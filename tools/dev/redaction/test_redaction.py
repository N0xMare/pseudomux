"""No tracked file names this machine.

The scrub is a mechanical transformation, not a set of edits. Hand-editing
evidence is forbidden here -- this repository has already caught a hand-written
receipt naming whatever HEAD happened to be when it was saved -- and what makes
a scrub different in kind is that the transformation itself is committed, can be
re-run, and is checked. This module is that check.

THE NEEDLES ARE ASKED OF THE RUNNING MACHINE, NEVER WRITTEN DOWN, and they are
asked through `tools/evidence_common/portable_paths.py`, which is also what the
emitters render through and what `tools/defect-log/machine.py` re-exports. Two
derivations of one map is two maps, and the second one is the one that goes
stale. Nothing in this file spells an identifier -- that is not a style choice.
A list of identifiers to look for is composed on the host that has already been
searched, so it is complete there and nowhere else, and it passes on the next
host for the same reason it passed on this one. It is also why
`docs/defect-log.md`'s map table describes its inputs instead of spelling them:
a table that spelled them would be the one live instance of the shape this
refuses, sitting inside the paragraph that declares it.

SCOPE is `git ls-files` -- every tracked file. The classes below that name
`docs/defect-log.md` and `evidence/` remain because each asserts something
about its own subject that the tree-wide check does not: that the log was
generated rather than edited, and that the receipts carry a placeholder at all.

The historical attempt ledger under `evidence/` is frozen. Do not reseal it.
The scan still opens it: a file that names this machine fails the tree-wide
check the same as any other tracked file.
"""

from __future__ import annotations

import pathlib
import re
import sys
import unittest

WORKSPACE = pathlib.Path(__file__).resolve().parents[3]
DEFECT_LOG = WORKSPACE / "docs" / "defect-log.md"

sys.path.insert(0, str(WORKSPACE / "tools" / "evidence_common"))

import portable_paths as machine  # noqa: E402 -- the one map; `machine` is its
# name in `tools/defect-log/`, kept here so the log's two halves read alike


def tracked_evidence() -> list[pathlib.Path]:
    """Every artefact git tracks under `evidence/`, asked of git.

    A subset of the tree-wide set, because the receipts carry an obligation the
    tree does not: they must SHOW the map was applied, not merely fail to carry
    an identifier. `README.md` is included on purpose -- it is prose, but it is
    prose about this machine's campaigns and it is exactly as publishable as the
    receipts it describes.
    """
    evidence = WORKSPACE / "evidence"
    return [
        path for path in machine.tracked_files(WORKSPACE) if evidence in path.parents
    ]


# Each message is reproduced verbatim inside one fenced block. The document's
# own prose is everything outside them -- including the table that names the
# placeholders, which is why the substitution has to be looked for in here.
QUOTED_MESSAGE = re.compile(r"^`````text\n(.*?)\n`````$", re.S | re.M)


class TrackedTreeTest(unittest.TestCase):
    """Every tracked file, which is the scope the publication actually needs.

    The two checks below are the whole rule: nothing tracked names this machine,
    and the map that would rename it has nothing left to do. The second is not a
    restatement of the first -- it is what catches a substitution that is not
    idempotent, or a placeholder that got substituted into.
    """

    def setUp(self) -> None:
        self.identifiers = machine.machine_identifiers(WORKSPACE)
        self.tracked = machine.tracked_files(WORKSPACE)

    def test_git_tracks_enough_of_a_tree_for_this_to_be_checking_something(self):
        """A broken checkout makes every check below vacuous over an empty set."""
        self.assertGreater(
            len(self.tracked),
            100,
            f"git lists almost nothing tracked: {self.tracked[:5]}",
        )

    def test_no_tracked_file_names_this_machine(self):
        found = machine.tree_offences(WORKSPACE, identifiers=self.identifiers)
        reported = [
            f"{name}:\n  " + "\n  ".join(lines[:5]) for name, lines in found.items()
        ]
        self.assertEqual(
            [],
            reported,
            "these tracked files name this machine. Re-run their emitter, or "
            "apply the map with `python3 tools/evidence_common/portable_paths.py "
            "--rewrite --tracked`; do not edit the lines by hand:\n"
            + "\n".join(reported),
        )

    def test_running_the_map_over_the_tree_again_would_change_nothing(self):
        """Idempotence, asserted over the artefact rather than over an example.

        A scrub that is not idempotent is a scrub nobody can safely re-run, and
        re-running it is the whole plan: the emitters render at generation time
        and this entry point catches whatever they missed. Two ways for that to
        fail are checked, because they fail differently -- a second pass that
        changes a byte, and a placeholder that ended up inside a placeholder.

        Every tracked file is in this pass, including the frozen attempt
        ledger. A second pass that would rewrite a file is a map that is not
        idempotent.
        """
        against = machine.substitutions(WORKSPACE)
        changed, nested = [], []
        for path in self.tracked:
            text = machine.read_text(path)
            name = path.relative_to(WORKSPACE).as_posix()
            if machine.render(text, against=against) != text:
                changed.append(name)
            if machine.nested_placeholders(text):
                nested.append(f"{name}: {machine.nested_placeholders(text)}")
        self.assertEqual([], changed, "a second pass of the map would rewrite these")
        self.assertEqual([], nested, "a placeholder was substituted into another")


class RedactionTest(unittest.TestCase):
    def setUp(self) -> None:
        self.identifiers = machine.machine_identifiers()
        self.text = DEFECT_LOG.read_text(encoding="utf-8")

    def test_the_derivation_finds_identifiers_and_each_one_can_be_caught(self):
        """The scanner is proven able to fire, on every needle it derived.

        A checker that derives an empty set reports success over nothing, and a
        needle the scanner cannot match is a needle that is not being checked.
        Both are refused here rather than discovered on the host that had
        something to find.
        """
        self.assertGreaterEqual(
            len(self.identifiers),
            4,
            "too few identifiers derived to be checking anything: "
            f"{sorted(self.identifiers)}",
        )
        for description, (needle, _) in self.identifiers.items():
            planted = f"a line that happens to carry {needle} in the middle of it"
            self.assertEqual(
                len(machine.offences(planted, {description: (needle, "")})),
                1,
                f"the scanner cannot catch the {description} it derived",
            )

    def test_the_map_substitutes_longer_identifiers_before_shorter_ones(self):
        """Order is the difference between a scrub and a half-scrub.

        The checkout path contains the home directory, which contains the login
        name. Applied shortest first the map leaves a mangled path that neither
        it nor this test recognises, so the ordering is a property of the map
        rather than a convention its callers remember.
        """
        lengths = [len(needle) for needle, _ in machine.substitutions()]
        self.assertEqual(lengths, sorted(lengths, reverse=True))

    def test_the_defect_log_is_present_and_substantial(self):
        """So a rename cannot make the checks below green over nothing."""
        self.assertTrue(DEFECT_LOG.is_file(), f"{DEFECT_LOG} is missing")
        self.assertGreater(
            len(self.text.splitlines()),
            1000,
            "the defect log is the whole pre-squash commit log; a short one is "
            "a truncated one",
        )
        self.assertGreaterEqual(
            len(QUOTED_MESSAGE.findall(self.text)),
            100,
            "the defect log quotes one fenced message per commit; this one "
            "quotes almost none",
        )

    def test_the_defect_log_carries_no_identifier_from_this_machine(self):
        found = machine.offences(self.text, self.identifiers)
        self.assertEqual(
            [],
            found,
            "docs/defect-log.md names this machine; re-run "
            "tools/defect-log/generate.py rather than editing the lines by "
            "hand:\n" + "\n".join(found),
        )

    def test_a_quoted_message_shows_the_substitution_happened(self):
        """Absence of an identifier is not evidence of a substitution.

        A document that never mentioned a path would pass the check above while
        proving nothing about the map. The placeholders are looked for inside
        the quoted messages and not in the document as a whole, because the
        document's own map table names all of them and would satisfy any check
        that read it -- which it did, until this test was run against it.
        """
        quoted = "\n".join(QUOTED_MESSAGE.findall(self.text))
        substituted = [token for token in machine.PLACEHOLDERS if token in quoted]
        self.assertNotEqual(
            [],
            substituted,
            "no quoted message carries any placeholder, so nothing shows the "
            "map was applied to the messages rather than only described above "
            "them",
        )

    def test_no_placeholder_was_substituted_into_another(self):
        """Idempotence, in the one form the artefact itself can witness.

        Running the map twice must be running it once. A second pass over an
        already-substituted document produces nested placeholders, so their
        absence is what a reader can check without re-running the generator.
        """
        for token in machine.PLACEHOLDERS:
            self.assertNotIn(f"<{token}>", self.text, f"{token} was substituted twice")


class CommittedEvidenceTest(unittest.TestCase):
    """The receipts, held to the same standard as the log, and by the same map."""

    def setUp(self) -> None:
        self.identifiers = machine.machine_identifiers()
        self.artefacts = tracked_evidence()

    def test_git_tracks_enough_evidence_for_this_to_be_checking_something(self):
        """A rename or a missing checkout makes every check below vacuous."""
        self.assertGreaterEqual(
            len(self.artefacts),
            10,
            f"git lists almost no evidence: {self.artefacts}",
        )
        self.assertTrue(all(path.is_file() for path in self.artefacts))

    def test_no_committed_artefact_names_this_machine(self):
        """The emitters render; this is what notices when one stops.

        Including the frozen attempt ledger. A file that names this machine
        fails here the same as any other tracked artefact.
        """
        reported = []
        for path in self.artefacts:
            text = path.read_text(encoding="utf-8", errors="replace")
            found = machine.offences(text, self.identifiers)
            if not found:
                continue
            reported.append(
                f"{path.relative_to(WORKSPACE)}:\n  " + "\n  ".join(found[:5])
            )
        self.assertEqual(
            [],
            reported,
            "these committed artefacts name this machine; re-run their emitter "
            "or render them with tools/evidence_common/portable_paths.py "
            "--rewrite:\n" + "\n".join(reported),
        )

    def test_a_rendered_receipt_shows_the_map_was_applied(self):
        """Absence of an identifier is not evidence of a substitution.

        A receipt that never recorded a path would satisfy the check above
        while proving nothing. At least one artefact has to carry a placeholder,
        or the emitters are being credited for work nobody did.

        RECEIPTS ONLY, not the prose beside them. `evidence/README.md` explains
        what the placeholders are and therefore spells all of them, so a check
        that read it would be green on a directory of unrendered receipts --
        which is the exact shape `test_a_quoted_message_shows_the_substitution_
        happened` above already had to be narrowed out of.
        """
        carrying = [
            path.relative_to(WORKSPACE).as_posix()
            for path in self.artefacts
            if path.suffix in {".json", ".ndjson"}
            and any(
                token in path.read_text(encoding="utf-8", errors="replace")
                for token in machine.PLACEHOLDERS
            )
        ]
        self.assertNotEqual(
            [], carrying, "no committed receipt records a path against a named root"
        )

    def test_no_placeholder_was_substituted_into_another(self):
        """Idempotence: re-running the renderer must be running it once."""
        for path in self.artefacts:
            text = path.read_text(encoding="utf-8", errors="replace")
            for token in machine.PLACEHOLDERS:
                self.assertNotIn(
                    f"<{token}>", text, f"{path.name}: {token} was substituted twice"
                )


if __name__ == "__main__":
    unittest.main()
