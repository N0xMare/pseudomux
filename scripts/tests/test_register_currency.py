#!/usr/bin/env python3
"""Every predicate in the currency check, driven until it refuses.

WHY A SYNTHETIC REPOSITORY AND NOT THIS ONE
-------------------------------------------

The rules are about pairs of commits, and this repository has exactly one pair
its full-scope campaign has ever been measured across. A check whose only test is
"it says MET on the tree it was written against" is the check this whole file
exists to replace, so each test below builds a repository of its own -- a
workspace, a mutation gate declaring its own `FULL_GLOBS`, a register, an
enumeration census -- makes ONE change, and asserts which rule fires and which do
not. The enumeration is injected rather than run: `cargo mutants --list` is 4.4 s
over the real globs and would need a real workspace here, and what is under test
is the attribution, not the tool that supplies the spans.

Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -v
"""

import datetime as dt
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import register_currency as currency  # noqa: E402

NOW = dt.datetime(2026, 8, 12, tzinfo=dt.timezone.utc)
MUTATED = "crates/demo/src/thing.rs"

GATE = """#!/usr/bin/env bash
readonly REQUIRED_CARGO_MUTANTS_VERSION="cargo-mutants 27.1.0"
readonly MUTATION_PROFILE="mutants"
readonly TEST_PACKAGES=(demo)
readonly FULL_GLOBS=(
  'crates/demo/src/thing.rs'
)
"""

# Line numbers are load-bearing here: `alpha` is 1-3, `beta` is 5-7, the
# module-level `const` with no function is line 9, and the `#[cfg(test)]` module
# is 11 to the end.
THING = """pub fn alpha(x: u64) -> u64 {
    x + 1
}

pub fn beta(x: u64) -> u64 {
    x * 2
}

pub const LIMIT: usize = 4 * 1024;

#[cfg(test)]
mod tests {
    mod extra;

    #[test]
    fn a_test() {
        assert_eq!(super::alpha(1), 2);
    }
}
"""


def mutant(file, function, span, genre, replacement, line, column):
    """One entry shaped exactly as `cargo mutants --list --json` emits it."""

    return {
        "file": file,
        "function": None
        if function is None
        else {
            "function_name": function,
            "span": {
                "start": {"line": span[0], "column": 1},
                "end": {"line": span[1], "column": 2},
            },
        },
        "genre": genre,
        "replacement": replacement,
        "name": f"{file}:{line}:{column}: replace X with {replacement}"
        + (f" in {function}" if function else ""),
        "span": {
            "start": {"line": line, "column": column},
            "end": {"line": line, "column": column + 1},
        },
    }


def enumeration():
    return [
        mutant(MUTATED, "alpha", (1, 3), "BinaryOperator", "-", 2, 7),
        mutant(MUTATED, "alpha", (1, 3), "FnValue", "0", 1, 1),
        mutant(MUTATED, "beta", (5, 7), "BinaryOperator", "/", 6, 7),
        mutant(MUTATED, None, (0, 0), "BinaryOperator", "+", 9, 30),
    ]


def register_of(entries, head, date="2026-08-12"):
    return {
        "schema": "pmux.mutation-survivor-register.json",
        "key_fields": list(currency.KEY_FIELDS),
        "recorded_at": {
            "head": head,
            "scope": "full",
            "date": date,
            "floor_percent": 94,
        },
        "entries": entries,
    }


def row(function, genre, replacement, occurrence, disposition, caught_by=None):
    entry = {
        "file": MUTATED,
        "function": function,
        "genre": genre,
        "replacement": replacement,
        "occurrence": occurrence,
        "disposition": disposition,
        "reason": "a row exists to say why",
    }
    if caught_by is not None:
        entry["caught_by"] = caught_by
    return entry


class Fixture:
    """A whole repository, two commits, and one change between them."""

    def __init__(self, directory: pathlib.Path):
        self.repo = directory
        self.write("Cargo.toml", '[workspace]\nmembers = ["crates/demo"]\n')
        self.write("rust-toolchain.toml", '[toolchain]\nchannel = "1.88.0"\n')
        self.write("scripts/gate-a-mutants.sh", GATE)
        self.write("crates/demo/Cargo.toml", '[package]\nname = "demo"\n')
        self.write("crates/demo/src/lib.rs", "pub mod thing;\npub mod helper;\n")
        self.write(MUTATED, THING)
        self.write("crates/demo/src/helper.rs", "pub fn help() -> u8 { 1 }\n")
        self.write("crates/demo/src/thing/tests/extra.rs", "#[test]\nfn extra() {}\n")
        self.write(
            "crates/demo/tests/wire.rs",
            "#[test]\nfn wire() {\n    assert!(true);\n}\n",
        )
        self.write("crates/demo/tests/support/mod.rs", "pub fn helper() {}\n")
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        self.entries = [
            row(
                "alpha",
                "BinaryOperator",
                "-",
                1,
                "KILLED",
                {"test": "thing::tests::a_test", "target": "-p demo --lib"},
            ),  # fmt: skip
            row(
                "beta",
                "BinaryOperator",
                "/",
                1,
                "KILLED",
                {"test": "wire", "target": "-p demo --test wire"},
            ),  # fmt: skip
            row("", "BinaryOperator", "+", 1, "ACCEPTED"),
        ]
        self.census_head = None

    # -- building ---------------------------------------------------------
    def write(self, relative, text):
        path = self.repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def git(self, *arguments):
        return subprocess.run(
            ["git", "-C", str(self.repo), *arguments],
            capture_output=True, text=True, check=True, timeout=120,
        ).stdout.strip()  # fmt: skip

    def commit(self, message="a commit"):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def record(self, entries=None, date="2026-08-12"):
        """Write the register and census, commit, and return that commit."""

        head = self.commit("the tree the register describes")
        self.write(
            "evidence/mutation-survivor-register.json",
            json.dumps(register_of(entries or self.entries, head, date), indent=1),
        )
        run = register_currency_keys()
        self.write(
            "evidence/mutation-enumeration.json",
            json.dumps(
                {
                    "schema": "pmux.mutation-enumeration.v1",
                    "key_fields": list(currency.KEY_FIELDS),
                    "recorded_at": {
                        "head": head,
                        "scope": "full",
                        "enumerated": len(run),
                        "globs": ["crates/demo/src/thing.rs"],
                        "test_packages": ["demo"],
                        "mutation_profile": "mutants",
                        "cargo_mutants_version": "cargo-mutants 27.1.0",
                        "toolchain_channel": "1.88.0",
                    },
                    "counts": __import__("mutation_register").census_of(run),
                },
                indent=1,
            ),
        )
        self.head = head
        return self.commit("the register")

    # -- measuring --------------------------------------------------------
    def assess(self, listing=None, **overrides):
        register = json.loads(
            (self.repo / "evidence/mutation-survivor-register.json").read_text()
        )
        census = json.loads(
            (self.repo / "evidence/mutation-enumeration.json").read_text()
        )
        arguments = {
            "register": register,
            "census": census,
            "now": NOW,
            "max_receipt_age_days": 14.0,
            "cargo_mutants": self.repo / "Cargo.toml",
            "enumerate_mutants": lambda *_: (
                enumeration() if listing is None else listing
            ),
        }
        arguments.update(overrides)
        return currency.assess(self.repo, self.git("rev-parse", "HEAD"), **arguments)


def register_currency_keys():
    import mutation_register  # noqa: PLC0415 -- resolved by the path insert above

    return mutation_register.enumerated(enumeration())


class CurrencyTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="pmux-currency-")
        self.addCleanup(self.directory.cleanup)
        self.fixture = Fixture(pathlib.Path(self.directory.name))

    # -- the case the whole thing exists for ------------------------------

    def test_a_tree_that_did_not_move_is_current_and_never_enumerates(self):
        self.fixture.record()

        def refuse(*_):
            raise AssertionError("enumeration must not run when nothing moved")

        state = self.fixture.assess(enumerate_mutants=refuse)
        self.assertTrue(state.current, state.reasons + state.escalations)
        self.assertEqual(state.stale_rows, [])

    def test_a_changed_function_body_stales_that_function_and_no_other(self):
        self.fixture.record()
        self.fixture.write(MUTATED, THING.replace("    x * 2", "    x * 3"))
        self.fixture.commit("beta changed")
        state = self.fixture.assess()
        self.assertEqual(state.escalations, [])
        self.assertEqual(state.stale_functions, [(MUTATED, "beta")])
        self.assertEqual(len(state.stale_rows), 1)
        self.assertIn("rule 1", state.reasons[0])

    def test_a_declaration_outside_every_item_escalates(self):
        self.fixture.record()
        self.fixture.write(
            MUTATED,
            THING.replace(
                "pub const LIMIT: usize = 4 * 1024;",
                "pub const LIMIT: usize = 8 * 1024;",
            ),
        )
        self.fixture.commit("the const changed")
        state = self.fixture.assess()
        self.assertTrue(any("rule 3(a)" in one for one in state.escalations), state)

    def test_a_change_inside_the_test_module_does_not_escalate(self):
        self.fixture.record()
        self.fixture.write(
            MUTATED, THING.replace("    fn a_test() {", "    fn a_test_renamed() {")
        )
        self.fixture.commit("a test changed")
        state = self.fixture.assess()
        self.assertEqual(state.escalations, [])

    def test_a_hunk_running_from_a_declaration_into_the_test_module_escalates(self):
        # One hunk, product code and test code at once. Excusing it as "a test
        # change" because it overlaps the region is the under-invalidation this
        # ordering exists to prevent.
        self.fixture.record()
        self.fixture.write(
            MUTATED,
            THING.replace(
                "pub const LIMIT: usize = 4 * 1024;\n\n#[cfg(test)]\nmod tests {\n",
                "pub const LIMIT: usize = 8 * 1024;\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n",
            ),
        )
        self.fixture.commit("a const and the module below it")
        state = self.fixture.assess()
        self.assertTrue(any("rule 3(a)" in one for one in state.escalations), state)

    # -- rule 2, the hole this closes -------------------------------------

    def test_a_changed_integration_test_stales_only_the_rows_it_caught(self):
        self.fixture.record()
        self.fixture.write(
            "crates/demo/tests/wire.rs",
            "#[test]\nfn wire() {\n    assert!(false);\n}\n",
        )
        self.fixture.commit("the catching test changed")
        state = self.fixture.assess()
        self.assertEqual(state.escalations, [])
        self.assertEqual(state.stale_functions, [(MUTATED, "beta")])
        self.assertIn("rule 2", state.reasons[0])

    def test_a_test_added_beside_the_catcher_stales_nothing(self):
        # The case the whole narrowing exists for. Adding a test is how a
        # survivor gets closed; if that invalidated every row caught by a test in
        # the same file, the whole-file rule would be back under another name.
        self.fixture.record()
        self.fixture.write(
            "crates/demo/tests/wire.rs",
            "#[test]\nfn wire() {\n    assert!(true);\n}\n\n#[test]\nfn added_later() {\n"
            "    let _ = 2;\n}\n",
        )
        self.fixture.write(
            MUTATED, THING.replace("mod tests {\n", "mod tests {\n    // a note\n")
        )
        self.fixture.commit("a test added above the catcher, and a note above another")
        state = self.fixture.assess()
        self.assertEqual(state.escalations, [])
        self.assertEqual(state.stale_rows, [])

    def test_a_deleted_catching_test_stales_the_row_it_killed(self):
        self.fixture.record()
        (self.fixture.repo / "crates/demo/tests/wire.rs").unlink()
        self.fixture.commit("the catching test was deleted")
        state = self.fixture.assess()
        self.assertEqual(state.stale_functions, [(MUTATED, "beta")])

    def test_a_shared_test_module_stales_the_target_that_compiles_it(self):
        self.fixture.record()
        self.fixture.write("crates/demo/tests/support/mod.rs", "pub fn helper() { }\n")
        self.fixture.commit("a shared test module changed")
        state = self.fixture.assess()
        self.assertEqual(state.stale_functions, [(MUTATED, "beta")])

    def test_a_lib_catcher_resolves_to_the_file_its_module_path_names(self):
        self.fixture.record()
        self.fixture.write(
            MUTATED, THING.replace("    fn a_test() {", "    fn a_test_renamed() {")
        )
        self.fixture.commit("the lib test changed")
        state = self.fixture.assess()
        self.assertEqual(state.stale_functions, [(MUTATED, "alpha")])
        self.assertIn("rule 2", state.reasons[0])

    def test_an_undetermined_catcher_is_stale_on_any_test_change(self):
        entries = list(self.fixture.entries)
        entries[1] = row(
            "beta", "BinaryOperator", "/", 1, "KILLED",
            {"test": None, "target": None, "undetermined": "timeout"},
        )  # fmt: skip
        self.fixture.record(entries)
        self.fixture.write(
            "crates/demo/tests/wire.rs",
            "#[test]\nfn wire() {\n    let _ = 1;\n}\n",
        )
        self.fixture.commit("some test changed")
        state = self.fixture.assess()
        self.assertIn((MUTATED, "beta"), state.stale_functions)
        self.assertEqual(
            [value for key, value in state.notes
             if key == "survivor_register_undetermined_catchers"],
            [1],
        )  # fmt: skip

    def test_a_survivor_row_a_new_test_could_falsify_is_counted(self):
        """The exposure of the rule that does not exist, as a number and not a
        paragraph. A survivor row names no catcher for rule 2 to watch and is
        falsified by a test being ADDED, so what `assess` reports is how many
        rows a run would leave undecided -- which is every survivor row that
        rule 1 has not already put in the stale set."""

        entries = list(self.fixture.entries) + [
            row("beta", "BinaryOperator", "*", 1, "ACCEPTED")
        ]
        self.fixture.record(entries)
        self.assertEqual(self.uncovered(self.fixture.assess()), 2)
        self.fixture.write(MUTATED, THING.replace("    x * 2", "    x * 3"))
        self.fixture.commit("beta changed")
        state = self.fixture.assess()
        self.assertIn((MUTATED, "beta"), state.stale_functions)
        self.assertEqual(self.uncovered(state), 1)

    @staticmethod
    def uncovered(state):
        return next(
            value
            for key, value in state.notes
            if key == "survivor_register_rows_a_new_test_could_falsify"
        )

    # -- rule 3, the escalations ------------------------------------------

    def test_a_new_file_under_the_globs_escalates(self):
        self.fixture.record()
        self.fixture.write("crates/demo/src/thing.rs", THING)
        self.fixture.write("crates/demo/src/other.rs", "pub fn other() {}\n")
        self.fixture.write(
            "scripts/gate-a-mutants.sh",
            GATE.replace(
                "  'crates/demo/src/thing.rs'\n",
                "  'crates/demo/src/thing.rs'\n  'crates/demo/src/other.rs'\n",
            ),
        )
        self.fixture.commit("a new mutated file")
        state = self.fixture.assess()
        self.assertTrue(any("rule 3(c)" in one for one in state.escalations), state)

    def test_a_callee_outside_the_globs_escalates(self):
        self.fixture.record()
        self.fixture.write("crates/demo/src/helper.rs", "pub fn help() -> u8 { 2 }\n")
        self.fixture.commit("a callee outside the scope changed")
        state = self.fixture.assess()
        self.assertTrue(any("rule 3(e)" in one for one in state.escalations), state)

    def test_a_test_only_module_outside_the_globs_does_not_escalate(self):
        self.fixture.record()
        self.fixture.write(
            "crates/demo/src/thing/tests/extra.rs",
            "#[test]\nfn extra() { let _ = 1; }\n",
        )
        self.fixture.commit("a test-only module outside the globs changed")
        state = self.fixture.assess()
        self.assertEqual(state.escalations, [])

    def test_a_moved_frame_escalates(self):
        self.fixture.record()
        self.fixture.write(
            "scripts/gate-a-mutants.sh", GATE.replace("(demo)", "(demo other)")
        )
        self.fixture.commit("TEST_PACKAGES moved")
        state = self.fixture.assess()
        self.assertTrue(any("rule 3(b)" in one for one in state.escalations), state)

    def test_a_register_older_than_the_receipt_bound_escalates(self):
        self.fixture.record(date="2026-01-01")
        state = self.fixture.assess()
        self.assertTrue(any("rule 3(g)" in one for one in state.escalations), state)

    def test_filtered_runs_costing_more_than_a_full_one_escalate(self):
        self.fixture.record()
        self.fixture.write(
            "evidence/mutation-filtered-run-a.json",
            json.dumps(
                {
                    "schema": "pmux.mutation-filtered-run.v1",
                    "counts": {"total_mutants": 9_000},
                }
            ),
        )
        self.fixture.commit("a filtered run that cost more than the full one")
        state = self.fixture.assess()
        self.assertTrue(any("rule 3(f)" in one for one in state.escalations), state)

    def test_a_row_whose_function_the_enumeration_lost_escalates(self):
        self.fixture.record()
        self.fixture.write(MUTATED, THING.replace("    x * 2", "    x * 3"))
        self.fixture.commit("beta changed")
        listing = [
            one
            for one in enumeration()
            if one["file"] != MUTATED
            or (one["function"] or {}).get("function_name") != "beta"
        ]
        state = self.fixture.assess(listing=listing)
        self.assertTrue(any("rule 3(d)" in one for one in state.escalations), state)

    def test_a_mutant_the_campaign_never_enumerated_escalates(self):
        self.fixture.record()
        self.fixture.write(MUTATED, THING.replace("    x * 2", "    x * 3"))
        self.fixture.commit("beta changed")
        listing = enumeration() + [
            mutant(MUTATED, "alpha", (1, 3), "BinaryOperator", "*", 2, 7)
        ]
        state = self.fixture.assess(listing=listing)
        self.assertTrue(
            any("never enumerated" in one for one in state.escalations), state
        )

    def test_a_stale_row_with_no_function_cannot_be_refiltered(self):
        entries = list(self.fixture.entries)
        entries[2] = row(
            "", "BinaryOperator", "+", 1, "KILLED",
            {"test": "wire", "target": "-p demo --test wire"},
        )  # fmt: skip
        self.fixture.record(entries)
        self.fixture.write(
            "crates/demo/tests/wire.rs",
            "#[test]\nfn wire() {\n    let _ = 1;\n}\n",
        )
        self.fixture.commit("the catching test changed")
        state = self.fixture.assess()
        self.assertTrue(
            any("`cargo mutants -F` selects by function name" in one
                for one in state.escalations),
            state,
        )  # fmt: skip

    def test_a_missing_cargo_mutants_escalates_rather_than_passing(self):
        self.fixture.record()
        self.fixture.write(MUTATED, THING.replace("    x * 2", "    x * 3"))
        self.fixture.commit("beta changed")
        state = self.fixture.assess(cargo_mutants=self.fixture.repo / "nothing-here")
        self.assertTrue(
            any("cannot be decided here" in one for one in state.escalations), state
        )

    def test_a_cfg_test_region_holding_a_mutant_is_not_trusted(self):
        self.fixture.record()
        self.fixture.write(
            MUTATED, THING.replace("    fn a_test() {", "    fn a_test_renamed() {")
        )
        self.fixture.commit("a test changed")
        # The enumeration now claims a mutable item inside the region, which is
        # what a wrong region boundary looks like from the outside.
        listing = enumeration() + [
            mutant(MUTATED, "inside_the_test_mod", (12, 19), "FnValue", "()", 13, 5)
        ]
        state = self.fixture.assess(listing=listing)
        self.assertTrue(
            any("cannot tell that file's test code" in one
                for one in state.escalations),
            state,
        )  # fmt: skip


class DerivationTest(unittest.TestCase):
    """The pieces each rule is built from, on their own."""

    def test_cfg_test_regions_finds_the_trailing_module(self):
        self.assertEqual(currency.cfg_test_regions(THING), [(11, 19)])

    def test_cfg_test_regions_refuses_a_module_it_cannot_close(self):
        # rustfmt puts a top-level close brace in column one; without one there
        # is no region, and a hunk there escalates rather than being excused.
        self.assertEqual(
            currency.cfg_test_regions("#[cfg(test)]\nmod tests {\n    fn a() {}\n"), []
        )

    def test_a_hunk_is_test_code_only_when_the_region_contains_all_of_it(self):
        spans = [(1, 3, "alpha"), (5, 7, "beta")]
        regions = [(11, 19)]
        self.assertEqual(currency.hunk_rule((6, 6), spans, regions), ["beta"])
        self.assertIs(currency.hunk_rule((12, 15), spans, regions), currency.TEST_CODE)
        # From the declaration above the module down into it: product code and
        # test code in one hunk, and rule 3(a) is the only safe reading.
        self.assertEqual(currency.hunk_rule((9, 15), spans, regions), [])
        # And from inside the module out past its end, for a region that is not
        # the last item in the file.
        self.assertEqual(currency.hunk_rule((15, 22), spans, regions), [])
        # A hunk that reaches a function AND the test module is still rule 1.
        self.assertEqual(currency.hunk_rule((6, 15), spans, regions), ["beta"])

    def test_gap_of_widens_a_functionless_mutant_to_the_whole_gap(self):
        spans = [(1, 3, "alpha"), (5, 7, "beta")]
        self.assertEqual(currency.gap_of(spans, 9, 19), (8, 19))
        self.assertEqual(currency.gap_of(spans, 4, 19), (4, 4))

    def test_a_test_target_resolves_to_its_file_and_the_shared_modules(self):
        with tempfile.TemporaryDirectory(prefix="pmux-currency-") as directory:
            fixture = Fixture(pathlib.Path(directory))
            packages = currency.workspace_packages(fixture.repo)
            self.assertEqual(
                currency.catcher_sources(
                    fixture.repo, packages,
                    {"test": "wire", "target": "-p demo --test wire"},
                ),
                ("crates/demo/tests/wire.rs", ["crates/demo/tests/support"]),
            )  # fmt: skip

    def test_a_lib_target_resolves_through_the_module_path(self):
        with tempfile.TemporaryDirectory(prefix="pmux-currency-") as directory:
            fixture = Fixture(pathlib.Path(directory))
            package = fixture.repo / "crates/demo"
            self.assertEqual(
                currency.lib_module_source(package, "thing::tests::a_test"),
                package / "src/thing.rs",
            )
            self.assertEqual(
                currency.lib_module_source(package, "thing::tests::extra::extra"),
                package / "src/thing/tests/extra.rs",
            )
            self.assertIsNone(currency.lib_module_source(package, "a_test"))

    def test_an_unresolvable_catcher_invalidates_on_the_whole_package(self):
        with tempfile.TemporaryDirectory(prefix="pmux-currency-") as directory:
            fixture = Fixture(pathlib.Path(directory))
            packages = currency.workspace_packages(fixture.repo)
            self.assertEqual(
                currency.catcher_sources(
                    fixture.repo, packages, {"target": "-p demo --doc"}
                ),
                (None, ["crates/demo"]),
            )

    def test_a_pure_deletion_is_reported_as_the_lines_it_landed_between(self):
        with tempfile.TemporaryDirectory(prefix="pmux-currency-") as directory:
            fixture = Fixture(pathlib.Path(directory))
            head = fixture.commit("first")
            fixture.write(MUTATED, THING.replace("    x * 2\n", ""))
            commit = fixture.commit("a line deleted")
            self.assertEqual(
                currency.changed_hunks(fixture.repo, head, commit, MUTATED),
                [(5, 6)],
            )


if __name__ == "__main__":
    unittest.main()
