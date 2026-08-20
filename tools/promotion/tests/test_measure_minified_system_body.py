"""Unit tests for /v1/messages body classification. No live Claude."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "tools" / "promotion" / "measure_minified_system_body.py"


def load():
    spec = importlib.util.spec_from_file_location(
        "measure_minified_system_body", MODULE_PATH
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {MODULE_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


body = load()
DISPLACER = "The user message is the entire instruction."
USER = "Reply with exactly the word OK and nothing else."


class ClassifyBody(unittest.TestCase):
    def test_separates_displacer_from_leftover_system(self) -> None:
        classified = body.classify_body(
            {
                "model": "claude-sonnet-4-6",
                "max_tokens": 100,
                "system": [
                    {"type": "text", "text": DISPLACER},
                    {
                        "type": "text",
                        "text": "Today's date is Wednesday.",
                        "cache_control": {"type": "ephemeral"},
                    },
                ],
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            {"type": "text", "text": "<total_tokens>0</total_tokens>"},
                            {"type": "text", "text": USER},
                        ],
                    }
                ],
                "tools": [],
            },
            DISPLACER,
            USER,
        )
        self.assertTrue(classified["displacer_in_system"])
        self.assertTrue(classified["user_prompt_present"])
        self.assertEqual(classified["tool_count"], 0)
        self.assertEqual(
            classified["leftover_system_chars"], len("Today's date is Wednesday.")
        )
        self.assertEqual(classified["leftover_user_chars"], len("<total_tokens>0</total_tokens>"))
        self.assertGreaterEqual(classified["marker_hits"].get("total_tokens"), 1)
        self.assertNotIn("You are Claude Code", classified["marker_hits"])

    def test_counts_tools_without_dumping_schemas(self) -> None:
        classified = body.classify_body(
            {
                "system": DISPLACER,
                "messages": [{"role": "user", "content": USER}],
                "tools": [
                    {"name": "Bash", "input_schema": {"type": "object"}},
                    {"name": "Read", "input_schema": {"type": "object"}},
                ],
            },
            DISPLACER,
            USER,
        )
        self.assertEqual(classified["tool_count"], 2)
        self.assertEqual(classified["tool_names"], ["Bash", "Read"])
        self.assertEqual(classified["system_parts"][0]["text"], DISPLACER)


class ScrubEmails(unittest.TestCase):
    def test_replaces_addresses_in_nested_text(self) -> None:
        scrubbed = body.scrub_emails(
            {"text": "The user's email address is someone@example.com."}
        )
        self.assertEqual(
            scrubbed["text"],
            "The user's email address is <USER_EMAIL>.",
        )

    def test_encode_scrubs_addresses(self) -> None:
        encoded = body.encode_body_receipt(
            {"note": "The user's email address is someone@example.com."}
        )
        self.assertIn("<USER_EMAIL>", encoded)
        self.assertNotIn("someone@example.com", encoded)

    def test_refuses_a_surviving_address(self) -> None:
        with self.assertRaises(body.MeasurementError) as raised:
            body.refuse_remaining_emails(
                '{"note": "contact still-there@example.com"}'
            )
        self.assertIn("email", str(raised.exception))


class SummarizeClassified(unittest.TestCase):
    def test_splits_quota_title_and_main(self) -> None:
        classified = [
            {
                "n": 1,
                "bytes": 10,
                "classification": {
                    "model": "claude-haiku-4-5-20251001",
                    "system_chars": 0,
                    "displacer_in_system": False,
                },
            },
            {
                "n": 2,
                "bytes": 4000,
                "classification": {
                    "model": "claude-haiku-4-5-20251001",
                    "system_chars": 3197,
                    "displacer_in_system": False,
                },
            },
            {
                "n": 3,
                "bytes": 1825,
                "classification": body.classify_body(
                    {
                        "model": "claude-sonnet-5",
                        "system": [
                            {
                                "type": "text",
                                "text": "x-anthropic-billing-header: cc_version=1;",
                            },
                            {
                                "type": "text",
                                "text": "You are Claude Code, Anthropic's official CLI for Claude.",
                            },
                            {"type": "text", "text": DISPLACER},
                        ],
                        "messages": [
                            {
                                "role": "user",
                                "content": [
                                    {
                                        "type": "text",
                                        "text": "<system-reminder># userEmail</system-reminder>",
                                    },
                                    {"type": "text", "text": USER},
                                ],
                            },
                            {
                                "role": "system",
                                "content": "<system-reminder><total_tokens>1</total_tokens></system-reminder>",
                            },
                        ],
                        "tools": [],
                    },
                    DISPLACER,
                    USER,
                ),
            },
        ]
        summary = body.summarize_classified(classified)
        self.assertEqual([row["n"] for row in summary["quota_turns"]], [1])
        self.assertEqual([row["n"] for row in summary["title_turns"]], [2])
        self.assertEqual([row["n"] for row in summary["main_turns"]], [3])
        self.assertTrue(summary["what_the_armed_turn_still_sends"]["billing_header"])
        self.assertEqual(summary["main_turns"][0]["tool_count"], 0)

    def test_refuses_captures_without_an_armed_turn(self) -> None:
        with self.assertRaises(body.MeasurementError):
            body.summarize_classified(
                [
                    {
                        "n": 1,
                        "classification": {
                            "model": "claude-haiku-4-5-20251001",
                            "system_chars": 0,
                            "displacer_in_system": False,
                        },
                    }
                ]
            )


if __name__ == "__main__":
    unittest.main()
