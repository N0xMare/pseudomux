#!/usr/bin/env python3
"""The register tool: what it reads, what it distils, what it refuses.

Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -v
"""

import argparse
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import mutation_register as register  # noqa: E402

# The tail of a real per-mutant log, from `run.bbUDg3`. Two `failures:` blocks
# because `cargo test` prints one as stdout headers and one as the list, and the
# `error:` line is what names the target to rerun.
CAUGHT_LOG = """running 67 tests
test some_other_test ... ok

failures:

---- native_frame_accumulator_is_fragmentation_invariant stdout ----
thread 'native_frame_accumulator_is_fragmentation_invariant' panicked at x.rs:270:13:
assertion `left == right` failed

failures:
    native_frame_accumulator_is_fragmentation_invariant

test result: FAILED. 66 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out.

error: test failed, to rerun pass `-p pseudomux-protocol --test v1_wire`

*** result: Failure(101)
"""

TIMEOUT_LOG = """running 12 tests
test a_test ... ok

*** timeout
"""


def mutant(name, file="crates/demo/src/thing.rs", function="alpha", summary=None):
    body = {
        "file": file,
        "function": None
        if function is None
        else {
            "function_name": function,
            "span": {
                "start": {"line": 1, "column": 1},
                "end": {"line": 3, "column": 2},
            },
        },
        "genre": "BinaryOperator",
        "replacement": "-",
        "name": name,
        "span": {"start": {"line": 2, "column": 7}, "end": {"line": 2, "column": 8}},
    }
    if summary is None:
        return body
    return {
        "scenario": {"Mutant": body},
        "summary": summary,
        "log_path": f"log/{name.replace(':', '_').replace(' ', '_')}.log",
    }


class CatcherTest(unittest.TestCase):
    def test_the_catching_test_and_its_target_come_out_of_the_log(self):
        self.assertEqual(
            register.catcher_in(CAUGHT_LOG),
            {
                "test": "native_frame_accumulator_is_fragmentation_invariant",
                "target": "-p pseudomux-protocol --test v1_wire",
            },
        )

    def test_a_log_naming_no_failing_target_yields_nothing(self):
        self.assertIsNone(register.catcher_in(TIMEOUT_LOG))

    def test_the_first_target_wins_because_cargo_test_stops_at_it(self):
        two = CAUGHT_LOG + CAUGHT_LOG.replace("v1_wire", "later_target")
        self.assertEqual(
            register.catcher_in(two)["target"], "-p pseudomux-protocol --test v1_wire"
        )

    def test_a_target_with_no_failure_block_still_names_the_target(self):
        text = "error: test failed, to rerun pass `-p demo --lib`\n"
        self.assertEqual(
            register.catcher_in(text), {"test": None, "target": "-p demo --lib"}
        )


class RebuiltTest(unittest.TestCase):
    """A mutant graded against the previous mutant's binary, told from one that
    was actually compiled. The distinction is one line of a cargo log and it is
    the difference between a measurement and a coincidence."""

    FRESH = (
        "       Fresh serde v1.0.0\n"
        "       Fresh pseudomux-service v0.1.0 (/tmp/x/crates/service)\n"
        "    Finished `mutants` profile [unoptimized] target(s) in 0.08s\n"
    )
    COMPILED = (
        "       Fresh serde v1.0.0\n"
        "   Compiling pseudomux-service v0.1.0 (/tmp/x/crates/service)\n"
        "    Finished `mutants` profile [unoptimized] target(s) in 13.72s\n"
    )

    def test_a_log_that_only_says_fresh_did_not_build_the_mutant(self):
        self.assertFalse(register.rebuilt_in(self.FRESH, "pseudomux-service"))

    def test_a_log_that_says_compiling_did(self):
        self.assertTrue(register.rebuilt_in(self.COMPILED, "pseudomux-service"))

    def test_another_crate_compiling_is_not_this_one(self):
        self.assertFalse(
            register.rebuilt_in(
                self.COMPILED.replace("pseudomux-service", "pseudomux-protocol"),
                "pseudomux-service",
            )
        )

    def test_the_package_of_a_file_is_the_longest_member_that_holds_it(self):
        directories = {
            "crates/service": "pseudomux-service",
            "crates/protocol": "pseudomux-protocol",
            "crates/service/extra": "pseudomux-service-extra",
        }
        self.assertEqual(
            register.package_of("crates/service/src/native.rs", directories),
            "pseudomux-service",
        )
        self.assertEqual(
            register.package_of("crates/service/extra/src/lib.rs", directories),
            "pseudomux-service-extra",
        )
        self.assertIsNone(register.package_of("bin/pmux/src/cli.rs", directories))


class ValidateTest(unittest.TestCase):
    def register_with(self, entry):
        return {
            "schema": register.SCHEMA,
            "key_fields": list(register.KEY_FIELDS),
            "entries": [entry],
        }

    def row(self, disposition, **extra):
        entry = {
            "file": "crates/demo/src/thing.rs",
            "function": "alpha",
            "genre": "BinaryOperator",
            "replacement": "-",
            "occurrence": 1,
            "disposition": disposition,
            "reason": "a row exists to say why",
        }
        entry.update(extra)
        return entry

    def test_a_killed_row_naming_no_catcher_is_refused(self):
        problems = register.validate(self.register_with(self.row("KILLED")))
        self.assertTrue(
            any("carries no `caught_by`" in one for one in problems), problems
        )

    def test_a_killed_row_naming_a_catcher_is_accepted(self):
        problems = register.validate(
            self.register_with(
                self.row("KILLED", caught_by={"test": "t", "target": "-p demo --lib"})
            )
        )
        self.assertEqual(problems, [])

    def test_a_caught_by_naming_neither_a_target_nor_a_reason_is_refused(self):
        problems = register.validate(
            self.register_with(self.row("KILLED", caught_by={"test": "t"}))
        )
        self.assertTrue(
            any("naming neither a target" in one for one in problems), problems
        )

    def test_a_removed_row_is_not_asked_to_name_a_test_that_never_ran(self):
        self.assertEqual(register.validate(self.register_with(self.row("REMOVED"))), [])

    def test_a_row_with_no_reason_is_still_refused(self):
        problems = register.validate(
            self.register_with(self.row("ACCEPTED", reason=" "))
        )
        self.assertTrue(any("has no reason" in one for one in problems), problems)


class EnumerationTest(unittest.TestCase):
    def test_a_list_json_and_an_outcomes_json_key_the_same_mutants(self):
        listing = [mutant("crates/demo/src/thing.rs:2:7: replace + with -")]
        outcomes = {
            "outcomes": [
                mutant(
                    "crates/demo/src/thing.rs:2:7: replace + with -",
                    summary="MissedMutant",
                )
            ]
        }
        self.assertEqual(
            list(register.enumerated(listing)), list(register.enumerated(outcomes))
        )

    def test_the_item_span_is_the_function_and_none_where_there_is_no_function(self):
        run = register.enumerated(
            [
                mutant("a", function="alpha"),
                mutant("b", function=None),
            ]
        )
        spans = {row["function"]: row["item"] for row in run.values()}
        self.assertEqual(spans["alpha"], (1, 3))
        self.assertIsNone(spans[""])

    def test_the_census_answers_whether_a_key_was_ever_enumerated(self):
        run = register.enumerated([mutant("a"), mutant("b")])
        counts = register.census_of(run)
        key = ("crates/demo/src/thing.rs", "alpha", "BinaryOperator", "-", 2)
        self.assertTrue(register.covers(counts, key))
        self.assertFalse(register.covers(counts, key[:4] + (3,)))
        self.assertFalse(register.covers(counts, key[:1] + ("beta",) + key[2:]))


class RecordCatchersTest(unittest.TestCase):
    def test_caught_by_is_written_and_undecided_rows_are_named(self):
        with tempfile.TemporaryDirectory(prefix="pmux-register-") as directory:
            root = pathlib.Path(directory)
            entry = {
                "file": "crates/demo/src/thing.rs",
                "function": "alpha",
                "genre": "BinaryOperator",
                "replacement": "-",
                "occurrence": 1,
                "disposition": "KILLED",
                "reason": "why",
            }
            absent = dict(entry, function="beta")
            (root / "register.json").write_text(
                json.dumps({"entries": [entry, absent]}), encoding="utf-8"
            )
            key = "|".join(str(entry[field]) for field in register.KEY_FIELDS)
            (root / "catchers.json").write_text(
                json.dumps(
                    {
                        "key_fields": list(register.KEY_FIELDS),
                        "key_separator": "|",
                        "run": "run.XXXX",
                        "catchers": {key: {"test": "t", "target": "-p demo --lib"}},
                    }
                ),
                encoding="utf-8",
            )
            status = register.record_catchers(
                argparse.Namespace(
                    register=str(root / "register.json"),
                    catchers=str(root / "catchers.json"),
                )
            )
            written = json.loads((root / "register.json").read_text())
            self.assertEqual(
                written["entries"][0]["caught_by"],
                {"test": "t", "target": "-p demo --lib", "run": "run.XXXX"},
            )
            self.assertNotIn("caught_by", written["entries"][1])
            # A closed row the run did not decide is a gap, and a tool that
            # reported success over it would be the gap nobody notices.
            self.assertEqual(status, 1)


if __name__ == "__main__":
    unittest.main()
