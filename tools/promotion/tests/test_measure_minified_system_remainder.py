"""Unit tests for the minified-system remainder instrument. No live Claude."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "tools" / "promotion" / "measure_minified_system_remainder.py"


def load():
    spec = importlib.util.spec_from_file_location(
        "measure_minified_system_remainder", MODULE_PATH
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {MODULE_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


remainder = load()


class BilledInput(unittest.TestCase):
    def test_sums_the_three_counters(self) -> None:
        self.assertEqual(
            remainder.billed_input(
                {
                    "input_tokens": 2,
                    "cache_creation_input_tokens": 1321,
                    "cache_read_input_tokens": 0,
                }
            ),
            1323,
        )

    def test_includes_cache_read(self) -> None:
        self.assertEqual(
            remainder.billed_input(
                {
                    "input_tokens": 10,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 50,
                }
            ),
            60,
        )

    def test_refuses_a_collapsed_non_int(self) -> None:
        with self.assertRaises(remainder.MeasurementError):
            remainder.billed_input(
                {
                    "input_tokens": 288,
                    "cache_creation_input_tokens": None,
                    "cache_read_input_tokens": 0,
                }
            )


class SummarizeTurn(unittest.TestCase):
    def test_remainder_uses_billed_not_collapsed_input(self) -> None:
        with self.assertRaises(remainder.MeasurementError) as raised:
            remainder.summarize_turn(
                {
                    "text": "OK",
                    "usage": {
                        "main": {
                            "input_tokens": 2,
                            "output_tokens": 4,
                            "cache_creation_input_tokens": 1321,
                            "cache_read_input_tokens": 0,
                        },
                        "sidechain": {
                            "input_tokens": 0,
                            "output_tokens": 0,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                        },
                        "combined": {
                            "input_tokens": 2,
                            "output_tokens": 4,
                            "cache_creation_input_tokens": 1321,
                            "cache_read_input_tokens": 0,
                        },
                    },
                },
                10.0,
                "cold",
                remainder.USER_PROMPT[:0] + "The user message is the entire instruction.",
            )
        self.assertIn("cache_creation=1321", str(raised.exception))

    def test_chars_over_4_on_zero_cache(self) -> None:
        displacer = "The user message is the entire instruction."
        summary = remainder.summarize_turn(
            {
                "text": "OK",
                "stop_reason": {"kind": "end_turn"},
                "usage": {
                    "main": {
                        "input_tokens": 288,
                        "output_tokens": 4,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                    },
                    "sidechain": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                    },
                    "combined": {
                        "input_tokens": 288,
                        "output_tokens": 4,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                    },
                },
            },
            12.5,
            "after_clear",
            displacer,
        )
        self.assertEqual(summary["billed_input_tokens"], 288)
        self.assertEqual(summary["remainder_tokens_est_chars_over_4"], 265)
        self.assertEqual(summary["displacer_tokens_est_chars_over_4"], 11)
        self.assertEqual(summary["user_prompt_tokens_est_chars_over_4"], 12)

    def test_refuses_wrong_text(self) -> None:
        with self.assertRaises(remainder.MeasurementError):
            remainder.summarize_turn(
                {
                    "text": "Sure",
                    "usage": {
                        "main": {
                            "input_tokens": 10,
                            "output_tokens": 1,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                        }
                    },
                },
                1.0,
                "cold",
                "x",
            )

    def test_refuses_missing_main(self) -> None:
        with self.assertRaises(remainder.MeasurementError):
            remainder.summarize_turn(
                {"text": "OK", "usage": {}},
                1.0,
                "cold",
                "x",
            )

    def test_refuses_nonzero_sidechain(self) -> None:
        with self.assertRaises(remainder.MeasurementError) as raised:
            remainder.summarize_turn(
                {
                    "text": "OK",
                    "usage": {
                        "main": {
                            "input_tokens": 10,
                            "output_tokens": 1,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                        },
                        "sidechain": {
                            "input_tokens": 3,
                            "output_tokens": 0,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                        },
                    },
                },
                1.0,
                "cold",
                "x",
            )
        self.assertIn("sidechain", str(raised.exception))


class ExtractDebug(unittest.TestCase):
    def test_does_not_invent_bodies_when_detail_is_absent(self) -> None:
        extracted = remainder.extract_debug(
            "[DEBUG] [api] API REQUEST source=quota_check\n"
            "[DEBUG] [api] API REQUEST source=generate_session_title\n"
            "[DEBUG] [api] API REQUEST source=repl_main_thread\n"
            "[DEBUG] [file] No CLAUDE.md/rules files found\n"
        )
        self.assertFalse(extracted["api_request_detail_emitted"])
        self.assertEqual(extracted["json_bodies_found"], 0)
        self.assertEqual(extracted["bodies_classified"], [])
        self.assertTrue(extracted["process_side_only"])
        self.assertEqual(
            extracted["api_sources"],
            ["quota_check", "generate_session_title", "repl_main_thread"],
        )
        self.assertTrue(
            any("No CLAUDE.md" in note for note in extracted["notes"])  # type: ignore[arg-type]
        )
        self.assertTrue(extracted["notes_are_process_log_not_model_visible"])

    def test_flags_detail_without_inventing_json(self) -> None:
        extracted = remainder.extract_debug(
            "[DEBUG] [api] API REQUEST DETAIL source=repl_main_thread\n"
            '{"model": "claude-sonnet-5"}\n'
        )
        self.assertTrue(extracted["api_request_detail_emitted"])
        self.assertEqual(extracted["json_bodies_found"], 0)
        self.assertEqual(extracted["bodies_classified"], [])


class DisplacerSource(unittest.TestCase):
    def test_parses_config_rs(self) -> None:
        source = (
            'pub const DEFAULT_SYSTEM_PROMPT: &str = '
            '"The user message is the entire instruction.";\n'
        )
        self.assertEqual(
            remainder.default_system_prompt_from_rust(source),
            "The user message is the entire instruction.",
        )

    def test_load_displacer_matches_config_rs(self) -> None:
        self.assertEqual(
            remainder.load_displacer(),
            "The user message is the entire instruction.",
        )


class CompactCensus(unittest.TestCase):
    def test_drops_socket_paths(self) -> None:
        compact = remainder.compact_census(
            {
                "idle": 1,
                "live": 1,
                "clearing": 0,
                "socket": "/tmp/qv4-turn-latency-abc/pmux.sock",
                "halted": None,
            }
        )
        self.assertEqual(compact["idle"], 1)
        self.assertNotIn("socket", compact)


class DestroyedFloor(unittest.TestCase):
    def test_clearing_is_recycle_not_remint(self) -> None:
        remainder.refuse_destroyed_floor(
            {
                "idle": 0,
                "live": 1,
                "clearing": 1,
                "tearing_down": 0,
                "halted": None,
            }
        )

    def test_empty_floor_is_not_after_clear(self) -> None:
        with self.assertRaises(remainder.MeasurementError) as raised:
            remainder.refuse_destroyed_floor(
                {
                    "idle": 0,
                    "live": 0,
                    "clearing": 0,
                    "tearing_down": 0,
                    "registered_instances": 0,
                    "halted": None,
                }
            )
        self.assertIn("remint", str(raised.exception))

    def test_tearing_down_is_not_after_clear(self) -> None:
        with self.assertRaises(remainder.MeasurementError):
            remainder.refuse_destroyed_floor(
                {
                    "idle": 0,
                    "live": 0,
                    "clearing": 0,
                    "tearing_down": 1,
                    "halted": None,
                }
            )

    def test_census_error_is_not_after_clear(self) -> None:
        with self.assertRaises(remainder.MeasurementError) as raised:
            remainder.refuse_destroyed_floor({"error": "doctor printed no JSON"})
        self.assertIn("cannot prove after_clear", str(raised.exception))


if __name__ == "__main__":
    unittest.main()
