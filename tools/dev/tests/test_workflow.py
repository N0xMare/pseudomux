"""The living three-command workflow, without spending a Claude turn."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import pathlib
import re
import subprocess
import tempfile
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


promote = load("promote")
operator_eval = load("operator_eval")


class CheckScript(unittest.TestCase):
    def test_help_names_push_and_no_real_claude(self) -> None:
        done = subprocess.run(
            [str(DEV / "check.sh"), "--help"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("--push", done.stdout)
        self.assertIn("PMUX_POOL_REAL_CLAUDE", done.stdout)
        self.assertIn("tools/dev tests", done.stdout)
        self.assertIn(
            "ruff check --no-cache tools/dev tools/evidence_common tools/promotion clients/python",
            done.stdout,
        )
        self.assertIn("tools/promotion tests", done.stdout)
        self.assertIn("private_runtime", done.stdout)

    def test_unknown_argument_is_exit_2(self) -> None:
        done = subprocess.run(
            [str(DEV / "check.sh"), "--gate-a"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(done.returncode, 2)
        self.assertIn("unknown argument", done.stderr)

    def test_is_executable(self) -> None:
        self.assertTrue(os.access(DEV / "check.sh", os.X_OK))

    def test_push_unsets_real_claude(self) -> None:
        text = (DEV / "check.sh").read_text(encoding="utf-8")
        self.assertIn("unset PMUX_POOL_REAL_CLAUDE", text)
        self.assertIn("tools/dev/tests", text)
        self.assertIn(
            'PMUX_DOCUMENTED_SURFACE_BIN_DIR="$root/target/debug"', text
        )
        self.assertIn("(cd clients/python &&", text)
        self.assertIn("vendor/rmux-server/Cargo.toml", text)
        self.assertIn("pane_io::tests::", text)
        self.assertIn(
            "cargo check --locked --offline --manifest-path vendor/rmux-server/Cargo.toml --all-targets --no-default-features",
            text,
        )
        self.assertIn("test_portable_paths.py", text)
        self.assertIn("--test private_runtime", text)
        self.assertIn("--ignored", text)
        self.assertIn(
            "ruff check --no-cache tools/dev tools/evidence_common tools/promotion clients/python",
            text,
        )
        self.assertIn("tools/promotion/tests", text)
        self.assertNotIn("tools/gate-a", text)
        self.assertNotIn("tools/phase0", text)
        self.assertNotIn("tools/linux-docker", text)
        self.assertNotIn("tools/package-smoke", text)


class PromoteWrapper(unittest.TestCase):
    def test_describe_does_not_need_a_drain(self) -> None:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            status = promote.main(["--describe"])
        self.assertEqual(status, 0, stderr.getvalue())
        text = stdout.getvalue()
        self.assertIn("drops --tested-claude-profile", text)
        self.assertIn("operator_eval.py", text)
        self.assertIn("pooled-transcript-drain-", text)
        payload = json.loads(text[text.index("{") :])
        self.assertIn("checks", payload)

    def test_missing_drain_refuses_without_calling_promotion(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stdout = io.StringIO()
            stderr = io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                status = promote.main(["--evidence-dir", tmp])
        self.assertEqual(status, 2)
        message = stderr.getvalue()
        self.assertIn("cannot drop the operator flag", message)
        self.assertIn("does not exist", message)
        self.assertIn("operator_eval.py", message)
        self.assertIn("Do not copy another OS", message)
        self.assertNotIn("promotion refused", message)

    def test_evidence_dir_without_path_is_exit_2(self) -> None:
        stderr = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
            status = promote.main(["--evidence-dir"])
        self.assertEqual(status, 2)
        self.assertIn("--evidence-dir needs a path", stderr.getvalue())

    def test_evidence_dir_equals_form_is_the_same_gate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stderr = io.StringIO()
            with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
                status = promote.main([f"--evidence-dir={tmp}"])
        self.assertEqual(status, 2)
        self.assertIn("cannot drop the operator flag", stderr.getvalue())

    def test_help_returns_0_without_a_drain(self) -> None:
        stdout = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
            status = promote.main(["--help"])
        self.assertEqual(status, 0)
        self.assertIn("cannot drop the operator flag", stdout.getvalue())

    def test_copied_foreign_os_drain_is_exit_2(self) -> None:
        host_os, host_arch = promote.host_identity()
        with tempfile.TemporaryDirectory() as tmp:
            dest = promote.drain_path(pathlib.Path(tmp), host_os, host_arch)
            dest.write_text(
                json.dumps(
                    {
                        "os": "not-" + host_os,
                        "arch": "not-" + host_arch,
                        "recommended_transcript_drain_ms": 1000,
                    }
                ),
                encoding="utf-8",
            )
            stderr = io.StringIO()
            with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
                status = promote.main(["--evidence-dir", tmp])
        self.assertEqual(status, 2)
        message = stderr.getvalue()
        self.assertIn("cannot drop the operator flag", message)
        self.assertIn("Do not copy another OS", message)
        self.assertNotIn("Traceback", message)

    def test_describe_ignores_valueless_evidence_dir(self) -> None:
        stdout = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
            status = promote.main(["--describe", "--evidence-dir"])
        self.assertEqual(status, 0)
        self.assertIn("drops --tested-claude-profile", stdout.getvalue())

    def test_later_evidence_dir_wins_over_an_earlier_broken_one(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stderr = io.StringIO()
            with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
                status = promote.main(
                    ["--evidence-dir", "--release-dir", "--evidence-dir", tmp]
                )
        self.assertEqual(status, 2)
        self.assertIn("cannot drop the operator flag", stderr.getvalue())

    def test_drain_path_is_per_os_arch(self) -> None:
        path = promote.drain_path(pathlib.Path("/tmp/evidence"), "linux", "x86_64")
        self.assertEqual(path.name, "pooled-transcript-drain-linux-x86_64.json")

    def test_unreadable_drain_is_exit_2_not_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            host_os, host_arch = promote.host_identity()
            path = promote.drain_path(pathlib.Path(tmp), host_os, host_arch)
            path.write_text("not-json", encoding="utf-8")
            stderr = io.StringIO()
            with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
                status = promote.main(["--evidence-dir", tmp])
        self.assertEqual(status, 2)
        message = stderr.getvalue()
        self.assertIn("cannot drop the operator flag", message)
        self.assertIn("unreadable", message)
        self.assertNotIn("Traceback", message)


class PromotionFloor(unittest.TestCase):
    """A promotion floor is per os/arch. The first `claude_version_floor` in
    compatibility.rs is the macos cell, not the linux one."""

    @classmethod
    def setUpClass(cls) -> None:
        sys_path = str(ROOT / "tools" / "promotion")
        if sys_path not in __import__("sys").path:
            __import__("sys").path.insert(0, sys_path)
        import promote_claude_version as promotion

        cls.promotion = promotion

    def test_macos_floor_is_not_inherited_by_linux(self) -> None:
        macos = self.promotion.promoted_version_floor("macos", "aarch64")
        self.assertEqual(macos, "2.1.220")
        try:
            linux = self.promotion.promoted_version_floor("linux", "x86_64")
        except self.promotion.PromotionRefused as error:
            self.assertIn("--floor", str(error))
            self.assertIn("linux/x86_64", str(error))
            linux = self.promotion.promoted_version_floor(
                "linux", "x86_64", "2.1.227"
            )
        self.assertEqual(linux, "2.1.227")

    def test_explicit_floor_cannot_disagree_with_a_shipped_cell(self) -> None:
        with self.assertRaises(self.promotion.PromotionRefused) as raised:
            self.promotion.promoted_version_floor("macos", "aarch64", "2.1.227")
        self.assertIn("disagrees", str(raised.exception))


class OperatorEval(unittest.TestCase):
    def test_describe_spends_nothing_and_lists_checks(self) -> None:
        stdout = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
            status = operator_eval.main(["--describe"])
        self.assertEqual(status, 0)
        text = stdout.getvalue()
        self.assertIn(operator_eval.SCHEMA, text)
        self.assertIn(operator_eval.GREEN, text)
        self.assertIn("not a promotion", text)
        for name in operator_eval.CHECK_ORDER:
            self.assertIn(name, text)

    def test_help_exits_0(self) -> None:
        stdout = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                operator_eval.main(["--help"])
        self.assertEqual(raised.exception.code, 0)
        self.assertIn("does not edit", stdout.getvalue())

    def test_missing_required_flags_exit_2(self) -> None:
        stderr = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                operator_eval.main([])
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("--release-dir", stderr.getvalue())

    def test_empty_equals_flags_are_missing(self) -> None:
        stderr = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                operator_eval.main(["--release-dir=", "--claude="])
        self.assertEqual(raised.exception.code, 2)

    def test_missing_claude_file_is_exit_2_not_a_traceback(self) -> None:
        stderr = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
            status = operator_eval.main(
                ["--release-dir", "/tmp", "--claude", "/no/such/claude-binary"]
            )
        self.assertEqual(status, 2)
        self.assertIn("operator-eval failed", stderr.getvalue())
        self.assertNotIn("Traceback", stderr.getvalue())

    def test_messages_turn_connection_failure_is_a_red_row_not_an_abort(self) -> None:
        row = operator_eval.messages_turn(
            "127.0.0.1:1",
            "operator-eval-test",
            {"model": "claude-sonnet-5-low", "messages": []},
        )
        self.assertEqual(row["status"], 0)
        self.assertIn("connection failed", row["raw_head"])
        self.assertEqual(row["conversation"], "operator-eval-test")

    def test_t2_sticky_payload_is_full_history(self) -> None:
        t1, t2 = operator_eval.sticky_messages_payloads(
            "claude-sonnet-5-low",
            "primer-user",
            "ACK token",
            "token",
        )
        self.assertEqual([m["role"] for m in t1["messages"]], ["user"])
        self.assertEqual(
            [m["role"] for m in t2["messages"]], ["user", "assistant", "user"]
        )
        self.assertEqual(t2["messages"][0]["content"], "primer-user")
        self.assertEqual(t2["messages"][1]["content"], "ACK token")
        self.assertIn("token", t2["messages"][2]["content"])
        self.assertNotEqual(t2["messages"][2]["content"], t2["messages"][0]["content"])

    def test_parse_message_body_joins_text_blocks(self) -> None:
        text, usage = operator_eval.parse_message_body(
            json.dumps(
                {
                    "content": [
                        {"type": "text", "text": "ACK "},
                        {"type": "text", "text": "token"},
                        {"type": "tool_use", "text": "ignored"},
                    ],
                    "usage": {"cache_read_input_tokens": 12},
                }
            )
        )
        self.assertEqual(text, "ACK token")
        self.assertEqual(usage["cache_read_input_tokens"], 12)

    def test_green_verdict_is_not_a_promotion_word(self) -> None:
        self.assertEqual(operator_eval.GREEN, "GREEN_OPERATOR")
        self.assertNotEqual(operator_eval.GREEN, "GREEN")
        self.assertEqual(operator_eval.SCHEMA, "pmux.operator-eval.v1")
        self.assertEqual(operator_eval.CHECK_ORDER[-1], "messages_sticky")
        self.assertEqual(
            operator_eval.PRE_MESSAGES_CHECKS, operator_eval.CHECK_ORDER[1:-1]
        )

    def test_eval_is_not_a_promotion_path(self) -> None:
        source = (DEV / "operator_eval.py").read_text(encoding="utf-8")
        self.assertNotIn("pooled_bound(", source)
        self.assertIn("portable_paths.render_document", source)
        self.assertIn('"may_ship_without_flag": False', source)


class LivingDocs(unittest.TestCase):
    def test_dev_readme_names_the_three_commands(self) -> None:
        text = (DEV / "README.md").read_text(encoding="utf-8")
        self.assertIn("tools/dev/check.sh", text)
        self.assertIn("tools/dev/operator_eval.py", text)
        self.assertIn("tools/dev/promote.py", text)
        self.assertIn(
            "ruff check --no-cache tools/dev tools/evidence_common tools/promotion clients/python",
            text,
        )

    def test_root_readme_points_at_tools_dev(self) -> None:
        text = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn("tools/dev", text)
        self.assertIn("check.sh", text)

    def test_gate_a_runner_is_gone(self) -> None:
        self.assertFalse((ROOT / "tools" / "gate-a" / "run_gate.py").is_file())
        self.assertFalse(
            (ROOT / "tools" / "gate-a-candidate" / "phase-manifest.json").is_file()
        )
        self.assertFalse((ROOT / "scripts" / "gate-in-worktree.sh").is_file())
        self.assertTrue((DEV / "check.sh").is_file())
        self.assertTrue((DEV / "tests" / "test_documented_surface.py").is_file())

    def test_freeze_envelope_is_gone(self) -> None:
        self.assertFalse((ROOT / "tools" / "phase0").exists())
        self.assertFalse((ROOT / "tools" / "linux-docker").exists())
        self.assertFalse((ROOT / "tools" / "package-smoke").exists())
        self.assertFalse(
            (ROOT / "tools" / "evidence_common" / "bounded_process.py").is_file()
        )
        self.assertFalse(
            (ROOT / "tools" / "evidence_common" / "managed_process.py").is_file()
        )
        self.assertTrue(
            (ROOT / "tools" / "evidence_common" / "portable_paths.py").is_file()
        )
        self.assertTrue((ROOT / "tools" / "promotion").is_dir())
        self.assertFalse((ROOT / ".dockerignore").is_file())
        portable = (
            ROOT / "tools" / "evidence_common" / "portable_paths.py"
        ).read_text(encoding="utf-8")
        self.assertNotIn("def sealed_records", portable)
        self.assertNotIn("absolute_placeholders", portable)

    def test_linux_pooled_drain_receipt_is_this_os_not_a_macos_copy(self) -> None:
        path = ROOT / "evidence" / "pooled-transcript-drain-linux-x86_64.json"
        self.assertTrue(path.is_file())
        receipt = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(receipt["os"], "linux")
        self.assertEqual(receipt["arch"], "x86_64")
        versions = receipt["claude_versions"]
        self.assertGreaterEqual(len(versions), 2)
        self.assertEqual(versions, ["2.1.227", "2.1.232", "2.1.233"])
        self.assertEqual(receipt["recommended_transcript_drain_ms"], 250)
        self.assertEqual(
            receipt["post_answer_arrivals"]["reachable_on_a_minified_cell"]["max_ms"],
            118,
        )
        self.assertNotIn("/home/", path.read_text(encoding="utf-8"))
        readme = (DEV / "README.md").read_text(encoding="utf-8")
        self.assertIn("pooled-transcript-drain-linux-x86_64.json", readme)
        self.assertNotIn("Linux currently has no pooled-drain receipt", readme)

    def test_testing_md_is_not_a_living_phase0_spec(self) -> None:
        text = (ROOT / "docs" / "testing.md").read_text(encoding="utf-8")
        self.assertNotIn("Phase 0 working specification", text)
        self.assertIn("Living verification is `tools/dev/`", text)
        self.assertIn("package-smoke deleted", text)
        self.assertNotIn("Actual package artifacts are part of the gate", text)
        self.assertRegex(text, r"\| PKG-01 \|.*\| HISTORICAL \|")

    def test_c6_heading_is_a_tombstone(self) -> None:
        text = (ROOT / "docs" / "current-state.md").read_text(encoding="utf-8")
        self.assertTrue(
            re.search(r"^### 9\.4 .+$", text, re.M),
            "path_b_done.py requires a ### 9.4 heading",
        )
        self.assertIn("### 9.4 Post-commit findings tombstone (C6)", text)
        self.assertNotIn("### 9.4 Post-commit findings still open (C6)", text)

    def test_bug_class_ordinal_matches_current_state(self) -> None:
        headings = re.findall(
            r"^### .*THE BUG CLASS, instance (\S+)",
            (ROOT / "docs" / "current-state.md").read_text(encoding="utf-8"),
            re.M,
        )
        self.assertTrue(headings, "current-state.md lost the bug-class heading")
        word = headings[-1]
        sites = [
            ROOT / "crates" / "protocol" / "src" / "v1.rs",
            ROOT / "crates" / "service" / "src" / "pool" / "mod.rs",
        ]
        restated = 0
        for path in sites:
            body = path.read_text(encoding="utf-8")
            restated += body.count(word)
            self.assertIn(
                word,
                body,
                f"{path} no longer restates the current-state ordinal {word}",
            )
        self.assertGreaterEqual(restated, 3)


if __name__ == "__main__":
    unittest.main()
