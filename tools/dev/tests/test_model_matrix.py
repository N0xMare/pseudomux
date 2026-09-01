"""The (model, effort) matrix probe, without spending a Claude turn.

Every assertion here reads `MODEL_TABLE` through the tool's own parser rather
than restating a cell, except the three shapes the table's doc comment states
as facts about named generations: opus-5 takes the full ladder, haiku-4-5 takes
no tier, and `fable` is an alias rather than a canonical id.
"""

from __future__ import annotations

import importlib.util
import io
import pathlib
import unittest
from contextlib import redirect_stderr, redirect_stdout

ROOT = pathlib.Path(__file__).resolve().parents[3]
DEV = ROOT / "tools" / "dev"


def load(name: str):
    path = DEV / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


model_matrix = load("model_matrix")
TABLE = model_matrix.parse_model_table(
    model_matrix.CLASS_RS.read_text(encoding="utf-8")
)


def entry(canonical: str) -> dict:
    for item in TABLE:
        if item["canonical"] == canonical:
            return item
    raise AssertionError(f"MODEL_TABLE has no {canonical}: {TABLE}")


class ClassParser(unittest.TestCase):
    def test_the_table_is_not_empty(self) -> None:
        self.assertTrue(TABLE)
        for item in TABLE:
            self.assertTrue(item["canonical"].startswith("claude-"), item)

    def test_opus_5_takes_the_full_ladder(self) -> None:
        self.assertEqual(
            entry("claude-opus-5")["efforts"],
            ["low", "medium", "high", "xhigh", "max"],
        )

    def test_haiku_takes_no_effort_tier(self) -> None:
        self.assertEqual(entry("claude-haiku-4-5")["efforts"], [])

    def test_fable_is_an_alias_and_not_a_canonical_id(self) -> None:
        owners = [item for item in TABLE if "fable" in item["aliases"]]
        self.assertEqual(len(owners), 1, TABLE)
        self.assertNotEqual(owners[0]["canonical"], "fable")
        self.assertNotIn("fable", [item["canonical"] for item in TABLE])

    def test_an_unknown_effort_set_is_refused(self) -> None:
        source = model_matrix.CLASS_RS.read_text(encoding="utf-8").replace(
            "efforts: EFFORTS_ALL,", "efforts: EFFORTS_INVENTED,"
        )
        with self.assertRaises(model_matrix.MatrixRefused) as raised:
            model_matrix.parse_model_table(source)
        self.assertIn("EFFORTS_INVENTED", str(raised.exception))

    def test_an_entry_the_regex_cannot_parse_is_refused(self) -> None:
        source = """
const LOW: AdmittedEffort = AdmittedEffort { level: L, argv: "low" };
const EFFORTS_ALL: &[AdmittedEffort] = &[LOW];
pub static MODEL_TABLE: &[ModelEntry] = &[
    ModelEntry {
        canonical: "claude-opus-5",
        aliases: &["opus"],
        efforts: EFFORTS_ALL,
    },
    ModelEntry {
        // A doc comment between the fields, which the regex cannot span.
        canonical: "claude-sonnet-5",
        /* reordered */
        efforts: EFFORTS_ALL,
        aliases: &["sonnet"],
    },
];
"""
        with self.assertRaises(model_matrix.MatrixRefused) as raised:
            model_matrix.parse_model_table(source)
        self.assertIn("silently", str(raised.exception))

    def test_the_table_slice_stops_at_its_own_closing_bracket(self) -> None:
        source = model_matrix.CLASS_RS.read_text(encoding="utf-8") + """
pub static LATER_TABLE: &[ModelEntry] = &[
    ModelEntry {
        canonical: "claude-invented-9",
        aliases: &[],
        efforts: EFFORTS_ALL,
    },
];
"""
        parsed = model_matrix.parse_model_table(source)
        self.assertEqual(parsed, TABLE)

    def test_a_table_with_no_entries_is_refused(self) -> None:
        source = model_matrix.CLASS_RS.read_text(encoding="utf-8").replace(
            "pub static MODEL_TABLE", "pub static RETIRED_TABLE"
        )
        with self.assertRaises(model_matrix.MatrixRefused):
            model_matrix.parse_model_table(source)


class RowDerivation(unittest.TestCase):
    def test_one_row_per_cell_plus_one_alias_row_per_model(self) -> None:
        rows = model_matrix.derive_rows(TABLE)
        cells = sum(len(item["efforts"]) or 1 for item in TABLE)
        aliases = sum(1 for item in TABLE if item["aliases"])
        self.assertEqual(len(rows), cells + aliases)
        self.assertEqual(sum(1 for row in rows if row["via_alias"]), aliases)

    def test_a_model_with_no_tiers_gets_one_bare_row(self) -> None:
        rows = model_matrix.derive_rows(TABLE, only=["claude-haiku-4-5"])
        self.assertEqual([row["effort"] for row in rows], [None, None])
        self.assertEqual([row["via_alias"] for row in rows], [False, True])
        self.assertEqual(rows[1]["alias_used"], entry("claude-haiku-4-5")["aliases"][0])

    def test_the_alias_row_uses_the_first_admitted_effort(self) -> None:
        rows = model_matrix.derive_rows(TABLE, only=["claude-opus-5"])
        alias = [row for row in rows if row["via_alias"]]
        self.assertEqual(len(alias), 1)
        self.assertEqual(alias[0]["effort"], entry("claude-opus-5")["efforts"][0])
        self.assertEqual(alias[0]["model"], "claude-opus-5")
        self.assertEqual(alias[0]["spelling"], entry("claude-opus-5")["aliases"][0])

    def test_skip_effort_drops_the_tier_everywhere(self) -> None:
        rows = model_matrix.derive_rows(TABLE, skip_efforts=["max", "xhigh"])
        self.assertEqual([row for row in rows if row["effort"] in ("max", "xhigh")], [])

    def test_skipping_every_tier_a_model_takes_drops_the_model(self) -> None:
        rows = model_matrix.derive_rows(
            TABLE,
            only=["claude-opus-4-5"],
            skip_efforts=entry("claude-opus-4-5")["efforts"],
        )
        self.assertEqual(rows, [])

    def test_only_refuses_a_model_the_table_does_not_carry(self) -> None:
        with self.assertRaises(model_matrix.MatrixRefused) as raised:
            model_matrix.derive_rows(TABLE, only=["fable"])
        self.assertIn("canonical model", str(raised.exception))

    def test_skip_effort_refuses_a_tier_the_table_does_not_admit(self) -> None:
        with self.assertRaises(model_matrix.MatrixRefused) as raised:
            model_matrix.derive_rows(TABLE, skip_efforts=["typo"])
        self.assertIn("typo", str(raised.exception))
        self.assertIn("effort tier", str(raised.exception))


class ReportedModel(unittest.TestCase):
    def test_the_canonical_id_verbatim_matches(self) -> None:
        self.assertTrue(
            model_matrix.reported_model_matches("claude-opus-5", "claude-opus-5")
        )

    def test_a_dated_build_of_the_canonical_id_matches(self) -> None:
        self.assertTrue(
            model_matrix.reported_model_matches(
                "claude-opus-4-5-20251101", "claude-opus-4-5"
            )
        )

    def test_a_later_generation_is_not_the_model_that_was_launched(self) -> None:
        self.assertFalse(
            model_matrix.reported_model_matches("claude-opus-5-1", "claude-opus-5")
        )
        self.assertFalse(
            model_matrix.reported_model_matches(
                "claude-opus-5-1-20260401", "claude-opus-5"
            )
        )

    def test_no_reported_model_is_not_a_match(self) -> None:
        self.assertFalse(model_matrix.reported_model_matches(None, "claude-opus-5"))
        self.assertFalse(model_matrix.reported_model_matches("", "claude-opus-5"))


class Describe(unittest.TestCase):
    def test_describe_needs_no_daemon_and_is_exit_0(self) -> None:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            status = model_matrix.main(["--describe"])
        self.assertEqual(status, 0, stderr.getvalue())
        text = stdout.getvalue()
        self.assertIn(model_matrix.SCHEMA, text)
        self.assertIn(model_matrix.GREEN, text)
        self.assertIn("not a promotion", text)
        rows = model_matrix.derive_rows(TABLE)
        self.assertEqual(text.count("pmux run --model "), len(rows))
        for item in TABLE:
            self.assertIn(f"--model {item['canonical']}", text)

    def test_describe_honours_only_and_skip_effort(self) -> None:
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            status = model_matrix.main(
                ["--describe", "--only", "claude-opus-5", "--skip-effort", "max"]
            )
        self.assertEqual(status, 0)
        text = stdout.getvalue()
        self.assertNotIn("--effort max", text)
        self.assertNotIn("claude-sonnet-5", text)

    def test_an_unknown_only_model_is_exit_2(self) -> None:
        stderr = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
            status = model_matrix.main(["--describe", "--only", "claude-invented-9"])
        self.assertEqual(status, 2)
        self.assertIn("model-matrix refused", stderr.getvalue())


def row(answered: bool = True, matches: bool = True) -> dict:
    return {
        "model": "claude-opus-5",
        "spelling": "claude-opus-5",
        "effort": "low",
        "answered": answered,
        "reported_model_matches": matches,
    }


class Verdict(unittest.TestCase):
    def test_every_cell_answering_on_a_healthy_pool_is_green(self) -> None:
        rows = [row(), row()]
        self.assertEqual(
            model_matrix.verdict(rows, {"halted": None, "leaked": 0}),
            model_matrix.GREEN,
        )

    def test_one_wrong_answer_is_red(self) -> None:
        rows = [row(), row(answered=False)]
        self.assertEqual(model_matrix.verdict(rows, {}), model_matrix.RED)

    def test_a_reported_model_mismatch_is_red(self) -> None:
        rows = [row(matches=False)]
        self.assertEqual(model_matrix.verdict(rows, {}), model_matrix.RED)
        self.assertEqual(
            model_matrix.summarize(rows)["reported_model_mismatches"],
            ["claude-opus-5/low"],
        )

    def test_a_halted_or_leaking_pool_is_red(self) -> None:
        self.assertEqual(
            model_matrix.verdict([row()], {"halted": "spawn failed"}),
            model_matrix.RED,
        )
        self.assertEqual(
            model_matrix.verdict([row()], {"leaked": 1}), model_matrix.RED
        )

    def test_an_empty_matrix_is_red_rather_than_vacuously_green(self) -> None:
        self.assertEqual(model_matrix.verdict([], {}), model_matrix.RED)

    def test_the_summary_counts_the_rows_it_was_given(self) -> None:
        summary = model_matrix.summarize([row(), row(answered=False)])
        self.assertEqual(summary["rows"], 2)
        self.assertEqual(summary["answered"], 1)
        self.assertEqual(summary["failed"], 1)


class NotAPromotion(unittest.TestCase):
    def test_the_tool_does_not_write_a_promotion_surface(self) -> None:
        source = (DEV / "model_matrix.py").read_text(encoding="utf-8")
        # Flattened, because the denial is wrapped across source lines.
        flat = " ".join(source.split())
        self.assertEqual(
            flat.count("PROMOTED_PROFILES"),
            flat.count("does not edit PROMOTED_PROFILES"),
            "every mention of PROMOTED_PROFILES here must be a denial",
        )
        self.assertNotIn("pooled_bound(", source)
        self.assertIn("portable_paths.render_document", source)
        self.assertIn('"not_a_promotion": True', source)

    def test_the_dev_readme_documents_it(self) -> None:
        text = (DEV / "README.md").read_text(encoding="utf-8")
        self.assertIn("tools/dev/model_matrix.py", text)
        self.assertIn(model_matrix.GREEN, text)


if __name__ == "__main__":
    unittest.main()
