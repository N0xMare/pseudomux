#!/usr/bin/env python3
"""Every durability predicate in the pinned-worktree runner, driven until it refuses.

WHAT THIS IS ABOUT
------------------

`scripts/gate-in-worktree.sh` writes the one document that says which commit a
gate run graded. A certification ran both gates correctly and passed `--receipt`
an ephemeral path; the runs were fine and the receipts were reaped, and
`scripts/path-b-done.sh` then reported criterion 4 NOT MET with
`cells_executed=0` over 62 cells that had really run. So the runner now defaults
`--receipt` to a durable path, refuses the paths that cannot outlive a run, and
copies the files it hashes to where the receipt is. Each of those is a predicate,
and a predicate nobody has watched refuse is a predicate nobody has tested.

WHY A SYNTHETIC REPOSITORY
--------------------------

The runner derives the repository from its own location and adds a real
`git worktree` to it. Pointed at this tree it would check out commits beside the
one being edited, so each test copies the script into a repository of its own --
one commit, one tracked directory, one ignored one -- and drives it there. What
is under test is where the receipt goes and whether its evidence goes with it,
not what any gate measures, so the gate command is `/bin/sh`.

WHY THE SCRATCH TREE IS UNDER `target/`, AND WHY THAT IS NOT ENOUGH BY ITSELF
-----------------------------------------------------------------------------

Not under a temporary directory, and for the runner's own reasons. `--work-root`
is refused if any component of its path carries the sticky bit -- `/tmp` is mode
1777 on every POSIX host -- and `--receipt` is now refused anywhere under the
temporary directory, which is where the synthetic repository would otherwise
sit. `target/` is ignored by git, pruned by `scripts/gate-a-residue.sh`'s cache
scan, and excluded from the source digest.

`target/` is durable **relative to where the tree is checked out**, and that
sentence used to stop one clause early. The one thing that runs this suite is
`gate_f/gate_driver_self_tests`, and Gate A itself is run pinned:
`scripts/gate-in-worktree.sh` checks every gate out under `$TMPDIR/gate-worktrees`,
so inside a Gate A run `target/` -- and the whole repository above it -- IS under
the temporary directory. MEASURED, the first time Gate A ever ran this file:
**nine of these tests failed**, every message naming the temporary root the
repository itself was under. A suite whose subject is a durability predicate may
not inherit its answer from where somebody happened to check the tree out, so
every runner invocation below is handed [`RUNNER_TMPDIR`] and `setUp` proves it
is not an ancestor of the tree it is testing.

WHY HERE AND NOT UNDER `scripts/tests`
--------------------------------------

`scripts/tests` is reached by `cargo test` through
`crates/service/tests/register_currency_self_tests.rs`, which puts it inside
`pseudomux-service` -- a `--test-package` of the mutation campaign, so every
test there is paid for once per mutant across roughly 1,653 of them. These
tests cost seconds each because they build a repository and add a real
worktree. This directory is discovered by the one `gate_f` cell instead, which
runs once per gate.

Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/gate-a/tests -v
"""

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

REPOSITORY = pathlib.Path(__file__).resolve().parents[3]
RUNNER = REPOSITORY / "scripts" / "gate-in-worktree.sh"
DONE_GATE = REPOSITORY / "scripts" / "path-b-done.sh"
SCRATCH_ROOT = REPOSITORY / "target" / "gate-in-worktree-tests"
# What "temporary" means to every runner this file starts, and the reason it is
# not what it means to this process.
#
# `ephemeral_root_of` asks the ENVIRONMENT, twice, and refuses a receipt under
# either answer. Inside a Gate A run this whole tree is under the host's, so the
# runner would correctly refuse its own default receipt path and nine tests
# below would be measuring the checkout's location instead of the predicate they
# name. A SIBLING of the scratch and never an ancestor of it -- which
# `assert_scratch_outlives_a_run` checks of whatever this resolves to, rather
# than of the spelling written here.
RUNNER_TMPDIR = SCRATCH_ROOT / "tmpdir"
# One artefact and two logs: the run below writes exactly this, and the tests
# count what the receipt hashes against it rather than against a number.
GATE_COMMAND = (
    'printf "graded\\n" > {artefacts}/gate-a-receipt.json; echo on-stdout; '
    "echo on-stderr >&2"
)


def durable_environment(overrides: dict | None = None) -> dict[str, str]:
    """The environment a runner invocation gets, with the temporary root moved.

    `TMP` and `TEMP` are POPPED and not merely left alone: the runner consults
    all three names, so one this process inherited would put the scratch back
    under a refused root under another spelling. `overrides` goes on last,
    because the three refusal tests below are exactly the ones that must be
    able to point the runner back at a doomed directory on purpose.
    """

    environment = dict(os.environ)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["TMPDIR"] = str(RUNNER_TMPDIR)
    for name in ("TMP", "TEMP"):
        environment.pop(name, None)
    # The second constraint, and it is the opposite one: `--work-root` defaults
    # to `$TMPDIR/gate-worktrees`, and the runner refuses a work root INSIDE the
    # repository -- which `RUNNER_TMPDIR` is, being under `target/`. Every test
    # that runs a gate passes `--work-root` explicitly; the one that only asks
    # `--print-receipt-path` over THIS repository does not, so the default is
    # sent to a sibling of the tree. Nothing is created there, because that
    # query answers and exits before the runner makes anything.
    environment["PMUX_WORKTREE_ROOT"] = str(
        REPOSITORY.parent / f"{REPOSITORY.name}.gate-worktrees"
    )
    environment.update(overrides or {})
    return environment


def assert_scratch_outlives_a_run(case: unittest.TestCase, tree: pathlib.Path) -> None:
    """The arrangement every default-receipt assertion below is only valid under.

    A receipt refused for being under the temporary directory looks, from the
    assertion's side, exactly like the durability predicate failing -- so the
    arrangement is checked rather than assumed, and checked against RESOLVED
    paths, because `/tmp` and `/var` are symlinks on macOS and the runner
    resolves before it compares.
    """

    RUNNER_TMPDIR.mkdir(parents=True, exist_ok=True)
    temporary = RUNNER_TMPDIR.resolve()
    resolved = tree.resolve()
    case.assertNotIn(
        temporary,
        [resolved, *resolved.parents],
        f"{temporary} is {resolved} or an ancestor of it, so every receipt this "
        "suite asks for would be refused for being under the temporary "
        "directory -- which is the condition these tests run UNDERNEATH, not IN",
    )


def git(repository: pathlib.Path, *arguments: str) -> str:
    done = subprocess.run(
        ["git", "-C", str(repository), *arguments],
        capture_output=True,
        text=True,
        check=True,
    )
    return done.stdout.strip()


class PinnedRunnerTest(unittest.TestCase):
    """A repository, a work root and a runner, thrown away after each test."""

    def setUp(self) -> None:
        SCRATCH_ROOT.mkdir(parents=True, exist_ok=True)
        self.base = pathlib.Path(
            tempfile.mkdtemp(dir=SCRATCH_ROOT, prefix="pinned.")
        ).resolve()
        self.addCleanup(self.remove_scratch)
        self.repository = self.base / "repo"
        (self.repository / "scripts").mkdir(parents=True)
        (self.repository / "notes").mkdir()
        (self.repository / "notes" / "kept.md").write_text(
            "tracked\n", encoding="utf-8"
        )
        (self.repository / ".gitignore").write_text(".context/\n", encoding="utf-8")
        shutil.copy2(RUNNER, self.repository / "scripts" / RUNNER.name)
        git(self.repository, "init", "-q")
        git(self.repository, "config", "user.email", "runner@example.invalid")
        git(self.repository, "config", "user.name", "pinned runner tests")
        git(self.repository, "add", "-A")
        git(self.repository, "commit", "-q", "-m", "the commit under test")
        self.commit = git(self.repository, "rev-parse", "HEAD")
        self.work_root = self.base / "work"
        self.work_root.mkdir()
        assert_scratch_outlives_a_run(self, self.repository)

    def remove_scratch(self) -> None:
        # A test that deliberately leaves an unreadable file behind still has to
        # take its scratch with it, and `rmtree` cannot descend what it cannot
        # read.
        for path in sorted(self.base.rglob("*"), reverse=True):
            try:
                path.chmod(0o700)
            except OSError:
                pass
        shutil.rmtree(self.base, ignore_errors=True)

    def run_runner(
        self, *arguments: str, gate: str | None = None, environment: dict | None = None
    ) -> subprocess.CompletedProcess:
        command = [
            "bash",
            str(self.repository / "scripts" / RUNNER.name),
            "--commit",
            self.commit,
            "--work-root",
            str(self.work_root),
            *arguments,
        ]
        if gate is not None:
            command += ["--", "/bin/sh", "-c", gate]
        return subprocess.run(
            command,
            capture_output=True,
            text=True,
            cwd=str(self.base),
            env=durable_environment(environment),
        )

    def default_receipt(self, label: str = "gate") -> pathlib.Path:
        return (
            self.repository
            / ".context"
            / "gate-a"
            / f"pinned-receipt-{label}-{self.commit[:7]}.json"
        )

    # -- the default ------------------------------------------------------

    def test_the_default_receipt_is_durable_and_names_the_label_and_commit(
        self,
    ) -> None:
        """A caller who says nothing about `--receipt` still gets evidence."""

        for label in ("gate", "gate-a", "gate-b"):
            with self.subTest(label=label):
                arguments = [] if label == "gate" else ["--label", label]
                done = self.run_runner("--print-receipt-path", *arguments)
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertEqual(
                    pathlib.Path(done.stdout.strip()), self.default_receipt(label)
                )

    def test_the_printed_receipt_path_is_the_path_the_run_writes(self) -> None:
        """The one convention, asked twice.

        `scripts/path_b_done.py` prints `--print-receipt-path`'s answer as the
        file it wants. If that answer and the file a run actually writes could
        drift, the remedy in the done-gate's refusal would send a reader to a
        path nothing produces -- which is the whole defect one level up.
        """

        printed = self.run_runner("--print-receipt-path", "--label", "gate-a")
        self.assertEqual(printed.returncode, 0, printed.stderr)
        done = self.run_runner("--label", "gate-a", gate=GATE_COMMAND)
        self.assertEqual(done.returncode, 0, done.stderr)
        written = pathlib.Path(printed.stdout.strip())
        self.assertTrue(written.is_file(), f"{written} was printed but not written")
        self.assertEqual(
            json.loads(written.read_text())["describes_commit"], self.commit
        )

    # -- the refusals -----------------------------------------------------

    def test_a_receipt_under_the_environments_temporary_directory_is_refused(
        self,
    ) -> None:
        elsewhere = self.base / "elsewhere"
        elsewhere.mkdir()
        done = self.run_runner(
            "--receipt",
            str(elsewhere / "gate-a.json"),
            gate=GATE_COMMAND,
            environment={"TMPDIR": str(elsewhere)},
        )
        self.assertEqual(done.returncode, 2, done.stdout)
        self.assertIn("does not outlive the run it describes", done.stderr)
        self.assertIn(str(elsewhere), done.stderr)
        self.assertFalse((elsewhere / "gate-a.json").exists())

    def test_the_platform_temporary_directory_is_refused_under_any_tmpdir(self) -> None:
        """`/tmp` is doomed whether or not this shell has been pointed away from it.

        The runner asks `tempfile.gettempdir()` twice -- once as the environment
        stands and once with `TMPDIR`, `TMP` and `TEMP` removed -- so the second
        answer is the platform default underneath whatever a caller set. Here
        `TMPDIR` names a durable directory and the receipt is refused anyway.
        """

        elsewhere = self.base / "elsewhere"
        elsewhere.mkdir()
        # Asked of the standard library with the overrides removed, exactly as
        # the runner asks it. Writing `/tmp` here would be this test agreeing
        # with itself about a literal on one platform.
        stripped = {
            name: value
            for name, value in os.environ.items()
            if name not in ("TMPDIR", "TMP", "TEMP")
        }
        platform_default = pathlib.Path(
            subprocess.run(
                [sys.executable, "-c", "import tempfile; print(tempfile.gettempdir())"],
                capture_output=True,
                text=True,
                check=True,
                env=stripped,
            ).stdout.strip()
        ).resolve()
        self.assertNotEqual(platform_default, elsewhere.resolve())
        done = self.run_runner(
            "--receipt",
            str(platform_default / "pinned" / "gate-a.json"),
            gate=GATE_COMMAND,
            environment={"TMPDIR": str(elsewhere)},
        )
        self.assertEqual(done.returncode, 2, done.stdout)
        self.assertIn("does not outlive the run it describes", done.stderr)
        self.assertFalse((platform_default / "pinned").exists())

    def test_a_receipt_inside_the_work_root_the_run_removes_is_refused(self) -> None:
        """The work root here is durable; what is doomed is being under it.

        This isolates the second predicate from the first: nothing about this
        path is temporary except that the runner deletes the checkout beneath it
        and its work directory shares that fate.
        """

        doomed = self.work_root / "gate.0000000.XXXXXX" / "tree" / "receipt.json"
        done = self.run_runner("--receipt", str(doomed), gate=GATE_COMMAND)
        self.assertEqual(done.returncode, 2, done.stdout)
        self.assertIn("--work-root", done.stderr)
        self.assertIn("does not outlive the run it describes", done.stderr)
        self.assertFalse(doomed.exists())

    def test_a_receipt_the_repository_would_track_is_refused(self) -> None:
        """A receipt that dirties the tree costs the whole done-gate, not itself.

        `scripts/path-b-done.sh` exits 2 without a verdict when the working tree
        is dirty, so writing a receipt at a tracked path takes down criteria 2
        and 5 as well. Asked of `git check-ignore`, so the same file under the
        ignored directory is accepted -- the rule is about tracking, not about a
        directory name.
        """

        tracked = self.repository / "notes" / "receipt.json"
        done = self.run_runner("--receipt", str(tracked), gate=GATE_COMMAND)
        self.assertEqual(done.returncode, 2, done.stdout)
        self.assertIn("git does not ignore", done.stderr)
        self.assertFalse(tracked.exists())
        self.assertEqual(git(self.repository, "status", "--porcelain"), "")

        ignored = self.repository / ".context" / "gate-a" / "elsewhere.json"
        accepted = self.run_runner("--receipt", str(ignored), gate=GATE_COMMAND)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertTrue(ignored.is_file())

    # -- the evidence -----------------------------------------------------

    def test_the_files_the_receipt_hashes_outlive_the_work_directory(self) -> None:
        """The half the receipt cannot be evidence without.

        Every path the receipt hashes is re-read after the work directory it was
        produced in has been deleted, and the digests are recomputed rather than
        compared to themselves.
        """

        done = self.run_runner(gate=GATE_COMMAND)
        self.assertEqual(done.returncode, 0, done.stderr)
        receipt = json.loads(self.default_receipt().read_text(encoding="utf-8"))
        self.assertTrue(receipt["evidence_durable"])
        self.assertIsNone(receipt["evidence_fault"])
        self.assertTrue(receipt["worktree_removed"])

        work_directory = pathlib.Path(receipt["artefacts_dir"]).parent
        hashed = [receipt["stdout_log"], receipt["stderr_log"], *receipt["artefacts"]]
        self.assertEqual(len(hashed), 3, receipt)
        shutil.rmtree(work_directory)
        for entry in hashed:
            path = pathlib.Path(entry["path"])
            self.assertFalse(
                work_directory in path.parents,
                f"{path} is inside the work directory the run left behind",
            )
            self.assertTrue(path.is_file(), f"{path} did not outlive {work_directory}")
            # `hashlib` and not `shasum`: the runner hashes with `shasum`, and a
            # digest checked by the tool that produced it agrees with itself
            # even when that tool is the thing that is wrong.
            recomputed = hashlib.sha256(path.read_bytes()).hexdigest()
            self.assertEqual(recomputed, entry["sha256"], str(path))
            self.assertEqual(path.stat().st_size, entry["bytes"], str(path))
            # The original is still named, so a reader who kept the work
            # directory can tell the two copies apart.
            self.assertTrue(work_directory in pathlib.Path(entry["origin"]).parents)

    def test_a_receipt_whose_evidence_did_not_copy_says_so_and_still_parses(
        self,
    ) -> None:
        """The stated-rather-than-pretended branch, and the JSON it must remain.

        The forced fault is an artefact its own producer left unreadable, which
        is only a fault for a process that can be denied: this asserts that
        precondition on the host rather than assuming it, because a process
        running as root reads a mode-000 file and would measure nothing here.

        MEASURED: the first version of this branch emitted `"sha256": ""` and
        `"bytes": ` for the file it could not read, and the receipt stopped
        being parseable JSON -- so the failure it existed to report was hidden
        by a second one.
        """

        denied = self.base / "denied.probe"
        denied.write_text("probe\n", encoding="utf-8")
        denied.chmod(0o000)
        try:
            denied.read_bytes()
            readable_anyway = True
        except OSError:
            readable_anyway = False
        finally:
            denied.chmod(0o600)
        if readable_anyway:
            self.skipTest(
                "this process can read a mode-000 file, so no artefact on this host "
                "can be made uncopyable this way"
            )

        done = self.run_runner(
            gate=GATE_COMMAND + "; echo secret > {artefacts}/locked.json; "
            "chmod 000 {artefacts}/locked.json"
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("EVIDENCE IS NOT DURABLE", done.stdout)

        receipt = json.loads(self.default_receipt().read_text(encoding="utf-8"))
        self.assertFalse(receipt["evidence_durable"])
        self.assertIsNone(receipt["evidence_dir"])
        self.assertIn("could not copy", receipt["evidence_fault"])
        self.assertIn("WILL NOT OUTLIVE IT", receipt["reader_warning"])
        work_directory = pathlib.Path(receipt["artefacts_dir"])
        by_name = {
            pathlib.Path(entry["path"]).name: entry for entry in receipt["artefacts"]
        }
        self.assertEqual(sorted(by_name), ["gate-a-receipt.json", "locked.json"])
        for entry in by_name.values():
            self.assertTrue(work_directory in pathlib.Path(entry["path"]).parents)
        self.assertIsNotNone(by_name["gate-a-receipt.json"]["sha256"])
        self.assertIsNone(by_name["locked.json"]["sha256"])
        self.assertIsNone(by_name["locked.json"]["bytes"])


class DoneGateRemedyTest(unittest.TestCase):
    """Criterion 4's remedy against the runner it claims to be quoting.

    Over THIS repository, deliberately: the thing being checked is that the two
    scripts still agree about where a pinned receipt goes, and a synthetic
    repository would only prove they agree about a synthetic one.
    """

    def test_the_remedy_names_the_path_the_runner_would_write(self) -> None:
        # This one really is about THIS tree, so when Gate A runs it the tree in
        # question is a pinned worktree under the host's temporary directory and
        # both children below need the same moved root -- the done-gate because
        # it re-invokes the runner to build its remedy, and the runner because
        # it would otherwise refuse to name a path at all.
        assert_scratch_outlives_a_run(self, REPOSITORY)
        environment = durable_environment()
        head = git(REPOSITORY, "rev-parse", "HEAD")
        printed = subprocess.run(
            ["bash", str(RUNNER), "--print-receipt-path", "--commit", head],
            capture_output=True,
            text=True,
            cwd=str(REPOSITORY),
            env=environment,
            check=True,
        ).stdout.strip()
        done = subprocess.run(
            ["bash", str(DONE_GATE), "--only", "4"],
            capture_output=True,
            text=True,
            cwd=str(REPOSITORY),
            env=environment,
        )
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        remedies = [
            line.split("remedy: ", 1)[1]
            for line in done.stdout.splitlines()
            if "remedy: " in line
        ]
        self.assertTrue(remedies, done.stdout)
        self.assertTrue(
            any(printed in line for line in remedies),
            f"criterion 4 named no path the runner would write ({printed}):\n"
            + "\n".join(remedies),
        )
        self.assertTrue(
            any(line.startswith(f"bash scripts/{RUNNER.name} ") for line in remedies),
            "criterion 4 printed no command that would produce it:\n"
            + "\n".join(remedies),
        )


if __name__ == "__main__":
    unittest.main()
