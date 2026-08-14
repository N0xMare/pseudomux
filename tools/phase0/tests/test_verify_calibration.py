"""Offline tests for tools/phase0/verify_calibration.py.

Every fixture here is a synthetic JSON/text tree built under a
`tempfile.TemporaryDirectory`, shaped like the real evidence
`phase0_lib.CampaignRunner` publishes (`attempt-<uuid>/{reservation.json,
outcome.json,pmux-<label>.stdout.<suffix>}`). No test in this file drives
pmux, Claude, rmux, or `phase0.py` itself -- that would spend the campaign's
real, budget-limited, credentialed attempts, and this module's whole job is
to be checkable without any of that.
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import re
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
PHASE0 = ROOT / "tools" / "phase0"
sys.path.insert(0, str(PHASE0))

import verify_calibration as vc  # noqa: E402


def sha256_hex(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


class HashExtractionTests(unittest.TestCase):
    def test_single_bare_hash_line_labels_as_poem(self) -> None:
        poem = "line one\nline two"
        final_text = f"{poem}\n\nSHA256: {sha256_hex(poem)}"
        body, hash_lines, meta = vc.extract_hash_lines_and_body(final_text)
        self.assertEqual(body, poem)
        self.assertEqual(len(hash_lines), 1)
        self.assertEqual(hash_lines[0].label, "poem")
        self.assertEqual(hash_lines[0].reported_hex, sha256_hex(poem))
        self.assertTrue(meta["separator_present"])
        self.assertEqual(meta["extra_trailing_blanks"], 0)

    def test_multiple_labelled_hash_lines_keep_top_to_bottom_order(self) -> None:
        poem = "autumn\nleaves fall"
        reversed_poem = poem[::-1]
        final_text = (
            f"{poem}\n\n"
            f"SHA256(poem): {sha256_hex(poem)}\n"
            f"SHA256(reversed): {sha256_hex(reversed_poem)}"
        )
        body, hash_lines, _ = vc.extract_hash_lines_and_body(final_text)
        self.assertEqual(body, poem)
        self.assertEqual([hl.label for hl in hash_lines], ["poem", "reversed"])

    def test_no_hash_line_returns_whole_text_as_body(self) -> None:
        final_text = "just a poem\nwith two lines"
        body, hash_lines, _ = vc.extract_hash_lines_and_body(final_text)
        self.assertEqual(body, final_text)
        self.assertEqual(hash_lines, [])

    def test_missing_blank_separator_is_noted_but_still_parsed(self) -> None:
        poem = "no blank line above this"
        final_text = f"{poem}\nSHA256: {sha256_hex(poem)}"
        body, hash_lines, meta = vc.extract_hash_lines_and_body(final_text)
        self.assertEqual(body, poem)
        self.assertEqual(len(hash_lines), 1)
        self.assertFalse(meta["separator_present"])

    def test_trailing_blank_lines_after_the_hash_are_ignored(self) -> None:
        poem = "poem text"
        final_text = f"{poem}\n\nSHA256: {sha256_hex(poem)}\n\n\n"
        body, hash_lines, _ = vc.extract_hash_lines_and_body(final_text)
        self.assertEqual(body, poem)
        self.assertEqual(len(hash_lines), 1)

    def test_case_insensitive_hex_is_lowercased_on_extraction(self) -> None:
        poem = "poem"
        final_text = f"{poem}\n\nSHA256: {sha256_hex(poem).upper()}"
        _, hash_lines, _ = vc.extract_hash_lines_and_body(final_text)
        self.assertEqual(hash_lines[0].reported_hex, sha256_hex(poem))


class CandidateEncodingTests(unittest.TestCase):
    def test_exact_and_trailing_newline_variants_are_always_offered(self) -> None:
        variants = dict(vc.candidate_encodings("poem"))
        self.assertEqual(variants["exact"], b"poem")
        self.assertEqual(variants["trailing_newline"], b"poem\n")

    def test_nfc_variants_are_only_added_when_they_differ(self) -> None:
        ascii_variants = vc.candidate_encodings("plain ascii")
        self.assertEqual(
            [label for label, _ in ascii_variants], ["exact", "trailing_newline"]
        )
        # U+0065 U+0301 (e + combining acute) normalizes to U+00E9 (é) under NFC.
        decomposed = "café"
        nfc_variants = dict(vc.candidate_encodings(decomposed))
        self.assertIn("nfc", nfc_variants)
        self.assertEqual(nfc_variants["nfc"], "café".encode("utf-8"))

    def test_duplicate_byte_encodings_are_not_repeated(self) -> None:
        # Already-composed text: NFC-normalizing it is a no-op, so only the
        # two base variants should be produced, not four.
        variants = vc.candidate_encodings("café")
        self.assertEqual(len(variants), 2)


class VerifyHashLinesTests(unittest.TestCase):
    def test_exact_match(self) -> None:
        body = "the poem"
        line = vc.HashLine("poem", sha256_hex(body), "SHA256: ...")
        checks = vc.verify_hash_lines(body, [line])
        self.assertTrue(checks[0]["match"])
        self.assertEqual(checks[0]["matched_variant"], "exact")

    def test_trailing_newline_variant_still_matches(self) -> None:
        body = "the poem"
        line = vc.HashLine("poem", sha256_hex(body + "\n"), "SHA256: ...")
        checks = vc.verify_hash_lines(body, [line])
        self.assertTrue(checks[0]["match"])
        self.assertEqual(checks[0]["matched_variant"], "trailing_newline")

    def test_mismatch_when_no_variant_reproduces_the_hash(self) -> None:
        body = "the real poem"
        line = vc.HashLine("poem", sha256_hex("a fabricated poem"), "SHA256: ...")
        checks = vc.verify_hash_lines(body, [line])
        self.assertFalse(checks[0]["match"])
        self.assertIsNone(checks[0]["matched_variant"])
        self.assertEqual(checks[0]["recomputed_example"], sha256_hex(body))

    def test_reversed_transform_is_applied_before_hashing(self) -> None:
        body = "abcdef"
        reversed_body = body[::-1]
        line = vc.HashLine(
            "reversed", sha256_hex(reversed_body), "SHA256(reversed): ..."
        )
        checks = vc.verify_hash_lines(body, [line])
        self.assertTrue(checks[0]["match"])

    def test_transform_applies_to_each_newline_variant_not_to_its_output(
        self,
    ) -> None:
        # A heredoc LF-terminates the last line, so the bytes Claude hashes are
        # `body + "\n"`. Appending that newline does not commute with reversal:
        # reverse(body + "\n") == "\n" + reverse(body). Transforming once and
        # then trying newline variants of the result can never reproduce this,
        # so every `reversed` grade fed by a heredoc reported a false mismatch.
        body = "line one\nline two\nline three"
        fed = body + "\n"
        for label in ("poem", "reversed", "upper"):
            with self.subTest(label=label):
                reported = sha256_hex(vc.TRANSFORMS[label](fed))
                line = vc.HashLine(label, reported, f"SHA256({label}): ...")
                checks = vc.verify_hash_lines(body, [line])
                self.assertTrue(checks[0]["match"], f"{label} did not verify")
                self.assertEqual(checks[0]["matched_variant"], "trailing_newline")

    def test_reversed_still_mismatches_a_genuinely_wrong_hash(self) -> None:
        # The newline fix must not become a wildcard that matches anything.
        body = "line one\nline two"
        line = vc.HashLine(
            "reversed", sha256_hex("a different poem"), "SHA256(reversed): ..."
        )
        checks = vc.verify_hash_lines(body, [line])
        self.assertFalse(checks[0]["match"])
        self.assertIsNone(checks[0]["matched_variant"])

    def test_unknown_label_is_reported_not_guessed(self) -> None:
        line = vc.HashLine("rot13", "0" * 64, "SHA256(rot13): ...")
        checks = vc.verify_hash_lines("body", [line])
        self.assertFalse(checks[0]["match"])
        self.assertEqual(checks[0]["reason"], "unknown_transform_label")


class ComputeGapTests(unittest.TestCase):
    def test_positive_gap_is_a_late_row(self) -> None:
        gap, reason = vc.compute_gap(
            {"terminal_candidate_at_ms": 9_000, "last_transcript_activity_at_ms": 9_600}
        )
        self.assertEqual(gap, 600)
        self.assertIsNone(reason)

    def test_negative_gap_is_kept_signed_not_clamped(self) -> None:
        gap, reason = vc.compute_gap(
            {"terminal_candidate_at_ms": 9_000, "last_transcript_activity_at_ms": 8_970}
        )
        self.assertEqual(gap, -30)
        self.assertIsNone(reason)

    def test_zero_gap_is_computable_and_counted_as_no_late_row_by_summarize(
        self,
    ) -> None:
        gap, reason = vc.compute_gap(
            {"terminal_candidate_at_ms": 9_000, "last_transcript_activity_at_ms": 9_000}
        )
        self.assertEqual(gap, 0)
        self.assertIsNone(reason)

    def test_field_not_published_is_a_distinct_reason(self) -> None:
        gap, reason = vc.compute_gap({"terminal_candidate_at_ms": 9_000})
        self.assertIsNone(gap)
        self.assertEqual(reason, "last_transcript_activity_at_ms_not_published")

    def test_missing_terminal_candidate_is_a_distinct_reason(self) -> None:
        gap, reason = vc.compute_gap({"last_transcript_activity_at_ms": 9_000})
        self.assertIsNone(gap)
        self.assertEqual(reason, "terminal_candidate_at_ms_absent")

    def test_non_integer_late_arrival_value_is_rejected(self) -> None:
        gap, reason = vc.compute_gap(
            {"terminal_candidate_at_ms": 9_000, "last_transcript_activity_at_ms": None}
        )
        self.assertIsNone(gap)
        self.assertEqual(reason, "last_transcript_activity_at_ms_not_an_integer")


class ComputeStopHookDeltaTests(unittest.TestCase):
    """`stop_hook_at_ms - last_transcript_activity_at_ms`. The sign is the
    entire answer, so every one of these tests is about the sign surviving."""

    def test_positive_delta_means_the_hook_followed_the_final_write(self) -> None:
        delta, reason = vc.compute_stop_hook_delta(
            {"last_transcript_activity_at_ms": 9_000, "stop_hook_at_ms": 9_120}
        )
        self.assertEqual(delta, 120)
        self.assertIsNone(reason)

    def test_negative_delta_is_kept_signed_not_clamped_or_absoluted(self) -> None:
        # The one observation that forbids the (stop_hook_observed ||
        # stable_for_ms >= drain) fast path. A max(0, ..), an abs() or an
        # unsigned cast anywhere on this path would turn "Stop fired 40 ms
        # before the last row was written" into "40 ms of safe headroom" or
        # into a benign zero, and the fast path would be built on it.
        delta, reason = vc.compute_stop_hook_delta(
            {"last_transcript_activity_at_ms": 9_000, "stop_hook_at_ms": 8_960}
        )
        self.assertEqual(delta, -40)
        self.assertIsNone(reason)

    def test_zero_delta_is_computable_and_neither_positive_nor_negative(self) -> None:
        delta, reason = vc.compute_stop_hook_delta(
            {"last_transcript_activity_at_ms": 9_000, "stop_hook_at_ms": 9_000}
        )
        self.assertEqual(delta, 0)
        self.assertIsNone(reason)
        summary = vc.summarize_stop_hook_deltas(
            [{"stop_hook_delta_ms": 0, "status": "pmux_exit_zero"}]
        )
        self.assertEqual(summary["zero"], 1)
        self.assertEqual(summary["positive"], 0)
        self.assertEqual(summary["negative"], 0)

    def test_absent_hook_field_is_uncomputable_with_a_reason_not_zero(self) -> None:
        delta, reason = vc.compute_stop_hook_delta(
            {"last_transcript_activity_at_ms": 9_000}
        )
        self.assertIsNone(delta)
        self.assertEqual(reason, "stop_hook_at_ms_not_published")
        self.assertIn(reason, vc.STOP_HOOK_UNCOMPUTABLE_REASONS)

    def test_absent_transcript_activity_is_a_distinct_reason(self) -> None:
        delta, reason = vc.compute_stop_hook_delta({"stop_hook_at_ms": 9_000})
        self.assertIsNone(delta)
        self.assertEqual(reason, "last_transcript_activity_at_ms_not_published")
        self.assertIn(reason, vc.STOP_HOOK_UNCOMPUTABLE_REASONS)

    def test_non_integer_values_are_rejected_rather_than_coerced(self) -> None:
        delta, reason = vc.compute_stop_hook_delta(
            {"last_transcript_activity_at_ms": 9_000, "stop_hook_at_ms": "9200"}
        )
        self.assertIsNone(delta)
        self.assertEqual(reason, "stop_hook_at_ms_not_an_integer")
        delta, reason = vc.compute_stop_hook_delta(
            {"last_transcript_activity_at_ms": None, "stop_hook_at_ms": 9_200}
        )
        self.assertIsNone(delta)
        self.assertEqual(reason, "last_transcript_activity_at_ms_not_an_integer")

    def test_booleans_are_not_accepted_as_millisecond_timestamps(self) -> None:
        delta, reason = vc.compute_stop_hook_delta(
            {"last_transcript_activity_at_ms": 9_000, "stop_hook_at_ms": True}
        )
        self.assertIsNone(delta)
        self.assertEqual(reason, "stop_hook_at_ms_not_an_integer")

    def test_a_single_negative_is_not_averaged_away_by_many_positives(self) -> None:
        # The tally is the answer precisely because the mean is not: nine
        # positives and one negative average to a comfortable positive, and the
        # fast path is still forbidden.
        records = [
            {"stop_hook_delta_ms": value, "status": "pmux_exit_zero"}
            for value in [400] * 9 + [-5]
        ]
        summary = vc.summarize_stop_hook_deltas(records)
        self.assertEqual(summary["negative"], 1)
        self.assertEqual(summary["positive"], 9)
        self.assertEqual(summary["min"], -5)

    def test_no_samples_summarizes_to_none_rather_than_an_empty_positive_run(
        self,
    ) -> None:
        self.assertIsNone(
            vc.summarize_stop_hook_deltas(
                [{"stop_hook_delta_ms": None, "status": "pmux_exit_zero"}]
            )
        )

    def test_the_stop_hook_is_not_confused_with_the_late_arrival_gap(self) -> None:
        # Both quantities read `last_transcript_activity_at_ms`, and phase0_lib
        # lists `stop_hook_at_ms` in KNOWN_TURN_TIMING_FIELDS so the
        # late-arrival field stays discoverable. Computing either gap from the
        # other's timestamp is the wrong-number failure that naming buys off.
        timings = {
            "terminal_candidate_at_ms": 9_000,
            "last_transcript_activity_at_ms": 9_400,
            "stop_hook_at_ms": 9_450,
        }
        self.assertEqual(vc.compute_gap(timings), (400, None))
        self.assertEqual(vc.compute_stop_hook_delta(timings), (50, None))


class NearestRankTests(unittest.TestCase):
    def test_matches_the_worked_example_used_for_phase0_libs_own_summary(self) -> None:
        # Mirrors DrainCalibrationTests.test_summary_reports_the_distribution_
        # and_its_headroom in tools/phase0/tests/test_phase0.py, so both the
        # acquisition library and this independent checker agree on what
        # "p95" means for the same four samples.
        gaps = sorted((0, 120, 400, 1_500))
        self.assertEqual(vc.nearest_rank(gaps, 50), 120)
        self.assertEqual(vc.nearest_rank(gaps, 95), 1_500)

    def test_single_sample_is_its_own_every_percentile(self) -> None:
        self.assertEqual(vc.nearest_rank([42], 50), 42)
        self.assertEqual(vc.nearest_rank([42], 95), 42)


class ParseTurnResultTests(unittest.TestCase):
    def test_json_stdout_is_the_turn_result_directly(self) -> None:
        payload = json.dumps({"text": "hi", "final_blocks": []}).encode("utf-8")
        result = vc.parse_turn_result(payload, "json")
        self.assertEqual(result["text"], "hi")

    def test_ndjson_stdout_uses_the_final_result_records_data(self) -> None:
        lines = [
            json.dumps({"type": "progress", "data": {}}),
            json.dumps({"type": "result", "data": {"text": "final"}}),
        ]
        payload = ("\n".join(lines) + "\n").encode("utf-8")
        result = vc.parse_turn_result(payload, "ndjson")
        self.assertEqual(result["text"], "final")

    def test_ndjson_without_a_trailing_result_record_is_rejected(self) -> None:
        payload = json.dumps({"type": "progress", "data": {}}).encode("utf-8")
        with self.assertRaises(vc.VerifyError):
            vc.parse_turn_result(payload, "ndjson")

    def test_ndjson_with_a_result_record_that_is_not_last_is_rejected(self) -> None:
        lines = [
            json.dumps({"type": "result", "data": {"text": "final"}}),
            json.dumps({"type": "progress", "data": {}}),
        ]
        payload = ("\n".join(lines) + "\n").encode("utf-8")
        with self.assertRaises(vc.VerifyError):
            vc.parse_turn_result(payload, "ndjson")

    def test_final_text_concatenates_blocks_with_no_separator(self) -> None:
        # Must match `final_text_blocks.concat()` at
        # crates/claude/src/engine.rs:843, which uses an EMPTY separator. This
        # test previously asserted "first\nsecond", pinning a separator pmux
        # never emits; a chunked terminal message would then hash to a digest
        # Claude could not have reported, and the run would blame pmux.
        result = {
            "text": "ignored fallback",
            "final_blocks": [
                {"kind": "text", "text": "first"},
                {"kind": "tool_use", "id": "x", "name": "bash", "input": {}},
                {"kind": "text", "text": "second"},
            ],
        }
        self.assertEqual(vc.final_text_from_turn_result(result), "firstsecond")

    def test_chunked_poem_reassembles_to_a_verifiable_hash(self) -> None:
        # The end-to-end consequence: a poem split across text blocks must
        # still reproduce the digest Claude computed over the whole poem.
        poem = "line one\nline two\nline three"
        cut = len(poem) // 2
        result = {
            "text": poem,
            "final_blocks": [
                {"kind": "text", "text": poem[:cut]},
                {"kind": "text", "text": poem[cut:]},
            ],
        }
        body = vc.final_text_from_turn_result(result)
        self.assertEqual(body, poem)
        line = vc.HashLine("poem", sha256_hex(poem + "\n"), "SHA256: ...")
        self.assertTrue(vc.verify_hash_lines(body, [line])[0]["match"])

    def test_final_text_falls_back_to_text_field_when_no_text_blocks(self) -> None:
        result = {"text": "fallback text", "final_blocks": []}
        self.assertEqual(vc.final_text_from_turn_result(result), "fallback text")


class SyntheticEvidenceCase(unittest.TestCase):
    """Builds a tiny, correctly-shaped `attempt-<uuid>/` evidence directory on
    disk, matching phase0_lib.CampaignRunner's publication layout closely
    enough for verify_calibration.py, without any of phase0_lib's ownership/
    symlink/atomic-rename hardening (that hardening is what makes evidence
    trustworthy; it is not what this reporting tool is testing)."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.evidence_root = self.root / "evidence"
        self.evidence_root.mkdir()
        self.prompts_dir = self.root / "prompts"
        self.prompts_dir.mkdir()

    def write_prompt(self, name: str, text: str) -> Path:
        path = self.prompts_dir / name
        path.write_text(text, encoding="utf-8")
        return path

    def write_attempt(
        self,
        *,
        suite_index: int,
        ordinal: int,
        effort: str = "low",
        status: str = "pmux_exit_zero",
        terminal_candidate_at_ms: int | None = 9_000,
        last_transcript_activity_at_ms: int | None = 9_000,
        # Default None, matching every attempt published before
        # TurnTimings::stop_hook_at_ms existed: the field is omitted entirely
        # rather than written as 0, because a 0 would be a positive ordering
        # sample the campaign never took.
        stop_hook_at_ms: int | None = None,
        # Both default to absent, which is what a pre-graduation build
        # published: no end-of-turn marker was timed and no paid-drain figure
        # was reported. Writing 0 instead would assert a marker was seen at
        # epoch and a commit was taken at zero stability, neither of which any
        # campaign observed.
        turn_duration_observed_at_ms: int | None = None,
        drain_ms: int | None = None,
        # `None` omits `compatibility.transcript_drain_ms` entirely, which is
        # what an attempt published by a build that reported no compatibility
        # block looks like. Distinct from 0.
        transcript_drain_ms: int | None = 2_000,
        final_text: str | None = None,
        final_blocks: list[dict] | None = None,
        label: str = "run",
        suffix: str = "json",
        prompt_sha256_override: str | None = None,
    ) -> Path:
        attempt_id = str(uuid.uuid4())
        attempt_dir = self.evidence_root / f"attempt-{attempt_id}"
        attempt_dir.mkdir()
        prompt_files = sorted(self.prompts_dir.glob("*.txt"))
        prompt_sha256 = prompt_sha256_override
        if prompt_sha256 is None and 1 <= suite_index <= len(prompt_files):
            prompt_sha256 = hashlib.sha256(
                prompt_files[suite_index - 1].read_bytes()
            ).hexdigest()
        reservation = {
            "attempt_id": attempt_id,
            "global_attempt_ordinal": ordinal,
            "prompt_suite_index": suite_index,
            "cell": {"effort": effort},
            "prompt": {"sha256": prompt_sha256},
        }
        (attempt_dir / "reservation.json").write_text(json.dumps(reservation))

        if status != "pmux_exit_zero":
            outcome = {
                "status": status,
                "error": "synthetic failure",
                "public_result_binding": None,
            }
            (attempt_dir / "outcome.json").write_text(json.dumps(outcome))
            return attempt_dir

        timings: dict[str, object] = {
            "submitted_at_ms": 1_000,
            "completed_at_ms": 11_400,
        }
        if terminal_candidate_at_ms is not None:
            timings["terminal_candidate_at_ms"] = terminal_candidate_at_ms
        if last_transcript_activity_at_ms is not None:
            timings["last_transcript_activity_at_ms"] = last_transcript_activity_at_ms
        if stop_hook_at_ms is not None:
            timings["stop_hook_at_ms"] = stop_hook_at_ms
        if turn_duration_observed_at_ms is not None:
            timings["turn_duration_observed_at_ms"] = turn_duration_observed_at_ms
        if drain_ms is not None:
            timings["drain_ms"] = drain_ms
        outcome = {
            "status": "pmux_exit_zero",
            "error": None,
            "public_result_binding": {
                "timings": timings,
                "compatibility": (
                    {}
                    if transcript_drain_ms is None
                    else {"transcript_drain_ms": transcript_drain_ms}
                ),
            },
        }
        (attempt_dir / "outcome.json").write_text(json.dumps(outcome))

        turn_result = {
            "text": final_text if final_text is not None else "",
            "final_blocks": final_blocks if final_blocks is not None else [],
        }
        payload = json.dumps(turn_result) if suffix == "json" else None
        if suffix == "ndjson":
            payload = (
                json.dumps({"type": "progress", "data": {}})
                + "\n"
                + json.dumps({"type": "result", "data": turn_result})
            )
        (attempt_dir / f"pmux-{label}.stdout.{suffix}").write_text(payload)
        return attempt_dir

    def write_unreadable_attempt(self) -> Path:
        """An attempt directory with no reservation.json: a spent ordinal the
        tool cannot attribute to any grade. It used to appear in no row of the
        report at all -- not in the header counts, not in the by-grade table."""

        attempt_dir = self.evidence_root / f"attempt-{uuid.uuid4()}"
        attempt_dir.mkdir()
        return attempt_dir

    def run_main(self, extra_args: list[str] | None = None) -> tuple[int, str]:
        buffer = io.StringIO()
        argv = [
            "--evidence-root",
            str(self.evidence_root),
            "--prompts-dir",
            str(self.prompts_dir),
            *(extra_args or []),
        ]
        with contextlib.redirect_stdout(buffer):
            exit_code = vc.main(argv)
        return exit_code, buffer.getvalue()


class EndToEndHashVerificationTests(SyntheticEvidenceCase):
    def test_hash_match_is_reported_and_exits_zero(self) -> None:
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        poem = "roses are red\nviolets are blue"
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text=f"{poem}\n\nSHA256: {sha256_hex(poem)}",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("'match': 1", output.replace('"', "'"))

    def test_hash_mismatch_is_reported_loudly_and_exits_nonzero(self) -> None:
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        poem = "roses are red\nviolets are blue"
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text=f"{poem}\n\nSHA256: {sha256_hex('not the poem at all')}",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 1)
        self.assertIn("HASH MISMATCHES", output)

    def test_missing_field_status_is_reported_as_incomplete(self) -> None:
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        attempt_dir = self.write_attempt(suite_index=1, ordinal=1)
        (attempt_dir / "outcome.json").unlink()
        exit_code, output = self.run_main()
        # CHANGED 2026-07-28: this asserted exit 0. The tree's only attempt is
        # incomplete, so nothing in it is calibratable and no gap is computable
        # anywhere -- a run that measured nothing must not report success.
        self.assertEqual(exit_code, 1)
        self.assertIn("incomplete: 1", output)

    def test_incomplete_attempt_is_no_result_not_not_applicable(self) -> None:
        # A crashed attempt on a hash-requesting grade must not be tallied
        # the same way as a grade that never asked for a hash: conflating
        # "nothing to check" with "never got to check it" is exactly the
        # absence-of-evidence mistake this tool exists to avoid.
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        attempt_dir = self.write_attempt(suite_index=1, ordinal=1)
        (attempt_dir / "outcome.json").unlink()
        exit_code, output = self.run_main(["--json"])
        # CHANGED 2026-07-28: was exit 0. The tree's only attempt is incomplete,
        # so no gap is computable at all. The hash claim under test is unchanged.
        self.assertEqual(exit_code, 1)
        parsed = json.loads(output)
        self.assertEqual(parsed["attempts"][0]["hash_overall"], "no_result")
        self.assertNotIn("not_applicable", parsed["hash_tally_overall"])

    def test_failed_attempt_is_no_result_not_not_applicable(self) -> None:
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        self.write_attempt(suite_index=1, ordinal=1, status="failed")
        exit_code, output = self.run_main(["--json"])
        # CHANGED 2026-07-28: was exit 0, for the same reason -- the only
        # attempt failed, so this run calibrates nothing.
        self.assertEqual(exit_code, 1)
        parsed = json.loads(output)
        self.assertEqual(parsed["attempts"][0]["hash_overall"], "no_result")

    def test_hash_expected_but_absent_is_distinguished_from_not_applicable(
        self,
    ) -> None:
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        self.write_attempt(
            suite_index=1, ordinal=1, final_text="just a poem, no hash line"
        )
        exit_code, output = self.run_main()
        # CHANGED 2026-07-28: was exit 0. A correctly DETECTED missing
        # proof-of-work still exited 0, so any script gating on the status saw
        # success (fix plan 1.4's last paragraph).
        self.assertEqual(exit_code, 1)
        self.assertIn("hash expected but absent", output)
        self.assertIn("reply carried none", output)


class EndToEndGapDistributionTests(SyntheticEvidenceCase):
    def test_negative_gap_attempt_counts_as_no_late_row(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=8_950,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("min=-50", output)
        self.assertIn("no-late-row=1", output)

    def test_zero_gap_attempt_counts_as_no_late_row_and_flags_absence(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_000,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("ABSENCE OF EVIDENCE", output)
        self.assertIn("Do not read it as permission to cut transcript_drain_ms", output)

    def test_the_printed_noise_band_line_carries_the_resolved_poll_interval(
        self,
    ) -> None:
        # The one assertion in this file that grades the line a reader actually
        # sees. `BannerCitationTests` grades module constants, and for this
        # citation that was not enough: the printed line carried
        # `actor.rs:83` -- the poll interval is on 85 -- for as long as anyone
        # can date, while the test named for it graded a comment two hundred
        # lines away that happened to be right.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_600,
            final_text="ok",
        )
        _, output = self.run_main()
        self.assertIn(
            f"ms of zero (one actor poll interval, {vc.ACTOR_POLL_INTERVAL_CITATION})",
            output,
        )

    def test_a_measured_late_row_suppresses_the_absence_of_evidence_banner(
        self,
    ) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_600,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertNotIn("ABSENCE OF EVIDENCE", output)
        self.assertIn("late-row=1", output)

    def test_mixed_grades_are_broken_out_separately_in_suite_order(self) -> None:
        self.write_prompt("01-baseline-trivial.txt", "reply ok")
        self.write_prompt("02-poem-hash.txt", "write a poem, report SHA256")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_000,
            final_text="ok",
        )
        poem = "a short poem"
        self.write_attempt(
            suite_index=2,
            ordinal=2,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_900,
            final_text=f"{poem}\n\nSHA256: {sha256_hex(poem)}",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        baseline_line = next(
            line
            for line in output.splitlines()
            if line.strip().startswith("01-baseline-trivial:")
        )
        poem_line = next(
            line
            for line in output.splitlines()
            if line.strip().startswith("02-poem-hash:")
        )
        self.assertIn("no-late-row=1", baseline_line)
        self.assertIn("late-row=1", poem_line)

    def test_field_not_published_attempt_is_excluded_from_the_distribution(
        self,
    ) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=None,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        # CHANGED 2026-07-28: was exit 0 with one bland line. This is the exact
        # shape a rename of LATE_ARRIVAL_FIELD would produce, and the module
        # docstring promises "a loud, safe failure rather than a silently wrong
        # number". Exiting 0 made the tripwire inert.
        self.assertEqual(exit_code, 1)
        self.assertIn("no attempt produced a computable gap", output)
        self.assertIn("NO COMPUTABLE LATE-ARRIVAL GAP ANYWHERE IN THIS RUN", output)
        self.assertIn("last_transcript_activity_at_ms_not_published: 1", output)


class ReportShapeTests(SyntheticEvidenceCase):
    def test_json_output_is_valid_json_and_carries_the_attempt_list(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_600,
            final_text="ok",
        )
        exit_code, output = self.run_main(["--json"])
        self.assertEqual(exit_code, 0)
        parsed = json.loads(output)
        self.assertEqual(len(parsed["attempts"]), 1)
        self.assertEqual(parsed["attempts"][0]["gap_ms"], 600)

    def test_ndjson_artifact_is_parsed_the_same_as_json(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_600,
            final_text="ok",
            label="turn",
            suffix="ndjson",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("late-row=1", output)

    def test_prompt_content_drift_is_flagged_without_crashing(self) -> None:
        # A prompt whose content is not in the prompts directory cannot be
        # graded by hash, so the index is used -- but the index names a
        # position, not a prompt, and the report must say so rather than
        # present the label as if it were established.
        #
        # CHANGED 2026-07-28: this asserted against `--json`, where the note has
        # always been present. The default command prints TEXT, and the text
        # said nothing -- so the test passed while DEFENDING the silence it was
        # written to prevent. A human running the default command is the reader
        # this claim is about, so the assertion now targets what that human sees.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text="ok",
            prompt_sha256_override="0" * 64,
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("not in the prompts directory", output)
        self.assertIn("graded by index, not content", output)
        # The by-grade row itself must carry the mark, so a reader scanning the
        # table cannot take the label for established.
        grade_line = next(
            line
            for line in output.splitlines()
            if line.strip().startswith("01-trivial:")
        )
        self.assertIn("[1 graded by index, not content]", grade_line)
        # And the JSON is unchanged, so nothing regressed for machine readers.
        _, json_output = self.run_main(["--json"])
        attempt = json.loads(json_output)["attempts"][0]
        self.assertEqual(attempt["grade_source"], "prompt_suite_index")
        self.assertTrue(
            any("not in the prompts directory" in note for note in attempt["notes"]),
            attempt["notes"],
        )

    def test_grade_comes_from_content_not_argv_position(self) -> None:
        # A resumed campaign that passes a SUBSET of the prompts restarts
        # prompt_suite_index at 1, so index 1 can carry the content of any
        # grade. Grading by position silently relabels every attempt.
        #
        # CHANGED 2026-07-28: asserted against `--json` for the same reason as
        # the drift test above. A grade established by content hash and a grade
        # guessed from an argv position rendered as the identical string in the
        # text report, which is how the grade-misattribution defect survived.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_prompt("02-second.txt", "write a poem")
        second_sha = hashlib.sha256(b"write a poem").hexdigest()
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text="ok",
            prompt_sha256_override=second_sha,
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        # The relabelling is stated, ...
        self.assertIn("would have labelled this 01-trivial.txt", output)
        self.assertIn("graded by content", output)
        # ... and the row is NOT marked, because this label WAS established.
        # Same string for both cases was the defect.
        self.assertNotIn("graded by index, not content", output)
        grade_line = next(
            line
            for line in output.splitlines()
            if line.strip().startswith("02-second:")
        )
        self.assertNotIn("[", grade_line)
        _, json_output = self.run_main(["--json"])
        attempt = json.loads(json_output)["attempts"][0]
        self.assertEqual(attempt["grade"], "02-second")
        self.assertEqual(attempt["grade_source"], "prompt_sha256")

    def test_only_low_effort_note_appears_when_no_medium_attempts_exist(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, effort="low", final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn('only "low" effort was exercised', output)

    def test_no_absence_note_needed_note_absent_once_medium_is_exercised(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, effort="medium", final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertNotIn('only "low" effort was exercised', output)


class SilentDetectionTests(SyntheticEvidenceCase):
    """Fix plan 2.1: the tool detected these things all along and the default
    text output showed none of them. Every assertion here is against the text a
    human running the default command actually reads."""

    def test_header_buckets_partition_the_discovered_attempts(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(suite_index=1, ordinal=2, status="failed")
        incomplete = self.write_attempt(suite_index=1, ordinal=3, final_text="ok")
        (incomplete / "outcome.json").unlink()
        self.write_unreadable_attempt()
        _, output = self.run_main()
        self.assertIn(
            "attempts discovered: 4 (successful: 1, failed: 1, incomplete: 1, "
            "unreadable: 1, fatal errors: 0)",
            output,
        )
        # The arithmetic is printed, because `discovered` vs
        # `(successful, incomplete, fatal)` left `failed` in no bucket at all.
        self.assertIn(
            "every discovered attempt is in exactly one bucket: "
            "1 + 1 + 1 + 1 + 0 = 4 (discovered 4)",
            output,
        )

    def test_failed_attempts_are_tallied_by_error_string_with_their_ordinals(
        self,
    ) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=7, status="failed")
        self.write_attempt(suite_index=1, ordinal=8, status="failed")
        self.write_attempt(suite_index=1, ordinal=9, final_text="ok")
        _, output = self.run_main()
        self.assertIn("attempts that ran and failed, by error string:", output)
        self.assertIn("2 x synthetic failure", output)
        self.assertIn("ordinal=7", output)
        self.assertIn("ordinal=8", output)

    def test_attempt_directory_without_a_reservation_is_still_reported(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        orphan = self.write_unreadable_attempt()
        _, output = self.run_main()
        self.assertIn("unreadable: 1", output)
        self.assertIn(str(orphan), output)
        self.assertIn("reservation.json missing", output)

    def test_notes_block_is_printed_even_when_no_attempt_has_notes(self) -> None:
        # An empty block and an omitted block must not look the same: the whole
        # defect class is "the tool recorded it somewhere nobody reads".
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("--- ATTEMPTS THIS REPORT COULD NOT FULLY GRADE ---", output)
        self.assertIn("(none: every discovered attempt graded cleanly)", output)

    def test_a_failed_attempts_error_string_reaches_the_text_report(self) -> None:
        # `notes.append(f"attempt did not succeed: {error}")` existed all along
        # and rendered nowhere, so a campaign that lost six ordinals to a
        # misfiring source fence printed the same report as a clean one.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(suite_index=1, ordinal=2, status="failed")
        _, output = self.run_main()
        self.assertIn("attempt did not succeed: synthetic failure", output)

    def test_formatting_notes_about_the_reply_reach_the_text_report(self) -> None:
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        poem = "roses are red"
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text=f"{poem}\nSHA256: {sha256_hex(poem)}",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("no blank line before the hash block", output)

    def test_missing_artifact_note_reaches_the_text_report(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        attempt_dir = self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        (attempt_dir / "pmux-run.stdout.json").unlink()
        _, output = self.run_main()
        self.assertIn("no pmux-{run,turn,claude-p}.stdout", output)

    def test_by_grade_heading_banners_any_index_graded_member(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(
            suite_index=1,
            ordinal=2,
            final_text="ok",
            prompt_sha256_override="0" * 64,
        )
        _, output = self.run_main()
        self.assertIn("grade label this tool did NOT establish", output)
        grade_line = next(
            line
            for line in output.splitlines()
            if line.strip().startswith("01-trivial:")
        )
        self.assertIn("[1 graded by index, not content]", grade_line)

    def test_a_clean_run_carries_no_grade_source_mark(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        _, output = self.run_main()
        self.assertNotIn("grade label this tool did NOT establish", output)
        self.assertIn("grade labels by source: {'prompt_sha256': 1}", output)


class UncomputableGapTests(SyntheticEvidenceCase):
    """Fix plan 2.4: `gap_uncomputable_reason` and
    `attempts_without_computable_gap` were computed and rendered nowhere, and a
    run that could calibrate nothing exited 0."""

    def test_a_dropped_sample_is_counted_and_its_reason_named(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_000,
            final_text="ok",
        )
        self.write_attempt(
            suite_index=1,
            ordinal=2,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=None,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        # A successful turn that published no timing is the field-rename
        # tripwire; `attempts=2 count=1` used to be the only trace of it.
        self.assertEqual(exit_code, 1)
        self.assertIn(
            "gaps NOT computable: 1 successful attempt(s) published no usable "
            "timing pair, by reason:",
            output,
        )
        self.assertIn("last_transcript_activity_at_ms_not_published: 1", output)
        grade_line = next(
            line
            for line in output.splitlines()
            if line.strip().startswith("01-trivial:")
        )
        self.assertIn("attempts=2 count=1", grade_line)
        self.assertIn("no-gap=1", grade_line)

    def test_absent_terminal_candidate_is_a_distinct_named_reason(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=None,
            last_transcript_activity_at_ms=9_000,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 1)
        self.assertIn("terminal_candidate_at_ms_absent: 1", output)

    def test_a_complete_run_states_that_no_sample_was_dropped(self) -> None:
        # The positive signal, not just the absence of a complaint: "every
        # sample is present" is a claim a reader can act on.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("every successful attempt published a computable gap", output)

    def test_incomplete_and_failed_attempts_are_not_counted_as_dropped_samples(
        self,
    ) -> None:
        # The gate must gate exactly its claim: a failed attempt never had
        # timings to publish, so counting it as a missing gap sample would add
        # false failures without adding protection.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(suite_index=1, ordinal=2, status="failed")
        incomplete = self.write_attempt(suite_index=1, ordinal=3, final_text="ok")
        (incomplete / "outcome.json").unlink()
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("every successful attempt published a computable gap", output)


class StopHookOrderingTests(SyntheticEvidenceCase):
    """Every assertion here is against the DEFAULT text output, because this
    quantity decides whether ~2,300 ms of per-turn drain is recoverable and a
    number that only exists under --json is silent detection."""

    def test_the_block_is_printed_even_when_no_attempt_timed_a_hook(self) -> None:
        # An omitted block and an empty one must not look the same.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn(
            "--- Stop-hook ordering "
            "(stop_hook_at_ms - last_transcript_activity_at_ms) ---",
            output,
        )
        self.assertIn("NO STOP-HOOK ORDERING SAMPLE IN THIS RUN", output)

    def test_absent_field_is_uncomputable_with_its_reason_never_zero(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn(
            "1 successful attempt(s) produced no stop-hook ordering sample "
            "(counted as uncomputable, NOT as zero), by reason:",
            output,
        )
        self.assertIn("stop_hook_at_ms_not_published: 1", output)
        self.assertIn("without the Hybrid lifecycle hook installed", output)
        # And it is not silently folded into the sign tally as a zero.
        self.assertNotIn("SIGN TALLY: count", output)
        _, json_output = self.run_main(["--json"])
        parsed = json.loads(json_output)
        self.assertIsNone(parsed["stop_hook_delta_distribution"])
        self.assertIsNone(parsed["stop_hook_ordering_permits_fast_path"])
        self.assertIsNone(parsed["attempts"][0]["stop_hook_delta_ms"])
        self.assertEqual(
            parsed["attempts"][0]["stop_hook_delta_uncomputable_reason"],
            "stop_hook_at_ms_not_published",
        )

    def test_a_positive_run_reports_the_sign_tally_and_still_hedges(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            last_transcript_activity_at_ms=9_000,
            stop_hook_at_ms=9_300,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("SIGN TALLY: count=1 positive=1 negative=0 zero=0", output)
        self.assertIn("magnitudes (secondary): min=300 median=300 max=300 ms", output)
        # Absence-of-evidence framing, mirroring the late-arrival banner: no
        # negative observed is not the same claim as ordering proved.
        self.assertIn("consistent so far, and only so far", output)
        self.assertIn("not a proof of ordering", output)
        self.assertIn("Do not cut the drain on this alone", output)

    def test_a_single_negative_is_unmissable_in_the_default_output(self) -> None:
        # Nine positives and one negative: the mean is comfortably positive and
        # the answer is still "do not build the fast path".
        self.write_prompt("01-trivial.txt", "reply ok")
        for ordinal in range(1, 10):
            self.write_attempt(
                suite_index=1,
                ordinal=ordinal,
                last_transcript_activity_at_ms=9_000,
                stop_hook_at_ms=9_400,
                final_text="ok",
            )
        self.write_attempt(
            suite_index=1,
            ordinal=10,
            last_transcript_activity_at_ms=9_000,
            stop_hook_at_ms=8_995,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        self.assertIn("SIGN TALLY: count=10 positive=9 negative=1 zero=0", output)
        self.assertIn("NEGATIVE STOP-HOOK ORDERING OBSERVED", output)
        self.assertIn("DO NOT BUILD THE FAST PATH", output)
        self.assertIn("most negative: -5 ms", output)
        self.assertIn("ordinal=10", output)
        self.assertIn("Stop preceded the last transcript write", output)
        self.assertNotIn("consistent so far", output)
        # A true reading about Claude's flush ordering is an OBSERVATION about
        # the world, not a defect in the audited evidence, so it must not
        # overwrite the product verdict.
        self.assertEqual(exit_code, 0)
        self.assertIn("exit code 0:", output)
        _, json_output = self.run_main(["--json"])
        parsed = json.loads(json_output)
        self.assertFalse(parsed["stop_hook_ordering_permits_fast_path"])
        self.assertEqual(len(parsed["stop_hook_negative_attempts"]), 1)
        self.assertEqual(
            parsed["stop_hook_negative_attempts"][0]["stop_hook_delta_ms"], -5
        )

    def test_the_two_distributions_are_reported_separately_not_averaged(self) -> None:
        # One attempt with a 0 ms late-arrival gap and a -50 ms stop-hook
        # delta. If the two were pooled, the gap distribution would acquire a
        # phantom -50 ms sample and the ordering answer would be diluted by a
        # number about drain length.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_000,
            stop_hook_at_ms=8_950,
            final_text="ok",
        )
        exit_code, output = self.run_main(["--json"])
        self.assertEqual(exit_code, 0)
        parsed = json.loads(output)
        self.assertEqual(parsed["overall_gap_distribution"]["count"], 1)
        self.assertEqual(parsed["overall_gap_distribution"]["min"], 0)
        self.assertEqual(parsed["overall_gap_distribution"]["max"], 0)
        self.assertEqual(parsed["stop_hook_delta_distribution"]["count"], 1)
        self.assertEqual(parsed["stop_hook_delta_distribution"]["negative"], 1)
        self.assertEqual(parsed["stop_hook_delta_distribution"]["min"], -50)
        _, text = self.run_main()
        # Two distinct headed sections in the text, in that order.
        self.assertLess(
            text.index("--- Late-arrival gap distribution"),
            text.index("--- Stop-hook ordering"),
        )

    def test_a_negative_sample_does_not_suppress_the_late_row_banner(self) -> None:
        # The two banners answer different questions and must both appear.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            terminal_candidate_at_ms=9_000,
            last_transcript_activity_at_ms=9_000,
            stop_hook_at_ms=8_900,
            final_text="ok",
        )
        _, output = self.run_main()
        self.assertIn("ABSENCE OF EVIDENCE", output)
        self.assertIn("NEGATIVE STOP-HOOK ORDERING OBSERVED", output)

    def test_a_complete_run_states_that_no_ordering_sample_was_dropped(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            last_transcript_activity_at_ms=9_000,
            stop_hook_at_ms=9_010,
            final_text="ok",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn(
            "every successful attempt published a computable stop-hook ordering sample",
            output,
        )

    def test_failed_attempts_are_not_counted_as_dropped_ordering_samples(self) -> None:
        # Same rule as the gap: a failed attempt had no timings for the field to
        # be missing from, so counting it would inflate the reason tally with
        # attempts that were never expected to publish one.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            last_transcript_activity_at_ms=9_000,
            stop_hook_at_ms=9_010,
            final_text="ok",
        )
        self.write_attempt(suite_index=1, ordinal=2, status="failed")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn(
            "every successful attempt published a computable stop-hook ordering sample",
            output,
        )

    def test_a_mixed_run_reports_both_the_tally_and_the_uncomputable_count(
        self,
    ) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            last_transcript_activity_at_ms=9_000,
            stop_hook_at_ms=9_200,
            final_text="ok",
        )
        self.write_attempt(suite_index=1, ordinal=2, final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("SIGN TALLY: count=1 positive=1 negative=0 zero=0", output)
        self.assertIn("stop_hook_at_ms_not_published: 1", output)


class VerdictTests(SyntheticEvidenceCase):
    def test_a_clean_run_prints_the_zero_verdict_it_returns(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 0)
        self.assertIn("--- VERDICT ---", output)
        self.assertIn("exit code 0:", output)

    def test_the_verdict_block_lists_every_condition_behind_a_nonzero_exit(
        self,
    ) -> None:
        # One tree, two independent failing conditions: a hash the tool cannot
        # reproduce and a successful attempt with no gap sample. Both must be
        # named -- a reader who fixes only the first must not be surprised.
        self.write_prompt("01-poem-hash.txt", "poem prompt asking for SHA256")
        poem = "roses are red"
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text=f"{poem}\n\nSHA256: {sha256_hex('a different poem')}",
        )
        self.write_attempt(
            suite_index=1,
            ordinal=2,
            last_transcript_activity_at_ms=None,
            final_text=f"{poem}\n\nSHA256: {sha256_hex(poem)}",
        )
        exit_code, output = self.run_main()
        self.assertEqual(exit_code, 1)
        verdict = output.split("--- VERDICT ---", 1)[1]
        self.assertIn("exit code 1: 2 condition(s)", verdict)
        self.assertIn("could not reproduce", verdict)
        self.assertIn("SUCCESSFUL attempt(s) published no computable gap", verdict)

    def test_an_observation_alone_never_changes_the_exit_status(self) -> None:
        # Notes, an absence-of-evidence banner and an index-graded row are
        # OBSERVATIONS. They must be loud in the text and must not fail the run:
        # a harness observation must not overwrite the product verdict.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text="ok",
            prompt_sha256_override="0" * 64,
        )
        exit_code, output = self.run_main()
        self.assertIn("ABSENCE OF EVIDENCE", output)
        self.assertIn("graded by index, not content", output)
        self.assertEqual(exit_code, 0)
        self.assertIn("exit code 0:", output)


class DeriveRequiredDrainTests(unittest.TestCase):
    """The EFFECTIVE drain, unit by unit.

    `compatibility.transcript_drain_ms` is what was configured. On a graduated
    build the commit gate required `graduated_drain_ms(configured,
    turn_duration_seen)` (crates/service/src/v1/backend.rs:244), which is 250
    when the end-of-turn marker was seen. Reading the first and calling it the
    second is Defect 1.
    """

    def test_no_marker_owes_the_full_configured_drain(self) -> None:
        # `graduated_drain_ms(2000, false) == 2000` (backend.rs:291).
        result = vc.derive_required_drain(False, 2_000, 2_400)
        self.assertEqual(result.required_ms, 2_000)
        self.assertEqual(result.lower_bound_ms, 2_000)
        self.assertEqual(result.state, "not_graduated_no_marker")

    def test_marker_plus_a_commit_below_the_configured_drain_proves_graduation(
        self,
    ) -> None:
        # The one-directional inference: the gate is `stable_for_ms >=
        # required`, so committing at 570 ms of stability is IMPOSSIBLE if the
        # gate required 2,000. The marker was seen, so the required value is
        # the floor.
        result = vc.derive_required_drain(True, 2_000, 570)
        self.assertEqual(result.required_ms, vc.TURN_DURATION_DRAIN_FLOOR_MS)
        self.assertEqual(result.required_ms, 250)
        self.assertEqual(result.state, "graduated")

    def test_a_commit_below_the_floor_refutes_graduation_instead_of_proving_it(
        self,
    ) -> None:
        # The same one-directional gate read from the other side. `stable_for_ms
        # >= required` makes a commit at 55 ms of stability impossible if the
        # gate required 250, so the marker does NOT establish the floor here --
        # it is refuted. A minified-cell fast-path turn (actor.rs:3007-3018)
        # commits exactly like this, and TurnResult publishes no cell field, so
        # calling it "graduated, required 250" would credit the attempt with 5x
        # the proof its own timings carry.
        result = vc.derive_required_drain(True, 2_000, 55)
        self.assertIsNone(result.required_ms)
        self.assertEqual(result.state, "below_graduated_floor")
        # Bounded by what was paid, and the bound is what any margin claim is
        # taken against -- the direction that cannot overstate margin.
        self.assertEqual(result.lower_bound_ms, 0)
        # The boundary itself still graduates: 250 satisfies a 250 ms gate.
        boundary = vc.derive_required_drain(
            True, 2_000, vc.TURN_DURATION_DRAIN_FLOOR_MS
        )
        self.assertEqual(boundary.state, "graduated")
        self.assertEqual(boundary.required_ms, 250)

    def test_a_marker_with_a_quiet_transcript_proves_nothing_either_way(
        self,
    ) -> None:
        # The CONVERSE does not hold and must not be claimed. A graduated turn
        # whose transcript simply stayed quiet for 3 s reports drain_ms >=
        # configured too, so this can only be bounded, never resolved.
        # Claiming "not graduated" here would manufacture 1,750 ms of margin
        # out of a quiet transcript.
        result = vc.derive_required_drain(True, 2_000, 3_000)
        self.assertIsNone(result.required_ms)
        self.assertEqual(result.lower_bound_ms, 250)
        self.assertEqual(result.state, "graduation_indeterminate")

    def test_a_missing_paid_drain_figure_is_indeterminate_not_graduated(
        self,
    ) -> None:
        # An older evidence tree that published no `drain_ms` at all: the
        # marker is not enough on its own, because this tool cannot see which
        # build wrote the tree.
        result = vc.derive_required_drain(True, 2_000, None)
        self.assertIsNone(result.required_ms)
        self.assertEqual(result.lower_bound_ms, 250)
        self.assertEqual(result.state, "graduation_indeterminate")

    def test_a_configured_drain_at_or_below_the_floor_cannot_graduate(
        self,
    ) -> None:
        # `graduated_drain_ms(10, true) == 10` (backend.rs:297): the floor only
        # applies when it is STRICTLY below the configured value. A tree like
        # crates/service/tests/completion_gate.rs:389 (transcript_drain_ms: 10)
        # can never exercise the graduated path, and must not be described as
        # if it had.
        result = vc.derive_required_drain(True, 10, 5)
        self.assertEqual(result.required_ms, 10)
        self.assertEqual(result.state, "floor_not_binding")

    def test_an_unpublished_configured_drain_says_nothing(self) -> None:
        for configured in (None, "2000", True):
            with self.subTest(configured=configured):
                result = vc.derive_required_drain(True, configured, 570)
                self.assertIsNone(result.required_ms)
                self.assertIsNone(result.lower_bound_ms)
                self.assertEqual(result.state, "unknown")

    def test_every_state_it_can_return_has_a_rendered_explanation(self) -> None:
        # A state with no label renders as "no explanation is recorded", which
        # is exactly the silence this report exists to prevent.
        self.assertEqual(set(vc.GRADUATION_STATES), set(vc.GRADUATION_STATE_LABELS))


class SummarizeRequiredDrainTests(unittest.TestCase):
    def test_one_established_value_rolls_up(self) -> None:
        rolled = vc.summarize_required_drain(
            [vc.RequiredDrain(250, 250, "graduated")] * 3
        )
        self.assertEqual(rolled.required_ms, 250)
        self.assertEqual(rolled.lower_bound_ms, 250)

    def test_a_single_unresolved_attempt_withholds_the_run_level_value(
        self,
    ) -> None:
        # Not "8 of 9 agree, call it 250". The headroom line is taken against
        # this number, so a partial answer must read as no answer plus a bound.
        rolled = vc.summarize_required_drain(
            [vc.RequiredDrain(250, 250, "graduated")] * 8
            + [vc.RequiredDrain(None, 250, "graduation_indeterminate")]
        )
        self.assertIsNone(rolled.required_ms)
        self.assertEqual(rolled.lower_bound_ms, 250)
        self.assertIn("did not establish", rolled.note)

    def test_disagreeing_attempts_do_not_average(self) -> None:
        rolled = vc.summarize_required_drain(
            [
                vc.RequiredDrain(250, 250, "graduated"),
                vc.RequiredDrain(2_000, 2_000, "not_graduated_no_marker"),
            ]
        )
        self.assertIsNone(rolled.required_ms)
        self.assertEqual(rolled.lower_bound_ms, 250)
        self.assertIn("varies across attempts", rolled.note)

    def test_an_unknown_attempt_erases_the_lower_bound_too(self) -> None:
        # A bound is a claim: "the gate required AT LEAST this". One attempt
        # this tool knows nothing about cannot be bounded, so the run cannot be
        # either.
        rolled = vc.summarize_required_drain(
            [
                vc.RequiredDrain(250, 250, "graduated"),
                vc.RequiredDrain(None, None, "unknown"),
            ]
        )
        self.assertIsNone(rolled.required_ms)
        self.assertIsNone(rolled.lower_bound_ms)


class EffectiveDrainReportTests(SyntheticEvidenceCase):
    """Defect 1 end to end: headroom must be taken against the drain the gate
    REQUIRED, and a run whose graduation was silently disabled must not print
    the same report as one where it was on."""

    def write_ordinal_70_shaped_attempt(self, **overrides) -> None:
        """The one observed near-miss, to scale.

        the drain-low campaign's 30-nonascii-input evidence,
        attempt-bbab531a-*: terminal candidate and marker at the same
        instant, last transcript activity 352 ms LATER -- above the 250 ms
        floor -- with the commit paid at 570 ms of stability against a
        configured 2,000.
        """

        defaults = dict(
            suite_index=1,
            ordinal=70,
            terminal_candidate_at_ms=9_000,
            turn_duration_observed_at_ms=9_000,
            last_transcript_activity_at_ms=9_352,
            drain_ms=570,
            transcript_drain_ms=2_000,
            final_text="ok",
        )
        defaults.update(overrides)
        self.write_attempt(**defaults)

    def test_headroom_is_taken_against_the_required_drain_not_the_configured_one(
        self,
    ) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt()
        exit_code, output = self.run_main(["--json"])
        report = json.loads(output)
        # The old number, and why it was wrong: 2000 - 352.
        self.assertEqual(report["configured_transcript_drain_ms"], 2_000)
        self.assertEqual(report["headroom_vs_configured_ms"], 1_648)
        # The right one: the gate required 250 ms, the transcript kept moving
        # for 352, so the margin against the mechanism under test is NEGATIVE.
        self.assertEqual(report["required_drain_ms"], 250)
        self.assertEqual(report["headroom_basis_ms"], 250)
        self.assertEqual(report["headroom_ms"], -102)
        self.assertEqual(exit_code, 0)

    def test_the_default_text_prints_the_required_drain_beside_the_configured_one(
        self,
    ) -> None:
        # BOTH, distinctly, in the DEFAULT output. A quantity that only exists
        # under --json is this project's signature defect.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt()
        _, output = self.run_main()
        self.assertIn("configured_transcript_drain_ms: 2000", output)
        self.assertIn(
            "required_drain_ms (EFFECTIVE, what the commit gate asked for): 250",
            output,
        )
        self.assertIn("headroom vs measured worst case: -102 ms", output)
        self.assertIn("graduated end-of-turn drain in effect: True", output)

    def test_a_late_arrival_past_the_required_drain_is_loud_and_not_fatal(
        self,
    ) -> None:
        # The governing asymmetry says a truncation would be unacceptable --
        # but no truncation happened here, and the reason is the safety
        # property: stable_for_ms is quiet-since-the-last-BYTE and re-arms.
        # So this is an OBSERVATION about drain sizing, not a defect in the
        # evidence, and this tool's exit 0 claims only the latter.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt()
        exit_code, output = self.run_main()
        self.assertIn(
            "THE WORST MEASURED LATE ARRIVAL EXCEEDED THE REQUIRED DRAIN", output
        )
        self.assertIn("RE-ARMS the full window", output)
        self.assertEqual(exit_code, 0)
        report = json.loads(self.run_main(["--json"])[1])
        self.assertNotIn("headroom", "\n".join(vc.failing_conditions(report)))

    def test_a_regression_that_silently_disabled_graduation_changes_the_report(
        self,
    ) -> None:
        # THE point of Defect 1. Before the fix, a graduated run and a run
        # whose graduation had been silently turned off published identical
        # verifier output: same tally, same gap distribution, same
        # `configured_transcript_drain_ms: 2000`, same headroom. The ONLY
        # published difference is `drain_ms`, and nothing read it.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt()
        graduated = json.loads(self.run_main(["--json"])[1])

        self.setUp()
        self.write_prompt("01-trivial.txt", "reply ok")
        # Identical in every respect EXCEPT the paid drain: graduation off
        # means the gate held out for the full configured 2,000 ms.
        self.write_ordinal_70_shaped_attempt(drain_ms=2_010)
        regressed = json.loads(self.run_main(["--json"])[1])

        self.assertEqual(graduated["run_is_graduated"], True)
        self.assertIsNone(regressed["run_is_graduated"])
        self.assertEqual(graduated["required_drain_ms"], 250)
        self.assertIsNone(regressed["required_drain_ms"])
        self.assertEqual(graduated["graduation_state_tally"], {"graduated": 1})
        self.assertEqual(
            regressed["graduation_state_tally"], {"graduation_indeterminate": 1}
        )
        # And the headroom moves with it, in the safe direction: the regressed
        # run can only be bounded below, so its margin is still measured
        # against 250, never credited with 2,000.
        self.assertEqual(graduated["headroom_ms"], -102)
        self.assertEqual(regressed["headroom_ms"], -102)
        self.assertIn("LOWER BOUND", regressed["headroom_basis"])

    def test_an_unmarked_run_reports_the_configured_drain_as_required(
        self,
    ) -> None:
        # The pre-graduation build, and the control for the test above: no
        # marker means the full configured drain really was owed, so
        # `required` and `configured` coincide and the headroom is the old
        # number -- correctly, this time.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt(turn_duration_observed_at_ms=None)
        exit_code, output = self.run_main(["--json"])
        report = json.loads(output)
        self.assertEqual(report["run_is_graduated"], False)
        self.assertEqual(report["required_drain_ms"], 2_000)
        self.assertEqual(report["headroom_ms"], 1_648)
        self.assertEqual(report["headroom_vs_configured_ms"], 1_648)
        self.assertEqual(exit_code, 0)
        _, text = self.run_main()
        self.assertNotIn("EXCEEDED THE REQUIRED DRAIN", text)

    def test_a_run_that_saw_no_late_row_publishes_no_headroom_at_all(
        self,
    ) -> None:
        # Unchanged by this fix and asserted so it stays unchanged: headroom
        # needs a MEASURED worst case, and a gap inside the noise band is not
        # one. This is the /70-graded-hash-oracle shape (max gap 1 ms).
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt(last_transcript_activity_at_ms=9_001)
        report = json.loads(self.run_main(["--json"])[1])
        self.assertIsNone(report["headroom_ms"])
        self.assertIsNone(report["headroom_vs_configured_ms"])
        # But the effective drain is still published, because "we measured no
        # late row" and "we do not know what window we measured it against"
        # are different admissions.
        self.assertEqual(report["required_drain_ms"], 250)
        self.assertEqual(report["run_is_graduated"], True)

    def test_a_mixed_run_refuses_a_single_required_drain(self) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt(ordinal=70)
        self.write_ordinal_70_shaped_attempt(
            ordinal=71, turn_duration_observed_at_ms=None
        )
        report = json.loads(self.run_main(["--json"])[1])
        self.assertIsNone(report["run_is_graduated"])
        self.assertIsNone(report["required_drain_ms"])
        self.assertIn("varies across attempts", report["required_drain_ms_note"])
        # Bounded below by the smallest value any attempt's gate required.
        self.assertEqual(report["required_drain_ms_lower_bound"], 250)
        self.assertEqual(report["headroom_ms"], -102)

    def test_the_configured_drain_override_re_derives_the_required_drain(
        self,
    ) -> None:
        # The override must not leave the two lines describing different
        # builds. At a configured 200 ms the 250 ms floor no longer binds, so
        # the same evidence stops being graduated.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_ordinal_70_shaped_attempt()
        report = json.loads(
            self.run_main(["--json", "--configured-drain-ms", "200"])[1]
        )
        self.assertEqual(report["configured_transcript_drain_ms"], 200)
        self.assertEqual(report["required_drain_ms"], 200)
        self.assertEqual(report["graduation_state_tally"], {"floor_not_binding": 1})
        self.assertIsNone(report["run_is_graduated"])


class TruncationOracleCoverageTests(SyntheticEvidenceCase):
    """Defect 2 end to end: `{'match': 7, 'not_applicable': 2}` reads as nine
    attempts cleared and is seven checked plus two never examined."""

    def build_seven_of_nine_tree(self) -> None:
        """The /70-graded-hash-oracle shape: two un-oracled grades, seven
        hash-bearing ones."""

        self.write_prompt("01-baseline-trivial.txt", "reply with the word ok")
        self.write_prompt("02-poem-only-no-tool.txt", "write a four line poem")
        self.write_prompt("03-poem-hash.txt", "write a poem then its SHA256:")
        self.write_attempt(suite_index=1, ordinal=73, final_text="ok")
        self.poem = "roses are red\nviolets are blue\nsugar is sweet\nand so are you"
        self.write_attempt(suite_index=2, ordinal=74, final_text=self.poem)
        for ordinal in range(75, 82):
            self.write_attempt(
                suite_index=3,
                ordinal=ordinal,
                final_text=f"{self.poem}\n\nSHA256: {sha256_hex(self.poem)}",
            )

    def test_the_uncovered_count_is_loud_in_the_default_text_output(self) -> None:
        self.build_seven_of_nine_tree()
        exit_code, output = self.run_main()
        self.assertIn("2 of 9 answer(s) had NO TRUNCATION ORACLE", output)
        # The tally is never again allowed to state a bare "9".
        self.assertIn("tally (over the 7 of 9 answer(s) an oracle covered", output)
        # Named individually, so the reader can go and look.
        self.assertIn("ordinal=73", output)
        self.assertIn("ordinal=74", output)
        self.assertIn(
            "grade(s) with NO oracle over ANY of the answers they produced: "
            "01-baseline-trivial, 02-poem-only-no-tool",
            output,
        )
        # LOUD, not fatal: these prompts are un-oracled by design and a gate
        # must gate exactly the claim it protects.
        self.assertEqual(exit_code, 0)
        report = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(vc.failing_conditions(report), [])

    def test_the_uncovered_count_is_loud_in_json_too(self) -> None:
        self.build_seven_of_nine_tree()
        report = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(report["answers_without_truncation_oracle"], 2)
        self.assertEqual(report["attempts_with_no_answer"], 0)
        coverage = report["truncation_oracle_coverage"]
        self.assertEqual(coverage["attempts_discovered"], 9)
        self.assertEqual(coverage["answers_discovered"], 9)
        self.assertEqual(coverage["answers_with_oracle"], 7)
        self.assertEqual(coverage["answers_without_oracle"], 2)
        self.assertEqual(coverage["unchecked_answers_by_reason"], {"not_applicable": 2})
        self.assertEqual(coverage["attempts_with_no_answer"], 0)
        self.assertEqual(coverage["no_answer_by_reason"], {})
        self.assertEqual(
            coverage["grades_without_oracle"],
            ["01-baseline-trivial", "02-poem-only-no-tool"],
        )
        self.assertEqual(
            {item["global_attempt_ordinal"] for item in coverage["unchecked_answers"]},
            {73, 74},
        )

    def test_emptying_an_un_oracled_poem_leaves_the_tally_identical(self) -> None:
        # The mutation that proved Defect 2 on the real tree: grade 02's whole
        # 437-character poem deleted. The tally, the failing conditions and
        # every note stay EXACTLY the same, because no oracle ever covered that
        # attempt. This test asserts that silence still exists -- it is a real
        # property of the evidence, not something a report can fix -- and the
        # next test asserts the report now says so out loud.
        self.build_seven_of_nine_tree()
        before = json.loads(self.run_main(["--json"])[1])
        self.setUp()
        self.build_seven_of_nine_tree()
        emptied = next(
            path
            for path in self.evidence_root.glob("attempt-*/pmux-run.stdout.json")
            if json.loads(path.read_text())["text"] == self.poem
        )
        emptied.write_text(json.dumps({"text": "", "final_blocks": []}))
        after = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(before["hash_tally_overall"], after["hash_tally_overall"])
        self.assertEqual(
            before["hash_tally_overall"], {"not_applicable": 2, "match": 7}
        )
        self.assertEqual(vc.failing_conditions(after), [])

    def test_the_report_names_the_attempt_whose_emptied_poem_nothing_checked(
        self,
    ) -> None:
        self.build_seven_of_nine_tree()
        emptied = next(
            path
            for path in self.evidence_root.glob("attempt-*/pmux-run.stdout.json")
            if json.loads(path.read_text())["text"] == self.poem
        )
        emptied.write_text(json.dumps({"text": "", "final_blocks": []}))
        _, output = self.run_main()
        self.assertIn("2 of 9 answer(s) had NO TRUNCATION ORACLE", output)
        self.assertIn("or entirely EMPTY -- reply would have graded exactly as", output)
        self.assertIn("ordinal=74", output)
        # And the by-grade row carries the same mark, so a reader scanning the
        # table rather than the banner cannot miss it either.
        self.assertIn("NO-ORACLE", output)

    def test_full_coverage_says_so_positively(self) -> None:
        # The other half of "mandatory in every branch": a clean tree must
        # state that it was fully covered, so "no banner" can never be the way
        # full coverage is communicated.
        self.write_prompt("01-poem-hash.txt", "write a poem then its SHA256:")
        poem = "roses are red"
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text=f"{poem}\n\nSHA256: {sha256_hex(poem)}",
        )
        exit_code, output = self.run_main()
        self.assertIn("every one of 1 answer(s) had a truncation oracle", output)
        # And the OTHER denominator is stated too, so a reader cannot mistake
        # "no attempt failed to answer" for "the report forgot to say".
        self.assertIn("every one of 1 discovered attempt(s) produced an answer", output)
        self.assertNotIn("NO TRUNCATION ORACLE", output)
        self.assertNotIn("NO-ORACLE", output)
        self.assertEqual(exit_code, 0)
        report = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(report["answers_without_truncation_oracle"], 0)

    def test_a_failed_attempt_is_nothing_to_check_not_an_unchecked_answer(
        self,
    ) -> None:
        # An ordinal that was SPENT and produced no reply at all. It is NOT an
        # oracle coverage hole: there was never an answer for an oracle to look
        # at, and the failure is already counted in the bucket header. Folding
        # it into the oracle count is what turned "3 unchecked answers" into a
        # headline reading "10 of 17 discovered attempt(s) had NO TRUNCATION
        # ORACLE" on the Gate B tree.
        self.write_prompt("01-poem-hash.txt", "write a poem then its SHA256:")
        self.write_attempt(suite_index=1, ordinal=1, status="pmux_nonzero_exit")
        report = json.loads(self.run_main(["--json"])[1])
        coverage = report["truncation_oracle_coverage"]
        self.assertEqual(coverage["answers_discovered"], 0)
        self.assertEqual(coverage["answers_without_oracle"], 0)
        self.assertEqual(coverage["unchecked_answers_by_reason"], {})
        self.assertEqual(coverage["attempts_with_no_answer"], 1)
        self.assertEqual(coverage["no_answer_by_reason"], {"no_result": 1})
        # A grade whose every attempt failed is not an oracle-blind grade.
        self.assertEqual(coverage["grades_without_oracle"], [])

    def test_the_two_denominators_are_separated_in_the_default_text_output(
        self,
    ) -> None:
        # The Gate B tree in miniature: two answers of which one is un-oracled,
        # plus three attempts that produced nothing. The headline must say
        # "1 of 2 answer(s)", never "4 of 5 discovered attempt(s)".
        self.write_prompt("01-baseline-trivial.txt", "reply with the word ok")
        self.write_prompt("02-poem-hash.txt", "write a poem then its SHA256:")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        poem = "roses are red"
        self.write_attempt(
            suite_index=2,
            ordinal=2,
            final_text=f"{poem}\n\nSHA256: {sha256_hex(poem)}",
        )
        for ordinal in range(3, 6):
            self.write_attempt(
                suite_index=1, ordinal=ordinal, status="pmux_nonzero_exit"
            )
        exit_code, output = self.run_main()
        self.assertIn("1 of 2 answer(s) had NO TRUNCATION ORACLE", output)
        self.assertNotIn("4 of 5 discovered attempt(s) had NO TRUNCATION", output)
        self.assertIn(
            "separately, and NOT part of the oracle figure above: 3 of 5 "
            "discovered attempt(s) produced no answer at all",
            output,
        )
        self.assertIn(
            "tally (over the 1 of 2 answer(s) an oracle covered, plus the 1 it "
            "did not, plus the 3 attempt(s) with no answer)",
            output,
        )
        report = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(report["answers_without_truncation_oracle"], 1)
        self.assertEqual(report["attempts_with_no_answer"], 3)
        self.assertEqual(exit_code, 0)

    def test_the_by_grade_row_charges_the_oracle_only_for_answers(self) -> None:
        # `oracle=0/8` on grade 01 of the Gate B tree blamed the oracle for six
        # attempts that produced no reply. The row now says `oracle=0/2
        # answers ... no-answer=6`-shaped arithmetic instead.
        self.write_prompt("01-baseline-trivial.txt", "reply with the word ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(suite_index=1, ordinal=2, final_text="ok")
        for ordinal in range(3, 6):
            self.write_attempt(
                suite_index=1, ordinal=ordinal, status="pmux_nonzero_exit"
            )
        _, output = self.run_main()
        self.assertIn("oracle=0/2 answers NO-ORACLE no-answer=3", output)
        self.assertNotIn("oracle=0/5", output)
        report = json.loads(self.run_main(["--json"])[1])
        grade = report["grades"]["01-baseline-trivial"]
        self.assertEqual(grade["attempts"], 5)
        self.assertEqual(grade["answers"], 2)
        self.assertEqual(grade["attempts_with_no_answer"], 3)
        self.assertEqual(grade["answers_with_truncation_oracle"], 0)
        self.assertEqual(grade["answers_without_truncation_oracle"], 2)

    def test_a_tree_where_nothing_answered_says_the_oracle_covered_nothing(
        self,
    ) -> None:
        # The degenerate denominator. `0 of 0 answers` would read as full
        # coverage; it has to read as no coverage at all.
        self.write_prompt("01-poem-hash.txt", "write a poem then its SHA256:")
        self.write_attempt(suite_index=1, ordinal=1, status="pmux_nonzero_exit")
        _, output = self.run_main()
        self.assertIn("NOT ONE of 1 discovered attempt(s) produced an answer", output)
        self.assertNotIn("had a truncation oracle over it", output)

    def test_a_partially_answered_hash_grade_still_counts_as_covered(self) -> None:
        # `partial` means every hash the reply DID carry was recomputed against
        # the body, so a truncation of that body would have been caught. The
        # omitted labels are a separate, already-failing condition and must not
        # be double-counted as a coverage hole.
        self.write_prompt(
            "05-transform.txt", "give SHA256(poem): and SHA256(reversed):"
        )
        poem = "roses are red"
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text=f"{poem}\n\nSHA256(poem): {sha256_hex(poem)}",
        )
        report = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(report["hash_tally_overall"], {"partial": 1})
        self.assertEqual(report["answers_without_truncation_oracle"], 0)

    def test_every_uncovered_reason_it_can_report_has_a_rendered_explanation(
        self,
    ) -> None:
        # The same rule the stop-hook reasons follow: a count with no words
        # beside it gets read as "nothing to see". Split into the two tables
        # the two denominators use, and asserted DISJOINT so a reason can never
        # be counted in both.
        unchecked = {
            "not_applicable",
            "missing",
            "hash_expectation_unknown",
            "error",
        }
        self.assertEqual(unchecked, set(vc.UNCHECKED_ANSWER_REASONS))
        self.assertEqual({"no_result"}, set(vc.NO_ANSWER_REASONS))
        self.assertEqual(
            set(vc.UNCHECKED_ANSWER_REASONS) & set(vc.NO_ANSWER_REASONS), set()
        )


class DrainBlockRenderSiteTests(SyntheticEvidenceCase):
    """Every drain quantity this tool computes must appear in the DEFAULT text
    output, in every branch.

    The regression these guard against is the one this project already paid
    for once: eight `notes.append` sites and zero render sites hid a real bug
    for weeks. The previous round reintroduced it in the fix FOR it --
    `headroom_basis = "no required drain could be derived"` was assigned at a
    site with no reachable renderer, so a run that could establish nothing said
    nothing about it unless you asked for --json.
    """

    DRAIN_BLOCK_LINES = (
        "configured_transcript_drain_ms:",
        "required_drain_ms (EFFECTIVE, what the commit gate asked for):",
        "  lower bound on the required drain:",
        "drain_ms actually PAID at commit",
        "graduated end-of-turn drain in effect:",
        "headroom basis:",
        "worst measured late arrival the margin is taken from:",
        "headroom vs measured worst case:",
        "for comparison only, NOT a margin the gate ever had:",
    )

    def assert_whole_drain_block_rendered(self, output: str) -> None:
        for line in self.DRAIN_BLOCK_LINES:
            self.assertIn(line, output)

    def test_the_no_required_drain_basis_reaches_the_default_text_output(
        self,
    ) -> None:
        # THE finding. An attempt that published no configured drain leaves the
        # required drain unknown AND unbounded, so `headroom_basis` becomes the
        # sentence "no required drain could be derived" -- which used to be
        # computed, stored, serialized under --json, and printed nowhere.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text="ok",
            transcript_drain_ms=None,
            last_transcript_activity_at_ms=9_400,
        )
        exit_code, output = self.run_main()
        self.assertIn(
            "headroom basis: None ms -- no required drain could be derived",
            output,
        )
        self.assertIn(
            "lower bound on the required drain: none could be derived either",
            output,
        )
        self.assertIn(
            "no required drain could be derived from the published timings",
            output,
        )
        self.assert_whole_drain_block_rendered(output)
        self.assertEqual(exit_code, 0)
        # And the same value under --json, so the two modes agree rather than
        # one of them being the only place it exists.
        report = json.loads(self.run_main(["--json"])[1])
        self.assertIsNone(report["headroom_basis_ms"])
        self.assertEqual(report["headroom_basis"], "no required drain could be derived")
        self.assertIsNone(report["required_drain_ms"])
        self.assertIsNone(report["required_drain_ms_lower_bound"])
        self.assertEqual(report["graduation_state_tally"], {"unknown": 1})

    def test_the_drain_block_prints_when_no_gap_is_computable_at_all(
        self,
    ) -> None:
        # The whole block used to live inside `overall_gap_distribution is not
        # None`. A run that published no usable timing pair therefore reported
        # a required drain, a graduation verdict and a headroom basis into
        # --json and printed none of them -- and the graduation-regression
        # signal, the entire point of the required-drain work, vanished with
        # them.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text="ok",
            last_transcript_activity_at_ms=None,
            turn_duration_observed_at_ms=9_000,
            drain_ms=570,
        )
        exit_code, output = self.run_main()
        self.assertIn("NO COMPUTABLE LATE-ARRIVAL GAP ANYWHERE IN THIS RUN", output)
        self.assert_whole_drain_block_rendered(output)
        # The graduation verdict specifically: a regression that silently
        # disabled it must still change this text.
        self.assertIn("graduated end-of-turn drain in effect: True", output)
        self.assertIn(
            "required_drain_ms (EFFECTIVE, what the commit gate asked for): 250",
            output,
        )
        # It is still not fatal, and the gap failure below is still fatal for
        # its own reason.
        self.assertEqual(exit_code, 1)

    def test_a_run_with_no_successful_attempt_still_renders_the_block(
        self,
    ) -> None:
        # The empty-tally branch: `graduation_state_tally` is `{}` and
        # `run_is_graduated` is None. "We could not tell" must not render as
        # "graduation is off", and it must not render as nothing.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, status="pmux_nonzero_exit")
        _, output = self.run_main()
        self.assert_whole_drain_block_rendered(output)
        self.assertIn("graduated end-of-turn drain in effect: None", output)
        self.assertIn(
            "no successful attempt, so there is no graduation state to report",
            output,
        )
        self.assertIn("not published by any of the 0 successful attempt(s)", output)

    def test_the_paid_drain_the_verdict_is_inferred_from_is_rendered(
        self,
    ) -> None:
        # `drain_ms` is the third input to every graduation verdict and reached
        # no default-text render site at all. Its summary is now printed beside
        # the verdict it produces.
        self.write_prompt("01-trivial.txt", "reply ok")
        for ordinal, paid in ((1, 569), (2, 570), (3, 601)):
            self.write_attempt(
                suite_index=1,
                ordinal=ordinal,
                final_text="ok",
                turn_duration_observed_at_ms=9_000,
                drain_ms=paid,
            )
        _, output = self.run_main()
        self.assertIn(
            "drain_ms actually PAID at commit, over the 3 successful "
            "attempt(s) that published it: min=569 median=570 max=601 ms "
            "(0 published none)",
            output,
        )
        report = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(
            report["observed_drain_ms_summary"],
            {
                "attempts_publishing_drain_ms": 3,
                "attempts_not_publishing_drain_ms": 0,
                "min": 569,
                "median": 570,
                "max": 601,
            },
        )

    def test_a_headroom_that_cannot_be_computed_says_why_rather_than_vanishing(
        self,
    ) -> None:
        # Two independent holes, both named. Silence used to cover both.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1,
            ordinal=1,
            final_text="ok",
            transcript_drain_ms=None,
            last_transcript_activity_at_ms=9_000,
        )
        _, output = self.run_main()
        self.assertIn("headroom vs measured worst case: NOT COMPUTED, because:", output)
        self.assertIn("no required drain could be derived from the published", output)
        self.assertIn("no late row was measured anywhere in this run", output)
        report = json.loads(self.run_main(["--json"])[1])
        self.assertEqual(len(report["headroom_uncomputable_reasons"]), 2)
        self.assertIsNone(report["headroom_worst_case_gap_ms"])


class ConfiguredDrainConstancyTests(SyntheticEvidenceCase):
    """ "Constant across every successful attempt" must not be claimed over
    attempts that were filtered out before the claim was made."""

    def test_an_attempt_with_no_configured_drain_is_named_as_excluded(
        self,
    ) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(
            suite_index=1, ordinal=2, final_text="ok", transcript_drain_ms=None
        )
        _, output = self.run_main()
        self.assertIn(
            "configured_transcript_drain_ms: 2000 (constant across the 1 of 2 "
            "successful attempt(s) that published one; 1 of 2 successful "
            "attempt(s) published no integer transcript_drain_ms and were "
            "EXCLUDED from this claim",
            output,
        )
        self.assertNotIn("constant across every successful attempt", output)

    def test_a_fully_published_run_states_its_denominator_too(self) -> None:
        # The honest version of the old sentence: still a constancy claim, but
        # one that says how many attempts it was checked over.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(suite_index=1, ordinal=2, final_text="ok")
        _, output = self.run_main()
        self.assertIn(
            "configured_transcript_drain_ms: 2000 (constant across the 2 of 2 "
            "successful attempt(s) that published one)",
            output,
        )
        self.assertNotIn("EXCLUDED from this claim", output)

    def test_a_run_where_nobody_published_one_says_so_instead_of_varying(
        self,
    ) -> None:
        # The old code fell through to `varies across attempts: []`, which
        # names a disagreement that does not exist and hides the real fact.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(
            suite_index=1, ordinal=1, final_text="ok", transcript_drain_ms=None
        )
        _, output = self.run_main()
        self.assertIn(
            "no configured value: not one of 1 successful attempt(s) published "
            "an integer compatibility.transcript_drain_ms",
            output,
        )
        self.assertNotIn("varies across", output)
        report = json.loads(self.run_main(["--json"])[1])
        self.assertIsNone(report["configured_transcript_drain_ms"])

    def test_disagreeing_attempts_report_the_denominator_and_the_exclusions(
        self,
    ) -> None:
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        self.write_attempt(
            suite_index=1, ordinal=2, final_text="ok", transcript_drain_ms=1_000
        )
        self.write_attempt(
            suite_index=1, ordinal=3, final_text="ok", transcript_drain_ms=None
        )
        _, output = self.run_main()
        self.assertIn(
            "varies across the 2 of 3 successful attempt(s) that published "
            "one: [1000, 2000]; 1 of 3 successful attempt(s) published no "
            "integer transcript_drain_ms and were EXCLUDED from this claim",
            output,
        )

    def test_the_override_says_what_the_attempts_themselves_published(
        self,
    ) -> None:
        # An override that erases the observed values would let a wrong
        # `--configured-drain-ms` pass unnoticed.
        self.write_prompt("01-trivial.txt", "reply ok")
        self.write_attempt(suite_index=1, ordinal=1, final_text="ok")
        _, output = self.run_main(["--configured-drain-ms", "500"])
        self.assertIn(
            "configured_transcript_drain_ms: 500 (overridden by "
            "--configured-drain-ms; what the attempts themselves published was "
            "[2000] over 1 of 1 successful attempt(s))",
            output,
        )


class BannerCitationTests(unittest.TestCase):
    """Every source citation this tool EMITS must point at what it claims.

    A safety banner that cites the wrong line teaches the next reader to
    distrust it, and the re-arm citation is the one carrying the entire
    explanation for why a 352 ms post-marker arrival did not truncate.
    """

    CITATION = re.compile(
        r"((?:crates|bin|tools)/[\w./-]+\.(?:rs|py)):(\d+)(?:-(\d+))?"
    )

    # Wider than `CITATION` on purpose: it also matches the bare
    # `driver_io.rs:2714` and `:3007-3017` shorthands, which are exactly the
    # forms that escaped every previous check because they carry no directory.
    PRODUCT_CITATION = re.compile(
        r"((?:(?:crates|bin|tools|clients)/[\w./-]+|[\w-]+)\.(?:rs|py)):(\d+)(?:-(\d+))?"
    )

    def cited_text(self, path_part: str, start: int, end: int) -> str:
        path = ROOT / path_part
        if not path.is_file():
            self.skipTest(f"{path_part} is not present in this checkout")
        lines = path.read_text(encoding="utf-8").splitlines()
        self.assertLessEqual(
            end, len(lines), f"{path_part}:{start}-{end} runs past end of file"
        )
        return "\n".join(lines[start - 1 : end])

    def assert_cites(self, citation: str, expected: str) -> None:
        match = self.CITATION.fullmatch(citation)
        self.assertIsNotNone(match, citation)
        path_part, start, end = match.group(1), match.group(2), match.group(3)
        text = self.cited_text(path_part, int(start), int(end or start))
        self.assertIn(
            expected,
            text,
            f"{citation} no longer contains {expected!r}",
        )

    def banner_driver_io_citations(self) -> list[str]:
        """Every driver_io.rs citation the banner makes, in the order it makes
        them.

        Read OUT OF the banner rather than restated here. Restating them made
        the line numbers live in two files, so a diff that moved the code (Path
        B moved `driver_io.rs` by ~2,000 lines) had to be chased through both,
        and a test that is chased is a test that gets loosened. The banner is
        the single source of truth; this suite only checks that what it claims
        is still where it says.
        """
        return [
            f"crates/service/src/driver_io.rs:{match.group(1)}"
            for match in re.finditer(
                r"driver_io\.rs:(\d+(?:-\d+)?)", vc.NEGATIVE_HEADROOM_BANNER
            )
        ]

    # There used to be a `module_lines_naming(needle)` helper here that returned
    # the tool's own source lines containing `needle`, so a citation living in a
    # COMMENT could still be graded. It is gone with the comments it read: every
    # product line number the tool holds is now a `cite(...)` result, so there is
    # an attribute to read each one out of, and a helper that grades source text
    # instead of a value is a helper that grades the wrong copy -- which is
    # precisely how `actor.rs:83` survived a test named for it.

    def sole_citation(self, text: str, filename: str) -> str:
        """The one citation of `filename` in `text`. Two is as bad as none.

        Taking the first of several would quietly check a claim other than the
        one under test, and pass while doing it.
        """
        found = [
            match.group(0)
            for match in self.CITATION.finditer(text)
            if match.group(1).endswith(filename)
        ]
        self.assertEqual(len(found), 1, f"{filename} citations in {text!r}: {found}")
        return found[0]

    def test_the_negative_headroom_banner_cites_the_rearm_mechanism(self) -> None:
        # The three lines that together are the safety property: the window is
        # measured from the last BYTE, it is re-stamped on every nonzero read,
        # and a poll that read nothing returns before the re-stamp.
        citations = self.banner_driver_io_citations()
        self.assertEqual(
            len(citations),
            3,
            f"expected exactly three driver_io.rs citations, got {citations}",
        )
        expected = (
            "state.last_change.elapsed()",
            "state.last_change = Instant::now();",
            "if read_len == 0 {",
        )
        for citation, snippet in zip(citations, expected, strict=True):
            self.assert_cites(citation, snippet)

    def test_the_rearm_citation_is_the_one_inside_read_observed_range(
        self,
    ) -> None:
        # `state.last_change = Instant::now()` appears twice in driver_io.rs.
        # The banner names `read_observed_range`, so it has to cite THAT one,
        # not the arm-boundary reset in the other function. Citing the wrong
        # one would still pass the snippet check above, because both lines are
        # byte-identical.
        path = ROOT / "crates" / "service" / "src" / "driver_io.rs"
        if not path.is_file():
            self.skipTest("driver_io.rs is not present in this checkout")
        lines = path.read_text(encoding="utf-8").splitlines()
        definition = next(
            index
            for index, line in enumerate(lines, start=1)
            if "fn read_observed_range" in line
        )
        rearm_citation = self.banner_driver_io_citations()[1]
        rearm_line = int(rearm_citation.rsplit(":", 1)[1].split("-")[0])
        self.assertGreater(rearm_line, definition)

    def test_the_graduation_label_cites_graduated_drain_ms(self) -> None:
        # Read out of the label, not restated beside it. The pair
        # `assertIn("...:244", label)` + `assert_cites("...:244", ...)` used to
        # be written here, and it could only ever prove the two literals in THIS
        # file agreed with each other.
        self.assert_cites(
            self.sole_citation(
                vc.GRADUATION_STATE_LABELS["not_graduated_no_marker"],
                "v1/backend.rs",
            ),
            "pub const fn graduated_drain_ms",
        )

    def test_the_declared_floor_matches_the_constant_it_names(self) -> None:
        self.assert_cites(
            vc.TURN_DURATION_DRAIN_FLOOR_CITATION,
            f"pub const TURN_DURATION_DRAIN_FLOOR_MS: u64 = "
            f"{vc.TURN_DURATION_DRAIN_FLOOR_MS};",
        )

    def test_the_noise_band_line_cites_the_actor_poll_interval(self) -> None:
        # This test is named for the line the DEFAULT text output prints, and it
        # used to check a different string entirely: `module_lines_naming` found
        # only the source lines containing the exact token `poll_interval`, and
        # the printed line says "one actor poll interval," with a space. So it
        # graded the copy in a comment while the emitted citation was two lines
        # off, and passed. Grade the emitted constant.
        self.assert_cites(
            vc.ACTOR_POLL_INTERVAL_CITATION,
            f"poll_interval: Duration::from_millis({vc.ACTOR_POLL_INTERVAL_MS})",
        )

    def test_the_stop_hook_reason_cites_the_absence_clause(self) -> None:
        self.assert_cites(
            self.sole_citation(
                vc.STOP_HOOK_UNCOMPUTABLE_REASONS[
                    f"{vc.STOP_HOOK_FIELD}_not_published"
                ],
                "protocol/src/v1.rs",
            ),
            "Absent on any turn where no Stop hook was observed",
        )

    def test_the_approved_efforts_note_cites_the_tuple_it_quotes(self) -> None:
        self.assert_cites(vc.APPROVED_EFFORTS_CITATION, "APPROVED_EFFORTS = (")
        # And the values this tool repeated rather than imported still agree
        # with the tuple at that line.
        import phase0_lib  # noqa: PLC0415

        self.assertEqual(vc.APPROVED_EFFORTS, phase0_lib.APPROVED_EFFORTS)

    def test_no_product_line_number_is_written_down_anywhere_in_the_tool(
        self,
    ) -> None:
        # The rule the whole `cite` mechanism exists to make keepable: a line
        # number into source OUTSIDE tools/phase0 is derived at import or it is
        # not written down. Comments and docstrings are searched too, not just
        # emitted strings: when this test was written the tool held 22 product
        # citations, 16 of them wrong, and 11 of the 16 were in comments and
        # docstrings. Everything anything had ever checked was in a banner, and
        # a banner is 5 of the 22.
        #
        # Citations INTO tools/phase0 are left alone deliberately: they move
        # with this file under one diff, every one of them was still exact when
        # this was measured, and pretending otherwise would be a rule kept for
        # its own sake.
        exempt = {path.name for path in PHASE0.glob("*.py")}
        self.assertIn("phase0_lib.py", exempt)
        source = Path(vc.__file__).read_text(encoding="utf-8")
        offenders = [
            f"line {index}: {line.strip()}"
            for index, line in enumerate(source.splitlines(), start=1)
            for match in self.PRODUCT_CITATION.finditer(line)
            if match.group(1) not in exempt
            and not match.group(1).startswith("tools/phase0/")
        ]
        self.assertEqual(offenders, [], "hand-written product line numbers")

    def test_a_citation_that_cannot_be_resolved_carries_no_line_number(
        self,
    ) -> None:
        # The degradation contract. `cite` never guesses: absent file, absent
        # anchor and ambiguous anchor all render WITHOUT `path:<digits>`, so a
        # failed lookup can never be mistaken for a located line -- by a reader
        # or by the tests above, which find their subject by that very pattern.
        cases = {
            "file not in this checkout": vc.cite(
                "crates/service/src/no_such_file.rs", "anything"
            ),
            "0 matching lines": vc.cite(
                "crates/service/src/v1/backend.rs", "no such text exists here"
            ),
            "2 matching lines": vc.cite(
                "crates/service/src/driver_io.rs", "state.last_change = Instant::now();"
            ),
            "0 lines match the opening": vc.cite(
                "crates/service/src/driver_io.rs",
                "state.last_change = Instant::now();",
                after="fn no_such_function(",
            ),
            "no line at or after the anchor holds": vc.cite(
                "crates/service/src/v1/backend.rs",
                "pub const fn graduated_drain_ms",
                through="no such closing text",
            ),
        }
        for reason, rendered in cases.items():
            with self.subTest(reason=reason):
                self.assertIsNone(self.CITATION.search(rendered), rendered)
                self.assertIn(reason, rendered)

    def test_the_two_byte_identical_rearm_lines_would_be_refused_not_guessed(
        self,
    ) -> None:
        # The ambiguity that makes `after` load-bearing rather than decorative:
        # without a scope, the anchor the banner relies on matches twice, and a
        # resolver that took the first would cite the arm boundary while every
        # snippet assertion above still passed.
        unscoped = vc.cite(
            "crates/service/src/driver_io.rs", "state.last_change = Instant::now();"
        )
        self.assertIsNone(self.CITATION.search(unscoped), unscoped)
        self.assertNotEqual(unscoped, vc.REARM_CITATION)
        self.assertIsNotNone(self.CITATION.fullmatch(vc.REARM_CITATION))

    def test_every_citation_in_every_emitted_string_is_in_bounds(self) -> None:
        # Drift guard over the whole surface, not just the banners this file
        # names one by one: any citation reachable in emitted text must at
        # least point inside a file that exists.
        emitted: list[str] = []
        for name, value in vars(vc).items():
            if name.isupper() and isinstance(value, str):
                emitted.append(value)
            elif name.isupper() and isinstance(value, dict):
                emitted.extend(item for item in value.values() if isinstance(item, str))
        found = 0
        for text in emitted:
            for match in self.CITATION.finditer(text):
                path_part, start, end = match.group(1), match.group(2), match.group(3)
                self.cited_text(path_part, int(start), int(end or start))
                found += 1
        self.assertGreater(found, 0, "no citations were checked at all")


if __name__ == "__main__":
    unittest.main()
